//! Project bootstrap helpers used by `ccteam new` (and reusable by the
//! M3+ inbox triage path). Pure: no tmux side effects, just file
//! creation under `~/projects/<slug>/`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::templates::{write_project_phase_templates, write_project_settings};
use crate::tool_surface::{
    ensure_skills_placeholders, link_recommended_agents_into, user_claude_dir,
    AgentLinkAction, LinkOptions,
};

/// Slugify a free-text project request: keep `[a-z0-9]`, collapse other
/// runs to `-`, trim, lower-case, and cap at 40 chars. When the cap
/// would split a word, the slug is rolled back to the previous `-` so
/// e.g. "Build a tiny Python CLI that converts CSV to JSON" stays
/// `build-a-tiny-python-cli-that-converts` rather than `...converts-cs`.
/// Empty result is replaced by `project`.
pub fn slugify(input: &str) -> String {
    const MAX: usize = 40;
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return "project".into();
    }
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    // Hard cap at MAX, then if we cut mid-word roll back to the
    // previous `-` so we don't ship a half-token like `converts-cs`.
    // Single tokens longer than MAX (e.g. `aaaa…`) keep the hard cut
    // since there's no boundary to fall back to.
    let head = &trimmed[..MAX];
    let trimmed_head = match head.rfind('-') {
        Some(idx) if idx > 0 => &head[..idx],
        _ => head,
    };
    trimmed_head.trim_end_matches('-').to_string()
}

/// 4-char hex suffix derived from the current sub-second wallclock.
/// Good enough for collision avoidance under interactive use; the
/// caller can retry on collision.
pub fn random_suffix() -> String {
    let nanos = Utc::now().timestamp_subsec_nanos();
    format!("{:04x}", nanos & 0xFFFF)
}

/// Pick an unused slug under `paths.projects_root`. Tries the bare
/// slugified base first, then `<base>-<suffix>` with up to 16 retries.
pub fn pick_unused_slug(paths: &CcteamPaths, base: &str) -> Result<String> {
    let base = slugify(base);
    if !paths.project_dir(&base).exists() {
        return Ok(base);
    }
    for _ in 0..16 {
        let candidate = format!("{base}-{}", random_suffix());
        if !paths.project_dir(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "could not pick an unused slug after 16 attempts (base: {base})",
    ))
}

/// Write the bootstrap files for a fresh project:
/// - `<project>/.ccteam/spec.md` ← `request`
/// - `<project>/.ccteam/state.json` ← `ProjectState::initial`
/// - `<project>/.claude/settings.json` ← M0.4 template
/// - `<project>/CLAUDE.md` ← header + spec link
///
/// Returns the full project directory path.
pub fn bootstrap_project(
    paths: &CcteamPaths,
    slug: &str,
    request: &str,
) -> Result<PathBuf> {
    let project_dir = paths.project_dir(slug);
    let ccteam_dir = paths.project_ccteam_dir(slug);
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let spec_path = ccteam_dir.join("spec.md");
    let spec_body = format!(
        "---\nslug: {slug}\ncreated_at: {now}\n---\n\n# 用户需求\n\n{request}\n",
    );
    std::fs::write(&spec_path, spec_body)
        .with_context(|| format!("write {}", spec_path.display()))?;

    let state = ProjectState::initial(slug.to_string());
    state.save(&paths.project_state(slug))?;

    write_project_settings(&project_dir)?;
    write_project_phase_templates(&project_dir)?;
    if let Err(err) = pre_trust_project(&project_dir) {
        // Failing to pre-trust is annoying (next launch shows the
        // "Trust this folder?" prompt) but not fatal — log + continue.
        tracing::warn!(
            project_dir = %project_dir.display(),
            error = %err,
            "could not pre-trust project in ~/.claude.json; first claude launch may show trust prompt",
        );
    }

    // M0.5.1 / M0.5.2: register plugin agents under ~/.claude/agents/
    // and pre-create the skills placeholder directories. Both must be
    // done **before** the orchestrator's ensure_session triggers
    // `tmux new-session`, since Claude Code scans agents/ once at
    // session start (per claude-code-tool-surface.md §1.2.5/6) and
    // attaches the SKILL.md watcher only to dirs that exist at startup
    // (§1.2.4). bootstrap_project runs in `ccteam new`, well before
    // the daemon's ensure_session, so we're inside the safe window.
    if let Err(err) = setup_tool_surface(&project_dir) {
        tracing::warn!(
            project_dir = %project_dir.display(),
            error = %err,
            "tool-surface setup failed; phase markdown that depends on plugin agents may not work",
        );
    }

    let claude_md = project_dir.join("CLAUDE.md");
    if !claude_md.exists() {
        let body = format!(
            "# CLAUDE.md (auto-managed by ccteam)\n\n## 项目上下文\n- slug: {slug}\n- 用户原始需求: 见 .ccteam/spec.md\n\n## 工作约定\n- 不要交互式询问。所有决策已在 .ccteam/plan-eng.md 中。\n- 测试不过不算完成。\n\n## 不做的事\n- 不要 git push(被 hook 拦截)\n- 不要修改 .ccteam/ 之外的元数据\n",
        );
        std::fs::write(&claude_md, body)
            .with_context(|| format!("write {}", claude_md.display()))?;
    }

    Ok(project_dir)
}

/// Pre-mark `project_dir` as trusted in `~/.claude.json` so the first
/// `claude --dangerously-skip-permissions` launch in this directory
/// doesn't sit on the "Trust this folder?" prompt waiting for the
/// keyboard. Honors `$HOME` (and the runner's `dirs::home_dir()`
/// fallback). No-ops gracefully if `~/.claude.json` is unwritable.
pub fn pre_trust_project(project_dir: &Path) -> Result<()> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory for ~/.claude.json"))?;
    let claude_json = home.join(".claude.json");
    write_trust_entry(&claude_json, project_dir)
}

/// Symlink the `RECOMMENDED_AGENTS` set into `~/.claude/agents/` and
/// pre-create the global + project-local skills placeholder dirs. See
/// the call site in `bootstrap_project` for the timing constraint.
fn setup_tool_surface(project_dir: &Path) -> Result<()> {
    let claude = user_claude_dir()?;
    let reports = link_recommended_agents_into(&claude, LinkOptions::default())?;
    for r in &reports {
        match &r.action {
            AgentLinkAction::Linked => tracing::info!(
                agent = r.agent.filename,
                target = %r.target.display(),
                "linked plugin agent into ~/.claude/agents/",
            ),
            AgentLinkAction::AlreadyLinked => tracing::debug!(
                agent = r.agent.filename,
                "plugin agent symlink already in place",
            ),
            AgentLinkAction::SkippedUserFile => tracing::warn!(
                agent = r.agent.filename,
                target = %r.target.display(),
                "user-authored agent file at target; not overwriting (use `ccteam doctor --install-recommended-agents --force` to replace)",
            ),
            AgentLinkAction::Replaced { previous_target } => tracing::warn!(
                agent = r.agent.filename,
                previous = %previous_target.display(),
                "agent symlink already pointed elsewhere; not replacing without --force",
            ),
            AgentLinkAction::SkippedSourceMissing { source } => tracing::warn!(
                agent = r.agent.filename,
                source = %source.display(),
                "plugin source missing — install claude-plugins-official to enable this agent",
            ),
            AgentLinkAction::DryRun { .. } => {}
        }
    }
    ensure_skills_placeholders(&claude, project_dir)?;
    Ok(())
}

/// `pre_trust_project` core, factored out for unit testing with an
/// injected `~/.claude.json` location.
pub(crate) fn write_trust_entry(claude_json: &Path, project_dir: &Path) -> Result<()> {
    let project_key = project_dir
        .to_str()
        .ok_or_else(|| anyhow!("project_dir not valid UTF-8: {}", project_dir.display()))?;

    let mut root = if claude_json.exists() {
        let bytes = std::fs::read(claude_json)
            .with_context(|| format!("read {}", claude_json.display()))?;
        let v: Value = if bytes.is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", claude_json.display()))?
        };
        match v {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    } else {
        Map::new()
    };

    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    let projects_map = match projects {
        Value::Object(m) => m,
        // someone (or a corrupted file) put a non-object at `projects` —
        // overwrite rather than refusing, since refusing means every
        // future launch sits at the trust prompt.
        _ => {
            *projects = Value::Object(Map::new());
            projects.as_object_mut().unwrap()
        }
    };

    let entry = projects_map
        .entry(project_key)
        .or_insert_with(|| Value::Object(Map::new()));
    let entry_map = match entry {
        Value::Object(m) => m,
        _ => {
            *entry = Value::Object(Map::new());
            entry.as_object_mut().unwrap()
        }
    };
    entry_map.insert("hasTrustDialogAccepted".into(), Value::Bool(true));

    let body = serde_json::to_string_pretty(&Value::Object(root))
        .context("serialize ~/.claude.json")?;

    if let Some(parent) = claude_json.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = {
        let mut s = claude_json.as_os_str().to_owned();
        s.push(".ccteam.tmp");
        PathBuf::from(s)
    };
    std::fs::write(&tmp, body.as_bytes())
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, claude_json)
        .with_context(|| format!("rename {} → {}", tmp.display(), claude_json.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_keeps_alphanumeric_lowercase() {
        assert_eq!(slugify("Hello World 123"), "hello-world-123");
        assert_eq!(slugify("Bookmark Manager (PWA)"), "bookmark-manager-pwa");
        assert_eq!(slugify("--leading-and-trailing--"), "leading-and-trailing");
        assert_eq!(slugify("multiple   spaces"), "multiple-spaces");
        assert_eq!(slugify("CamelCaseName"), "camelcasename");
    }

    #[test]
    fn slugify_falls_back_to_project_for_empty_input() {
        assert_eq!(slugify(""), "project");
        assert_eq!(slugify("中文 only"), "only");
    }

    #[test]
    fn slugify_truncates_to_40_chars() {
        let long = "a".repeat(80);
        assert!(slugify(&long).len() <= 40);
    }

    #[test]
    fn slugify_rolls_back_to_dash_boundary_when_cut_would_split_word() {
        // Without rollback this would yield `build-a-tiny-python-cli-that-converts-cs`
        // (40 chars) — a half token. With rollback we drop `cs` and keep the slug
        // ending on the prior `-`.
        let s = slugify("Build a tiny Python CLI that converts CSV to JSON");
        assert!(
            s.len() <= 40,
            "slug must respect 40-char cap, got len={}: {s}",
            s.len()
        );
        assert!(
            !s.ends_with("-cs"),
            "slug should roll back past the half-token, got: {s}",
        );
        assert!(
            s.ends_with("converts"),
            "expected slug to end at the dash boundary `converts`, got: {s}",
        );
    }

    #[test]
    fn slugify_keeps_single_long_token_truncated() {
        // No `-` to fall back to, so a single megaword keeps the hard cap.
        let s = slugify(&"a".repeat(80));
        assert_eq!(s.len(), 40);
        assert!(s.chars().all(|c| c == 'a'));
    }

    #[test]
    fn write_trust_entry_creates_file_with_project_marked_trusted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        let project = tmp.path().join("projects/abc");
        std::fs::create_dir_all(&project).unwrap();

        write_trust_entry(&claude_json, &project).unwrap();
        let body = std::fs::read_to_string(&claude_json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let key = project.to_str().unwrap();
        assert_eq!(
            v["projects"][key]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "expected projects[{key}].hasTrustDialogAccepted=true; got {body}",
        );
    }

    #[test]
    fn write_trust_entry_preserves_existing_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        // Pre-existing config with another project + unrelated top-level keys.
        std::fs::write(
            &claude_json,
            r#"{
              "userID": "rob",
              "projects": {
                "/some/other/project": {"hasTrustDialogAccepted": true, "extra": 7}
              }
            }"#,
        )
        .unwrap();

        let project = tmp.path().join("projects/new");
        std::fs::create_dir_all(&project).unwrap();
        write_trust_entry(&claude_json, &project).unwrap();

        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["userID"], "rob");
        assert_eq!(
            v["projects"]["/some/other/project"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
        assert_eq!(v["projects"]["/some/other/project"]["extra"], 7);
        let key = project.to_str().unwrap();
        assert_eq!(
            v["projects"][key]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
    }

    #[test]
    fn bootstrap_project_writes_phase_templates_into_dot_ccteam_phases() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let slug = "demo";
        bootstrap_project(&paths, slug, "demo request").unwrap();
        let phases_dir = paths.project_dir(slug).join(".ccteam/phases");
        assert!(phases_dir.join("plan-eng.md").exists());
        assert!(phases_dir.join("implement.md").exists());
        assert!(phases_dir.join("ship.md").exists());
        // No prefixed copies — those live in ~/.ccteam/phases/, not the
        // project tree (which is what the phase prompt references).
        assert!(!phases_dir.join("02-plan-eng.md").exists());
    }

    #[test]
    fn bootstrap_project_creates_project_local_skills_placeholder() {
        // M0.5.2: the project-side `<project>/.claude/skills/` dir must
        // exist at session start so Claude Code's live SKILL.md monitor
        // attaches there. Independent of the global `~/.claude/skills/`
        // path (which `setup_tool_surface` also creates but is harder
        // to assert here without env-var fiddling).
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request").unwrap();
        let skills = paths.project_dir("demo").join(".claude/skills");
        assert!(
            skills.is_dir(),
            "expected {} to exist after bootstrap_project",
            skills.display(),
        );
    }

    #[test]
    fn bootstrap_project_settings_uses_absolute_ccteam_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request").unwrap();
        let settings = paths.project_dir("demo").join(".claude/settings.json");
        let body = std::fs::read_to_string(&settings).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            cmd.starts_with('/'),
            "settings.json hook command must be an absolute path, got: {cmd}",
        );
        assert!(
            cmd.ends_with(" hook load-context"),
            "settings.json hook command should still invoke `hook load-context`, got: {cmd}",
        );
        // Reject the placeholder having survived the render — that's the
        // PATH-dependent failure mode we're guarding against.
        assert!(
            !cmd.contains("__CCTEAM_BIN__"),
            "settings.json placeholder should be substituted, got: {cmd}",
        );
    }
}
