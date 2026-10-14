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
//! - [`terminate`] ends a body's PROCESS GROUP (the body and every
//!   background process it started — bodies are spawned as group leaders)
//!   on an EXPLICIT user stop (`/stop`, `agent_stop`, project stop): the
//!   session is over, and so is its work. A body that is not its own group
//!   leader (spawned by an older build, inside the daemon's group) gets only
//!   its own pid signalled: ccteam never signals a group it did not create.
//! - [`kill_body`] ends ONLY the body's process, keeping the session (a user
//!   `/interrupt`, or a turn queued behind a detached body that is idle or
//!   stalled): AGENTS.md §三 "kill 的是进程,不是会话". The processes it
//!   started are the user's work and are never signalled; the ones still
//!   running afterwards are reported ([`Orphan`]) so nobody has to guess what
//!   holds a port.

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

/// End a body and the background processes it started: SIGTERM, wait up to
/// `grace` for the body to go, then SIGKILL. Each signal re-verifies the
/// fingerprint first so a pid recycled mid-call is never touched, and goes to
/// the body's process group when the body leads its own group (see the
/// module doc) — `claude` answers SIGTERM by ending its background tasks
/// itself, and the group SIGKILL sweeps whatever ignored that, so no orphaned
/// dev server is left holding a port the resumed session needs. Returns
/// `Ok(true)` if the body is gone afterwards.
pub fn terminate(body: &SessionBody, sid: &str, grace: Duration) -> Result<bool> {
    if !body_is_alive(body, sid) {
        // The body is gone — another end (an `/interrupt`) may have killed
        // just it — but its group may not be: a stop still ends what the
        // body started.
        sweep_group_left_by(body.pid, sid)?;
        return Ok(true);
    }
    let group = leads_own_group(body.pid);
    signal_body(body.pid, group, libc::SIGTERM)?;
    let started = Instant::now();
    while started.elapsed() < grace && body_is_alive(body, sid) {
        std::thread::sleep(Duration::from_millis(100));
    }
    if body_is_alive(body, sid) {
        signal_body(body.pid, group, libc::SIGKILL)?;
    } else if group {
        // The leader is gone; sweep group members that outlived it. The
        // group id cannot be reused while any member is alive.
        signal_group(body.pid, libc::SIGKILL)?;
    }
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

/// SIGKILL the process group a dead body led, when a member is still
/// running AND names this sid in its environment (the body's children
/// inherit `CCTEAM_CHAT_SID`). A group id cannot be reused while a member
/// lives; the environment check keeps a group that merely shares the number
/// — the body's pid recycled as another group's leader — untouched.
fn sweep_group_left_by(pgid: u32, sid: &str) -> Result<()> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Ok(());
    };
    let ours = entries.flatten().any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            return false;
        };
        let in_group = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().nth(2))
                    .and_then(|g| g.parse::<u32>().ok())
            })
            == Some(pgid);
        in_group && environ_names_sid(pid, sid) == Some(true)
    });
    if ours {
        signal_group(pgid, libc::SIGKILL)?;
    }
    Ok(())
}

/// Signal the body, as a group when it leads one.
fn signal_body(pid: u32, group: bool, sig: libc::c_int) -> Result<()> {
    if group {
        signal_group(pid, sig)
    } else {
        signal(pid, sig)
    }
}

/// True iff `pid` is the leader of its own process group (pgid == pid).
#[cfg(unix)]
fn leads_own_group(pid: u32) -> bool {
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: getpgid(2) reads a process attribute; no memory is touched.
    let pgid = unsafe { libc::getpgid(raw) };
    pgid == raw
}

#[cfg(not(unix))]
fn leads_own_group(_pid: u32) -> bool {
    false
}

/// A process a body started that is still running after the body was ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orphan {
    pub pid: u32,
    /// Its command line, redacted and shortened for a receipt
    /// ([`redact_cmdline`]).
    pub cmd: String,
    /// Its OS start-time fingerprint, so a report made later never names a
    /// process that merely reuses the pid.
    pub fingerprint: String,
}

impl Orphan {
    /// Is this exact process (pid + start time) still running?
    pub fn still_running(&self) -> bool {
        !process_is_zombie(self.pid)
            && process_fingerprint(self.pid).is_ok_and(|now| now == self.fingerprint)
    }
}

/// Longest command line an [`Orphan`] carries.
const ORPHAN_CMD_MAX: usize = 80;

/// Every live descendant of `pid` with its start fingerprint, both taken
/// from ONE read of `/proc/<n>/stat` (Linux; empty where there is no
/// `/proc`).
fn descendants(pid: u32) -> Vec<(u32, String)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    // (pid, ppid, starttime)
    let mut table: Vec<(u32, u32, String)> = Vec::new();
    for entry in entries.flatten() {
        let Some(child) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{child}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After `)`: state(0) ppid(1) … starttime(19).
        let (Some(ppid), Some(start)) = (
            fields.get(1).and_then(|p| p.parse::<u32>().ok()),
            fields.get(19),
        ) else {
            continue;
        };
        table.push((child, ppid, (*start).to_string()));
    }
    let mut out: Vec<(u32, String)> = Vec::new();
    let mut frontier = vec![pid];
    while let Some(parent) = frontier.pop() {
        for (child, ppid, start) in &table {
            if *ppid == parent && !out.iter().any(|(p, _)| p == child) {
                out.push((*child, start.clone()));
                frontier.push(*child);
            }
        }
    }
    out
}

fn cmdline_argv(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// A command line fit for an IM receipt and a ledger: what the user would
/// recognise, never a secret or the home path.
///
/// - claude's Bash tool wraps every command as `bash -c 'source
///   ~/.claude/shell-snapshots/… && eval '<cmd>' …'`; the eval payload is
///   what was run, so that is shown instead of the wrapper.
/// - Redacted to `***`: `key=value` and `--key value` when the key names a
///   token, secret, password, passwd, key, auth or credential; `Bearer <x>`;
///   the `user:pass@` of a URL; values with a known token prefix (`sk-`,
///   `ghp_`, `gho_`, `github_pat_`, `xox?-`, `AKIA`); any run of 32 or more
///   base64/hex characters.
/// - Control characters and backticks are replaced (it is shown inside a
///   Markdown code span), the home directory reads as `~`, and the result is
///   cut at [`ORPHAN_CMD_MAX`] characters.
pub fn redact_cmdline(argv: &[String], home: Option<&str>) -> String {
    use std::sync::OnceLock;
    // (pattern, match on the ASCII-lowercased text?). Each pattern names the
    // secret part `v` (or `v2`/`v3` for the quoted alternatives); only that
    // is replaced. ASCII lowercasing keeps byte
    // offsets, so a match on the lowercased copy maps onto the original.
    static RULES: OnceLock<Vec<(regex::Regex, bool)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        // Key names that mark their value secret; `pwd` only as a whole word.
        const SENSITIVE: &str = "(?:[a-z0-9_.-]*(?:token|secret|password|passwd|pass|key|auth|credential)[a-z0-9_.-]*|pwd)";
        // A value: single-quoted, double-quoted, or bare.
        const VALUE: &str = r#"(?:'(?P<v>[^']*)'|"(?P<v2>[^"]*)"|(?P<v3>[^ \t'"]+))"#;
        [
            // An HTTP auth scheme's credentials.
            (r#"(?:bearer|basic)[ \t]+(?P<v>[^ \t'"]+)"#.to_string(), true),
            // URL userinfo, the user possibly empty (`redis://:pw@h`).
            (r#"://(?P<v>[^/ \t@]*:[^/ \t@]+)@"#.to_string(), false),
            // KEY=value, --key=value.
            (format!(r#"(?:^|[^a-z0-9_.-])-{{0,2}}{SENSITIVE}={VALUE}"#), true),
            // --key value.
            (
                format!(
                    r#"(?:^|[ \t])--?{SENSITIVE}[ \t]+(?:'(?P<v>[^']*)'|"(?P<v2>[^"]*)"|(?P<v3>[^ \t'"-][^ \t'"]*))"#
                ),
                true,
            ),
            // A header or a JSON member: `X-Api-Key: v`, `{"token":"v"}`.
            (
                r#"(?:^|[^a-z0-9_-])[a-z0-9_-]*(?:token|secret|key|auth|password|pass)[a-z0-9_-]*"?[ \t]*:[ \t]*"?(?:(?:bearer|basic)[ \t]+)?(?P<v>[^ \t'",}]+)"#
                    .to_string(),
                true,
            ),
            // `curl -u user:pass`.
            (r#"(?:^|[ \t])-u[ \t]+[^ \t:'"/]+:(?P<v>[^ \t'"/][^ \t'"]*)"#.to_string(), false),
            // mysql / mariadb tools: `-pSECRET`.
            (
                r#"(?:mysql|mariadb|mysqldump|mysqladmin)(?:[ \t][^\n]*?)?[ \t]-p(?P<v>[^ \t'"]+)"#
                    .to_string(),
                true,
            ),
            // A cookie header carries a session: the whole value.
            (
                r#"(?:^|[^a-z0-9_-])(?:set-)?cookie[ \t]*:[ \t]*(?P<v>[^'"\n]+)"#.to_string(),
                true,
            ),
            // `docker login … -p SECRET`.
            (
                r#"docker[ \t]+login(?:[ \t][^|;&\n]*?)?[ \t]-p[ \t]+(?P<v>[^ \t'"]+)"#.to_string(),
                true,
            ),
            // `echo SECRET | docker login --password-stdin` and the like.
            (
                r#"(?:echo|printf)[ \t]+(?:-n[ \t]+)?(?:'(?P<v>[^']*)'|"(?P<v2>[^"]*)"|(?P<v3>[^ \t'"|]+))[ \t]*\|[^|\n]*--password-stdin"#
                    .to_string(),
                true,
            ),
            // `sshpass -p SECRET`.
            (r#"sshpass[ \t]+-p[ \t]+(?P<v>[^ \t'"]+)"#.to_string(), true),
            // Values with a known token prefix.
            (
                r"(?:^|[^A-Za-z0-9_])(?P<v>(?:sk-|ghp_|gho_|github_pat_|xox[abposr]-|AKIA)[A-Za-z0-9_-]+)"
                    .to_string(),
                false,
            ),
            // Anything long enough to be a key.
            (r"(?P<v>[A-Za-z0-9+/=_-]{32,})".to_string(), false),
        ]
        .into_iter()
        .map(|(pattern, lower)| (regex::Regex::new(&pattern).expect("redaction rule"), lower))
        .collect()
    });
    let joined = argv.join(" ");
    let mut cmd = claude_bash_payload(argv).unwrap_or(joined);
    if let Some(home) = home.filter(|h| h.len() > 1) {
        cmd = cmd.replace(home, "~");
    }
    for (rule, lower) in rules {
        let haystack = if *lower {
            cmd.to_ascii_lowercase()
        } else {
            cmd.clone()
        };
        let mut out = String::with_capacity(cmd.len());
        let mut last = 0;
        for caps in rule.captures_iter(&haystack) {
            let Some(v) = caps
                .name("v")
                .or_else(|| caps.name("v2"))
                .or_else(|| caps.name("v3"))
            else {
                continue;
            };
            out.push_str(&cmd[last..v.start()]);
            out.push_str("***");
            last = v.end();
        }
        out.push_str(&cmd[last..]);
        cmd = out;
    }
    let cmd: String = cmd
        .chars()
        .map(|c| match c {
            '`' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cmd = cmd.trim().to_string();
    if cmd.chars().count() > ORPHAN_CMD_MAX {
        let cut: String = cmd.chars().take(ORPHAN_CMD_MAX).collect();
        format!("{cut}…")
    } else {
        cmd
    }
}

/// The command claude's Bash tool actually ran, out of its
/// `bash -c '… shell-snapshots … eval '<cmd>' …'` wrapper.
fn claude_bash_payload(argv: &[String]) -> Option<String> {
    let [shell, flag, script, ..] = argv else {
        return None;
    };
    if !shell.ends_with("bash") || flag != "-c" || !script.contains("shell-snapshots") {
        return None;
    }
    let start = script.find("eval '")? + "eval '".len();
    let rest = &script[start..];
    // The payload is single-quoted, with each inner quote spelled '"'"'.
    let unescaped = rest.replace("'\"'\"'", "\u{0}");
    let end = unescaped.find('\'')?;
    Some(unescaped[..end].replace('\u{0}', "'"))
}

/// End a detached body's PROCESS and nothing else: one SIGKILL to the
/// verified pid. `claude` answers SIGTERM by killing every background task it
/// started (measured, 2.1.296), and those are the user's work — a dev server,
/// a stack, a job scheduled for later — so no SIGTERM, no group signal. The
/// descendants are listed (fingerprinted) BEFORE the kill, and the ones still
/// running after it come back as [`Orphan`]s: never signalled, only reported.
///
/// A body killed mid-write can leave its transcript's last line torn — the
/// case for `/interrupt` and for a body ended as stalled; claude resumes past
/// a torn line (measured). `Ok(None)` when the body survived the SIGKILL.
pub fn kill_body(body: &SessionBody, sid: &str) -> Result<Option<Vec<Orphan>>> {
    if !body_is_alive(body, sid) {
        return Ok(Some(Vec::new()));
    }
    let children = descendants(body.pid);
    let home = dirs::home_dir().map(|h| h.display().to_string());
    let cmds: Vec<String> = children
        .iter()
        .map(|(pid, _)| redact_cmdline(&cmdline_argv(*pid), home.as_deref()))
        .collect();
    // The /proc walk took time: re-verify the incarnation right before the
    // signal, so a pid recycled meanwhile is never touched.
    if !body_is_alive(body, sid) {
        return Ok(Some(Vec::new()));
    }
    signal(body.pid, libc::SIGKILL)?;
    let started = Instant::now();
    while body_is_alive(body, sid) {
        if started.elapsed() > Duration::from_secs(2) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(Some(
        children
            .into_iter()
            .zip(cmds)
            .map(|((pid, fingerprint), cmd)| Orphan {
                pid,
                cmd,
                fingerprint,
            })
            .filter(Orphan::still_running)
            .collect(),
    ))
}

/// kill(2) to the process group `pgid` (ESRCH — the group is gone — is
/// success).
#[cfg(unix)]
fn signal_group(pgid: u32, sig: libc::c_int) -> Result<()> {
    let raw = libc::pid_t::try_from(pgid).context("pgid out of range")?;
    if raw <= 1 {
        anyhow::bail!("refusing to signal process group {raw}");
    }
    // SAFETY: kill(2) on a group this module verified it leads; the return
    // value reports failure, no memory is touched.
    let rc = unsafe { libc::kill(-raw, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).with_context(|| format!("signal {sig} to body process group {pgid}"))
}

#[cfg(not(unix))]
fn signal_group(_pgid: u32, _sig: i32) -> Result<()> {
    anyhow::bail!("session bodies are only managed on Unix")
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

    #[test]
    fn terminate_ends_a_group_leader_body_with_its_background_children() {
        use std::os::unix::process::CommandExt;
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("bg.pid");
        // A body that leads its own group and started a background process
        // that ignores SIGTERM (an orphan-to-be dev server).
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "sh -c 'trap \"\" TERM; sleep 60' & echo $! > {}; wait",
                pidfile.display()
            ))
            .env(BODY_SID_ENV, "s13")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        record(tmp.path(), "s13", Some(child.id()), "test").unwrap();
        let started = Instant::now();
        let bg = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no bg pid");
            std::thread::sleep(Duration::from_millis(20));
        };
        let body = read(tmp.path(), "s13").unwrap();
        // The test process is the body's parent: reap it while waiting.
        let reaper = std::thread::spawn(move || child.wait());
        assert!(terminate(&body, "s13", Duration::from_millis(500)).unwrap());
        reaper.join().unwrap().unwrap();
        let started = Instant::now();
        while process_exists(bg) && started.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !process_exists(bg),
            "the background child died with its group"
        );
    }

    #[test]
    fn terminate_never_signals_a_group_the_body_does_not_lead() {
        // A body inside ANOTHER group (the test runner's, standing in for an
        // older daemon's): only the body's own pid may be signalled.
        let tmp = tempfile::tempdir().unwrap();
        let mut child = spawn_sleep("s14");
        record(tmp.path(), "s14", Some(child.id()), "test").unwrap();
        assert!(!leads_own_group(child.id()));
        let body = read(tmp.path(), "s14").unwrap();
        let reaper = std::thread::spawn(move || child.wait());
        assert!(terminate(&body, "s14", Duration::from_secs(3)).unwrap());
        reaper.join().unwrap().unwrap();
        // Still here: the runner's group was not signalled.
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kill_body_ends_only_the_body_and_reports_its_surviving_children() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("bg.pid");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 61 & echo $! > {}; wait", pidfile.display()))
            .env(BODY_SID_ENV, "s15")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        record(tmp.path(), "s15", Some(child.id()), "test").unwrap();
        let started = Instant::now();
        let bg = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no bg pid");
            std::thread::sleep(Duration::from_millis(20));
        };
        let body = read(tmp.path(), "s15").unwrap();
        let reaper = std::thread::spawn(move || child.wait());
        let orphans = kill_body(&body, "s15").unwrap().expect("body gone");
        reaper.join().unwrap().unwrap();
        assert!(
            process_exists(bg),
            "the background child is never signalled"
        );
        assert_eq!(orphans.len(), 1, "{orphans:?}");
        assert_eq!(orphans[0].pid, bg);
        assert_eq!(orphans[0].cmd, "sleep 61");
        assert!(orphans[0].still_running());
        signal(bg, libc::SIGKILL).unwrap();
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn redact_cmdline_hides_secrets_and_the_home_path() {
        let home = Some("/home/alice");
        let cases: &[(&[&str], &str)] = &[
            (&["npm", "run", "dev"], "npm run dev"),
            (
                &["deploy", "--api-key", "abc123", "--port", "80"],
                "deploy --api-key *** --port 80",
            ),
            (
                &["env", "GITHUB_TOKEN=abc", "PASSWORD=hunter2", "x"],
                "env GITHUB_TOKEN=*** PASSWORD=*** x",
            ),
            (
                &["curl", "-H", "Authorization: Bearer eyJabc.def"],
                "curl -H Authorization: Bearer ***",
            ),
            (
                &["git", "clone", "https://bob:pw@github.com/x"],
                "git clone https://***@github.com/x",
            ),
            (
                &[
                    "run",
                    "sk-proj-abcdef",
                    "ghp_xyz",
                    "xoxb-1-2",
                    "AKIAEXAMPLE",
                ],
                "run *** *** *** ***",
            ),
            (
                &[
                    "sh",
                    "/home/alice/bin/x",
                    "0123456789abcdef0123456789abcdef01",
                ],
                "sh ~/bin/x ***",
            ),
            (&["echo", "a`b", "c\u{7}d"], "echo a'b c d"),
            (
                &["psql", "--password='hunter2'", "db"],
                "psql --password='***' db",
            ),
            (
                &["TOKEN=\"abc123\"", "npm", "start"],
                "TOKEN=\"***\" npm start",
            ),
            (&["tool", "--token", "\"abc\""], "tool --token \"***\""),
            (
                &["curl", "-H", "'X-Api-Key: abc123'"],
                "curl -H 'X-Api-Key: ***'",
            ),
            (
                &["curl", "-H", "'Authorization: Basic dXNlcjpwYXNz'"],
                "curl -H 'Authorization: Basic ***'",
            ),
            (
                &["echo", "{\"token\":\"abc123\"}"],
                "echo {\"token\":\"***\"}",
            ),
            (&["curl", "-u", "admin:s3cret", "h"], "curl -u admin:*** h"),
            (&["mysql", "-u", "root", "-phunter2"], "mysql -u root -p***"),
            (
                &["sshpass", "-p", "hunter2", "ssh", "h"],
                "sshpass -p *** ssh h",
            ),
            (&["env", "DB_PASS=hunter2", "x"], "env DB_PASS=*** x"),
            (&["tool", "--pass", "x"], "tool --pass ***"),
            (
                &["redis-cli", "-u", "redis://:pw@h"],
                "redis-cli -u redis://***@h",
            ),
            (
                &["app", "--port", "3000", "PWD=/x"],
                "app --port 3000 PWD=***",
            ),
            (
                &["curl", "-H", "'Cookie: session=abc; theme=dark'"],
                "curl -H 'Cookie: ***'",
            ),
            (
                &["curl", "-H", "'Set-Cookie: id=xyz'"],
                "curl -H 'Set-Cookie: ***'",
            ),
            (
                &["docker", "login", "-u", "bob", "-p", "s3cret", "ghcr.io"],
                "docker login -u bob -p *** ghcr.io",
            ),
            (
                &[
                    "echo",
                    "s3cret",
                    "|",
                    "docker",
                    "login",
                    "--password-stdin",
                    "-u",
                    "bob",
                ],
                "echo *** | docker login --password-stdin -u bob",
            ),
            (
                &[
                    "printf",
                    "'s3cret'",
                    "|",
                    "docker",
                    "login",
                    "--password-stdin",
                ],
                "printf '***' | docker login --password-stdin",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact_cmdline(&argv(input), home), *want, "{input:?}");
        }
    }

    #[test]
    fn redact_cmdline_shows_the_command_claude_ran_not_its_wrapper() {
        let wrapper = argv(&[
            "/bin/bash",
            "-c",
            "source /home/alice/.claude/shell-snapshots/snapshot-bash-1.sh 2>/dev/null || true \
             && eval 'PORT=3000 TOKEN=s3cr3t npm run dev -- --name '\"'\"'my app'\"'\"'' < /dev/null \
             && pwd -P >| /tmp/claude-1-cwd",
        ]);
        let shown = redact_cmdline(&wrapper, Some("/home/alice"));
        assert_eq!(shown, "PORT=3000 TOKEN=*** npm run dev -- --name 'my app'");
        assert!(!shown.contains("/home/alice"));
        let long = argv(&["x".repeat(200).as_str()]);
        assert!(redact_cmdline(&long, None).chars().count() <= ORPHAN_CMD_MAX + 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminate_after_the_body_already_died_still_ends_its_group() {
        use std::os::unix::process::CommandExt;
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("bg.pid");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 63 & echo $! > {}; wait", pidfile.display()))
            .env(BODY_SID_ENV, "s16")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        record(tmp.path(), "s16", Some(child.id()), "test").unwrap();
        let started = Instant::now();
        let bg = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no bg pid");
            std::thread::sleep(Duration::from_millis(20));
        };
        let body = read(tmp.path(), "s16").unwrap();
        // An /interrupt's SIGKILL got the body first.
        let reaper = std::thread::spawn(move || child.wait());
        assert!(kill_body(&body, "s16").unwrap().is_some());
        reaper.join().unwrap().unwrap();
        assert!(process_exists(bg));
        // The stop that took over still ends what the body started.
        assert!(terminate(&body, "s16", Duration::from_millis(200)).unwrap());
        let started = Instant::now();
        while process_exists(bg)
            && !process_is_zombie(bg)
            && started.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !process_exists(bg) || process_is_zombie(bg),
            "the group was swept"
        );
    }
}
