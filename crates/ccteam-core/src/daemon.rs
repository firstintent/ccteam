//! Gateway daemon lifecycle core (v0.9.7).
//!
//! Codex-style pure-userland daemon management — the single lifecycle
//! mechanism on Linux / macOS / WSL (systemd / launchd are retired):
//!
//! - **pid record**: `~/.ccteam/state/orchestrator.pid` now holds a JSON
//!   [`PidRecord`] (`pid` + `process_start_time` + `version` +
//!   `started_at`). Only the launcher (`ccteam daemon start`) writes it;
//!   a record whose `(pid, process_start_time)` matches a live OS
//!   process is **managed**. The legacy bare-pid format fails to parse
//!   and is treated as stale (pre-v1.0: no migration).
//! - **operation lock**: `~/.ccteam/state/daemon.lock` (flock) serialises
//!   every mutating lifecycle operation (start / stop / restart).
//!   Read-only probes never take it.
//! - **versioned probe**: an MCP `initialize` handshake on
//!   `~/.ccteam/run/mcp.sock` yields `{ready, serverInfo.version}` so
//!   callers can compare the running daemon against the on-disk binary.
//!   The cheap connect-only [`check_health`] stays for hot paths.
//! - **graceful stop** = SIGTERM + poll until the record no longer
//!   matches a live process (the trigger-file channel is retired).
//!   SIGKILL only via the explicit `--force` escalation (owner-gated
//!   exception D2), and only ever for the daemon itself — never agent
//!   sessions.
//!
//! Adapted from openai/codex `app-server-daemon` (`backend/pid.rs`,
//! `lib.rs`, `client.rs`), Apache-2.0 — see `LICENSES.md`.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::CcteamPaths;

/// Filename under `<root>/state/` where the launcher stores the managed
/// daemon's [`PidRecord`] (JSON since v0.9.7; the filename is stable).
pub const PIDFILE_NAME: &str = "orchestrator.pid";

/// Filename under `<root>/state/` for the lifecycle operation lock
/// (flock; serialises start / stop / restart).
pub const OPERATION_LOCK_NAME: &str = "daemon.lock";

/// Filename under `<root>/` where a detached daemon's stdout + stderr
/// land (`ccteam daemon logs` reads it).
pub const DAEMON_LOG_NAME: &str = "daemon.log";

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

/// Maximum time a cheap liveness probe may spend trying to connect to
/// the daemon MCP socket. Unix-domain socket connects are normally
/// immediate; the bound keeps status/doctor/MCP probes honest if the
/// platform stalls unexpectedly.
pub const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// Single-attempt budget for the versioned MCP `initialize` probe
/// (connect + handshake + version extraction).
pub const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Default bound on waiting for the operation lock. Sized so a
/// legitimate concurrent `restart` (stop ≤40s + start ≤15s) finishes
/// before we give up with a readable error — never a silent forever-wait.
pub const OPERATION_LOCK_TIMEOUT: Duration = Duration::from_secs(75);

/// Poll cadence shared by the ready-wait and stop-wait loops.
pub const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Default budget for a spawned daemon to become ready (probe answers).
pub const START_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Default budget for a SIGTERM'd daemon to exit (its internal drain is
/// ~5s per task; 40s leaves generous headroom).
pub const STOP_TERM_WAIT: Duration = Duration::from_secs(40);

/// Extra budget after a `--force` SIGKILL escalation.
pub const STOP_KILL_WAIT: Duration = Duration::from_secs(5);

/// How much of the daemon log tail to surface on a failed start.
pub const DAEMON_LOG_TAIL_BYTES: u64 = 4096;

/// Resolve the pid-record path for a given ccteam root.
pub fn pidfile_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("state").join(PIDFILE_NAME)
}

/// Resolve the lifecycle operation-lock path for a given ccteam root.
pub fn operation_lock_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join("state").join(OPERATION_LOCK_NAME)
}

/// Resolve the detached daemon's log file for a given ccteam root.
pub fn daemon_log_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join(DAEMON_LOG_NAME)
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

// ---------------------------------------------------------------------------
// pid record
// ---------------------------------------------------------------------------

/// Managed-daemon ownership record (JSON body of the pidfile).
///
/// `(pid, process_start_time)` together identify one specific OS process
/// — a recycled pid with a different start time never matches, so stale
/// records can never make ccteam signal an innocent process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidRecord {
    pub pid: u32,
    /// OS start-time fingerprint: Linux = `/proc/<pid>/stat` field 22
    /// (clock ticks since boot, stringified); macOS = `ps -o lstart=`.
    pub process_start_time: String,
    /// Workspace version of the launcher that spawned this daemon.
    pub version: String,
    /// RFC3339 spawn timestamp (informational).
    pub started_at: String,
}

/// Read + parse the pid record. `None` covers "missing", "unreadable"
/// AND "unparseable" (including the pre-v0.9.7 bare-pid format): all
/// three mean "no managed daemon" — a stale record is simply taken over
/// on the next start.
pub fn read_pid_record(path: &Path) -> Option<PidRecord> {
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Atomically publish a pid record (tmp + rename in the same dir).
pub fn write_pid_record(path: &Path, record: &PidRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("pid.tmp");
    let body = serde_json::to_vec(record).context("serialize pid record")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("publish pid record {}", path.display()));
    }
    Ok(())
}

/// True iff a process with this pid exists (kill(pid, 0); EPERM counts
/// as "exists" — it proves the pid is live even if owned by another
/// user).
#[cfg(unix)]
pub fn process_exists(pid: u32) -> bool {
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if raw <= 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) probes existence; failure is reported via the
    // return value, no memory is touched.
    let rc = unsafe { libc::kill(raw, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn process_exists(_pid: u32) -> bool {
    false
}

/// Read the OS start-time fingerprint for `pid`.
///
/// Linux: field 22 (`starttime`) of `/proc/<pid>/stat` — clock ticks
/// since boot; immutable for the life of the process, recycled pids get
/// a different value. The comm field may contain spaces/parens, so
/// fields are parsed after the LAST `)`.
#[cfg(target_os = "linux")]
pub fn read_process_start_time(pid: u32) -> Result<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("read /proc/{pid}/stat"))?;
    let (_, rest) = stat
        .rsplit_once(')')
        .ok_or_else(|| anyhow!("malformed /proc/{pid}/stat (no comm terminator)"))?;
    // After `)` the fields are state(3) ppid(4) … starttime(22): the
    // 20th whitespace-separated token.
    let start = rest
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow!("/proc/{pid}/stat has no starttime field"))?;
    Ok(start.to_string())
}

/// macOS (and other non-Linux Unix) variant: `ps -p <pid> -o lstart=`
/// (the codex approach — second-resolution but stable for the process
/// lifetime).
#[cfg(all(unix, not(target_os = "linux")))]
pub fn read_process_start_time(pid: u32) -> Result<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .context("invoke ps for daemon start time")?;
    if !output.status.success() {
        anyhow::bail!("failed to read start time for pid {pid}");
    }
    let start_time = String::from_utf8(output.stdout).context("ps lstart was not utf-8")?;
    let start_time = start_time.trim();
    if start_time.is_empty() {
        anyhow::bail!("pid {pid} has no recorded start time");
    }
    Ok(start_time.to_string())
}

#[cfg(not(unix))]
pub fn read_process_start_time(_pid: u32) -> Result<String> {
    anyhow::bail!("daemon lifecycle is only supported on Unix")
}

/// True iff the record's `(pid, process_start_time)` matches a live OS
/// process. Conservative: any verification failure counts as "not ours"
/// so ccteam never signals a process it cannot prove it spawned.
pub fn process_matches_record(record: &PidRecord) -> bool {
    if !process_exists(record.pid) {
        return false;
    }
    match read_process_start_time(record.pid) {
        Ok(start_time) => start_time == record.process_start_time,
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// operation lock
// ---------------------------------------------------------------------------

/// Held flock on `state/daemon.lock`. Dropping releases the lock.
#[derive(Debug)]
pub struct OperationLock {
    _file: std::fs::File,
}

/// Typed lifecycle failure: `code` feeds the `--json` machine contract
/// (`error{code,message}`), `message` is the human line.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct LifecycleError {
    pub code: &'static str,
    pub message: String,
}

impl LifecycleError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// flock(LOCK_EX | LOCK_NB): `Ok(true)` = acquired, `Ok(false)` = held
/// elsewhere.
#[cfg(unix)]
fn try_lock_file(file: &std::fs::File) -> Result<bool> {
    use std::os::fd::AsRawFd;

    // SAFETY: flock on a valid owned fd; failure is reported via errno.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false);
    }
    Err(err).context("flock daemon operation lock")
}

#[cfg(not(unix))]
fn try_lock_file(_file: &std::fs::File) -> Result<bool> {
    anyhow::bail!("daemon lifecycle is only supported on Unix")
}

/// Acquire the lifecycle operation lock with the default bound.
pub fn acquire_operation_lock(paths: &CcteamPaths) -> Result<OperationLock> {
    acquire_operation_lock_with_timeout(paths, OPERATION_LOCK_TIMEOUT)
}

/// Acquire the lifecycle operation lock, waiting at most `timeout`.
/// A busy lock past the bound is a readable [`LifecycleError`]
/// (`code = "lockBusy"`), never a silent forever-wait.
pub fn acquire_operation_lock_with_timeout(
    paths: &CcteamPaths,
    timeout: Duration,
) -> Result<OperationLock> {
    let path = operation_lock_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open daemon operation lock {}", path.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        if try_lock_file(&file)? {
            return Ok(OperationLock { _file: file });
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::new(
                "lockBusy",
                format!(
                    "another ccteam daemon operation is in progress (lock {} is held); \
                     retry once it finishes",
                    path.display()
                ),
            )
            .into());
        }
        std::thread::sleep(LIFECYCLE_POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// versioned probe
// ---------------------------------------------------------------------------

/// Result of the versioned readiness probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonProbe {
    /// The daemon answered an MCP `initialize` on its socket.
    pub ready: bool,
    /// `serverInfo.version` from the handshake (the running daemon's
    /// workspace version), when the response carried one.
    pub version: Option<String>,
}

/// Versioned readiness probe against the daemon socket for `paths`.
pub fn probe_daemon(paths: &CcteamPaths) -> DaemonProbe {
    probe_daemon_at(&daemon_socket_path(paths), DAEMON_PROBE_TIMEOUT)
}

/// Versioned readiness probe: connect to the MCP socket, send one
/// `initialize`, read one response line, extract `serverInfo.version`.
/// The whole attempt is bounded by `timeout` (worker thread + channel,
/// same pattern as [`check_health_at`]); any failure = not ready.
pub fn probe_daemon_at(socket: &Path, timeout: Duration) -> DaemonProbe {
    use std::sync::mpsc;

    let socket = socket.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe_blocking(&socket, timeout));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(version)) => DaemonProbe {
            ready: true,
            version,
        },
        _ => DaemonProbe {
            ready: false,
            version: None,
        },
    }
}

#[cfg(unix)]
fn probe_blocking(socket: &Path, timeout: Duration) -> std::result::Result<Option<String>, String> {
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket).map_err(|err| err.to_string())?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "clientInfo": { "name": "ccteam-daemon-probe", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {},
        },
    });
    let mut stream = stream;
    let mut line = serde_json::to_string(&request).map_err(|err| err.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|err| err.to_string())?;
    stream.flush().map_err(|err| err.to_string())?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|err| err.to_string())?;
    if response.trim().is_empty() {
        return Err("daemon closed the socket without answering initialize".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_str(response.trim()).map_err(|err| err.to_string())?;
    Ok(value
        .pointer("/result/serverInfo/version")
        .and_then(|v| v.as_str())
        .map(str::to_string))
}

#[cfg(not(unix))]
fn probe_blocking(
    _socket: &Path,
    _timeout: Duration,
) -> std::result::Result<Option<String>, String> {
    Err("MCP Unix socket probing is only supported on Unix".to_string())
}

// ---------------------------------------------------------------------------
// lifecycle: start / stop / status
// ---------------------------------------------------------------------------

/// What the launcher spawns and where its output goes. `program` is
/// `canonicalize(current_exe())` in production; tests inject a fake.
#[derive(Debug, Clone)]
pub struct DaemonStartSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// stdout + stderr of the detached daemon (append mode).
    pub log_path: PathBuf,
    /// Budget for the spawned daemon to become probe-ready.
    pub ready_timeout: Duration,
}

/// Outcome of a start request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartVerdict {
    Started { pid: u32, version: Option<String> },
    AlreadyRunning { version: Option<String> },
}

/// Outcome of a stop request. Refusals and timeouts are verdicts (not
/// panics) so callers own the exit-code / JSON mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopVerdict {
    Stopped {
        pid: u32,
    },
    NotRunning,
    /// Socket is ready but no matching pid record: a foreground
    /// `ccteam start` or a user-supervised instance. Never signalled.
    RefusedNotManaged {
        hint: String,
    },
    /// Still alive after the wait (and after SIGKILL when forced).
    TimedOut {
        pid: u32,
    },
}

/// Stop-loop tuning (injectable for tests; defaults per PRD F1.5).
#[derive(Debug, Clone, Copy)]
pub struct StopTuning {
    pub poll_interval: Duration,
    pub term_wait: Duration,
    pub kill_wait: Duration,
}

impl Default for StopTuning {
    fn default() -> Self {
        Self {
            poll_interval: LIFECYCLE_POLL_INTERVAL,
            term_wait: STOP_TERM_WAIT,
            kill_wait: STOP_KILL_WAIT,
        }
    }
}

/// Send SIGTERM (ESRCH = already gone = Ok).
#[cfg(unix)]
fn send_sigterm(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGTERM)
}

/// Send SIGKILL (ESRCH = already gone = Ok). Only ever used on the
/// daemon process itself via the explicit `--force` escalation.
#[cfg(unix)]
fn send_sigkill(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGKILL)
}

#[cfg(unix)]
fn send_signal(pid: u32, sig: libc::c_int) -> Result<()> {
    let raw = libc::pid_t::try_from(pid).with_context(|| format!("pid {pid} out of range"))?;
    // SAFETY: kill with a validated pid; failure reported via errno.
    let rc = unsafe { libc::kill(raw, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).with_context(|| format!("signal {sig} to daemon pid {pid}"))
}

/// Reap `pid` iff it is a zombie child of THIS process (waitpid WNOHANG,
/// best-effort). Matters when the stopper is also the spawner (tests,
/// same-invocation restart): an unreaped zombie still answers
/// `kill(pid, 0)`, which would wedge the stop-wait loop.
#[cfg(unix)]
fn reap_if_child(pid: u32) {
    if let Ok(raw) = libc::pid_t::try_from(pid) {
        if raw > 0 {
            // SAFETY: WNOHANG waitpid never blocks; ECHILD (not our
            // child) is the common, harmless case.
            unsafe { libc::waitpid(raw, std::ptr::null_mut(), libc::WNOHANG) };
        }
    }
}

#[cfg(not(unix))]
fn reap_if_child(_pid: u32) {}

/// Read the last ~[`DAEMON_LOG_TAIL_BYTES`] of a log file, trimmed to
/// whole lines. `None` when the file is missing or empty.
pub fn read_log_tail(path: &Path, byte_limit: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let start = len.saturating_sub(byte_limit);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|b| *b == b'\n') {
            bytes.drain(..=newline);
        }
    }
    let contents = String::from_utf8_lossy(&bytes).trim_end().to_string();
    if contents.is_empty() {
        None
    } else {
        Some(contents)
    }
}

/// Build a `startFailed` [`LifecycleError`] with the daemon log tail
/// appended (the operator's first diagnostic).
fn start_failed(log_path: &Path, mut message: String) -> anyhow::Error {
    if let Some(tail) = read_log_tail(log_path, DAEMON_LOG_TAIL_BYTES) {
        message.push_str(&format!("\n\ndaemon log tail ({}):", log_path.display()));
        for line in tail.lines() {
            message.push_str("\n  ");
            message.push_str(line);
        }
    } else {
        message.push_str(&format!(
            "\n\n(no daemon log output at {})",
            log_path.display()
        ));
    }
    LifecycleError::new("startFailed", message).into()
}

/// Kill + reap a just-spawned child on a bookkeeping failure path
/// (codex error-path hygiene: never leave an untracked process behind).
#[cfg(unix)]
fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Start a managed daemon. **Caller must hold the operation lock.**
///
/// Four-quadrant behavior (PRD F1.3):
/// - ready → `AlreadyRunning` (idempotent; never fights a foreground or
///   user-supervised instance either).
/// - not ready + record matches a live process → wait for ready (a
///   launcher may have died mid-boot); never double-spawns.
/// - not ready + stale/garbled/missing record → take over: clear the
///   record and spawn fresh (covers dead pids, PID reuse, and the legacy
///   bare-pid format).
///
/// Every bookkeeping failure after spawn kills the child and removes the
/// pid record; failures carry the daemon log tail.
#[cfg(unix)]
pub fn start_managed(paths: &CcteamPaths, spec: &DaemonStartSpec) -> Result<StartVerdict> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let socket = daemon_socket_path(paths);
    let probe = probe_daemon_at(&socket, DAEMON_PROBE_TIMEOUT);
    if probe.ready {
        return Ok(StartVerdict::AlreadyRunning {
            version: probe.version,
        });
    }

    let pidfile = pidfile_path(paths);
    if let Some(parent) = pidfile.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if let Some(record) = read_pid_record(&pidfile) {
        if process_matches_record(&record) {
            // A managed daemon is alive but not (yet) ready — most
            // likely still booting after a launcher died mid-wait.
            // Wait instead of double-spawning.
            let deadline = Instant::now() + spec.ready_timeout;
            loop {
                let probe = probe_daemon_at(&socket, DAEMON_PROBE_TIMEOUT);
                if probe.ready {
                    return Ok(StartVerdict::AlreadyRunning {
                        version: probe.version,
                    });
                }
                if !process_matches_record(&record) {
                    // It died while we waited — fall through to a fresh
                    // takeover spawn below.
                    let _ = std::fs::remove_file(&pidfile);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(start_failed(
                        &spec.log_path,
                        format!(
                            "a managed daemon (pid {}) is running but its MCP socket at {} \
                             never became ready; inspect `ccteam daemon logs` or stop it with \
                             `ccteam daemon stop`",
                            record.pid,
                            socket.display()
                        ),
                    ));
                }
                std::thread::sleep(LIFECYCLE_POLL_INTERVAL);
            }
        } else {
            // Stale record (dead pid or PID reuse with a different start
            // time): take over.
            let _ = std::fs::remove_file(&pidfile);
        }
    } else if pidfile.exists() {
        // Unparseable (legacy bare-pid) record = stale by definition.
        let _ = std::fs::remove_file(&pidfile);
    }

    if let Some(parent) = spec.log_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)
        .with_context(|| format!("open daemon log {}", spec.log_path.display()))?;
    let log_stderr = log
        .try_clone()
        .with_context(|| format!("clone daemon log handle {}", spec.log_path.display()))?;

    let mut command = std::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_stderr));
    // SAFETY: setsid() in the forked child detaches it from our session
    // + controlling terminal (survives SSH disconnect / logout); it is
    // async-signal-safe as required by pre_exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|err| {
        start_failed(
            &spec.log_path,
            format!(
                "failed to spawn detached daemon using {}: {err}",
                spec.program.display()
            ),
        )
    })?;
    let pid = child.id();

    let process_start_time = match read_process_start_time(pid) {
        Ok(t) => t,
        Err(err) => {
            kill_and_reap(&mut child);
            let _ = std::fs::remove_file(&pidfile);
            return Err(start_failed(
                &spec.log_path,
                format!("failed to fingerprint daemon process {pid}: {err:#}"),
            ));
        }
    };
    let record = PidRecord {
        pid,
        process_start_time,
        version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(err) = write_pid_record(&pidfile, &record) {
        kill_and_reap(&mut child);
        let _ = std::fs::remove_file(&pidfile);
        return Err(start_failed(
            &spec.log_path,
            format!("failed to publish pid record: {err:#}"),
        ));
    }

    // Wait for readiness. A child that dies fails fast; a live child
    // that never answers fails at the deadline but is LEFT RUNNING with
    // its record intact (it is managed — `ccteam daemon stop` can end
    // it; killing a merely-slow boot would be worse).
    let deadline = Instant::now() + spec.ready_timeout;
    loop {
        let probe = probe_daemon_at(&socket, DAEMON_PROBE_TIMEOUT);
        if probe.ready {
            return Ok(StartVerdict::Started {
                pid,
                version: probe.version,
            });
        }
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        if exited || !process_matches_record(&record) {
            let _ = std::fs::remove_file(&pidfile);
            return Err(start_failed(
                &spec.log_path,
                format!("daemon process {pid} exited before becoming ready"),
            ));
        }
        if Instant::now() >= deadline {
            return Err(start_failed(
                &spec.log_path,
                format!(
                    "daemon (pid {pid}) did not become ready on {} within {:?}; it is still \
                     running — inspect `ccteam daemon logs` or `ccteam daemon stop` it",
                    socket.display(),
                    spec.ready_timeout
                ),
            ));
        }
        std::thread::sleep(LIFECYCLE_POLL_INTERVAL);
    }
}

#[cfg(not(unix))]
pub fn start_managed(_paths: &CcteamPaths, _spec: &DaemonStartSpec) -> Result<StartVerdict> {
    anyhow::bail!("daemon lifecycle is only supported on Unix")
}

/// Stop a managed daemon with default tuning. **Caller must hold the
/// operation lock.**
pub fn stop_managed(paths: &CcteamPaths, force: bool) -> Result<StopVerdict> {
    stop_managed_with(paths, force, StopTuning::default())
}

/// Stop with explicit tuning. Semantics (PRD F1.5 / F1.6):
/// - managed (record matches a live process) → SIGTERM, poll until the
///   record no longer matches, up to `term_wait`; with `force`, escalate
///   to SIGKILL after that wait (daemon only — agent sessions are never
///   touched) and wait `kill_wait` more.
/// - ready but not managed → refuse with guidance (foreground instance →
///   Ctrl-C; self-supervised → their supervisor).
/// - nothing running → `NotRunning` (stale records are swept).
#[cfg(unix)]
pub fn stop_managed_with(
    paths: &CcteamPaths,
    force: bool,
    tuning: StopTuning,
) -> Result<StopVerdict> {
    let pidfile = pidfile_path(paths);
    match read_pid_record(&pidfile) {
        Some(record) if process_matches_record(&record) => {
            let pid = record.pid;
            send_sigterm(pid)?;
            if wait_record_gone(&record, tuning.term_wait, tuning.poll_interval) {
                let _ = std::fs::remove_file(&pidfile);
                return Ok(StopVerdict::Stopped { pid });
            }
            if force {
                send_sigkill(pid)?;
                if wait_record_gone(&record, tuning.kill_wait, tuning.poll_interval) {
                    let _ = std::fs::remove_file(&pidfile);
                    return Ok(StopVerdict::Stopped { pid });
                }
            }
            Ok(StopVerdict::TimedOut { pid })
        }
        record => {
            // Stale (dead pid / PID reuse / legacy format) or missing —
            // sweep the file, then classify what is actually running.
            if record.is_some() || pidfile.exists() {
                let _ = std::fs::remove_file(&pidfile);
            }
            let probe = probe_daemon(paths);
            if probe.ready {
                let running = probe
                    .version
                    .map(|v| format!("version {v}"))
                    .unwrap_or_else(|| "unknown version".to_string());
                Ok(StopVerdict::RefusedNotManaged {
                    hint: format!(
                        "a ccteam daemon ({running}) is serving {} but is not managed by \
                         `ccteam daemon` — if it is a foreground `ccteam start`, press Ctrl-C \
                         in its terminal; if you run it under your own supervisor, stop it \
                         with that supervisor",
                        daemon_socket_path(paths).display()
                    ),
                })
            } else {
                Ok(StopVerdict::NotRunning)
            }
        }
    }
}

#[cfg(not(unix))]
pub fn stop_managed_with(
    _paths: &CcteamPaths,
    _force: bool,
    _tuning: StopTuning,
) -> Result<StopVerdict> {
    anyhow::bail!("daemon lifecycle is only supported on Unix")
}

/// Poll until the record no longer matches a live process. Reaps a
/// zombie child each round (see [`reap_if_child`]).
#[cfg(unix)]
fn wait_record_gone(record: &PidRecord, total: Duration, poll: Duration) -> bool {
    let deadline = Instant::now() + total;
    loop {
        reap_if_child(record.pid);
        if !process_matches_record(record) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Read-only dual-verdict snapshot (no lock): `ready` × `managed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatusReport {
    pub ready: bool,
    /// Running daemon's version from the probe (when ready).
    pub running_version: Option<String>,
    /// Record matches a live OS process.
    pub managed: bool,
    pub record: Option<PidRecord>,
    pub socket: PathBuf,
}

/// Probe both ownership signals: socket readiness (versioned) and pid
/// record ↔ live process.
pub fn daemon_status(paths: &CcteamPaths) -> DaemonStatusReport {
    let record = read_pid_record(&pidfile_path(paths));
    let managed = record.as_ref().map(process_matches_record).unwrap_or(false);
    let probe = probe_daemon(paths);
    DaemonStatusReport {
        ready: probe.ready,
        running_version: probe.version,
        managed,
        record,
        socket: daemon_socket_path(paths),
    }
}

// ---------------------------------------------------------------------------
// legacy heartbeat (deferred ccteam-flow only)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// cheap connect-only health check (hot paths)
// ---------------------------------------------------------------------------

/// Outcome of a daemon health check. `Healthy` means a client can
/// connect to the daemon MCP socket; `Unreachable` is a fail-loud signal
/// for callers (MCP tools, status/doctor, web) to surface "daemon down"
/// rather than silently continue.
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
                 start it with `ccteam daemon start`",
                socket.display()
            ),
        }
    }
}

/// Connect to the daemon MCP socket and classify daemon liveness.
/// Cheap connect-only check — the versioned handshake is
/// [`probe_daemon`].
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

    fn live_self_record() -> PidRecord {
        let pid = std::process::id();
        PidRecord {
            pid,
            process_start_time: read_process_start_time(pid).unwrap(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn pid_record_roundtrips_through_atomic_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state").join("orchestrator.pid");
        let record = live_self_record();
        write_pid_record(&path, &record).unwrap();
        assert_eq!(read_pid_record(&path), Some(record));
        // No tmp residue.
        assert!(!path.with_extension("pid.tmp").exists());
    }

    #[test]
    fn legacy_bare_pid_content_reads_as_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("orchestrator.pid");
        std::fs::write(&path, "12345\n").unwrap();
        assert_eq!(
            read_pid_record(&path),
            None,
            "bare pid = parse failure = stale"
        );
        std::fs::write(&path, "not json at all").unwrap();
        assert_eq!(read_pid_record(&path), None);
    }

    #[test]
    fn missing_pid_record_reads_as_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(read_pid_record(&tmp.path().join("absent.pid")), None);
    }

    #[cfg(unix)]
    #[test]
    fn process_matches_record_accepts_self_and_rejects_pid_reuse() {
        let record = live_self_record();
        assert!(process_matches_record(&record), "self must match");

        // Same live pid, wrong start time = PID-reuse guard trips.
        let reused = PidRecord {
            process_start_time: "0".to_string(),
            ..record.clone()
        };
        assert!(
            !process_matches_record(&reused),
            "a recycled pid with a different start time must not match"
        );

        // Dead pid never matches (pid 0 is never a real process here).
        let dead = PidRecord {
            pid: 0,
            ..record.clone()
        };
        assert!(!process_matches_record(&dead));
    }

    #[cfg(unix)]
    #[test]
    fn process_exists_detects_self_and_rejects_zero() {
        assert!(process_exists(std::process::id()));
        assert!(!process_exists(0));
    }

    #[cfg(unix)]
    #[test]
    fn operation_lock_is_exclusive_and_reports_busy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let held = acquire_operation_lock(&p).unwrap();
        // flock treats separate open file descriptions independently, so
        // a second acquisition in the same process still contends.
        let err = acquire_operation_lock_with_timeout(&p, Duration::from_millis(150)).unwrap_err();
        let lifecycle = err
            .downcast_ref::<LifecycleError>()
            .expect("busy lock must be a typed LifecycleError");
        assert_eq!(lifecycle.code, "lockBusy");
        drop(held);
        // Released → acquirable again.
        let _relock = acquire_operation_lock_with_timeout(&p, Duration::from_millis(150)).unwrap();
    }

    #[test]
    fn probe_reports_not_ready_when_socket_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let probe = probe_daemon(&p);
        assert!(!probe.ready);
        assert_eq!(probe.version, None);
    }

    #[cfg(unix)]
    #[test]
    fn probe_extracts_server_info_version_from_initialize() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("mcp.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let req: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                assert_eq!(req["method"], "initialize");
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "ccteam", "version": "7.7.7" },
                    },
                });
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let mut stream = stream;
                let _ = stream.write_all(out.as_bytes());
            }
        });
        let probe = probe_daemon_at(&socket, Duration::from_secs(2));
        assert!(probe.ready);
        assert_eq!(probe.version.as_deref(), Some("7.7.7"));
    }

    #[cfg(unix)]
    #[test]
    fn probe_not_ready_when_listener_never_answers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("mute.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // Accepts connections but never responds → versioned probe says
        // NOT ready (while the cheap connect check would say healthy).
        let probe = probe_daemon_at(&socket, Duration::from_millis(300));
        assert!(!probe.ready);
        assert!(check_health_at(&socket, Duration::from_millis(200)).is_healthy());
    }

    #[test]
    fn read_log_tail_returns_recent_complete_lines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("daemon.log");
        let contents = format!("{}\nrecent error\nusage", "x".repeat(4100));
        std::fs::write(&log, contents).unwrap();
        assert_eq!(
            read_log_tail(&log, DAEMON_LOG_TAIL_BYTES),
            Some("recent error\nusage".to_string())
        );
        // Missing / empty → None.
        assert_eq!(read_log_tail(&tmp.path().join("absent.log"), 4096), None);
        std::fs::write(tmp.path().join("empty.log"), "").unwrap();
        assert_eq!(read_log_tail(&tmp.path().join("empty.log"), 4096), None);
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
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(listener);
        std::thread::sleep(Duration::from_millis(10));

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
        assert!(down.describe().contains("ccteam daemon start"));
        assert!(down.describe().contains("missing.sock"));
    }

    #[cfg(unix)]
    #[test]
    fn daemon_status_reports_stale_record_as_unmanaged() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let record = PidRecord {
            pid: 0,
            process_start_time: "0".into(),
            version: "0.0.0".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        write_pid_record(&pidfile_path(&p), &record).unwrap();
        let report = daemon_status(&p);
        assert!(!report.ready);
        assert!(!report.managed, "dead pid must not read as managed");
        assert_eq!(report.record, Some(record));
    }
}
