//! V0.6.1 F139 — `~/.ccteam/hooks/hook.sh` materialization + idempotent
//! install helper.
//!
//! Claude Code per-hook entry points are slow when they cold-spawn the
//! Rust `ccteam` binary: ~200 ms × 4 hooks × ~1.5 turns/sec ⇒ user-
//! visible 1+ s of chat sluggishness. The dispatcher script in this
//! module POSTs the Claude Code hook stdin payload to the long-running
//! ccteam daemon's `POST /internal/hook/:kind[/:action]` route instead
//! (~10 ms round-trip via curl on loopback). When the daemon is
//! unreachable (no token on disk, connection refused, HTTP error) the
//! script falls back to `ccteam internal hook ...` so behaviour stays
//! identical to the pre-F139 path.
//!
//! The script is embedded in the ccteam binary (`include_str!`) so a
//! single-binary install can `ccteam init` / `ccteam doctor
//! --install-hooks` and have the dispatcher land on disk without a
//! separate file copy step. Idempotent: a no-op re-run reports
//! [`InstallHooksAction::Unchanged`].

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::CcteamPaths;

/// Embedded `hook.sh` body. Materialized at `<paths.root>/hooks/hook.sh`
/// by [`install_hooks`]. See the script header for the wire contract +
/// fallback behaviour.
pub const HOOK_DISPATCHER_SH: &str = include_str!("hook.sh");

/// V0.8 rmux W6 — env flag that opts a host into the daemon-bus hook
/// reroute (Option C: hook subprocess → `~/.ccteam/run/hook.sock` →
/// orchestrator single-writer). When `CCTEAM_HOOK_VIA_DAEMON=1`,
/// [`ensure_chat_hooks_installed`](crate::execution::claude_tui::ensure_chat_hooks_installed)
/// generates hook commands that call `ccteam mux hook-emit ...` and the
/// orchestrator (`ccteam start`) binds a [`ccteam_harness::HookSink`] to
/// consume them.
pub const HOOK_VIA_DAEMON_ENV: &str = "CCTEAM_HOOK_VIA_DAEMON";

/// V0.8 rmux W6 — single source of truth for the daemon-bus hook
/// reroute opt-in. Returns `true` ONLY when `CCTEAM_HOOK_VIA_DAEMON=1`.
///
/// Read at two points that MUST agree: the install-time branch in
/// `ensure_chat_hooks_installed` (which hook command string to write
/// into `.claude/settings.json`) and the sink-bind decision in
/// `ccteam start` (whether to bind the `hook.sock` listener). Sharing
/// this helper guarantees they never drift.
///
/// NOTE: this reads the env of the *current* process. The flag must be
/// set in the operator's shell when `ccteam start` runs (and when
/// `ccteam init` / `ccteam doctor` install the hooks); it is NOT
/// auto-propagated into spawned tmux sessions. `MuxSessionSpec::env`
/// seeds the chat session's env, not the orchestrator's own — so the
/// orchestrator only binds the sink if the operator launched `ccteam
/// start` with the flag set.
pub fn hook_via_daemon_enabled() -> bool {
    std::env::var(HOOK_VIA_DAEMON_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Outcome of one [`install_hooks`] call. The caller renders these to
/// the operator (e.g. via `ccteam doctor --install-hooks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallHooksAction {
    /// Script written for the first time.
    Created,
    /// Existing script differed from the embedded body and was rewritten.
    Updated,
    /// On-disk content already matched the embedded body — no-op.
    Unchanged,
}

/// Write the daemon-aware Claude Code hook dispatcher to
/// `<paths.root>/hooks/hook.sh` and chmod it `0o755` on Unix. Idempotent.
///
/// Called from `ccteam init` (first install) and
/// `ccteam doctor --install-hooks` (forces a refresh after a ccteam
/// binary upgrade ships a new embedded body).
pub fn install_hooks(paths: &CcteamPaths) -> Result<(PathBuf, InstallHooksAction)> {
    let dir = paths.hooks_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = paths.hooks_script();
    let action = match std::fs::read_to_string(&path) {
        Ok(existing) if existing == HOOK_DISPATCHER_SH => InstallHooksAction::Unchanged,
        Ok(_) => {
            std::fs::write(&path, HOOK_DISPATCHER_SH)
                .with_context(|| format!("rewrite {}", path.display()))?;
            InstallHooksAction::Updated
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&path, HOOK_DISPATCHER_SH)
                .with_context(|| format!("write {}", path.display()))?;
            InstallHooksAction::Created
        }
        Err(err) => {
            return Err(err).with_context(|| format!("read {}", path.display()));
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .with_context(|| format!("stat {}", path.display()))?
            .permissions();
        if perms.mode() & 0o777 != 0o755 {
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)
                .with_context(|| format!("chmod 0755 {}", path.display()))?;
        }
    }
    Ok((path, action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn embedded_body_routes_through_http_with_cli_fallback() {
        assert!(
            HOOK_DISPATCHER_SH.contains("/internal/hook/"),
            "dispatcher must POST to /internal/hook/* (daemon fast path)"
        );
        assert!(
            HOOK_DISPATCHER_SH.contains("ccteam internal hook"),
            "dispatcher must fall back to `ccteam internal hook ...`"
        );
        assert!(
            HOOK_DISPATCHER_SH.starts_with("#!/bin/sh"),
            "dispatcher must be a POSIX shell script"
        );
    }

    #[test]
    fn install_creates_then_unchanged() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let (path, first) = install_hooks(&paths).unwrap();
        assert_eq!(first, InstallHooksAction::Created);
        assert!(path.exists());
        let (_, second) = install_hooks(&paths).unwrap();
        assert_eq!(second, InstallHooksAction::Unchanged);
    }

    #[test]
    fn install_rewrites_after_hand_edit() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let (path, _) = install_hooks(&paths).unwrap();
        std::fs::write(&path, "#!/bin/sh\necho stale\n").unwrap();
        let (_, action) = install_hooks(&paths).unwrap();
        assert_eq!(action, InstallHooksAction::Updated);
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, HOOK_DISPATCHER_SH);
    }

    #[cfg(unix)]
    #[test]
    fn install_sets_mode_0755() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let (path, _) = install_hooks(&paths).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
}
