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

pub const SESSION_PREFIX: &str = "ccteam-";

/// Build the conventional tmux session name for a project slug.
pub fn session_name_for_slug(slug: &str) -> String {
    format!("{SESSION_PREFIX}{slug}")
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
        let _ = Command::new("tmux")
            .args([
                "resize-window",
                "-t",
                &format!("{}:0", self.name),
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

    /// PID of the first pane's leader process (the `claude` process,
    /// in production). Returns `None` if the session doesn't exist or
    /// the pane has no associated PID yet.
    pub fn pane_pid(&self) -> Result<Option<i32>> {
        if !self.exists() {
            return Ok(None);
        }
        let target = format!("{}:0", self.name);
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", &target, "-F", "#{pane_pid}"])
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

    /// Send literal text to the first pane and press Enter. Two tmux
    /// invocations: a `-l` (literal) send so unprintable characters in
    /// `text` aren't reinterpreted, then an `Enter` keypress to submit.
    /// Multi-line prompts arrive M0.8 (idle-aware injection).
    pub fn send_keys(&self, text: &str) -> Result<()> {
        if !self.exists() {
            bail!("send_keys: session does not exist: {}", self.name);
        }
        let target = format!("{}:0", self.name);

        let output = Command::new("tmux")
            .args(["send-keys", "-t", &target, "-l", "--", text])
            .output()
            .with_context(|| format!("spawn tmux send-keys (-l) for {}", self.name))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux send-keys (-l) failed: {}", stderr);
        }

        let output = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .output()
            .with_context(|| format!("spawn tmux send-keys Enter for {}", self.name))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux send-keys Enter failed: {}", stderr);
        }
        Ok(())
    }
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
