//! ccteam path resolver. Centralizes the global (`~/.ccteam/`) and
//! project-local (`~/projects/<slug>/.ccteam/`) layouts documented in
//! `docs/interfaces.md` §1 so hooks, orchestrator, and CLI agree.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

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
