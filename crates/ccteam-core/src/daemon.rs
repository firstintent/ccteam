//! Gateway daemon lifecycle helpers.
//!
//! - **pidfile** at `~/.ccteam/state/orchestrator.pid` so `ccteam stop`
//!   can find a running daemon.
//! - **liveness** is the daemon MCP Unix socket at
//!   `~/.ccteam/run/mcp.sock`: callers must successfully connect to the
//!   socket. A leftover socket file from a crashed daemon is not alive.
//! - **graceful stop** via SIGTERM (Unix). The orchestrator's tokio
//!   `select!` already listens on Ctrl-C; we route SIGTERM through the
//!   same path on Unix.
//! - **session reattach**: `discover_projects` already walks
//!   `~/projects/*/.ccteam/state.json`; `ensure_session` does the
//!   `tmux has-session` + `kill -0` double-check before re-spawning.
//!   M1.5 explicitly does **not** kill any tmux sessions on stop.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::paths::CcteamPaths;

/// Filename under `<root>/state/` where the running orchestrator stores
/// its PID.
pub const PIDFILE_NAME: &str = "orchestrator.pid";

/// Legacy flow/orchestrator heartbeat file. Gateway daemon liveness does
/// not use this file; it is retained for the deferred `ccteam-flow`
/// runtime until that layer is migrated separately.
pub const HEARTBEAT_NAME: &str = "orchestrator.heartbeat";

/// Filename under `<root>/run/` for the gateway daemon's MCP socket.
pub const MCP_SOCKET_NAME: &str = "mcp.sock";

/// How often the deferred flow/orchestrator runtime touches its legacy
/// heartbeat file.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum mtime age for the deferred flow/orchestrator legacy
/// heartbeat. Gateway daemon health does not use this value.
pub const HEARTBEAT_GRACE: Duration = Duration::from_secs(60);

/// Maximum time a liveness probe may spend trying to connect to the
/// daemon MCP socket. Unix-domain socket connects are normally
/// immediate; the bound keeps status/doctor/MCP probes honest if the
/// platform stalls unexpectedly.
pub const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// Resolve the pidfile path for a given ccteam root.
pub fn pidfile_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("state").join(PIDFILE_NAME)
}

/// Resolve the deferred flow/orchestrator heartbeat-file path for a
/// given ccteam root.
pub fn heartbeat_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("state").join(HEARTBEAT_NAME)
}

/// Resolve the MCP socket that proves the gateway daemon is accepting
/// control-plane connections.
pub fn daemon_socket_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("run").join(MCP_SOCKET_NAME)
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
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
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
    let body = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
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

/// Touch the deferred flow/orchestrator heartbeat file
/// (create-or-bump-mtime). Gateway daemon liveness is MCP socket
/// reachability, not this file.
pub fn write_heartbeat(paths: &CcteamPaths) -> Result<()> {
    let path = heartbeat_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
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

/// Outcome of a daemon health check. `Healthy` means a client can
/// connect to the daemon MCP socket; `Unreachable` is a fail-loud signal
/// for callers (MCP tools, meta-agent skill startup, status/doctor) to
/// surface "daemon down" rather than silently continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonHealth {
    /// MCP socket accepted a connection.
    Healthy { socket: PathBuf },
    /// MCP socket could not be reached.
    Unreachable { socket: PathBuf, reason: String },
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
            DaemonHealth::Healthy { socket } => {
                format!(
                    "daemon healthy: MCP socket reachable at {}",
                    socket.display()
                )
            }
            DaemonHealth::Unreachable { socket, reason } => format!(
                "daemon down: cannot connect to MCP socket at {} ({reason}); \
                 start it with `ccteam start`",
                socket.display()
            ),
        }
    }
}

/// Connect to the daemon MCP socket and classify daemon liveness.
pub fn check_health(paths: &CcteamPaths) -> DaemonHealth {
    check_health_at(&daemon_socket_path(paths), DAEMON_CONNECT_TIMEOUT)
}

/// Boolean variant of [`check_health`] for callers that only care "up
/// or down" (text/json `ls` annotation).
pub fn daemon_reachable(paths: &CcteamPaths) -> bool {
    check_health(paths).is_healthy()
}

/// Testable inner: classify based on whether `path` accepts a Unix
/// socket connection before `timeout`.
pub fn check_health_at(path: &Path, timeout: Duration) -> DaemonHealth {
    match connect_mcp_socket(path, timeout) {
        Ok(()) => DaemonHealth::Healthy {
            socket: path.to_path_buf(),
        },
        Err(reason) => DaemonHealth::Unreachable {
            socket: path.to_path_buf(),
            reason,
        },
    }
}

#[cfg(unix)]
fn connect_mcp_socket(path: &Path, timeout: Duration) -> std::result::Result<(), String> {
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    let path = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = UnixStream::connect(&path)
            .map(|_| ())
            .map_err(|err| err.to_string());
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("connect timed out after {}ms", timeout.as_millis()))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("connect worker exited".to_string()),
    }
}

#[cfg(not(unix))]
fn connect_mcp_socket(_path: &Path, _timeout: Duration) -> std::result::Result<(), String> {
    Err("MCP Unix socket liveness is only supported on Unix".to_string())
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
    fn remove_heartbeat_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        remove_heartbeat(&p);
        write_heartbeat(&p).unwrap();
        remove_heartbeat(&p);
        assert!(!heartbeat_path(&p).exists());
    }

    #[test]
    fn check_health_reports_unreachable_when_socket_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let health = check_health(&p);
        assert!(
            matches!(health, DaemonHealth::Unreachable { .. }),
            "got {health:?}"
        );
        assert!(!health.is_healthy());
    }

    #[cfg(unix)]
    #[test]
    fn check_health_reports_healthy_when_mcp_socket_accepts_connections() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let socket = daemon_socket_path(&p);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let health = check_health(&p);
        assert!(health.is_healthy(), "got {health:?}");
        assert!(daemon_reachable(&p));
    }

    #[cfg(unix)]
    #[test]
    fn check_health_rejects_stale_socket_file_without_listener() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let socket = daemon_socket_path(&p);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        {
            let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        }

        let health = check_health(&p);
        assert!(
            matches!(health, DaemonHealth::Unreachable { .. }),
            "got {health:?}"
        );
        assert!(!daemon_reachable(&p));
    }

    #[test]
    fn daemon_health_describe_is_actionable_when_down() {
        let down = DaemonHealth::Unreachable {
            socket: PathBuf::from("/tmp/missing.sock"),
            reason: "not found".to_string(),
        };
        assert!(down.describe().contains("ccteam start"));
        assert!(down.describe().contains("missing.sock"));
    }
}
