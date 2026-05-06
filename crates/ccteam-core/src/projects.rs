//! Project bootstrap helpers used by `ccteam new` (and reusable by the
//! M3+ inbox triage path). Pure: no tmux side effects, just file
//! creation under `~/projects/<slug>/`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::templates::{
    team_bundle, write_global_helper_templates, write_project_phase_templates_for_team,
    write_project_settings,
};
use crate::phases::PhaseTemplate;
use crate::tool_surface::{
    link_recommended_agents_for_phases_into, user_claude_dir, AgentLinkAction, LinkOptions,
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
/// - `<project>/.ccteam/state.json` ← `ProjectState::initial_for_team(slug, team)`
/// - `<project>/.claude/settings.json` ← M0.4 template
/// - `<project>/CLAUDE.md` ← header + spec link
///
/// `team` lands in state.json so the orchestrator can route this
/// project through the matching phase set (M3.1 F12/F13).
///
/// Returns the full project directory path.
pub fn bootstrap_project(
    paths: &CcteamPaths,
    slug: &str,
    request: &str,
    team: &str,
) -> Result<PathBuf> {
    let project_dir = paths.project_dir(slug);
    let ccteam_dir = paths.project_ccteam_dir(slug);
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let spec_path = ccteam_dir.join("spec.md");
    let spec_body = format!(
        "---\nslug: {slug}\ncreated_at: {now}\nteam: {team}\n---\n\n# 用户需求\n\n{request}\n",
    );
    std::fs::write(&spec_path, spec_body)
        .with_context(|| format!("write {}", spec_path.display()))?;

    let state = ProjectState::initial_for_team(slug.to_string(), team.to_string());
    state.save(&paths.project_state(slug))?;

    write_project_settings(&project_dir)?;
    write_project_phase_templates_for_team(&project_dir, team)?;
    // M2.4: ensure ~/.ccteam/templates/ has the helper templates so
    // any phase markdown's `@~/.ccteam/templates/<name>.md` reference
    // resolves. Idempotent — no-ops when files already exist, so this
    // doesn't fight `ccteam init` if the operator ran it first.
    if let Err(err) = write_global_helper_templates(&paths.root, false) {
        tracing::warn!(
            global_dir = %paths.root.display(),
            error = %err,
            "could not stamp helper templates into ~/.ccteam/templates/; phase markdown using @-references may fail",
        );
    }
    if let Err(err) = pre_trust_project(&project_dir) {
        // Failing to pre-trust is annoying (next launch shows the
        // "Trust this folder?" prompt) but not fatal — log + continue.
        tracing::warn!(
            project_dir = %project_dir.display(),
            error = %err,
            "could not pre-trust project in ~/.claude.json; first claude launch may show trust prompt",
        );
    }

    // M0.5.1 / M0.5.2 / M2.1: register plugin agents under
    // ~/.claude/agents/ (recommended set + every sub_skill plugin agent
    // referenced in phase YAML) and pre-create the skills placeholder
    // directories. Both must be done **before** the orchestrator's
    // ensure_session triggers `tmux new-session`, since Claude Code
    // scans agents/ once at session start (claude-code-tool-surface.md
    // §1.2.5/6) and attaches the SKILL.md watcher only to dirs that
    // exist at startup (§1.2.4). bootstrap_project runs in `ccteam
    // new`, well before the daemon's ensure_session.
    let templates = load_phase_templates_for_bootstrap(team);
    if let Err(err) = setup_tool_surface(&project_dir, &templates) {
        tracing::warn!(
            project_dir = %project_dir.display(),
            error = %err,
            "tool-surface setup failed; phase markdown that depends on plugin agents may not work",
        );
    }

    let claude_md = project_dir.join("CLAUDE.md");
    if !claude_md.exists() {
        std::fs::write(&claude_md, render_project_claude_md(slug, team))
            .with_context(|| format!("write {}", claude_md.display()))?;
    }

    Ok(project_dir)
}

/// Build the `<project>/CLAUDE.md` body for `team`. dev keeps its
/// historical "no git push, tests must pass" wording; product-research
/// gets a research-specific contract; unknown teams get a generic
/// shell that doesn't bake in dev or research assumptions.
fn render_project_claude_md(slug: &str, team: &str) -> String {
    match team {
        "dev" => format!(
            "# CLAUDE.md (auto-managed by ccteam)\n\n## 项目上下文\n- slug: {slug}\n- team: dev\n- 用户原始需求: 见 .ccteam/spec.md\n\n## 工作约定\n- 不要交互式询问。所有决策已在 .ccteam/plan-eng.md 中。\n- 测试不过不算完成。\n\n## 不做的事\n- 不要 git push(被 hook 拦截)\n- 不要修改 .ccteam/ 之外的元数据\n",
        ),
        "product-research" => format!(
            "# CLAUDE.md (auto-managed by ccteam)\n\n## 项目上下文\n- slug: {slug}\n- team: product-research\n- 用户原始需求: 见 .ccteam/spec.md\n\n## 工作约定\n- 不写代码,只产研究报告。\n- 至少 3 个独立信息源,**不要编造数据**;不确定时标 \"未确认\"。\n- 决策走 outbox(`async` 模式)或 AskUserQuestion,**不要**自行假设关键事实。\n\n## 不做的事\n- 不要修改 .ccteam/ 之外的元数据。\n- 不要在 verdict 之前下定论 — 让 phase DAG 走完。\n- 不要把 dev 的 \"测试不过不算完成\" 套用到本项目;研究报告的 done 是 verdict.md 写出 + rationale.md 自洽。\n",
        ),
        _ => format!(
            "# CLAUDE.md (auto-managed by ccteam)\n\n## 项目上下文\n- slug: {slug}\n- team: {team}\n- 用户原始需求: 见 .ccteam/spec.md\n\n## 工作约定\n- 跟随该 team 的 phase 模板指示。\n- 不要修改 .ccteam/ 之外的元数据。\n",
        ),
    }
}

/// Parse the embedded phase templates for `team` into a
/// `Vec<PhaseTemplate>` so `setup_tool_surface` can ln -sf every
/// sub_skill plugin agent the team's pipeline declares. Filter out
/// parse errors with a warn — a broken shipped template would crash
/// the build, but if it ever happens we want bootstrap to keep
/// working for unrelated phases.
///
/// Returns empty for the meta-agent team (no DAG) and for unknown
/// teams (the user is expected to populate `~/.ccteam/<phase_dir>/`
/// manually for hand-rolled teams).
fn load_phase_templates_for_bootstrap(team: &str) -> Vec<PhaseTemplate> {
    let Some(bundle) = team_bundle(team) else {
        return Vec::new();
    };
    bundle
        .phases
        .iter()
        .filter_map(|(name, body)| match PhaseTemplate::parse(body) {
            Ok(t) => Some(t),
            Err(err) => {
                tracing::warn!(
                    template = %name,
                    team,
                    error = %err,
                    "embedded phase template did not parse during bootstrap; skipping for sub_skill linking",
                );
                None
            }
        })
        .collect()
}

/// Pre-mark `project_dir` as trusted in `~/.claude.json` so the first
/// `claude --dangerously-skip-permissions` launch in this directory
/// doesn't sit on the "Trust this folder?" prompt waiting for the
/// keyboard.
///
/// **Test isolation** (two opt-in mechanisms — same shape as
/// `setup_tool_surface`):
///
/// 1. `CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP=1` → no-op entirely.
///    Tests that exercise `bootstrap_project` for unrelated assertions
///    set this via `disable_tool_surface_bootstrap_for_tests()` and
///    don't want either the agent symlinks or the trust entry leaking
///    into the developer's real home.
/// 2. `CLAUDE_CONFIG_HOME=<dir>` → write to `<dir>/../.claude.json`
///    instead of `$HOME/.claude.json`. Mirrors the resolution
///    `user_claude_dir()` does for the agents/skills surface so a test
///    setting just `CLAUDE_CONFIG_HOME=<tmp>/.claude` gets full
///    redirection across the whole tool surface.
///
/// Without these guards, every test invoking `bootstrap_project` would
/// append a `/tmp/.tmpXXXXXX/projects/<slug>` entry to the developer's
/// real `~/.claude.json`, eventually bloating the file enough to break
/// Claude login (regression observed 2026-05-06).
///
/// No-ops gracefully if the resolved `.claude.json` is unwritable.
pub fn pre_trust_project(project_dir: &Path) -> Result<()> {
    if std::env::var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    {
        tracing::debug!(
            "CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP set; skipping ~/.claude.json trust entry write",
        );
        return Ok(());
    }
    let claude_json = resolve_claude_json_path()?;
    write_trust_entry(&claude_json, project_dir)
}

/// Resolve which `.claude.json` to write the trust entry into.
///
/// `CLAUDE_CONFIG_HOME` takes precedence so the production-equivalent
/// isolation tests (`tool_surface_e2e_test`-style) get a redirected
/// trust write too. Production sets neither and falls through to
/// `dirs::home_dir()`.
fn resolve_claude_json_path() -> Result<PathBuf> {
    resolve_claude_json_path_from_env(
        std::env::var("CLAUDE_CONFIG_HOME").ok(),
        dirs::home_dir(),
    )
}

/// Pure resolution helper for `resolve_claude_json_path`. Factored out
/// so unit tests can exercise the path logic without mutating process
/// env vars (which would race against parallel tests in the same
/// binary).
fn resolve_claude_json_path_from_env(
    config_home: Option<String>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(s) = config_home {
        let claude_dir = PathBuf::from(s);
        // CLAUDE_CONFIG_HOME points at the `.claude/` dir; `.claude.json`
        // is its sibling. If the env var has no parent (root path,
        // weird input), fall back to writing inside it — better than
        // silently touching the real home.
        return Ok(match claude_dir.parent() {
            Some(parent) => parent.join(".claude.json"),
            None => claude_dir.join(".claude.json"),
        });
    }
    let h = home
        .ok_or_else(|| anyhow!("could not resolve home directory for ~/.claude.json"))?;
    Ok(h.join(".claude.json"))
}

/// Symlink the `RECOMMENDED_AGENTS` set + every plugin agent named in
/// `templates`' `sub_skills` into `~/.claude/agents/`, then pre-create
/// the global + project-local skills placeholder dirs. See the call
/// site in `bootstrap_project` for the timing constraint.
///
/// **Test isolation**: tests that call `bootstrap_project` but don't
/// want to mutate the developer's real `~/.claude/` should set the
/// env var `CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP=1` (or set
/// `CLAUDE_CONFIG_HOME` to a tempdir and let it run). Production
/// never sets the disable flag, so the only way for the symlinks to
/// land is through bootstrap_project.
///
/// The project-local `<project>/.claude/skills/` placeholder is
/// created unconditionally — it lives under `project_dir` (a
/// tempdir during tests) and carries no global-pollution risk.
fn setup_tool_surface(project_dir: &Path, templates: &[PhaseTemplate]) -> Result<()> {
    // Project-local placeholder always created — see doc comment.
    let project_skills = project_dir.join(".claude").join("skills");
    std::fs::create_dir_all(&project_skills)
        .with_context(|| format!("create {}", project_skills.display()))?;

    if std::env::var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    {
        tracing::debug!(
            "CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP set; skipping global ~/.claude/ tool-surface setup",
        );
        return Ok(());
    }
    let claude = user_claude_dir()?;
    let reports = link_recommended_agents_for_phases_into(
        &claude,
        templates,
        LinkOptions::default(),
    )?;
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
            AgentLinkAction::Replaced { previous_target } => tracing::info!(
                agent = r.agent.filename,
                previous = %previous_target.display(),
                "replaced foreign symlink with plugin source (force=true)",
            ),
            AgentLinkAction::Kept { previous_target } => tracing::warn!(
                agent = r.agent.filename,
                previous = %previous_target.display(),
                "agent symlink points elsewhere — `Task(subagent_type=...)` will hit the foreign target. \
                 run `ccteam doctor --install-recommended-agents --force` to replace.",
            ),
            AgentLinkAction::SkippedUserFile => tracing::warn!(
                agent = r.agent.filename,
                target = %r.target.display(),
                "user-authored agent file at target; not overwriting (use `ccteam doctor --install-recommended-agents --force` to replace)",
            ),
            AgentLinkAction::SkippedSourceMissing { source } => tracing::warn!(
                agent = r.agent.filename,
                source = %source.display(),
                "plugin source missing — install claude-plugins-official to enable this agent",
            ),
            AgentLinkAction::DryRun { .. } => {}
        }
    }
    // Global ~/.claude/skills/ — only when not in test-disable mode
    // (covered above). Project-local skills dir was already created
    // unconditionally at function entry.
    let global_skills = claude.join("skills");
    std::fs::create_dir_all(&global_skills)
        .with_context(|| format!("create {}", global_skills.display()))?;
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
    use std::sync::OnceLock;

    use super::*;

    static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
    fn ensure_isolation() {
        DISABLE_TOOL_SURFACE
            .get_or_init(crate::tool_surface::disable_tool_surface_bootstrap_for_tests);
    }

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

    // ---- resolution logic: pure-function tests, no env mutation ----

    #[test]
    fn resolve_claude_json_path_honors_claude_config_home() {
        // CLAUDE_CONFIG_HOME points at the .claude/ dir; .claude.json is
        // its sibling. Resolution must redirect there (mirroring
        // user_claude_dir's CLAUDE_CONFIG_HOME handling).
        let resolved = resolve_claude_json_path_from_env(
            Some("/some/test/.claude".to_string()),
            Some(PathBuf::from("/should/not/be/used")),
        )
        .unwrap();
        assert_eq!(resolved, std::path::Path::new("/some/test/.claude.json"));
    }

    #[test]
    fn resolve_claude_json_path_handles_claude_config_home_at_root() {
        // Defensive: CLAUDE_CONFIG_HOME without a parent (e.g. exactly
        // "/") falls back to writing inside the dir rather than silently
        // resolving to "/.claude.json" (which would still be on the
        // wrong filesystem from the user's perspective).
        let resolved = resolve_claude_json_path_from_env(
            Some("/".to_string()),
            Some(PathBuf::from("/home/rob")),
        )
        .unwrap();
        // "/" has parent = None per std::path semantics, so we expect
        // the inner-dir join.
        assert_eq!(resolved, std::path::Path::new("/.claude.json"));
    }

    #[test]
    fn resolve_claude_json_path_falls_back_to_home_when_env_unset() {
        let resolved =
            resolve_claude_json_path_from_env(None, Some(PathBuf::from("/home/rob"))).unwrap();
        assert_eq!(resolved, std::path::Path::new("/home/rob/.claude.json"));
    }

    #[test]
    fn resolve_claude_json_path_errors_when_neither_available() {
        let err = resolve_claude_json_path_from_env(None, None).unwrap_err();
        assert!(format!("{err:#}").contains("home directory"));
    }

    // ---- side-effect guards ----
    //
    // The full "bootstrap_project doesn't write to real ~/.claude.json"
    // assertion lives in `crates/ccteam-core/tests/tool_surface_e2e_test.rs`
    // where CLAUDE_CONFIG_HOME redirection runs in its own test binary
    // process — there it's safe to mutate the env var because no
    // other test in the same binary reads it concurrently.
    //
    // Inline tests below verify the *guard logic* without mutating any
    // process-wide env var, since `bootstrap_project_*` siblings in
    // this same binary read CLAUDE_CONFIG_HOME via bootstrap_project →
    // pre_trust_project → resolve_claude_json_path; an env_lock here
    // would protect our tests from each other but not from those
    // siblings, which is precisely the race condition that broke an
    // earlier draft of these tests.

    #[test]
    fn disable_flag_recognized_when_ensure_isolation_ran() {
        // Regression hook for the 2026-05-06 ~/.claude.json bloat: the
        // disable flag was wired only into setup_tool_surface, not
        // pre_trust_project. The unit-level guarantee we want is that
        // ensure_isolation() — which all bootstrap_project-touching
        // tests call — surfaces a `true` from the same env-var check
        // pre_trust_project uses.
        ensure_isolation();
        let v = std::env::var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP").ok();
        assert!(
            matches!(v.as_deref(), Some("1") | Some("true") | Some("yes")),
            "ensure_isolation must set CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP \
             to a truthy value the disable check recognizes; got {v:?}",
        );
    }

    #[test]
    fn bootstrap_project_writes_phase_templates_into_dot_ccteam_phases() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let slug = "demo";
        bootstrap_project(&paths, slug, "demo request", "dev").unwrap();
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
        // attaches there. The project-local mkdir runs **before** the
        // tool-surface gate, so it works even with the disable flag set
        // (other tests in this binary may have set it).
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let skills = paths.project_dir("demo").join(".claude/skills");
        assert!(
            skills.is_dir(),
            "expected {} to exist after bootstrap_project",
            skills.display(),
        );
    }

    #[test]
    fn bootstrap_project_writes_helper_templates_to_global_dir() {
        // M2.4: bootstrap_project ensures ~/.ccteam/templates/ is
        // populated with the embedded helper templates so phase
        // markdown's `@~/.ccteam/templates/<name>` reference resolves
        // even when the user skipped `ccteam init`.
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let templates = paths.root.join("templates");
        assert!(
            templates.join("review-with-user-loop.md").is_file(),
            "review-with-user-loop.md missing from {}",
            templates.display(),
        );
        assert!(
            templates.join("kickoff-reverse-interview.md").is_file(),
            "kickoff-reverse-interview.md missing",
        );
    }

    #[test]
    fn bootstrap_project_helper_templates_do_not_overwrite_user_edits() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // First bootstrap stamps fresh templates.
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let path = paths.root.join("templates/review-with-user-loop.md");
        std::fs::write(&path, "USER EDIT\n").unwrap();
        // Second project bootstrap (e.g. user runs `ccteam new` again)
        // must not clobber the user edit.
        bootstrap_project(&paths, "demo2", "another request", "dev").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT\n");
    }

    #[test]
    fn bootstrap_project_settings_uses_absolute_ccteam_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
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
