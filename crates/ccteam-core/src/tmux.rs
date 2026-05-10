//! Thin wrapper around the `tmux` CLI for project-level long sessions.
//!
//! Lifecycle (`docs/tech-design.md` §6.1):
//! - **First launch**: `tmux new-session -d -s ccteam-<slug> -c <wd> <cmd>`
//!   where `<cmd>` is `claude --dangerously-skip-permissions` for prod
//!   and a sleeping shell for tests.
//! - **Reattach** (orchestrator restart): `is_alive(expected_pid)`
//!   does the documented double-check — `tmux has-session -t <name>`
//!   plus `kill -0 <pid>` (the latter weeds out cases where tmux still
//!   reports the session but the inner claude process has died).
//!
//! Output streams: every tmux call goes through `Command::output()`
//! so stdout/stderr are captured and surfaced in the `Result` error
//! message instead of leaking onto the orchestrator's terminal.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;

pub const SESSION_PREFIX: &str = "ccteam-";

/// Build the conventional tmux session name for a project slug.
pub fn session_name_for_slug(slug: &str) -> String {
    format!("{SESSION_PREFIX}{slug}")
}

/// Resolve the live tmux session name for a project slug.
///
/// Most projects use the conventional `ccteam-<slug>` name, but
/// meta-agent sessions intentionally do not: slug `cto-meta` maps to
/// tmux session `ccteam-meta-cto`. `state.json.tmux_session` is the
/// source of truth for those cases. If the state is missing or malformed
/// we fall back to the conventional name so diagnostic surfaces continue
/// to degrade the same way older builds did.
pub fn session_name_for_project(paths: &CcteamPaths, slug: &str) -> String {
    let fallback = session_name_for_slug(slug);
    let state_path = paths.project_state(slug);
    match ProjectState::load(&state_path) {
        Ok(state) if !state.tmux_session.trim().is_empty() => state.tmux_session,
        _ => fallback,
    }
}

/// Handle to a (possibly non-existent) tmux session. Methods are
/// thin wrappers over the `tmux` CLI; nothing is cached, so callers
/// always observe the live state of the system.
#[derive(Debug, Clone)]
pub struct TmuxSession {
    name: String,
}

impl TmuxSession {
    pub fn for_slug(slug: &str) -> Self {
        Self {
            name: session_name_for_slug(slug),
        }
    }

    /// Construct from a pre-built session name (no validation).
    pub fn from_name(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// `tmux has-session -t <name>`.
    pub fn exists(&self) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", &self.name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// `tmux new-session -d -s <name> -c <working_dir> -x 200 -y 50 <argv...>`.
    /// Errors if the session already exists or `tmux` rejects the
    /// invocation (caller can `kill` first if it wants to recreate).
    ///
    /// `-x 200 -y 50` sets a default pane size: tmux otherwise defaults
    /// to 80×24 only when invoked from a controlling TTY; under daemon
    /// (or test) invocation the pane collapses to 1×1, which silently
    /// truncates everything send-keys writes. The wider default makes
    /// long phase prompts visible without truncation; user `tmux
    /// attach` resizes to the client window automatically.
    pub fn start(&self, working_dir: &Path, argv: &[&str]) -> Result<()> {
        if self.exists() {
            bail!("tmux session already exists: {}", self.name);
        }
        if argv.is_empty() {
            bail!("tmux start: argv must be non-empty (no command to run)");
        }
        let working_dir_str = working_dir
            .to_str()
            .ok_or_else(|| anyhow!("working_dir is not valid UTF-8: {}", working_dir.display()))?;

        let mut cmd = Command::new("tmux");
        cmd.args([
            "new-session",
            "-d",
            "-s",
            &self.name,
            "-c",
            working_dir_str,
            "-x",
            "200",
            "-y",
            "50",
        ]);
        cmd.args(argv);

        let output = cmd
            .output()
            .with_context(|| format!("spawn tmux new-session for {}", self.name))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux new-session failed for {}: {}", self.name, stderr);
        }

        // Force the window size again post-creation. With `-d` (detached
        // start) and no controlling client, tmux otherwise inherits a
        // server-default that can be as small as 1×1, silently
        // truncating everything send-keys writes. resize-window is a
        // no-op when a real client later attaches.
        //
        // Target the session by name (no `:N` suffix) so we hit whatever
        // the active window is — users with `base-index 1` in tmux.conf
        // have no window 0, and a hard-coded `:0` errors out.
        let _ = Command::new("tmux")
            .args([
                "resize-window",
                "-t",
                &self.name,
                "-x",
                "200",
                "-y",
                "50",
            ])
            .output();

        Ok(())
    }

    /// `tmux kill-session -t <name>`. Tolerates a missing session
    /// (returns Ok) so callers can use `kill` as idempotent cleanup.
    pub fn kill(&self) -> Result<()> {
        if !self.exists() {
            return Ok(());
        }
        let output = Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .output()
            .with_context(|| format!("spawn tmux kill-session for {}", self.name))?;
        if !output.status.success() {
            // Race: someone else may have killed it between exists() and now.
            if !self.exists() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux kill-session failed for {}: {}", self.name, stderr);
        }
        Ok(())
    }

    /// PID of the active pane's leader process (the `claude` process,
    /// in production). Returns `None` if the session doesn't exist or
    /// the pane has no associated PID yet.
    ///
    /// Targeted by session name only — tmux resolves to the active
    /// window/pane, which avoids assumptions about `base-index`.
    pub fn pane_pid(&self) -> Result<Option<i32>> {
        if !self.exists() {
            return Ok(None);
        }
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", &self.name, "-F", "#{pane_pid}"])
            .output()
            .with_context(|| format!("spawn tmux display-message for {}", self.name))?;
        if !output.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8_lossy(&output.stdout);
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let pid: i32 = trimmed
            .parse()
            .with_context(|| format!("parse tmux pane_pid `{trimmed}`"))?;
        Ok(Some(pid))
    }

    /// Documented `is_alive` check: tmux still has the session AND
    /// the in-pane process matches the expected PID and is alive.
    /// `expected_pid: None` skips the kill-0 check (useful right after
    /// `start` before state.json has been refreshed).
    pub fn is_alive(&self, expected_pid: Option<i32>) -> bool {
        if !self.exists() {
            return false;
        }
        match expected_pid {
            None => true,
            Some(pid) => {
                if !pid_is_alive(pid) {
                    return false;
                }
                // Defense in depth: confirm tmux still associates the
                // session with that PID. If a fresh process recycled
                // the PID, `tmux has-session` succeeded but the live
                // pane PID will differ — that's genuinely a stale
                // session and we must not "reattach" to it.
                match self.pane_pid().ok().flatten() {
                    Some(actual) => actual == pid,
                    None => false,
                }
            }
        }
    }

    /// Send literal text to the active pane and press Enter. Two tmux
    /// invocations: a `-l` (literal) send so unprintable characters in
    /// `text` aren't reinterpreted, then an `Enter` keypress to submit.
    /// Multi-line prompts arrive M0.8 (idle-aware injection).
    ///
    /// Targeted by session name only — tmux resolves to the active
    /// window/pane, so this works regardless of the user's `base-index`
    /// setting (`base-index 1` in tmux.conf would otherwise break a
    /// hard-coded `:0` target).
    pub fn send_keys(&self, text: &str) -> Result<()> {
        if !self.exists() {
            bail!("send_keys: session does not exist: {}", self.name);
        }

        let output = Command::new("tmux")
            .args(["send-keys", "-t", &self.name, "-l", "--", text])
            .output()
            .with_context(|| format!("spawn tmux send-keys (-l) for {}", self.name))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux send-keys (-l) failed: {}", stderr);
        }

        let output = Command::new("tmux")
            .args(["send-keys", "-t", &self.name, "Enter"])
            .output()
            .with_context(|| format!("spawn tmux send-keys Enter for {}", self.name))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux send-keys Enter failed: {}", stderr);
        }
        Ok(())
    }
}

/// Capture the last `lines` lines of the project's tmux pane, addressed
/// by slug. Returns `None` when tmux isn't installed, the session is
/// missing, or the invocation otherwise fails — every call site is a
/// best-effort surface (no Result propagation).
///
/// The captured text only ever lands in user-facing surfaces
/// (`needs_attention.outbox.json` payload / meta-agent NL translation)
/// — **never** parsed by the orchestrator's state machine (CLAUDE.md
/// "永不解析 tmux 终端输出" red line).
///
/// `with_ansi=false` runs `tmux capture-pane -p` (plain text, the F35
/// silence-classifier path). `with_ansi=true` runs `tmux capture-pane
/// -e -p` so escape sequences round-trip — useful when the captured
/// text will be shown to the user (eg. NL translation that quotes a
/// short snippet). F38 screenshot rendering uses `capture_pane_with_ansi`
/// instead (which returns raw bytes for `vt100::Parser`).
pub fn capture_pane_tail(slug: &str, lines: usize, with_ansi: bool) -> Option<String> {
    let session = session_name_for_slug(slug);
    let mut cmd = Command::new("tmux");
    cmd.arg("capture-pane").arg("-p");
    if with_ansi {
        cmd.arg("-e");
    }
    cmd.args(["-t", &session, "-S", &format!("-{lines}")]);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// V0.2.2 F38 helper — capture the active pane's contents **with ANSI
/// escape sequences preserved** (`tmux capture-pane -e -p -S -<lines>`).
/// Returns the raw bytes so callers can feed them to a terminal state
/// machine (`vt100::Parser`) for rendering.
///
/// `lines` is the scrollback depth (`-S -<n>`). Use a small value
/// (~50) for screenshots so big projects don't dump the full history.
///
/// Returns `Ok(None)` (not Err) when the session doesn't exist or
/// tmux fails — callers degrade gracefully (no PNG produced) rather
/// than aborting the enclosing main path.
///
/// **Red line**: this output is for rendering only. The architecture
/// red line "永不解析 tmux 终端输出" (CLAUDE.md §3) means this byte
/// stream MUST NOT feed any state-machine / phase-classification
/// path; `progress.jsonl` remains the single state source of truth.
///
/// Sibling helper [`capture_pane_tail`] returns the same bytes as a
/// `String` for NL surface use; this one keeps raw bytes for `vt100`.
pub fn capture_pane_with_ansi(slug: &str, lines: usize) -> Result<Option<Vec<u8>>> {
    let name = session_name_for_slug(slug);
    capture_pane_with_ansi_from_session(&name, lines)
}

/// Capture a tmux pane by exact session name. Use this when the
/// caller has already resolved `state.json.tmux_session` instead of a
/// conventional project slug.
pub fn capture_pane_with_ansi_from_session(
    session_name: &str,
    lines: usize,
) -> Result<Option<Vec<u8>>> {
    let lines_arg = format!("-{lines}");
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-e",
            "-p",
            "-t",
            session_name,
            "-S",
            &lines_arg,
        ])
        .output()
        .with_context(|| format!("spawn tmux capture-pane for {session_name}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

/// V0.2.2 F38 helper — query the active pane's height/width via
/// `tmux display-message -p '#{pane_height} #{pane_width}'`. Returns
/// `Ok(None)` on session-missing / tmux failure.
pub fn query_pane_dims(slug: &str) -> Result<Option<(u16, u16)>> {
    let name = session_name_for_slug(slug);
    query_pane_dims_from_session(&name)
}

/// Query pane dimensions by exact tmux session name.
pub fn query_pane_dims_from_session(session_name: &str) -> Result<Option<(u16, u16)>> {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            session_name,
            "-F",
            "#{pane_height} #{pane_width}",
        ])
        .output()
        .with_context(|| format!("spawn tmux display-message for {session_name}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&output.stdout);
    let trimmed = s.trim();
    let mut it = trimmed.split_ascii_whitespace();
    let rows: u16 = match it.next().and_then(|n| n.parse().ok()) {
        Some(r) => r,
        None => return Ok(None),
    };
    let cols: u16 = match it.next().and_then(|n| n.parse().ok()) {
        Some(c) => c,
        None => return Ok(None),
    };
    Ok(Some((rows, cols)))
}

/// `kill -0 <pid>`: returns true iff the process exists and the caller
/// has permission to signal it. Used in the documented double-check
/// (`tmux has-session` + `kill -0 pid`) for orchestrator reattach.
pub fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Probe whether tmux is installed and runnable. Used to skip
/// integration tests gracefully on machines without tmux.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
