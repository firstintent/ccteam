//! V0.6.0 F107 + Wave 3 F112 — `CodexExecAdapter` (replaces V0.5.x
//! `CodexAdapter`).
//!
//! ## Lifecycle (Wave 3)
//!
//! - `start_thread`: `tmux new-session -d -s ccteam-<slug>-<sid> -c <cwd>
//!   codex <extra_args...>` — codex's interactive shell stays the V0.5.1
//!   tmux long-session container so the cost-status pane keeps working.
//!   Wave 3's per-turn `codex exec --json` subprocess is launched
//!   **independently** from this container; the tmux pane is just a
//!   convenient place for the `CODEX_STATUS:` observer line to surface.
//! - `submit_turn` (Wave 3): spawn `codex exec --json [prompt]` (or
//!   `codex resume <id> --json [prompt]` when `raw_extras.resumed`).
//!   Stdout JSONL is translated to [`ThreadEvent`]s and pushed into a
//!   per-thread broadcast so `events()` can drain.
//! - `events` (Wave 3): subscribe to the per-thread broadcast.
//! - `resume_thread` (Wave 3): synthesise a [`ThreadHandle`] whose
//!   `raw_extras.resumed == true` and `identity = persistent_id`; the
//!   *next* `submit_turn` invokes `codex resume <id>` instead of
//!   `codex exec`.
//! - `close_thread`: send `q` + Enter (codex's documented quit
//!   keybinding), 500 ms grace, then `tmux kill-session -t <name>`
//!   fallback (parity with V0.5.1).
//!
//! ## Test hooks
//!
//! - `CCTEAM_CODEX_BIN` env override redirects the per-turn subprocess
//!   from the real `codex` binary to a fake script that emits
//!   deterministic JSONL. Used by `tests/codex_exec_test.rs`.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, Mutex};

use crate::execution::codex_app_server::CODEX_BIN_ENV;
use crate::harness::{
    pluck_f64, pluck_pct, pluck_str, AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter,
    HarnessError, HarnessSnapshot, SpawnCtx, ThreadErrorEvent, ThreadEvent, ThreadHandle,
    ThreadItem, ThreadItemDetails, TurnId, TurnInput, UnifiedTokenUsage, CODEX_STATUS_MARKER,
};
use crate::paths::CcteamPaths;
use crate::tmux::{session_name_for_slug, TmuxSession};

/// Per-thread event broadcast buffer. Codex bursts items per turn so
/// 256 lines of headroom is comfortable for a single subscriber.
const EVENT_CHANNEL_BUFFER: usize = 256;

/// V0.6.0 F107 + Wave 3 F112 [`HarnessAdapter`] for OpenAI's `codex`
/// CLI. Combines a tmux long-session container (for the cost-status
/// pane) with per-turn `codex exec --json` subprocesses (for the
/// actual prompting + structured event stream).
#[derive(Clone, Default)]
pub struct CodexExecAdapter {
    /// Per-thread broadcast — populated lazily on the first
    /// `submit_turn` (or `events()` call) for a given thread identity.
    /// `Arc<Mutex<...>>` so `Clone` + `Send + Sync` constraints from
    /// `HarnessAdapter` hold without leaking dyn-state to the caller.
    threads: Arc<Mutex<HashMap<String, broadcast::Sender<ThreadEvent>>>>,
    /// Monotonic turn counter for synthesising `TurnId` when codex's
    /// JSONL stream omits one.
    turn_seq: Arc<AtomicU64>,
}

impl std::fmt::Debug for CodexExecAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexExecAdapter").finish_non_exhaustive()
    }
}

impl CodexExecAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the codex binary path. Honors `CCTEAM_CODEX_BIN` env
    /// override (hermetic tests) before falling back to PATH's `codex`.
    fn codex_bin() -> String {
        std::env::var(CODEX_BIN_ENV).unwrap_or_else(|_| "codex".to_string())
    }

    /// Get (or create) the broadcast sender for a thread identity.
    async fn channel_for(&self, identity: &str) -> broadcast::Sender<ThreadEvent> {
        let mut guard = self.threads.lock().await;
        if let Some(s) = guard.get(identity) {
            return s.clone();
        }
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_BUFFER);
        guard.insert(identity.to_string(), tx.clone());
        tx
    }

    /// Mint the next synthetic turn id.
    fn next_turn_id(&self) -> TurnId {
        let n = self.turn_seq.fetch_add(1, Ordering::SeqCst);
        TurnId(format!("codex-exec-{n}"))
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
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let prompt = render_prompt(&input)?;
        let resume_id = h
            .raw_extras
            .get("resumed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            .then(|| {
                h.raw_extras
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&h.identity)
                    .to_string()
            });
        let argv = build_exec_argv(resume_id.as_deref());
        let bin = Self::codex_bin();
        let tx = self.channel_for(&h.identity).await;
        let turn_id = self.next_turn_id();

        let mut child = tokio::process::Command::new(&bin)
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                HarnessError::SubmitFailed(format!("spawn {bin} {argv:?}: {err}"))
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            let prompt_clone = prompt.clone();
            tokio::spawn(async move {
                if let Err(err) = stdin.write_all(prompt_clone.as_bytes()).await {
                    tracing::warn!(error = %err, "codex exec: stdin write failed");
                }
                let _ = stdin.shutdown().await;
            });
        }

        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::SubmitFailed("codex exec: missing stdout pipe".to_string())
        })?;
        let stderr = child.stderr.take();

        // Spawn a reader task that translates JSONL → ThreadEvent and
        // pushes into the per-thread broadcast. Detached — the events()
        // stream is the synchronisation point for callers that care.
        let turn_id_for_task = turn_id.clone();
        let tx_for_task = tx.clone();
        tokio::spawn(async move {
            let buf = BufReader::new(stdout);
            let mut lines = buf.lines();
            let mut saw_completion = false;
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(trimmed) {
                            Ok(v) => {
                                for evt in translate_jsonl_event(&v, &turn_id_for_task) {
                                    if matches!(
                                        evt,
                                        ThreadEvent::TurnCompleted { .. }
                                            | ThreadEvent::TurnFailed { .. }
                                    ) {
                                        saw_completion = true;
                                    }
                                    let _ = tx_for_task.send(evt);
                                }
                            }
                            Err(err) => {
                                tracing::debug!(
                                    line = %trimmed,
                                    error = %err,
                                    "codex exec: skipping non-JSON line"
                                );
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        tracing::warn!(error = %err, "codex exec: stdout read error");
                        break;
                    }
                }
            }
            let status = child.wait().await;
            if !saw_completion {
                match status {
                    Ok(s) if s.success() => {
                        let _ = tx_for_task.send(ThreadEvent::TurnCompleted {
                            turn_id: turn_id_for_task.0.clone(),
                            usage: UnifiedTokenUsage::default(),
                        });
                    }
                    Ok(s) => {
                        let _ = tx_for_task.send(ThreadEvent::TurnFailed {
                            turn_id: turn_id_for_task.0.clone(),
                            err: ThreadErrorEvent {
                                kind: "nonzero_exit".into(),
                                message: format!(
                                    "codex exec exited with {} (no turn.completed seen)",
                                    s.code().unwrap_or(-1)
                                ),
                            },
                        });
                    }
                    Err(err) => {
                        let _ = tx_for_task.send(ThreadEvent::TurnFailed {
                            turn_id: turn_id_for_task.0.clone(),
                            err: ThreadErrorEvent {
                                kind: "wait_failed".into(),
                                message: err.to_string(),
                            },
                        });
                    }
                }
            }
        });

        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let buf = BufReader::new(stderr);
                let mut lines = buf.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.trim().is_empty() {
                        tracing::warn!(stderr = %line, "codex exec stderr");
                    }
                }
            });
        }

        Ok(turn_id)
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let adapter = self.clone();
        let identity = h.identity.clone();
        let setup = async move { adapter.channel_for(&identity).await.subscribe() };
        let s = stream::once(setup).flat_map(|rx| {
            stream::unfold(rx, |mut rx| async move {
                loop {
                    match rx.recv().await {
                        Ok(evt) => return Some((evt, rx)),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n, "codex_exec events subscriber lagged");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            })
        });
        Box::pin(s)
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        if persistent_id.is_empty() {
            return Err(HarnessError::SpawnFailed(
                "codex resume: persistent_id is empty".into(),
            ));
        }
        Ok(ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Bg,
            identity: persistent_id.to_string(),
            started_at: Utc::now(),
            raw_extras: serde_json::json!({
                "thread_id": persistent_id,
                "resumed": true,
            }),
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

/// Build `codex exec --json` (or `codex resume <id> --json`) argv.
/// The prompt itself is piped via stdin so we don't have to escape
/// shell metacharacters on the command line.
pub fn build_exec_argv(resume_id: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if let Some(id) = resume_id {
        argv.push("resume".to_string());
        argv.push(id.to_string());
    } else {
        argv.push("exec".to_string());
    }
    argv.push("--json".to_string());
    // Pipe prompt via stdin (`codex exec -`).
    argv.push("-".to_string());
    argv
}

/// Convert a [`TurnInput`] into a single prompt string suitable for
/// piping into `codex exec` over stdin. Mirrors the codex-app-server
/// adapter's `turn_input_to_items` but flattens to one text blob since
/// `codex exec` only accepts text on stdin.
pub fn render_prompt(input: &TurnInput) -> Result<String, HarnessError> {
    Ok(match input {
        TurnInput::UserText(t) => t.clone(),
        TurnInput::Artifact(p) => {
            let body = std::fs::read_to_string(p)
                .map_err(|e| HarnessError::SubmitFailed(format!("read artifact: {e}")))?;
            format!("<artifact path=\"{}\">\n{body}\n</artifact>", p.display())
        }
        TurnInput::SystemDirective(d) => {
            return Err(HarnessError::SubmitFailed(format!(
                "codex exec: SystemDirective '{d}' not supported (codex has no slash-command \
                 surface; wrap as user text instead)"
            )))
        }
        TurnInput::Image(p) => format!("[image: {}]", p.display()),
        TurnInput::ToolResult { call_id, content } => serde_json::to_string(&serde_json::json!({
            "call_id": call_id,
            "content": content,
        }))
        .unwrap_or_else(|_| "{}".to_string()),
    })
}

/// Translate one parsed `codex exec --json` JSONL value into zero or
/// more [`ThreadEvent`]s. The codex stream uses dot-separated `type`
/// discriminators (`thread.started`, `item.started`, etc.); see
/// `references/codex/codex-rs/exec/src/exec_events.rs`.
pub fn translate_jsonl_event(v: &Value, turn_id: &TurnId) -> Vec<ThreadEvent> {
    let Some(kind) = v.get("type").and_then(|t| t.as_str()) else {
        return vec![];
    };
    match kind {
        "thread.started" => {
            let tid = v
                .get("thread_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            vec![ThreadEvent::ThreadStarted { thread_id: tid }]
        }
        "turn.started" => vec![ThreadEvent::TurnStarted {
            turn_id: turn_id.0.clone(),
        }],
        "turn.completed" => {
            let usage = v
                .get("usage")
                .and_then(|u| serde_json::from_value(u.clone()).ok())
                .unwrap_or_default();
            vec![ThreadEvent::TurnCompleted {
                turn_id: turn_id.0.clone(),
                usage,
            }]
        }
        "turn.failed" => vec![ThreadEvent::TurnFailed {
            turn_id: turn_id.0.clone(),
            err: ThreadErrorEvent {
                kind: "turn_failed".into(),
                message: v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("(no message)")
                    .to_string(),
            },
        }],
        "item.started" | "item.updated" | "item.completed" => {
            let item = parse_jsonl_item(v.get("item").unwrap_or(v));
            let evt = match kind {
                "item.started" => ThreadEvent::ItemStarted { item },
                "item.updated" => ThreadEvent::ItemUpdated { item },
                _ => ThreadEvent::ItemCompleted { item },
            };
            vec![evt]
        }
        "error" => vec![ThreadEvent::Error(ThreadErrorEvent {
            kind: "codex_error".into(),
            message: v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("(no message)")
                .to_string(),
        })],
        // V0.6.3 F142 — forward-compat: a `codex exec --json` event
        // `type` we don't translate is **skipped** (empty event vec) so
        // the stream keeps flowing for the events we *do* understand.
        // Warn once per unknown kind so a Codex CLI event-vocabulary
        // drift is visible without flooding the log per JSONL line.
        other => {
            crate::vendor_compat::warn_unknown_vendor_token(
                "codex_exec_event",
                other,
                "skipping this event; rest of the stream is unaffected",
            );
            vec![]
        }
    }
}

fn parse_jsonl_item(item: &Value) -> ThreadItem {
    let id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let details = match kind {
        "agent_message" => ThreadItemDetails::AgentMessage(
            item.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "reasoning" => ThreadItemDetails::Reasoning(
            item.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "command_execution" => ThreadItemDetails::CommandExecution {
            cmd: item
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("in_progress")
                .to_string(),
        },
        "file_change" => {
            let path = item
                .get("changes")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("path"))
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            let kind = item
                .get("changes")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("kind"))
                .and_then(|v| v.as_str())
                .unwrap_or("update")
                .to_string();
            ThreadItemDetails::FileChange { path, kind }
        }
        "mcp_tool_call" => ThreadItemDetails::ToolCall {
            name: item
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            args: item.get("arguments").cloned().unwrap_or(Value::Null),
        },
        "web_search" => ThreadItemDetails::WebSearch {
            query: item
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "error" => ThreadItemDetails::Error(
            item.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        // V0.6.3 F142 — forward-compat: an unrecognised item `type`
        // degrades to an empty agent message (no panic, no stream
        // break). Warn once so a Codex item-vocabulary drift is visible.
        other => {
            crate::vendor_compat::warn_unknown_vendor_token(
                "codex_exec_item",
                other,
                "degraded to empty agent message",
            );
            ThreadItemDetails::AgentMessage(String::new())
        }
    };
    ThreadItem { id, details }
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

    // V0.6.3 F142 — forward-compat regression tests. OpenAI may ship a
    // `codex` CLI that emits a `--json` event with an unknown `type`
    // and/or extra fields; ccteam must skip it (no panic, no broken
    // stream) and warn once.

    #[test]
    fn translate_unknown_jsonl_event_type_is_skipped() {
        let v = serde_json::json!({
            "type": "turn.checkpoint",
            "checkpoint_id": "ckpt-42",
            "future_field": {"a": 1},
        });
        let evts = translate_jsonl_event(&v, &TurnId("t-1".into()));
        assert!(evts.is_empty(), "unknown event type must be skipped");
    }

    #[test]
    fn translate_known_event_with_extra_fields_does_not_panic() {
        // A known event carrying future extra fields must still parse.
        let v = serde_json::json!({
            "type": "thread.started",
            "thread_id": "th-1",
            "future_field": [1, 2, 3],
            "schema_version": 7,
        });
        let evts = translate_jsonl_event(&v, &TurnId("t-1".into()));
        assert!(matches!(
            evts.as_slice(),
            [ThreadEvent::ThreadStarted { .. }]
        ));
    }

    #[test]
    fn parse_jsonl_item_unknown_type_degrades_to_empty_message() {
        let item = serde_json::json!({
            "id": "i-9",
            "type": "holographic_artifact",
            "payload": {"unknown": true},
        });
        let parsed = parse_jsonl_item(&item);
        assert_eq!(parsed.id, "i-9");
        match parsed.details {
            ThreadItemDetails::AgentMessage(s) => assert_eq!(s, ""),
            other => panic!("expected empty agent message, got {other:?}"),
        }
    }
}
