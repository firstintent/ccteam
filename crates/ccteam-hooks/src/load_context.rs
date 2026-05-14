//! `ccteam hook load-context` — SessionStart handler. M0 writes the
//! `<project>/.ccteam/ready` marker so the orchestrator knows the
//! tmux-launched claude is up. M0.10 extends this to bridge the
//! pre-reset progress summary into the new session via a CLAUDE.md
//! append.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use ccteam_core::{session_context_from_cwd, CcteamPaths};

pub fn load_context(paths: &CcteamPaths, stdin: &serde_json::Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let cwd_path = Path::new(cwd);
    let project_dir = session_context_from_cwd(cwd_path, paths)
        .map(|c| c.project_dir)
        .unwrap_or_else(|_| cwd_path.to_path_buf());

    let ready = CcteamPaths::project_ready_in(&project_dir);
    if let Some(parent) = ready.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&ready, b"").with_context(|| format!("write {}", ready.display()))?;
    Ok(())
}
