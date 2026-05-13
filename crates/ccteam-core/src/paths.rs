//! ccteam path resolver. Centralizes the global (`~/.ccteam/`) and
//! project-local (`~/projects/<slug>/.ccteam/`) layouts documented in
//! `docs/interfaces.md` §1 so hooks, orchestrator, and CLI agree.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::state::ProjectState;
use crate::team::TeamKind;

#[derive(Debug, Clone)]
pub struct CcteamPaths {
    /// `~/.ccteam/` — the global ccteam root.
    pub root: PathBuf,
    /// `~/projects/` — where project working trees live.
    pub projects_root: PathBuf,
}

impl CcteamPaths {
    /// Resolve from the running user's home directory. Honors the
    /// `CCTEAM_HOME` and `CCTEAM_PROJECTS_ROOT` env vars for tests and
    /// custom layouts.
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
        let root = std::env::var("CCTEAM_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".ccteam"));
        let projects_root = std::env::var("CCTEAM_PROJECTS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join("projects"));
        Ok(Self {
            root,
            projects_root,
        })
    }

    pub fn progress_jsonl(&self, slug: &str) -> PathBuf {
        self.progress_dir().join(format!("{slug}.jsonl"))
    }

    pub fn progress_jsonl_for_session(&self, slug: &str, sid: &str) -> PathBuf {
        self.progress_dir().join(slug).join(format!("{sid}.jsonl"))
    }

    /// `~/.ccteam/progress/` — directory holding `<slug>.jsonl`
    /// streams. Public since V0.3 M5.2 so the `ccteam-web` watcher
    /// can attach a recursive `notify` watcher without re-deriving
    /// the path.
    pub fn progress_dir(&self) -> PathBuf {
        self.root.join("progress")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.root.join("inbox")
    }

    pub fn control_dir(&self) -> PathBuf {
        self.root.join("control")
    }

    pub fn phases_dir(&self) -> PathBuf {
        self.root.join("phases")
    }

    /// `~/.ccteam/templates/` — global helper templates that phase
    /// markdown can `@`-reference (M2.4, interfaces §5). Distinct from
    /// `phases/` because helpers are *prompt fragments*, not whole
    /// phases — they have no front-matter, no DAG position.
    pub fn templates_dir(&self) -> PathBuf {
        self.root.join("templates")
    }

    pub fn project_dir(&self, slug: &str) -> PathBuf {
        self.projects_root.join(slug)
    }

    pub fn project_ccteam_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join(".ccteam")
    }

    pub fn project_state(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug).join("state.json")
    }

    pub fn project_sessions_dir(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug).join("sessions")
    }

    pub fn project_session_dir(&self, slug: &str, sid: &str) -> PathBuf {
        self.project_sessions_dir(slug).join(sid)
    }

    pub fn progress_jsonl_for_context(&self, context: &ProjectSessionContext) -> PathBuf {
        if context.team_kind == TeamKind::Flex {
            let sid = context
                .sid
                .as_deref()
                .unwrap_or(crate::harness::DEFAULT_CLAUDE_SID);
            self.progress_jsonl_for_session(&context.slug, sid)
        } else {
            self.progress_jsonl(&context.slug)
        }
    }

    pub fn project_state_in(project_dir: &Path) -> PathBuf {
        project_dir.join(".ccteam").join("state.json")
    }

    pub fn project_ready_in(project_dir: &Path) -> PathBuf {
        project_dir.join(".ccteam").join("ready")
    }

    /// `<project>/.ccteam/pending-inject.json` — V0.2.2 F36 deferred
    /// phase-inject record. See `crate::pending_inject` for shape +
    /// lifecycle.
    pub fn project_pending_inject(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug)
            .join(crate::pending_inject::PENDING_INJECT_FILE)
    }

    /// `<project>/.ccteam/screenshots/` — V0.2.2 F38 PNG screenshot
    /// directory. Created lazily by `screenshot::render_screenshot`.
    pub fn project_screenshots_dir(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug).join("screenshots")
    }

    /// `~/.ccteam/pty/` — V0.3.2 F56 directory holding FIFO files used
    /// by the web layer's `tmux pipe-pane` relay (one FIFO per active
    /// `<slug>` or `<slug>-<sid>` subscription). Files are created /
    /// unlinked at runtime by `ccteam_web::routes::pty_ws`.
    ///
    /// **Architectural red line** (CLAUDE.md §三, PRD §F56 §6): this
    /// directory is a presentation-layer control plane. The
    /// orchestrator never reads it; `progress.jsonl` remains the
    /// single source of truth.
    pub fn pty_dir(&self) -> PathBuf {
        self.root.join("pty")
    }

    /// `~/.ccteam/harness/` — V0.3.1 F46 dual-write target for the
    /// Claude Code statusline wrapper (and future Codex equivalent).
    /// Each session deposits one `<slug>-<sid>.json` file holding the
    /// most recent harness statusline JSON; the ccteam-web watcher
    /// tails this dir and broadcasts `harness_snapshot` events.
    ///
    /// **Architectural red line** (CLAUDE.md §三, PRD §3.3): files in
    /// this directory are *presentation only*. The orchestrator state
    /// machine never reads them — `progress.jsonl` remains the single
    /// source of truth.
    pub fn harness_dir(&self) -> PathBuf {
        self.root.join("harness")
    }

    /// V0.2.2 F38: Build a unique PNG path under
    /// `<project>/.ccteam/screenshots/<utc>.png`. The timestamp is
    /// the same compact RFC3339-no-colons format used by inbox
    /// filenames so screenshots sort lexically by capture time.
    pub fn project_screenshot_path(
        &self,
        slug: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> PathBuf {
        let stamp = now
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .replace(':', "");
        self.project_screenshots_dir(slug)
            .join(format!("{stamp}.png"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSessionContext {
    pub slug: String,
    pub sid: Option<String>,
    pub project_dir: PathBuf,
    pub team_kind: TeamKind,
}

/// Read a project's slug by loading `<project_dir>/.ccteam/state.json`.
/// Hooks use this to bridge from the `cwd` field of a Claude Code hook
/// payload to the global progress.jsonl path.
pub fn slug_from_project_dir(project_dir: &Path) -> Result<String> {
    let state_path = CcteamPaths::project_state_in(project_dir);
    let bytes = std::fs::read(&state_path)
        .with_context(|| format!("read {}", state_path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", state_path.display()))?;
    let slug = v
        .get("slug")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("state.json missing `slug` field at {}", state_path.display()))?;
    Ok(slug.to_string())
}

pub fn session_context_from_cwd(cwd: &Path, paths: &CcteamPaths) -> Result<ProjectSessionContext> {
    if cwd
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!("cwd must not contain `..`: {}", cwd.display()));
    }
    let rel = cwd.strip_prefix(&paths.projects_root).with_context(|| {
        format!(
            "cwd {} is not under {}",
            cwd.display(),
            paths.projects_root.display()
        )
    })?;
    let mut comps = rel.components();
    let slug = match comps.next() {
        Some(std::path::Component::Normal(s)) => s
            .to_str()
            .ok_or_else(|| anyhow!("project slug path is not UTF-8: {}", cwd.display()))?
            .to_string(),
        _ => {
            return Err(anyhow!(
                "could not derive project slug from cwd {}",
                cwd.display()
            ))
        }
    };
    let project_dir = paths.project_dir(&slug);
    let state = ProjectState::load(&paths.project_state(&slug))?;
    let sid = sid_from_components(comps);
    Ok(ProjectSessionContext {
        slug: state.slug,
        sid,
        project_dir,
        team_kind: state.team_kind,
    })
}

fn sid_from_components<'a>(
    mut comps: impl Iterator<Item = std::path::Component<'a>>,
) -> Option<String> {
    match comps.next() {
        Some(std::path::Component::Normal(n)) if n == ".ccteam" => {}
        _ => return None,
    }
    match comps.next() {
        Some(std::path::Component::Normal(n)) if n == "sessions" => {}
        _ => return None,
    }
    match comps.next() {
        Some(std::path::Component::Normal(n)) => n.to_str().map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::TeamKind;
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn f49_session_context_detects_sid_subdir() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let slug = "flex-demo";
        let project_dir = paths.project_dir(slug);
        std::fs::create_dir_all(paths.project_session_dir(slug, "claude-1")).unwrap();
        let mut state = ProjectState::initial_for_team(slug.into(), "flex".into());
        state.team_kind = TeamKind::Flex;
        state.save(&paths.project_state(slug)).unwrap();

        let cwd = paths.project_session_dir(slug, "claude-1").join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let context = session_context_from_cwd(&cwd, &paths).unwrap();
        assert_eq!(context.slug, slug);
        assert_eq!(context.sid.as_deref(), Some("claude-1"));
        assert_eq!(context.project_dir, project_dir);
        assert_eq!(
            paths.progress_jsonl_for_context(&context),
            paths.progress_jsonl_for_session(slug, "claude-1"),
        );
    }

    #[test]
    fn f49_session_context_keeps_workflow_progress_flat() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let slug = "dev-demo";
        std::fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
        ProjectState::initial_for_team(slug.into(), "dev".into())
            .save(&paths.project_state(slug))
            .unwrap();

        let context = session_context_from_cwd(&paths.project_dir(slug), &paths).unwrap();
        assert_eq!(context.sid, None);
        assert_eq!(context.team_kind, TeamKind::Workflow);
        assert_eq!(
            paths.progress_jsonl_for_context(&context),
            paths.progress_jsonl(slug),
        );
    }
}
