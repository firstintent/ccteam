//! V0.6.0 F107 — `CodexExecAdapter` (replaces V0.5.x `CodexAdapter`).
//!
//! Wave 1 parity with V0.5.1 behaviour:
//!
//! - `start_thread`: `tmux new-session -d -s ccteam-<slug>-<sid> -c <cwd>
//!   codex <extra_args...>` — codex has no `--bg` surface today, so we
//!   keep the V0.4.0 F62 tmux long-session container. Initial state
//!   observer file `~/.ccteam/codex/<sid>/state.json` is written
//!   best-effort.
//! - `close_thread`: send `q` + Enter (codex's documented quit
//!   keybinding), 500 ms grace, then `tmux kill-session -t <name>`
//!   fallback.
//! - `events`: empty stream for Wave 1; Wave 3 F112 fills with `codex
//!   exec --json` stdout JSONL → [`crate::harness::ThreadEvent`].
//! - `submit_turn` / `resume_thread`: Wave 1 stub
//!   `HarnessError::NotImplemented`; Wave 3 F112 fills both via `codex
//!   exec --json` stdin pipe + `codex resume <UUID>`.
//!
//! Free helpers ([`parse_status_line`], [`snapshot_from_status`]) are
//! kept `pub` so non-trait callers (web layer, cost summary) can
//! consume codex status without going through the trait. V0.6.0 F107
//! drops `ingest_snapshot` from the trait surface — direct fn calls
//! are the new convention.

use std::process::Command;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream};

use crate::harness::{
    pluck_f64, pluck_pct, pluck_str, AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter,
    HarnessError, HarnessSnapshot, SpawnCtx, ThreadEvent, ThreadHandle, TurnId, TurnInput,
    CODEX_STATUS_MARKER,
};
use crate::paths::CcteamPaths;
use crate::tmux::{session_name_for_slug, TmuxSession};

/// V0.6.0 F107 [`HarnessAdapter`] for OpenAI's `codex` CLI (tmux
/// long-session container; Wave 3 F112 will add `codex exec --json` +
/// `codex app-server` UDS paths).
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexExecAdapter;

impl CodexExecAdapter {
    pub const fn new() -> Self {
        Self
    }

    /// Resolve `~/.ccteam/codex/<sid>/state.json` for a session.
    pub fn state_json_path(sid: &str) -> Option<std::path::PathBuf> {
        let paths = CcteamPaths::from_env().ok()?;
        Some(paths.root.join("codex").join(sid).join("state.json"))
    }

    fn write_initial_state(sid: &str, pid: Option<u32>) {
        let Some(target) = Self::state_json_path(sid) else {
            return;
        };
        let body = serde_json::json!({
            "status": "starting",
            "pid": pid,
            "model": "codex",
            "context_pct": 0,
            "cost_usd": 0.0,
        });
        let raw = match serde_json::to_string(&body) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, sid = %sid, "codex state.json serialise");
                return;
            }
        };
        if let Some(parent) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %err, path = %parent.display(), "codex state.json mkdir");
                return;
            }
        }
        if let Err(err) = std::fs::write(&target, raw.as_bytes()) {
            tracing::warn!(error = %err, path = %target.display(), "codex state.json write");
        }
    }
}

/// Parse a tmux `capture-pane -p` body for the `CODEX_STATUS:` marker
/// line. Returns the JSON payload of the **last** matching line (most
/// recent status wins). Free fn so callers (web layer, tests) can drive
/// it without going through the trait.
pub fn parse_status_line(pane: &str) -> Option<serde_json::Value> {
    pane.lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(CODEX_STATUS_MARKER))
        .and_then(|rest| serde_json::from_str(rest.trim()).ok())
}

/// Build a [`HarnessSnapshot`] from a parsed `CODEX_STATUS:` JSON
/// payload (or `None` for the permissive fallback shape).
pub fn snapshot_from_status(payload: Option<serde_json::Value>) -> HarnessSnapshot {
    let value = payload.unwrap_or(serde_json::Value::Null);
    let model_display_name = pluck_str(&value, &["model"])
        .or_else(|| pluck_str(&value, &["model_display_name"]))
        .unwrap_or_else(|| "codex".to_string());
    let context_used_pct = pluck_pct(&value, &["context_pct"])
        .or_else(|| pluck_pct(&value, &["context_used_pct"]))
        .unwrap_or(0);
    let cost_usd_total = pluck_f64(&value, &["cost_usd"])
        .or_else(|| pluck_f64(&value, &["cost_usd_total"]))
        .unwrap_or(0.0);
    let rate_limit_pct = pluck_pct(&value, &["rate_limit_pct"]);
    let cwd = pluck_str(&value, &["cwd"]).map(std::path::PathBuf::from);

    HarnessSnapshot {
        harness: "codex".to_string(),
        model_display_name,
        context_used_pct,
        cost_usd_total,
        rate_limit_pct,
        cwd,
        raw: value,
        captured_at: Utc::now(),
    }
}

/// Ingest a tmux pane capture for codex status data. Permissive:
/// missing / malformed marker returns the fallback snapshot rather
/// than failing — the snapshot pipeline is presentation-only.
pub fn ingest_codex_pane(raw: &str) -> Result<HarnessSnapshot, HarnessError> {
    Ok(snapshot_from_status(parse_status_line(raw)))
}

#[async_trait]
impl HarnessAdapter for CodexExecAdapter {
    fn name(&self) -> &'static str {
        "codex-exec"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Codex
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        let tmux_session = format!("{}-{}", session_name_for_slug(&ctx.slug), ctx.sid)
            .trim_start_matches('-')
            .to_string();

        let session_name = tmux_session.clone();
        let cwd = ctx.cwd.clone();
        let extra_args = ctx.extra_args.clone();
        let sid = ctx.sid.clone();

        let result = tokio::task::spawn_blocking(move || -> Result<(String, Option<u32>), HarnessError> {
            let session = TmuxSession::from_name(session_name.clone());
            if session.exists() {
                return Err(HarnessError::SpawnFailed(format!(
                    "tmux session already exists: {session_name} \
                     (sid collision; F49 next_sid_seq accounting drifted)"
                )));
            }
            let mut argv: Vec<String> = vec!["codex".to_string()];
            argv.extend(extra_args.iter().cloned());
            let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            session
                .start(&cwd, &argv_refs)
                .map_err(|err| HarnessError::SpawnFailed(format!("tmux new-session: {err:#}")))?;
            let pid = session
                .pane_pid()
                .ok()
                .flatten()
                .and_then(|n| u32::try_from(n).ok());
            CodexExecAdapter::write_initial_state(&sid, pid);
            Ok((session_name, pid))
        })
        .await
        .map_err(|err| HarnessError::SpawnFailed(format!("join blocking spawn: {err}")))??;

        let (session_name, pid) = result;
        let mut extras = serde_json::json!({ "tmux_session": session_name });
        if let Some(pid_val) = pid {
            extras["pid"] = serde_json::json!(pid_val);
        }

        Ok(ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Bg,
            identity: tmux_session,
            started_at: Utc::now(),
            raw_extras: extras,
        })
    }

    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "Wave 3 F112 fills CodexExecAdapter::submit_turn via `codex exec --json` \
                     stdin pipe + thread/start UDS"
                .to_string(),
        })
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // Wave 1: empty stream. Wave 3 F112 will populate from `codex
        // exec --json` stdout JSONL → ThreadEvent translation.
        Box::pin(stream::empty())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "Wave 3 F112 fills CodexExecAdapter::resume_thread via `codex resume \
                     <UUID>`"
                .to_string(),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let session_name = h.identity.clone();
        tokio::task::spawn_blocking(move || -> Result<(), HarnessError> {
            let session = TmuxSession::from_name(session_name.clone());
            if !session.exists() {
                return Ok(());
            }
            if let Err(err) = send_codex_quit_keys(&session_name) {
                tracing::warn!(
                    error = %err,
                    session = %session_name,
                    "CodexExecAdapter::close_thread: send-keys q failed; falling through to \
                     tmux kill-session",
                );
            }
            std::thread::sleep(Duration::from_millis(500));
            if session.exists() {
                session.kill().map_err(|err| {
                    HarnessError::ShutdownFailed(format!("tmux kill-session: {err:#}"))
                })?;
            }
            Ok(())
        })
        .await
        .map_err(|err| HarnessError::ShutdownFailed(format!("join blocking close: {err}")))?
    }
}

/// Send `q` + Enter to the named tmux session — codex's standard
/// quit keybinding.
fn send_codex_quit_keys(tmux_session: &str) -> std::io::Result<()> {
    let send_lit = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "-l", "--", "q"])
        .output()?;
    if !send_lit.status.success() {
        return Err(std::io::Error::other(format!(
            "tmux send-keys -l q: {}",
            String::from_utf8_lossy(&send_lit.stderr)
        )));
    }
    let send_enter = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "Enter"])
        .output()?;
    if !send_enter.status.success() {
        return Err(std::io::Error::other(format!(
            "tmux send-keys Enter: {}",
            String::from_utf8_lossy(&send_enter.stderr)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_line_picks_most_recent() {
        let pane = "CODEX_STATUS: {\"model\":\"o1\",\"context_pct\":10}\n\
                    intermediate\n\
                    CODEX_STATUS: {\"model\":\"o3\",\"context_pct\":80}\n";
        let v = parse_status_line(pane).expect("parses");
        assert_eq!(v["model"], "o3");
    }

    #[test]
    fn parse_status_line_empty_returns_none() {
        assert!(parse_status_line("").is_none());
    }

    #[test]
    fn snapshot_fallback_when_no_payload() {
        let snap = snapshot_from_status(None);
        assert_eq!(snap.harness, "codex");
        assert_eq!(snap.model_display_name, "codex");
    }
}
