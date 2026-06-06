//! `ccteam hook load-context` — SessionStart handler.
//!
//! Currently a no-op: it validates the hook stdin shape (so a malformed
//! payload fails loudly) but performs no filesystem side effects. The
//! handler is kept registered as a SessionStart seam for future
//! per-session bootstrap work.

use std::path::Path;

use anyhow::{anyhow, Result};

use ccteam_core::{session_context_from_cwd, CcteamPaths};

pub fn load_context(paths: &CcteamPaths, stdin: &serde_json::Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let cwd_path = Path::new(cwd);
    let _project_dir = session_context_from_cwd(cwd_path, paths)
        .map(|c| c.project_dir)
        .unwrap_or_else(|_| cwd_path.to_path_buf());
    Ok(())
}
