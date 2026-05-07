//! Orchestrator daemon lifecycle helpers (M1.5).
//!
//! - **pidfile** at `~/.ccteam/state/orchestrator.pid` so `ccteam stop`
//!   can find a running daemon.
//! - **heartbeat** at `~/.ccteam/state/orchestrator.heartbeat` (M0.23.1):
//!   the orchestrator touches it every `HEARTBEAT_INTERVAL`; supervisors
//!   (MCP entrypoints, meta-agent skill startup) read its mtime and
//!   declare the daemon dead when it lags `HEARTBEAT_GRACE` past now.
//!   Pure stat — no IPC.
//! - **graceful stop** via SIGTERM (Unix). The orchestrator's tokio
//!   `select!` already listens on Ctrl-C; we route SIGTERM through the
//!   same path on Unix.
//! - **session reattach**: `discover_projects` already walks
//!   `~/projects/*/.ccteam/state.json`; `ensure_session` does the
//!   `tmux has-session` + `kill -0` double-check before re-spawning.
//!   M1.5 explicitly does **not** kill any tmux sessions on stop.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};

use crate::paths::CcteamPaths;

/// Filename under `<root>/state/` where the running orchestrator stores
/// its PID.
pub const PIDFILE_NAME: &str = "orchestrator.pid";

/// Filename under `<root>/state/` for the daemon heartbeat (M0.23.1).
pub const HEARTBEAT_NAME: &str = "orchestrator.heartbeat";

/// How often the orchestrator touches the heartbeat file. Supervisors
/// must allow ≥ 2× this before declaring the daemon dead.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum mtime age before a heartbeat is considered stale. Set to
/// 2× `HEARTBEAT_INTERVAL` so a single missed tick (GC pause, blocked
/// disk write) doesn't flap. A genuinely-down daemon trips this in ≤ 60s.
pub const HEARTBEAT_GRACE: Duration = Duration::from_secs(60);

/// Resolve the pidfile path for a given ccteam root.
pub fn pidfile_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("state").join(PIDFILE_NAME)
}

/// Resolve the heartbeat-file path for a given ccteam root.
pub fn heartbeat_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("state").join(HEARTBEAT_NAME)
}

/// Write the current process's PID to the pidfile. Called by
/// `ccteam start --foreground` right after the orchestrator is
/// constructed but before the run loop spins. Returns the path written.
///
/// Refuses to overwrite a pidfile whose owner process is still alive
/// (so two `ccteam start` invocations can't silently fight over the
/// same state). Stale pidfiles (PID gone) are reclaimed.
pub fn write_pidfile(paths: &CcteamPaths) -> Result<PathBuf> {
    let path = pidfile_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    if path.exists() {
        match read_pidfile(&path) {
            Ok(pid) if pid_alive(pid) => {
                return Err(anyhow!(
                    "ccteam start: another orchestrator (pid {pid}) is already running. \
                     Run `ccteam stop` first, or remove {} if you're sure it's stale.",
                    path.display(),
                ));
            }
            _ => {
                tracing::warn!(
                    pidfile = %path.display(),
                    "stale pidfile reclaimed",
                );
            }
        }
    }
    let pid = std::process::id();
    std::fs::write(&path, format!("{pid}\n"))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Best-effort pidfile cleanup; called on graceful shutdown. Errors
/// are logged but not propagated.
pub fn remove_pidfile(paths: &CcteamPaths) {
    let path = pidfile_path(paths);
    if let Err(err) = std::fs::remove_file(&path) {
        if path.exists() {
            tracing::warn!(
                pidfile = %path.display(),
                error = %err,
                "could not remove pidfile",
            );
        }
    }
}

/// Read the PID from `path`. Surfaces both "missing" and "garbled" as
/// errors so the caller can decide whether to bail or treat as stale.
pub fn read_pidfile(path: &Path) -> Result<u32> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    body.trim()
        .parse::<u32>()
        .with_context(|| format!("parse pid from {}", path.display()))
}

/// `kill -0 <pid>` — true iff the process exists.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) is well-defined for any pid; failure is
    // signalled via the return value, not memory unsafety.
    let pid_i32 = pid as i32;
    // Avoid pulling in nix/libc just for one syscall — shell out.
    std::process::Command::new("kill")
        .args(["-0", &pid_i32.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    // Non-Unix platforms aren't a target for ccteam M1.
    false
}

/// Send SIGTERM to a running orchestrator described by its pidfile.
/// Returns `Ok(None)` when no daemon is running (pidfile absent or
/// process already gone).
#[cfg(unix)]
pub fn send_sigterm_to_pidfile(paths: &CcteamPaths) -> Result<Option<u32>> {
    let path = pidfile_path(paths);
    if !path.exists() {
        return Ok(None);
    }
    let pid = read_pidfile(&path)?;
    if !pid_alive(pid) {
        // Stale; reap the file.
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .with_context(|| format!("spawn `kill -TERM {pid}`"))?;
    if !status.success() {
        return Err(anyhow!("kill -TERM {pid} exited with {status}"));
    }
    Ok(Some(pid))
}

#[cfg(not(unix))]
pub fn send_sigterm_to_pidfile(_paths: &CcteamPaths) -> Result<Option<u32>> {
    Err(anyhow!("ccteam stop is only implemented on Unix"))
}

/// Touch the heartbeat file (create-or-bump-mtime). Called by the
/// orchestrator main loop on a fixed cadence (`HEARTBEAT_INTERVAL`).
/// Idempotent.
pub fn write_heartbeat(paths: &CcteamPaths) -> Result<()> {
    let path = heartbeat_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let pid = std::process::id();
    let now = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, format!("{pid} {now}\n"))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Best-effort cleanup; non-critical on shutdown.
pub fn remove_heartbeat(paths: &CcteamPaths) {
    let path = heartbeat_path(paths);
    if let Err(err) = std::fs::remove_file(&path) {
        if path.exists() {
            tracing::warn!(
                heartbeat = %path.display(),
                error = %err,
                "could not remove heartbeat",
            );
        }
    }
}

/// Outcome of a daemon health check. `Healthy` means the heartbeat is
/// fresh; everything else is fail-loud signal for callers (MCP tools,
/// meta-agent skill startup) to surface "daemon down" rather than
/// silently continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonHealth {
    /// Heartbeat file present and within `HEARTBEAT_GRACE`.
    Healthy { age_secs: u64 },
    /// Heartbeat file missing (daemon never ran, or removed).
    NoHeartbeat,
    /// Heartbeat present but mtime older than `HEARTBEAT_GRACE`.
    Stale { age_secs: u64 },
}

impl DaemonHealth {
    /// True iff the daemon is healthy. Callers usually want to fail-loud
    /// otherwise.
    pub fn is_healthy(&self) -> bool {
        matches!(self, DaemonHealth::Healthy { .. })
    }

    /// Human-readable explanation for surfacing to users.
    pub fn describe(&self) -> String {
        match self {
            DaemonHealth::Healthy { age_secs } => {
                format!("orchestrator daemon healthy ({age_secs}s since last heartbeat)")
            }
            DaemonHealth::NoHeartbeat => {
                "orchestrator daemon down: no heartbeat file (start it with `ccteam start \
                 --foreground` in another terminal)"
                    .into()
            }
            DaemonHealth::Stale { age_secs } => format!(
                "orchestrator daemon down: heartbeat is {age_secs}s old (grace = {}s); \
                 restart with `ccteam start --foreground`",
                HEARTBEAT_GRACE.as_secs()
            ),
        }
    }
}

/// Stat the heartbeat file and classify daemon liveness. Pure read; no
/// IPC, no daemon contact. Designed for hot paths (every MCP call).
pub fn check_health(paths: &CcteamPaths) -> DaemonHealth {
    check_health_at(&heartbeat_path(paths), SystemTime::now())
}

/// Testable inner: classify based on the file at `path` relative to `now`.
pub fn check_health_at(path: &Path, now: SystemTime) -> DaemonHealth {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return DaemonHealth::NoHeartbeat,
    };
    let mtime = metadata
        .modified()
        .ok()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
    let age_secs = age.as_secs();
    if age <= HEARTBEAT_GRACE {
        DaemonHealth::Healthy { age_secs }
    } else {
        DaemonHealth::Stale { age_secs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(tmp: &tempfile::TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn write_pidfile_creates_file_with_current_pid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let path = write_pidfile(&p).unwrap();
        let pid = read_pidfile(&path).unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn write_pidfile_refuses_to_overwrite_live_owner() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        write_pidfile(&p).unwrap();
        // Same process tries again — should error since the owner is
        // alive (it's us).
        let err = write_pidfile(&p).unwrap_err();
        assert!(format!("{err:#}").contains("already running"));
    }

    #[test]
    fn write_pidfile_reclaims_stale_pidfile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let path = pidfile_path(&p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // PID 1 is init; it exists. Use 0 (always invalid) to simulate stale.
        std::fs::write(&path, "0\n").unwrap();
        let written = write_pidfile(&p).unwrap();
        let pid = read_pidfile(&written).unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn remove_pidfile_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        // No pidfile — must not error.
        remove_pidfile(&p);
        // Now write + remove.
        write_pidfile(&p).unwrap();
        remove_pidfile(&p);
        assert!(!pidfile_path(&p).exists());
    }

    #[test]
    fn read_pidfile_errors_on_garbled_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("pid");
        std::fs::write(&path, "not a number\n").unwrap();
        assert!(read_pidfile(&path).is_err());
    }

    #[test]
    fn write_heartbeat_creates_file_with_pid_and_timestamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        write_heartbeat(&p).unwrap();
        let body = std::fs::read_to_string(heartbeat_path(&p)).unwrap();
        assert!(body.starts_with(&std::process::id().to_string()));
        assert!(body.contains('T')); // RFC3339 marker
    }

    #[test]
    fn check_health_reports_no_heartbeat_when_file_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        assert_eq!(check_health(&p), DaemonHealth::NoHeartbeat);
        assert!(!check_health(&p).is_healthy());
    }

    #[test]
    fn check_health_reports_healthy_for_fresh_heartbeat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        write_heartbeat(&p).unwrap();
        let health = check_health(&p);
        assert!(health.is_healthy(), "got {health:?}");
    }

    #[test]
    fn check_health_reports_stale_when_mtime_past_grace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        write_heartbeat(&p).unwrap();
        let path = heartbeat_path(&p);
        // Compare against a time `HEARTBEAT_GRACE * 2` past the file's
        // mtime to simulate a long-dead daemon.
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let now = mtime + HEARTBEAT_GRACE * 2;
        let health = check_health_at(&path, now);
        assert!(matches!(health, DaemonHealth::Stale { .. }), "got {health:?}");
        assert!(!health.is_healthy());
    }

    #[test]
    fn remove_heartbeat_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        remove_heartbeat(&p);
        write_heartbeat(&p).unwrap();
        remove_heartbeat(&p);
        assert!(!heartbeat_path(&p).exists());
    }

    #[test]
    fn daemon_health_describe_is_actionable_when_down() {
        let no = DaemonHealth::NoHeartbeat;
        assert!(no.describe().contains("ccteam start"));
        let stale = DaemonHealth::Stale { age_secs: 120 };
        assert!(stale.describe().contains("ccteam start"));
        assert!(stale.describe().contains("120"));
    }
}
