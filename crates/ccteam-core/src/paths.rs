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
        self.root.join("progress").join(format!("{slug}.jsonl"))
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
