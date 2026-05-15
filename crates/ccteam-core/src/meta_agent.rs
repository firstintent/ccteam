//! Meta-agent session bootstrap.
//!
//! The meta-agent is a single ccteam-managed long tmux session under
//! `~/projects/meta/` that the human attaches to with
//! `tmux attach -t ccteam-meta`. There is exactly one meta-agent per
//! ccteam install — no per-user multiplexing — so all paths and the
//! tmux session name are now literal.
//!
//! Historical context: V0.1–V0.4.0 carried a `<user_handle>` parameter
//! threaded through every meta-agent helper (`meta_slug("rob")` →
//! `"meta-rob"`, etc.), reserved for an imagined multi-user channel
//! adapter. In practice the channel adapter routes by message
//! `source_user` (inbox front matter), not by the meta-agent slug, so
//! the parameter was dead weight. V0.4.1 drops it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};

use crate::inbox::SessionMailbox;
use crate::paths::CcteamPaths;
use crate::projects::bootstrap_project;
use crate::state::ProjectState;

/// Canonical team name for the user-facing NL dispatcher session.
pub const META_TEAM_NAME: &str = "meta-agent";

/// Canonical meta-agent project slug. `~/projects/meta/` is the
/// canonical filesystem location.
pub const META_SLUG: &str = "meta";

/// tmux session name for the meta-agent. There's no `<user>` suffix
/// — one ccteam install ⇒ one meta-agent.
pub const META_SESSION_NAME: &str = "ccteam-meta";

/// Embedded role prompt template. Sourced from
/// `crates/ccteam-core/src/templates/meta_agent_role.md`.
const META_ROLE_PROMPT_TEMPLATE: &str = include_str!("templates/meta_agent_role.md");

/// Build the meta-agent's canonical project slug. Always `"meta"`.
/// Returns `String` (not `Result`) since there's nothing to validate.
pub fn meta_slug() -> String {
    META_SLUG.to_string()
}

/// `ccteam-meta` tmux session name.
pub fn meta_session_name() -> String {
    META_SESSION_NAME.to_string()
}

/// Render the role prompt. The template uses fixed `~/projects/meta/`
/// references; `__GENERATED_AT__` carries the bootstrap timestamp so
/// the operator can see when the role was last (re)written.
pub fn render_meta_role_prompt() -> String {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    // V0.4.1: legacy `__USER_HANDLE__` placeholder is replaced with
    // empty so any stale template fragments still get a clean output.
    META_ROLE_PROMPT_TEMPLATE
        .replace("meta-__USER_HANDLE__", META_SLUG)
        .replace("__USER_HANDLE__", "")
        .replace("__GENERATED_AT__", &now)
}

/// Sweep `paths.projects_root` for legacy V0.1-V0.4.0 meta layouts
/// (`meta-<handle>/`, e.g. `meta-rob`, `meta-cto`) and remove them.
///
/// Marker-checked: only deletes a directory if its `state.json::team`
/// equals [`META_TEAM_NAME`]. Random user dirs that happen to start
/// with `meta-` are left alone. The canonical `meta/` directory is
/// never touched. Returns the removed paths so callers can surface a
/// summary line.
///
/// Per CLAUDE.md §三 we don't kill any tmux sessions named after the
/// removed layout (e.g. `ccteam-meta-cto`) — those are still owned by
/// the user and will become orphaned but harmless after this sweep.
pub fn clean_stale_meta_layouts(paths: &CcteamPaths) -> Result<Vec<PathBuf>> {
    let root = &paths.projects_root;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for entry in
        std::fs::read_dir(root).with_context(|| format!("read_dir {}", root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if name == META_SLUG || !name.starts_with("meta-") {
            continue;
        }
        let state_path = paths.project_state(&name);
        if !state_path.exists() {
            continue;
        }
        let Ok(state) = ProjectState::load(&state_path) else {
            continue;
        };
        if state.team != META_TEAM_NAME {
            continue;
        }
        let dir = entry.path();
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove stale meta layout {}", dir.display()))?;
        removed.push(dir);
    }
    Ok(removed)
}

/// Bootstrap the meta-agent's project tree:
/// 1. Sweep legacy `meta-<handle>/` layouts (V0.4.1: one ccteam install
///    = one meta-agent; old per-handle directories are dead).
/// 2. Run `bootstrap_project(...team=meta-agent)` to lay down
///    `state.json`, `.claude/settings.json`, and the project skeleton.
/// 3. Overwrite the auto-generated `CLAUDE.md` with the meta-agent
///    role prompt so the new session loads dispatcher behavior.
/// 4. Pre-create `inbox/` + `outbox/` so an inotify watcher can attach
///    immediately at session startup.
///
/// Idempotent: re-running refreshes the CLAUDE.md role prompt + the
/// canonical state.json fields (slug, team, tmux_session) so doctor can
/// repair drift in-place.
pub fn bootstrap_meta_project(paths: &CcteamPaths) -> Result<MetaBootstrapReport> {
    let removed_stale =
        clean_stale_meta_layouts(paths).context("sweep stale meta-<handle> layouts")?;
    let slug = meta_slug();
    let project_dir = paths.project_dir(&slug);
    let already_existed = paths.project_state(&slug).exists();

    let request = format!(
        "ccteam meta-agent session. \
         Dispatch incoming requests to the right team via `ccteam new`.\n\
         Generated: {}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    );

    if !already_existed {
        bootstrap_project(paths, &slug, &request, META_TEAM_NAME)
            .context("bootstrap meta-agent project tree")?;
    }
    let state_path = paths.project_state(&slug);
    let mut state = ProjectState::load(&state_path)
        .with_context(|| format!("reload meta state {}", state_path.display()))?;
    state.slug = slug.clone();
    state.team = META_TEAM_NAME.into();
    state.tmux_session = meta_session_name();
    state.save(&state_path)?;

    let claude_md = project_dir.join("CLAUDE.md");
    std::fs::write(&claude_md, render_meta_role_prompt())
        .with_context(|| format!("write {}", claude_md.display()))?;

    let mailbox = SessionMailbox::for_ccteam_dir(&paths.project_ccteam_dir(&slug));
    mailbox.ensure_dirs()?;

    Ok(MetaBootstrapReport {
        slug,
        project_dir,
        claude_md,
        already_existed,
        removed_stale,
    })
}

/// Result of `bootstrap_meta_project` — useful for the doctor command's
/// human-readable report.
#[derive(Debug, Clone)]
pub struct MetaBootstrapReport {
    pub slug: String,
    pub project_dir: PathBuf,
    pub claude_md: PathBuf,
    pub already_existed: bool,
    /// Legacy `meta-<handle>/` directories removed in this call (V0.4.1
    /// cleanup). Empty on fresh installs and on idempotent re-runs after
    /// the first sweep.
    pub removed_stale: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;
    use crate::tool_surface::disable_tool_surface_bootstrap_for_tests;

    static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
    fn isolation() {
        DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
    }

    fn paths(tmp: &tempfile::TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn meta_slug_is_literal_meta() {
        assert_eq!(meta_slug(), "meta");
    }

    #[test]
    fn meta_session_name_is_literal() {
        assert_eq!(meta_session_name(), "ccteam-meta");
    }

    #[test]
    fn render_role_prompt_substitutes_placeholders() {
        let body = render_meta_role_prompt();
        // No literal placeholders left over.
        assert!(!body.contains("__USER_HANDLE__"));
        assert!(!body.contains("__GENERATED_AT__"));
    }

    #[test]
    fn render_role_prompt_uses_canonical_meta_project_path() {
        let body = render_meta_role_prompt();
        assert!(
            body.contains("~/projects/meta/.ccteam/inbox/"),
            "role prompt should mention canonical meta inbox path",
        );
        assert!(
            body.contains("~/projects/meta/.ccteam/outbox/"),
            "role prompt should mention canonical meta outbox path",
        );
    }

    #[test]
    fn render_role_prompt_includes_seven_required_chapters() {
        let body = render_meta_role_prompt();
        for required in [
            "你是谁",
            "决策树",
            "克制规则",
            "派单工具",
            "监控规则",
            "inbox",
            "outbox",
        ] {
            assert!(
                body.contains(required),
                "meta-agent role prompt must include `{required}` section",
            );
        }
    }

    #[test]
    fn render_role_prompt_includes_dev_and_research_team_options() {
        let body = render_meta_role_prompt();
        assert!(body.contains("research"));
        // V0.4.2: switched to space-separated CLI flags
        // (`--team dev` / `--team research`) when F75 simplified the
        // `ccteam new` surface.
        assert!(body.contains("--team dev") && body.contains("--team research"));
        assert!(
            body.contains("不确定要不要做") || body.contains("调研下"),
            "team-selection heuristics must include the unsure-idea / research signal",
        );
    }

    #[test]
    fn bootstrap_meta_project_creates_full_skeleton() {
        isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let report = bootstrap_meta_project(&p).unwrap();

        assert_eq!(report.slug, "meta");
        assert!(report.project_dir.is_dir());
        assert!(report.claude_md.is_file());
        assert!(p.project_state(&report.slug).is_file());

        let state = ProjectState::load(&p.project_state(&report.slug)).unwrap();
        assert_eq!(state.team, META_TEAM_NAME);
        assert_eq!(state.slug, "meta");
        assert_eq!(state.tmux_session, "ccteam-meta");

        let cc = p.project_ccteam_dir(&report.slug);
        assert!(cc.join("inbox").is_dir());
        assert!(cc.join("outbox").is_dir());
    }

    #[test]
    fn bootstrap_meta_project_is_idempotent_and_refreshes_role_prompt() {
        isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        bootstrap_meta_project(&p).unwrap();

        let cm = p.project_dir("meta").join("CLAUDE.md");
        std::fs::write(&cm, "stale\n").unwrap();
        let report = bootstrap_meta_project(&p).unwrap();
        assert!(report.already_existed);
        let body = std::fs::read_to_string(&cm).unwrap();
        assert!(body.contains("决策树"), "role prompt should be re-rendered");
    }

    /// Plant a legacy V0.1-V0.4.0 `meta-<handle>` dir with the
    /// meta-agent marker so the cleanup sweep recognises it. Returns
    /// the absolute project dir.
    fn plant_legacy_meta_dir(p: &CcteamPaths, slug: &str) -> PathBuf {
        let dir = p.project_dir(slug);
        std::fs::create_dir_all(p.project_ccteam_dir(slug)).unwrap();
        let mut state = ProjectState::initial_for_team(slug.into(), META_TEAM_NAME.into());
        state.tmux_session = format!("ccteam-{slug}");
        state.save(&p.project_state(slug)).unwrap();
        dir
    }

    #[test]
    fn bootstrap_removes_legacy_meta_handle_dirs() {
        isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);

        let cto = plant_legacy_meta_dir(&p, "meta-cto");
        let rob = plant_legacy_meta_dir(&p, "meta-rob");

        // Unrelated user dir that happens to start with `meta-` — has
        // no state.json so the marker check skips it.
        let userdir = p.projects_root.join("meta-notes");
        std::fs::create_dir_all(&userdir).unwrap();

        let report = bootstrap_meta_project(&p).unwrap();

        assert_eq!(report.removed_stale.len(), 2);
        assert!(!cto.exists(), "stale meta-cto must be removed");
        assert!(!rob.exists(), "stale meta-rob must be removed");
        assert!(userdir.exists(), "non-ccteam meta-* dir must be left alone");
        assert!(p.project_dir("meta").is_dir(), "new meta/ must exist");
    }

    #[test]
    fn bootstrap_skips_meta_prefixed_dir_with_wrong_team() {
        isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);

        // A user project that happens to be named `meta-research` and
        // has a real state.json — but team is `research`, not
        // `meta-agent`. Must not be deleted.
        let slug = "meta-research";
        std::fs::create_dir_all(p.project_ccteam_dir(slug)).unwrap();
        let state = ProjectState::initial_for_team(slug.into(), "research".into());
        state.save(&p.project_state(slug)).unwrap();

        let report = bootstrap_meta_project(&p).unwrap();
        assert!(report.removed_stale.is_empty());
        assert!(
            p.project_dir(slug).is_dir(),
            "user project named meta-research must survive"
        );
    }

    #[test]
    fn bootstrap_is_clean_on_fresh_install() {
        isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let report = bootstrap_meta_project(&p).unwrap();
        assert!(report.removed_stale.is_empty());
    }
}
