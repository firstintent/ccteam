//! Which OS process is a managed session's **body** right now — the durable
//! fact behind the rule *one sid, one body*.
//!
//! A session's identity is its `sid` (persistent, monotone, survives daemon
//! restarts). Its body is the vendor process that currently carries it: the
//! `claude` stream-json child, an ACP child, a pi rpc child. The daemon that
//! spawned a body owns its stdio; nothing else can observe it. When that
//! daemon exits — gracefully or not — a stdio body is NOT necessarily gone:
//! an idle one exits on stdin EOF, but a body that is mid-turn keeps working
//! until it is done (measured on `claude`: the in-flight turn, plus any
//! self-continuation, runs to completion after the pipe closes). The next
//! daemon must therefore never assume "every child died with me", or it
//! spawns a second body for a sid whose first body is still writing into the
//! same working tree — the 2026-08-19 double-writer incident.
//!
//! This module records the body at spawn (`<project>/.ccteam/chat/<sid>/
//! body.json`: pid + an OS start-time fingerprint so a recycled pid can never
//! match), clears it when the body's exit is OBSERVED, and answers the one
//! question the spawn gate asks: *is a body for this sid provably alive right
//! now?* The record is a hint; liveness is always re-verified against the OS.
//!
//! ## Honesty / red lines
//!
//! - Conservative by construction: any verification failure (no fingerprint,
//!   fingerprint mismatch, a readable `environ` that does not name this sid)
//!   answers "no live body", so the gate never blocks a sid on a process
//!   ccteam cannot prove it spawned.
//! - Same-uid soft identity, like every other rung: a same-uid process can
//!   forge a `body.json`. What this prevents is a *misattribution by ccteam
//!   itself* (two bodies for one sid), not an attack.
//! - [`terminate`] exists for EXPLICIT user stops (`/stop`, `session_stop`,
//!   project stop). The daemon never calls it on its own initiative — a
//!   detached body is waited for, not killed (AGENTS.md §三 "永不主动 kill").

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::fs_atomic::atomic_write_durable;
use super::turns_mirror::chat_dir;

/// File name of the body record under the session's chat dir.
pub const BODY_FILENAME: &str = "body.json";

/// The environment variable every managed spawn sets on its body (see each
/// adapter's env builder). On Linux the probe cross-checks it in
/// `/proc/<pid>/environ` as a second, independent proof of identity.
pub const BODY_SID_ENV: &str = "CCTEAM_CHAT_SID";

/// One session's recorded body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBody {
    /// OS pid of the vendor process.
    pub pid: u32,
    /// OS start-time fingerprint of that pid (see [`process_fingerprint`]);
    /// `(pid, fingerprint)` identifies exactly one process incarnation.
    pub fingerprint: String,
    /// The adapter that spawned it (`claude-stream-json`, `grok-acp`, …).
    /// Informational — the gate does not branch on it.
    #[serde(default)]
    pub adapter: String,
    /// RFC3339 time the record was written (informational).
    #[serde(default)]
    pub recorded_at: String,
}

/// What [`probe`] found for a sid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyProbe {
    /// No record on disk — nothing was ever spawned for this sid here, or the
    /// last body's exit was observed and the record cleared.
    Absent,
    /// A record exists but its process is gone (exited unobserved, or the pid
    /// was recycled — either way, not a live body of this sid).
    Gone(SessionBody),
    /// The recorded process is alive AND provably this sid's body.
    Alive(SessionBody),
}

impl BodyProbe {
    /// The live body, if any.
    pub fn alive(&self) -> Option<&SessionBody> {
        match self {
            BodyProbe::Alive(body) => Some(body),
            _ => None,
        }
    }
}

/// `<project>/.ccteam/chat/<sid>/body.json`.
pub fn body_path(project_dir: &Path, sid: &str) -> PathBuf {
    chat_dir(project_dir, sid).join(BODY_FILENAME)
}

/// Record `pid` as the body of `sid` (atomic write; creates the chat dir).
/// Call right after the spawn succeeds and BEFORE any handshake that could
/// let the body run away from the record. `None` pid (already reaped) records
/// nothing. Best-effort for callers: a write failure only means the next
/// daemon cannot see this body — log it, never fail the spawn over it.
pub fn record(project_dir: &Path, sid: &str, pid: Option<u32>, adapter: &str) -> Result<()> {
    let Some(pid) = pid else {
        return Ok(());
    };
    if sid.is_empty() {
        return Ok(());
    }
    let fingerprint = process_fingerprint(pid)
        .with_context(|| format!("read start-time fingerprint for body pid {pid} of {sid}"))?;
    let body = SessionBody {
        pid,
        fingerprint,
        adapter: adapter.to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
    };
    let path = body_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&body).context("serialize body.json")?;
    atomic_write_durable(&path, &bytes)
}

/// Forget the recorded body of `sid` (its exit was observed, or it was
/// stopped). Idempotent: a missing file is not an error.
pub fn clear(project_dir: &Path, sid: &str) {
    let path = body_path(project_dir, sid);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "session_body: clear failed");
        }
    }
}

/// Read the record, if any. Unparseable content reads as `None` (and is left
/// in place for inspection — a probe then answers `Absent`).
pub fn read(project_dir: &Path, sid: &str) -> Option<SessionBody> {
    let raw = std::fs::read_to_string(body_path(project_dir, sid)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Is a body for `sid` provably alive right now? See the module doc for the
/// exact proof: pid exists ∧ fingerprint unchanged ∧ (Linux, when readable)
/// `environ` names this sid.
pub fn probe(project_dir: &Path, sid: &str) -> BodyProbe {
    let Some(body) = read(project_dir, sid) else {
        return BodyProbe::Absent;
    };
    if body_is_alive(&body, sid) {
        BodyProbe::Alive(body)
    } else {
        BodyProbe::Gone(body)
    }
}

/// Re-verify a known record against the OS (the watcher's poll). Same proof
/// as [`probe`] without the disk read.
pub fn body_is_alive(body: &SessionBody, sid: &str) -> bool {
    if !process_exists(body.pid) || process_is_zombie(body.pid) {
        return false;
    }
    match process_fingerprint(body.pid) {
        Ok(now) if now == body.fingerprint => {}
        _ => return false,
    }
    match environ_names_sid(body.pid, sid) {
        // Readable and it names another sid (or none): a recycled pid that
        // happens to share the fingerprint resolution — not ours.
        Some(false) => false,
        // Readable and ours, or not inspectable on this platform/pid (the
        // fingerprint already proved the incarnation).
        _ => true,
    }
}

/// Terminate a body that a user EXPLICITLY asked to stop: SIGTERM, wait up to
/// `grace`, then SIGKILL. Each signal re-verifies the fingerprint first so a
/// pid recycled mid-call is never touched. Returns `Ok(true)` if the process
/// is gone afterwards.
pub fn terminate(body: &SessionBody, sid: &str, grace: Duration) -> Result<bool> {
    if !body_is_alive(body, sid) {
        return Ok(true);
    }
    signal(body.pid, libc::SIGTERM)?;
    let started = Instant::now();
    while started.elapsed() < grace {
        if !body_is_alive(body, sid) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !body_is_alive(body, sid) {
        return Ok(true);
    }
    signal(body.pid, libc::SIGKILL)?;
    // SIGKILL is not instantaneous; give the kernel a moment to reap.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if !body_is_alive(body, sid) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(!body_is_alive(body, sid))
}

#[cfg(unix)]
fn signal(pid: u32, sig: libc::c_int) -> Result<()> {
    let raw = libc::pid_t::try_from(pid).context("pid out of range")?;
    // SAFETY: kill(2) on a pid we just verified; the return value reports
    // failure, no memory is touched.
    let rc = unsafe { libc::kill(raw, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).with_context(|| format!("signal {sig} to body pid {pid}"))
}

#[cfg(not(unix))]
fn signal(_pid: u32, _sig: i32) -> Result<()> {
    anyhow::bail!("session bodies are only managed on Unix")
}

/// True iff a process with this pid exists (`kill(pid, 0)`; EPERM counts as
/// "exists" — it proves the pid is live even if owned by another user).
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

/// Read the OS start-time fingerprint for `pid` — the value that, together
/// with the pid, identifies ONE process incarnation (a recycled pid gets a
/// different value).
///
/// Linux: field 22 (`starttime`) of `/proc/<pid>/stat` — clock ticks since
/// boot, immutable for the life of the process. The comm field may contain
/// spaces/parens, so fields are parsed after the LAST `)`.
#[cfg(target_os = "linux")]
pub fn process_fingerprint(pid: u32) -> Result<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("read /proc/{pid}/stat"))?;
    let (_, rest) = stat
        .rsplit_once(')')
        .ok_or_else(|| anyhow::anyhow!("malformed /proc/{pid}/stat (no comm terminator)"))?;
    // After `)` the fields are state(3) ppid(4) … starttime(22): the 20th
    // whitespace-separated token.
    let start = rest
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow::anyhow!("/proc/{pid}/stat has no starttime field"))?;
    Ok(start.to_string())
}

/// macOS (and other non-Linux Unix): `ps -p <pid> -o lstart=` — second
/// resolution but stable for the process lifetime (the codex approach).
#[cfg(all(unix, not(target_os = "linux")))]
pub fn process_fingerprint(pid: u32) -> Result<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .context("invoke ps for process start time")?;
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
pub fn process_fingerprint(_pid: u32) -> Result<String> {
    anyhow::bail!("process fingerprints are only supported on Unix")
}

/// An exited-but-unreaped process still "exists" for `kill(pid, 0)` and
/// still has a readable `/proc/<pid>/stat`; it is not a live body. (A body
/// orphaned by a dead daemon is reparented to init and reaped promptly; this
/// matters for bodies whose parent is still around — tests, or a daemon that
/// has not dropped the handle yet.)
#[cfg(target_os = "linux")]
fn process_is_zombie(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    matches!(rest.split_whitespace().next(), Some("Z") | Some("X"))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_is_zombie(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .ok()
        .map(|out| {
            out.status.success() && String::from_utf8_lossy(&out.stdout).trim().starts_with('Z')
        })
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn process_is_zombie(_pid: u32) -> bool {
    false
}

/// Linux: does `/proc/<pid>/environ` carry `CCTEAM_CHAT_SID=<sid>`?
/// `None` = not inspectable (no /proc, unreadable, zombie/exec'd away).
#[cfg(target_os = "linux")]
fn environ_names_sid(pid: u32, sid: &str) -> Option<bool> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    if raw.is_empty() {
        // A zombie or a process that dropped its environment: nothing to
        // cross-check against.
        return None;
    }
    let needle = format!("{BODY_SID_ENV}={sid}");
    Some(
        raw.split(|b| *b == 0)
            .any(|entry| entry == needle.as_bytes()),
    )
}

#[cfg(not(target_os = "linux"))]
fn environ_names_sid(_pid: u32, _sid: &str) -> Option<bool> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn spawn_sleep(sid: &str) -> std::process::Child {
        Command::new("sleep")
            .arg("30")
            .env(BODY_SID_ENV, sid)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep")
    }

    #[test]
    fn absent_without_record() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(probe(tmp.path(), "s1"), BodyProbe::Absent);
        // clear is idempotent on a missing file.
        clear(tmp.path(), "s1");
    }

    #[test]
    fn record_probe_alive_then_gone_after_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = spawn_sleep("s7");
        record(tmp.path(), "s7", Some(child.id()), "test").unwrap();
        let body = read(tmp.path(), "s7").expect("record written");
        assert_eq!(body.pid, child.id());
        assert_eq!(body.adapter, "test");
        assert!(!body.fingerprint.is_empty());
        assert!(matches!(probe(tmp.path(), "s7"), BodyProbe::Alive(_)));

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            matches!(probe(tmp.path(), "s7"), BodyProbe::Gone(_)),
            "the record stays until an observer clears it, but the body is gone"
        );
        clear(tmp.path(), "s7");
        assert_eq!(probe(tmp.path(), "s7"), BodyProbe::Absent);
    }

    #[test]
    fn fingerprint_mismatch_is_not_a_live_body() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = spawn_sleep("s8");
        record(tmp.path(), "s8", Some(child.id()), "test").unwrap();
        // Forge a stale fingerprint (a recycled pid from an earlier boot).
        let mut body = read(tmp.path(), "s8").unwrap();
        body.fingerprint = "0".to_string();
        let path = body_path(tmp.path(), "s8");
        std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(matches!(probe(tmp.path(), "s8"), BodyProbe::Gone(_)));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn environ_naming_another_sid_is_not_a_live_body() {
        let tmp = tempfile::tempdir().unwrap();
        // The process carries CCTEAM_CHAT_SID=s9 but is recorded under s10:
        // same pid + same fingerprint, yet provably not s10's body.
        let mut child = spawn_sleep("s9");
        record(tmp.path(), "s10", Some(child.id()), "test").unwrap();
        assert!(matches!(probe(tmp.path(), "s10"), BodyProbe::Gone(_)));
        // And recorded under its own sid it IS alive.
        record(tmp.path(), "s9", Some(child.id()), "test").unwrap();
        assert!(matches!(probe(tmp.path(), "s9"), BodyProbe::Alive(_)));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn terminate_stops_a_live_body_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = spawn_sleep("s11");
        record(tmp.path(), "s11", Some(child.id()), "test").unwrap();
        let body = read(tmp.path(), "s11").unwrap();
        assert!(terminate(&body, "s11", Duration::from_secs(3)).unwrap());
        // Reap so the pid cannot linger as a zombie in this test process.
        let _ = child.wait();
        assert!(!body_is_alive(&body, "s11"));
        // A second call on the dead body is a no-op success.
        assert!(terminate(&body, "s11", Duration::from_millis(100)).unwrap());
    }

    #[test]
    fn record_with_no_pid_or_empty_sid_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        record(tmp.path(), "s12", None, "test").unwrap();
        assert_eq!(probe(tmp.path(), "s12"), BodyProbe::Absent);
        record(tmp.path(), "", Some(std::process::id()), "test").unwrap();
        assert!(!body_path(tmp.path(), "").exists());
    }
}
