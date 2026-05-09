//! Meta-agent session bootstrap (M1.0).
//!
//! The meta-agent is a ccteam-managed long tmux session under
//! `~/projects/<user>-meta/` that the human user attaches to with
//! `tmux attach -t ccteam-meta-<user>` and talks to in NL. It dispatches
//! project requests via `ccteam new`, monitors active projects, and
//! routes inbox/outbox messages.
//!
//! Two design rules:
//!
//! 1. **Evergreen flag, not hardcoded branch** (V0.2 §6.4 candidate 5):
//!    meta-agent behavior (event loop, no phase DAG, never terminal,
//!    no per-project cost cap) is declared in
//!    `teams/meta-agent.yaml` via `evergreen: true` + `cost_policy:
//!    {kind: none}`. The orchestrator dispatches off these flags
//!    (`Orchestrator::is_evergreen` / `cost_policy`) — any
//!    user-authored evergreen team (e.g. V0.3 watchdog / reviewer
//!    agents) takes the same code path. The
//!    `Orchestrator::process_meta_project` call still drives the
//!    actual session lifecycle.
//! 2. **Role prompt template lives in-binary**: shipped as
//!    `include_str!`, rendered at bootstrap time with the user handle
//!    interpolated. Anything fancier (hot-reload, per-user templates)
//!    is M2+.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};

use crate::inbox::SessionMailbox;
use crate::paths::CcteamPaths;
use crate::projects::{bootstrap_project, slugify};
use crate::state::ProjectState;

/// Canonical team name for the user-facing NL dispatcher session.
/// V0.2 §6.4 candidate 5: behavior is now driven by
/// `teams/meta-agent.yaml.evergreen: true` rather than a string
/// comparison in the orchestrator. This const remains as the slug
/// passed to `bootstrap_project` so meta-agent's state.json says
/// `team: meta-agent` and the orchestrator looks up its TeamSpec by
/// the same key.
pub const META_TEAM_NAME: &str = "meta-agent";

/// tmux session prefix for meta-agent sessions. The full name is
/// `META_SESSION_PREFIX + <user-handle>` (e.g. `ccteam-meta-rob`).
/// User project sessions use `ccteam-` (see `tmux::SESSION_PREFIX`)
/// so there's no collision risk.
pub const META_SESSION_PREFIX: &str = "ccteam-meta-";

/// Embedded role prompt template. Sourced from
/// `crates/ccteam-core/src/templates/meta_agent_role.md` so the
/// document is reviewable / diffable as markdown.
const META_ROLE_PROMPT_TEMPLATE: &str =
    include_str!("templates/meta_agent_role.md");

/// Build the meta-agent's project slug from a user handle. Slug is
/// `<user>-meta`, slugified — so `Rob` and `rob` collapse to the same
/// directory, which is what we want.
pub fn meta_slug(user_handle: &str) -> Result<String> {
    let trimmed = user_handle.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("meta-agent user handle must be non-empty"));
    }
    Ok(format!("{}-meta", slugify(trimmed)))
}

/// `ccteam-meta-<user>` tmux session name.
pub fn meta_session_name(user_handle: &str) -> Result<String> {
    let trimmed = user_handle.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("meta-agent user handle must be non-empty"));
    }
    Ok(format!("{}{}", META_SESSION_PREFIX, slugify(trimmed)))
}

/// Render the role prompt with `<user>` placeholder interpolated. Pure;
/// returns the string the caller writes to disk.
pub fn render_meta_role_prompt(user_handle: &str) -> String {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    META_ROLE_PROMPT_TEMPLATE
        .replace("__USER_HANDLE__", user_handle)
        .replace("__GENERATED_AT__", &now)
}

/// Bootstrap the meta-agent's project tree:
/// 1. Run `bootstrap_project(...team=meta-agent)` to lay down
///    `state.json`, `.claude/settings.json`, and the project skeleton.
/// 2. Overwrite the auto-generated `CLAUDE.md` with the meta-agent
///    role prompt so the new session loads dispatcher behavior.
/// 3. Pre-create `inbox/` + `outbox/` so an inotify watcher can attach
///    immediately at session startup.
///
/// Idempotent: re-running refreshes the CLAUDE.md role prompt (caller
/// passes `force=true` to also overwrite settings.json) but leaves
/// state.json + spec.md alone if they already exist.
pub fn bootstrap_meta_project(
    paths: &CcteamPaths,
    user_handle: &str,
) -> Result<MetaBootstrapReport> {
    let slug = meta_slug(user_handle)?;
    let project_dir = paths.project_dir(&slug);
    let already_existed = paths.project_state(&slug).exists();

    let request = format!(
        "ccteam meta-agent session for {user_handle}. \
         Dispatch incoming requests to the right team via `ccteam new`.\n\
         Generated: {}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    );

    if !already_existed {
        bootstrap_project(paths, &slug, &request, META_TEAM_NAME)
            .context("bootstrap meta-agent project tree")?;
    }
    // Always normalize the meta-agent's tmux_session name. bootstrap_project
    // initializes it as `ccteam-<slug>` (which would yield `ccteam-rob-meta`),
    // but the strategic doc + interfaces.md call for `ccteam-meta-<user>`
    // — distinct prefix so meta sessions are visually separated from
    // project sessions in `tmux ls`. Rewrite after bootstrap_project runs.
    let state_path = paths.project_state(&slug);
    let mut state = ProjectState::load(&state_path)
        .with_context(|| format!("reload meta state {}", state_path.display()))?;
    state.team = META_TEAM_NAME.into();
    state.tmux_session = meta_session_name(user_handle)?;
    state.save(&state_path)?;

    // Always (re)write CLAUDE.md so the role prompt picks up template
    // edits when ccteam ships a newer version.
    let claude_md = project_dir.join("CLAUDE.md");
    let role_body = render_meta_role_prompt(user_handle);
    std::fs::write(&claude_md, role_body)
        .with_context(|| format!("write {}", claude_md.display()))?;

    let mailbox = SessionMailbox::for_ccteam_dir(&paths.project_ccteam_dir(&slug));
    mailbox.ensure_dirs()?;

    Ok(MetaBootstrapReport {
        slug,
        project_dir,
        claude_md,
        already_existed,
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
    fn meta_slug_combines_user_with_meta_suffix() {
        assert_eq!(meta_slug("rob").unwrap(), "rob-meta");
        assert_eq!(meta_slug("Rob Test User").unwrap(), "rob-test-user-meta");
    }

    #[test]
    fn meta_slug_rejects_empty() {
        assert!(meta_slug("").is_err());
        assert!(meta_slug("   ").is_err());
    }

    #[test]
    fn meta_session_name_uses_dedicated_prefix() {
        let n = meta_session_name("rob").unwrap();
        assert_eq!(n, "ccteam-meta-rob");
        // Must not collide with project tmux prefix `ccteam-<slug>`.
        assert!(!n.starts_with("ccteam-rob"));
    }

    #[test]
    fn render_role_prompt_substitutes_user_handle() {
        let body = render_meta_role_prompt("rob");
        assert!(body.contains("rob"), "role prompt should mention user handle");
        assert!(!body.contains("__USER_HANDLE__"), "placeholder should be substituted");
        assert!(!body.contains("__GENERATED_AT__"));
    }

    #[test]
    fn render_role_prompt_includes_seven_required_chapters() {
        // Per task brief §5: the role prompt must contain all seven
        // chapters covering identity / decision tree / dispatcher
        // restraint / dispatch tools / monitoring / inbox / outbox.
        let body = render_meta_role_prompt("rob");
        // The headings live in the template under section anchors.
        for required in [
            "你是谁",            // identity
            "决策树",            // decision tree
            "克制规则",          // dispatcher-not-worker
            "派单工具",          // dispatch tools
            "监控规则",          // monitoring
            "inbox",             // inbox handling
            "outbox",            // outbox emission
        ] {
            assert!(
                body.contains(required),
                "meta-agent role prompt must include `{required}` section",
            );
        }
    }

    #[test]
    fn render_role_prompt_includes_dev_and_research_team_options() {
        // M3.7: §2 step-2 (team selection) must mention both dev and
        // research so meta-agent's NL dispatch knows the catalog of
        // options. V0.2.2 F40: the canonical name is `research`; the
        // legacy alias `product-research` may still appear in
        // explanatory prose but is no longer the primary command shape.
        let body = render_meta_role_prompt("rob");
        assert!(
            body.contains("research"),
            "team selection step must mention research",
        );
        assert!(
            body.contains("--team=dev") && body.contains("--team=research"),
            "dispatch examples must show both teams' canonical command shapes",
        );
        // The decision-tree wording from task M3.7 must be present —
        // gives meta-agent NL heuristics for picking dev vs research.
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
        let report = bootstrap_meta_project(&p, "rob").unwrap();

        assert_eq!(report.slug, "rob-meta");
        assert!(report.project_dir.is_dir());
        assert!(report.claude_md.is_file());
        assert!(p.project_state(&report.slug).is_file());

        let state = ProjectState::load(&p.project_state(&report.slug)).unwrap();
        assert_eq!(state.team, META_TEAM_NAME);
        // tmux name uses the dedicated `ccteam-meta-<user>` prefix
        // rather than `ccteam-<slug>` so it visually separates from
        // project sessions in `tmux ls`.
        assert_eq!(state.tmux_session, "ccteam-meta-rob");

        let body = std::fs::read_to_string(&report.claude_md).unwrap();
        assert!(body.contains("rob"));

        // inbox/outbox dirs ready for the watcher.
        let cc = p.project_ccteam_dir(&report.slug);
        assert!(cc.join("inbox").is_dir());
        assert!(cc.join("outbox").is_dir());
    }

    #[test]
    fn bootstrap_meta_project_is_idempotent_and_refreshes_role_prompt() {
        isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        bootstrap_meta_project(&p, "rob").unwrap();

        // Tamper with CLAUDE.md, re-run, verify it gets refreshed and the
        // tampering is gone.
        let cm = p.project_dir("rob-meta").join("CLAUDE.md");
        std::fs::write(&cm, "stale\n").unwrap();
        let report = bootstrap_meta_project(&p, "rob").unwrap();
        assert!(report.already_existed);
        let body = std::fs::read_to_string(&cm).unwrap();
        assert!(body.contains("决策树"), "role prompt should be re-rendered");
    }
}
