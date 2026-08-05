//! v0.8.11 E1 — `ClaudeStreamJsonAdapter`: the Claude vendor's **second**
//! spawn path, a long-running `claude` child driven over a bidirectional
//! NDJSON (stream-json) pipe instead of a tmux PTY. It implements the same
//! [`HarnessAdapter`] trait and emits the same [`ThreadEvent`]
//! (CanonicalEvent) stream as [`super::claude_tui::ClaudeTuiAdapter`], so
//! the gateway's `spawn_event_pump` — the live daemon's only turns/progress
//! writer — consumes it **unchanged** (PRD §〇 decision 1; §七 ④ SoT writer
//! reuse).
//!
//! ## The four seams (PRD §七 ①)
//!
//! - [`spawn_spec`] — pure argv/env/cwd builder (host-portable).
//! - [`transport`] — bidirectional NDJSON over a generic `(reader, writer)`;
//!   the consumer never holds the [`tokio::process::Child`] (WS-replaceable).
//! - [`translate`] — NDJSON → [`ThreadEvent`].
//! - this module — the adapter + its live-session registry + SoT-writer
//!   reuse (the gateway pump).
//!
//! ## Red lines
//!
//! - **Zero injection**: persona only via `--agent`; [`spawn_spec`] never
//!   emits `--append-system-prompt` and this adapter never sends an
//!   `initialize.systemPrompt`.
//! - **Never kill a long session**: idle release / wake = close stdin +
//!   `--resume` (≡ resume-by-session-id); `close_thread` is the only kill
//!   path and is user-initiated. The deterministic per-(slug,sid) uuid is
//!   what makes `--resume` stateless across daemon restart.
//! - **No terminal scraping**: there is no terminal — naturally satisfied.

pub mod bridge;
pub mod protocol;
pub mod spawn_spec;
pub mod translate;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};

use crate::execution::claude_common;
use crate::execution::progress_bridge::{
    append_event, build_chat_session_reset_event_with_reason, progress_jsonl_from_env,
};
use crate::execution::session_status::{read_status_file, write_status_file};
use crate::execution::transcript_tail::anthropic_project_dir;
use crate::{
    AgentSpecBrief, AgentVendor, ChoiceOption, ChoicePrompt, Directive, DirectiveOutcome,
    ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus,
    TurnId, TurnInput, TurnRouting, TurnSubmission,
};

use bridge::{ApprovalDecision, CanUseToolResolver, SlashClass};
use protocol::{ClaudeModelOption, Outbound};
use spawn_spec::StreamJsonSpawnInput;
use translate::StreamTranslator;
use transport::StreamJsonTransport;

/// §七 ⑤ — host-facet-friendly session identity. `sid → vendor_uuid` is a
/// stable mapping (the uuid is derived deterministically from `(slug,
/// sid)`); `host` reserves the v0.9 host axis (`local` today; a `Sandbox
/// CR` ref later) without a one-shot re-key.
#[derive(Debug, Clone)]
pub struct SessionIdentity {
    pub sid: String,
    pub vendor_uuid: String,
    pub host: String,
}

/// One live stream-json session: the transport (owns the child privately)
/// plus the identity / routing context the adapter needs across calls.
struct LiveSession {
    identity: SessionIdentity,
    transport: Arc<StreamJsonTransport>,
    slug: String,
    role: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    /// Slash-command table from `system:init` (bridge gate, Wave 2).
    commands: Vec<String>,
    /// The REAL model list captured from the `initialize` control_response
    /// (`response.models[]`). A bare `/model` builds its NeedsChoice picker
    /// strictly from this (`claude_model_options`) — never a hardcoded list.
    /// Empty (older claude / capture failure) → the model arm falls back to
    /// the usage-text rejection.
    models: Vec<ClaudeModelOption>,
    /// Live session status (model + context-window usage) for
    /// [`HarnessAdapter::thread_status`] → IM `/sessions` + the web statusline
    /// bar. Seeded with the `initialize` model; the per-session **status tap**
    /// ([`spawn_status_tap`]) overwrites it from each `assistant`/`result`
    /// message's `usage` as turns run (interior-mutable, shared with the tap).
    status: Arc<StdMutex<ThreadStatus>>,
    /// v0.8.20 `/status` — the session's currently-running subagent/workflow
    /// tasks, reflected from claude's `system:task_*` lifecycle by the same
    /// [`spawn_status_tap`] (`task_started` adds, a terminal status removes; the
    /// per-turn `result` clears as a safety net). Read by
    /// [`HarnessAdapter::running_tasks`]. Interior-mutable, shared with the tap.
    running_tasks: Arc<StdMutex<Vec<crate::RunningTask>>>,
    /// Adapter-local vendor-turn occupancy for truthful Started vs Injected
    /// submission receipts. Set before writing input; cleared on TurnResult.
    active_turn: Arc<AtomicBool>,
}

/// The Claude stream-json adapter. A per-vendor singleton (mirrors
/// `CodexAppServerAdapter`) holding every live session keyed by its vendor
/// uuid. `ThreadHandle` (serializable, restart-surviving) carries only the
/// uuid + routing extras — never the live child — so a daemon restart
/// rebuilds via `--resume`.
#[derive(Clone, Default)]
pub struct ClaudeStreamJsonAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
    /// HITL resolver for `can_use_tool` reverse RPCs. `None` = no HITL
    /// wiring (a hitl session then default-denies, the safe direction).
    ///
    /// Interior-mutable (`Arc<StdMutex<..>>`, not a plain field) because this
    /// adapter is a per-(vendor,protocol) SINGLETON constructed inside
    /// `ccteam_im::daemon::default_adapter_factory` — before the gateway's
    /// pending-approval machinery (event sink + pending registry) exists.
    /// The daemon calls [`Self::set_resolver`] once that machinery is wired
    /// (`run_daemon_with_shutdown`); every clone of this adapter — including
    /// the one already captured inside the factory closure the gateway holds
    /// — observes the update because the cell is shared, not copied. Each new
    /// session spawn reads the CURRENT value at `start_thread` time, so
    /// wiring the resolver after the adapter is constructed (but before any
    /// session spawns) works correctly (v0.8.22 P0-2). Tests inject a
    /// deterministic stub via [`Self::with_resolver`].
    resolver: Arc<StdMutex<Option<Arc<dyn CanUseToolResolver>>>>,
}

impl std::fmt::Debug for ClaudeStreamJsonAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeStreamJsonAdapter")
            .finish_non_exhaustive()
    }
}

/// Adapter `name()` — the stable id used in handles, logs, and tests.
pub const STREAM_JSON_ADAPTER_NAME: &str = "claude-stream-json";

/// Spawn the per-session HITL dispatcher: watch the transport for
/// `can_use_tool` reverse RPCs, resolve each via the wired resolver, and
/// reply with a `control_response`. A missing resolver default-denies (the
/// safe direction). `deny` blocks ONLY the tool call — the turn continues.
fn spawn_hitl_dispatcher(
    transport: Arc<StreamJsonTransport>,
    sid: String,
    resolver: Option<Arc<dyn CanUseToolResolver>>,
) {
    let mut sub = transport.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(Outbound::ControlRequest(creq)) => {
                        let Some(req) = bridge::parse_can_use_tool(&creq) else { continue };
                        let decision = match &resolver {
                            Some(r) => r.resolve(&sid, &req).await,
                            None => ApprovalDecision::deny(
                                "HITL approval is unavailable (no resolver wired) — denied",
                            ),
                        };
                        let line = protocol::can_use_tool_response_line(
                            &req.request_id,
                            decision.allow,
                            &req.input,
                            &decision.message,
                        );
                        if transport.send_line(line).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
}

/// Carry the `[1m]` context-window tag from the CURRENT model onto the API's
/// `message.model` (which omits it). The user requests 1M via `set_model
/// opus[1m]`, but claude's API model id is the bare `claude-opus-4-8`; without
/// this the status tap would re-stamp the session bare → the window heuristic
/// (`context_window_for_model`) reads 200k and the `[1m]` display is lost. A
/// later `set_model` resets the current model (with or without `[1m]`), so this
/// only preserves the live intent — it never invents a tag the user didn't ask
/// for, and a switch to a non-`[1m]` model clears it.
fn preserve_1m_tag(current: Option<&str>, api_model: &str) -> String {
    let had_1m = current
        .map(|c| c.to_ascii_lowercase().ends_with("[1m]"))
        .unwrap_or(false);
    if had_1m && !api_model.to_ascii_lowercase().ends_with("[1m]") {
        format!("{api_model}[1m]")
    } else {
        api_model.to_string()
    }
}

/// Spawn the per-session status tap: keep the shared [`ThreadStatus`] current
/// so [`HarnessAdapter::thread_status`] reports the live model + context-window
/// usage without parsing a transcript. The `assistant` message updates the
/// model id; the per-turn `result` reads context STRICTLY from claude's own
/// `get_context_usage` (totalTokens / maxTokens) — never a heuristic estimate,
/// and clears the context (statusline shows none) when the vendor can't answer.
/// Runs for the session's whole life (ends when the transport closes).
fn spawn_status_tap(
    transport: Arc<StreamJsonTransport>,
    status: Arc<StdMutex<ThreadStatus>>,
    running_tasks: Arc<StdMutex<Vec<crate::RunningTask>>>,
    active_turn: Arc<AtomicBool>,
    project_dir: PathBuf,
    sid: String,
) {
    let mut sub = transport.subscribe();
    tokio::spawn(async move {
        // v0.8.20 — throttle the mid-turn context refresh so `/sessions` tracks
        // a long working turn's GROWING context (authoritative `get_context_usage`
        // on `assistant` steps), without one control_request per streamed step.
        let mut last_ctx: Option<Instant> = None;
        const CTX_REFRESH_MIN: Duration = Duration::from_secs(2);
        // Whether this turn already asked the CLI for its applied effort. The
        // level was only read at `TurnResult`, so the whole of a session's FIRST
        // turn rendered `effort —` even when the session was spawned with an
        // explicit level — and a long first turn is exactly when someone checks.
        // Probing on the first assistant step closes that window. Reset per turn
        // (the user can `/effort` mid-session), and skipped entirely once the
        // level is known, so a settled session pays nothing.
        let mut effort_probed_this_turn = false;
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(Outbound::Assistant(env)) => {
                        // Keep the live model id current from the API `model` field,
                        // carrying over a user-set `[1m]` tag (the API omits it).
                        let api_model = env
                            .message
                            .get("model")
                            .and_then(|v| v.as_str())
                            .filter(|m| !m.is_empty());
                        // v0.8.20 — ALSO refresh the live context window mid-turn
                        // (throttled) so `/sessions` reflects a long working turn's
                        // GROWING context, not only the value frozen at the last
                        // TurnResult. STILL authoritative — claude's own
                        // `get_context_usage` (totalTokens/maxTokens), never a
                        // heuristic. A transient miss leaves the last value (no
                        // blink): unlike TurnResult, a mid-turn None does NOT clear.
                        let now = Instant::now();
                        let fresh_ctx = if last_ctx
                            .is_none_or(|t| now.duration_since(t) >= CTX_REFRESH_MIN)
                        {
                            last_ctx = Some(now);
                            get_context_usage(&transport).await
                        } else {
                            None
                        };
                        // Mid-turn effort, on the SAME observe-don't-echo rule as
                        // model and context: the number comes from the vendor's
                        // own `get_settings`, never from the spawn request — a
                        // requested level that the CLI silently declined must not
                        // be displayed as applied. One extra control request per
                        // turn, and none at all once the answer is in.
                        let effort_unknown =
                            status.lock().map(|s| s.effort.is_none()).unwrap_or(false);
                        let fresh_effort = if effort_unknown && !effort_probed_this_turn {
                            effort_probed_this_turn = true;
                            get_applied_effort(&transport).await
                        } else {
                            None
                        };
                        if api_model.is_some() || fresh_ctx.is_some() || fresh_effort.is_some() {
                            let snapshot = if let Ok(mut s) = status.lock() {
                                if let Some(m) = api_model {
                                    s.model = Some(preserve_1m_tag(s.model.as_deref(), m));
                                }
                                if let Some(e) = fresh_effort {
                                    s.effort = Some(e);
                                }
                                if let Some((used, window)) = fresh_ctx {
                                    s.context = Some(crate::ContextUsage::known(
                                        used,
                                        window,
                                        crate::ContextSource::Derived,
                                    ));
                                    // Tag the 1M model id when the real window is
                                    // 1M (same rule the TurnResult path applies).
                                    if window >= 1_000_000 {
                                        if let Some(m) = s.model.as_mut() {
                                            if !m.to_ascii_lowercase().ends_with("[1m]") {
                                                m.push_str("[1m]");
                                            }
                                        }
                                    }
                                }
                                Some(s.clone())
                            } else {
                                None
                            };
                            if let Some(snap) = snapshot {
                                write_status_file(&project_dir, &sid, &snap);
                            }
                        }
                    }
                    Ok(Outbound::TurnResult(_)) => {
                        active_turn.store(false, Ordering::Release);
                        effort_probed_this_turn = false;
                        // The turn ended — turn-scoped tasks (sync subagents)
                        // cannot still be running, so drop them as a safety net in
                        // case a terminal task event was missed. Background
                        // workflows AND background shells (Bash run_in_background
                        // / Monitor) OUTLIVE the turn by design (the tool returns
                        // immediately and the run keeps going), so they stay until
                        // their own terminal task event arrives — this is what
                        // lets an idle session still show its in-flight work.
                        if let Ok(mut t) = running_tasks.lock() {
                            t.retain(task_outlives_turn);
                        }
                        // Context is read STRICTLY from claude's OWN accounting
                        // (`get_context_usage` → real totalTokens + maxTokens). No
                        // heuristic estimate: if the vendor build doesn't answer, the
                        // context is cleared (None) and the statusline shows none.
                        let real = get_context_usage(&transport).await;
                        // The vendor's runtime-resolved reasoning effort (Opus
                        // 4.6+ / Codex), e.g. `xhigh`. None on an older CLI
                        // without `get_settings` or a model with no effort axis.
                        let effort = get_applied_effort(&transport).await;
                        let snapshot = if let Ok(mut s) = status.lock() {
                            if let Some(e) = effort {
                                s.effort = Some(e);
                            }
                            // ONLY claude's own number; otherwise no context at all.
                            s.context = real.map(|(used, window)| {
                                crate::ContextUsage::known(
                                    used,
                                    window,
                                    crate::ContextSource::Derived,
                                )
                            });
                            // Show the FULL model id (…[1m]) when the real window
                            // is 1M — both statusline surfaces tag the 1M id the
                            // same way (rmux derives the window FROM the [1m];
                            // stream-json derives the [1m] FROM the real window).
                            let is_1m =
                                matches!(&s.context, Some(c) if c.window_tokens >= 1_000_000);
                            if is_1m {
                                if let Some(m) = s.model.as_mut() {
                                    if !m.to_ascii_lowercase().ends_with("[1m]") {
                                        m.push_str("[1m]");
                                    }
                                }
                            }
                            Some(s.clone())
                        } else {
                            None
                        };
                        if let Some(snap) = snapshot {
                            write_status_file(&project_dir, &sid, &snap);
                        }
                    }
                    // v0.8.20 `/status` — reflect claude's subagent/workflow task
                    // lifecycle into the running-task list (the authoritative
                    // running-subagent source; ccteam mirrors, never folds/counts).
                    Ok(Outbound::System(sys)) => reflect_task_event(&running_tasks, &sys),
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
}

/// True for a task that legitimately OUTLIVES the turn that spawned it, so the
/// turn-end safety net must not evict it. Thin alias for
/// [`crate::RunningTask::outlives_turn`] (the vocabulary's single authority,
/// shared with the IM `/status` working-signal check): background workflows
/// AND background shells (Bash `run_in_background` / Monitor) survive; sync
/// subagents are turn-scoped.
fn task_outlives_turn(t: &crate::RunningTask) -> bool {
    t.outlives_turn()
}

/// True for a task `status` that means the task is no longer running, so it
/// must leave the running-subagent list. Anything else (`running`,
/// `in_progress`, …) keeps it.
///
/// claude speaks TWO status vocabularies here: `task_notification` closes
/// with `completed`/`failed`/`stopped` (TaskStop and cancel paths emit
/// `stopped`), while `task_updated` patches carry the internal task state
/// whose terminal values are `completed`/`failed`/`killed`. Both kill-words
/// must be in this set: a background workflow has no turn-end eviction (it
/// outlives turns by design), so dropping its one terminal event shows it
/// "running" forever in `/status`.
fn task_status_is_terminal(s: &str) -> bool {
    matches!(
        s,
        "completed"
            | "failed"
            | "stopped"
            | "killed"
            | "cancelled"
            | "canceled"
            | "error"
            | "aborted"
            | "timed_out"
    )
}

/// Reflect one claude `system:task_*` line into the session's running-task list
/// — the AUTHORITATIVE running-subagent/workflow source for `/status`. ccteam
/// does NOT count or fold; it mirrors claude's own lifecycle:
/// - `task_started` → add a [`crate::RunningTask`] (idempotent on `task_id`),
/// - `task_updated` whose `patch.status` is terminal → remove,
/// - `task_notification` whose `status` is terminal → remove (redundant safety).
///
/// A plain `Agent` subagent and a workflow task both flow through these events
/// (distinguished by `task_type`). Non-task system subtypes (`init`,
/// `commands_changed`) are ignored.
fn reflect_task_event(
    running_tasks: &StdMutex<Vec<crate::RunningTask>>,
    sys: &protocol::SystemMsg,
) {
    match sys.subtype.as_str() {
        "task_started" if !sys.task_id.is_empty() => {
            if let Ok(mut tasks) = running_tasks.lock() {
                // Idempotent: a duplicate `task_started` must not double-insert.
                if tasks.iter().any(|t| t.task_id == sys.task_id) {
                    return;
                }
                tasks.push(crate::RunningTask {
                    task_id: sys.task_id.clone(),
                    kind: sys.subagent_type.clone(),
                    description: sys.description.clone(),
                    task_type: sys.task_type.clone(),
                    started: Instant::now(),
                });
            }
        }
        "task_updated" if !sys.task_id.is_empty() => {
            let terminal = sys
                .patch
                .as_ref()
                .and_then(|p| p.get("status"))
                .and_then(|s| s.as_str())
                .map(task_status_is_terminal)
                .unwrap_or(false);
            if terminal {
                if let Ok(mut tasks) = running_tasks.lock() {
                    tasks.retain(|t| t.task_id != sys.task_id);
                }
            }
        }
        "task_notification" if !sys.task_id.is_empty() && task_status_is_terminal(&sys.status) => {
            if let Ok(mut tasks) = running_tasks.lock() {
                tasks.retain(|t| t.task_id != sys.task_id);
            }
        }
        _ => {}
    }
}

/// Query claude's REAL context accounting via the `get_context_usage`
/// control_request → `(totalTokens, maxTokens)`. This is the vendor's actual
/// window for the session (e.g. a default Opus 4.8 session reports
/// `maxTokens: 200000` even though the model advertises a 1M capability), so
/// it replaces the brittle `[1m]`-suffix → 1M/200k heuristic for the live
/// context bar. `None` on timeout / error / an older CLI without the subtype
/// (the caller then falls back to the usage-sum + heuristic). Short timeout —
/// it must never stall the status tap.
async fn get_context_usage(transport: &StreamJsonTransport) -> Option<(u64, u64)> {
    let body = transport
        .request_control("get_context_usage", json!({}), Duration::from_secs(3))
        .await
        .ok()?;
    if body.subtype != "success" {
        return None;
    }
    let resp = body.response.as_ref()?;
    let used = resp.get("totalTokens").and_then(|v| v.as_u64())?;
    let window = resp
        .get("maxTokens")
        .and_then(|v| v.as_u64())
        .or_else(|| resp.get("rawMaxTokens").and_then(|v| v.as_u64()))?;
    Some((used, window))
}

/// `get_usage` control_request → ACCOUNT-level usage / rate-limits
/// ([`crate::AccountUsage`]). The vendor response carries `subscription_type`
/// and `rate_limits.{five_hour,seven_day}.{utilization,resets_at}` + a `limits[]`
/// array (group·severity) + `extra_usage` (credits). `None` on timeout / error /
/// a CLI without the subtype (probed: `get_usage` is the real subtype;
/// `rate_limits`/`usage` error). Short timeout — must never stall the dashboard.
async fn get_account_usage(transport: &StreamJsonTransport) -> Option<crate::AccountUsage> {
    let body = transport
        .request_control("get_usage", json!({}), Duration::from_secs(3))
        .await
        .ok()?;
    if body.subtype != "success" {
        return None;
    }
    let resp = body.response.as_ref()?;
    let rl = resp.get("rate_limits");
    let five = rl.and_then(|r| r.get("five_hour"));
    let seven = rl.and_then(|r| r.get("seven_day"));
    let extra = rl.and_then(|r| r.get("extra_usage"));
    // Weekly severity (e.g. "warning") is in the `limits[]` entry grouped "weekly".
    let weekly_severity = rl
        .and_then(|r| r.get("limits"))
        .and_then(|l| l.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|x| x.get("group").and_then(|g| g.as_str()) == Some("weekly"))
        })
        .and_then(|x| x.get("severity").and_then(|s| s.as_str()))
        .map(str::to_string);
    let pct = |o: Option<&serde_json::Value>| {
        o.and_then(|x| x.get("utilization"))
            .and_then(|v| v.as_f64())
            .map(|f| f.round() as u8)
    };
    let resets = |o: Option<&serde_json::Value>| {
        o.and_then(|x| x.get("resets_at"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let usage = crate::AccountUsage {
        subscription: resp
            .get("subscription_type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        five_hour_pct: pct(five),
        five_hour_resets_at: resets(five),
        weekly_pct: pct(seven),
        weekly_resets_at: resets(seven),
        weekly_severity,
        credits_pct: pct(extra),
    };
    if usage == crate::AccountUsage::default() {
        None
    } else {
        Some(usage)
    }
}

/// The reasoning-effort levels claude accepts (Opus 4.6+), low→high. Mirrors
/// the vendor `EFFORT_LEVELS` (`/effort`); used to validate a `/model <id>
/// <effort>` / `/effort <level>` argument before it touches settings.
pub(crate) const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Normalize a user-typed effort token to a canonical level, or `None` if it
/// isn't one (so `/model opus xhigh` splits cleanly but `/model opus-4-8`
/// never mis-reads a model fragment as effort).
pub(crate) fn normalize_effort(arg: &str) -> Option<String> {
    let a = arg.trim().to_ascii_lowercase();
    EFFORT_LEVELS
        .iter()
        .find(|l| **l == a)
        .map(|l| (*l).to_string())
}

/// Split a `/model` argument into `(model, effort?)`: the effort is the
/// trailing whitespace-separated token IFF it's a valid level, so
/// `opus[1m] xhigh` → `("opus[1m]", Some("xhigh"))` while a bare model id
/// (`opus-4-8`, `claude-3-5-haiku`) is never mis-split.
pub(crate) fn split_model_effort(arg: &str) -> (String, Option<String>) {
    let arg = arg.trim();
    if let Some((head, tail)) = arg.rsplit_once(char::is_whitespace) {
        if let Some(eff) = normalize_effort(tail) {
            return (head.trim().to_string(), Some(eff));
        }
    }
    (arg.to_string(), None)
}

/// Build the bare-`/model` picker options from claude's REAL model list
/// (the `initialize` `response.models[]`, captured on [`LiveSession`]). One
/// [`ChoiceOption`] per (value, effort) so the picked `id` is EXACTLY the
/// `/model <id> [effort]` arg form the set_model arm parses via
/// [`split_model_effort`] (`id = "<value> <effort>"`); a model with no effort
/// axis (e.g. haiku) yields a single bare-id option (`id = "<value>"`).
/// Strictly deterministic — never a hardcoded list. Mirrors codex's
/// `model_options`.
fn claude_model_options(models: &[ClaudeModelOption]) -> Vec<ChoiceOption> {
    let mut out = Vec::new();
    for m in models {
        if m.efforts.is_empty() {
            out.push(ChoiceOption {
                id: m.value.clone(),
                label: m.value.clone(),
            });
        } else {
            for e in &m.efforts {
                out.push(ChoiceOption {
                    id: format!("{} {e}", m.value),
                    label: format!("{} ({e})", m.value),
                });
            }
        }
    }
    out
}

/// Build a single-select [`ChoicePrompt`] with a per-prompt unique token
/// (≤16B ASCII, no `:`). The gateway resolves callbacks token-globally, so a
/// name-based token would collide when two sessions raise the same picker at
/// once. Mirrors `claude_tui::claude_popup_prompt`'s `cj{hex}` scheme.
fn claude_choice_prompt(title: &str, options: Vec<ChoiceOption>) -> ChoicePrompt {
    ChoicePrompt {
        token: claude_common::unique_prompt_token("cm"),
        title: title.to_string(),
        options,
        multi: false,
    }
}

/// Read the vendor's REAL runtime-resolved reasoning effort via the
/// `get_settings` control_request (the level that "will actually be sent to
/// the API", after env / session / model defaults). The response shape has
/// moved across CLI versions — older builds report `applied.effort`, 2.1.2xx
/// reports the merged `effective.effortLevel` (verified against the live
/// 2.1.212 wire) — so read both, old shape first. `None` on timeout / error /
/// an older CLI without the subtype, or a model with no effort axis. Short
/// timeout — must never stall the status tap.
async fn get_applied_effort(transport: &StreamJsonTransport) -> Option<String> {
    let body = transport
        .request_control("get_settings", json!({}), Duration::from_secs(3))
        .await
        .ok()?;
    if body.subtype != "success" {
        return None;
    }
    extract_effort_from_settings(body.response.as_ref()?)
}

/// Pull the effort level out of a `get_settings` response body, tolerating
/// every known shape: `applied.effort` (pre-2.1.2xx), `effective.effortLevel`
/// (2.1.212+), and `effective.effort` (defensive). Pure — unit-testable
/// against captured wire fixtures.
fn extract_effort_from_settings(response: &serde_json::Value) -> Option<String> {
    response
        .get("applied")
        .and_then(|a| a.get("effort"))
        .or_else(|| response.get("effective").and_then(|e| e.get("effortLevel")))
        .or_else(|| response.get("effective").and_then(|e| e.get("effort")))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// Persist a reasoning-effort level into the project's ccteam-managed
/// `.claude/settings.local.json` as `effortLevel` (the vendor key claude reads
/// at startup; there is NO live `set_effort` control — `set_model` is
/// model-only). Idempotent + non-clobbering: every sibling key is preserved.
/// Like the plugin-enable path, it lands in the gitignored `local` layer and
/// NEVER touches the user's `settings.json`; it takes effect on the session's
/// next start (`/new`). `cwd` is the session's project root.
fn set_effort_level(cwd: &Path, level: &str) -> std::io::Result<()> {
    let path = cwd.join(".claude").join("settings.local.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    root.as_object_mut()
        .expect("filtered to object")
        .insert("effortLevel".to_string(), json!(level));
    let body = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// Live-apply a settings delta to a running stream-json session via the
/// `apply_flag_settings` control_request — vendor doc: *"Merges the provided
/// settings into the flag settings layer, updating the active configuration."*
/// i.e. an IMMEDIATE, no-restart settings change (the same control iOS/remote
/// clients use for runtime config — there is no per-setting control like a
/// `set_effort`; this generic merge is the mechanism). Verified empirically
/// against the live vendor (`applied.effort` flips high→low with no restart).
///
/// This is the generic hook every live-config command rides on (`/effort`
/// today, `/set <key> <value>` for anything else, more dedicated commands
/// later). `settings` is the JSON object to merge (e.g. `{"effortLevel":"max"}`).
/// Returns Ok on a `success` control_response, else Err(vendor reason).
async fn apply_flag_settings_live(
    live: &LiveSession,
    settings: serde_json::Value,
) -> Result<(), String> {
    let body = live
        .transport
        .request_control(
            "apply_flag_settings",
            json!({ "settings": settings }),
            init_timeout(),
        )
        .await
        .map_err(|e| format!("apply_flag_settings 失败: {e}"))?;
    if body.subtype != "success" {
        return Err(body
            .error
            .unwrap_or_else(|| "vendor rejected apply_flag_settings".into()));
    }
    Ok(())
}

/// Settings keys a chat `/set` command must NEVER live-mutate: the
/// safety / HITL / execution boundary. (`apply_flag_settings` is powerful — it
/// can merge ANY settings key into the active config — so the user-facing
/// escape hatch is fenced to keep a chat command from silently weakening
/// permissions, hooks, or the MCP surface.)
const SET_PROTECTED_KEYS: &[&str] = &["permissions", "hooks", "mcpServers"];

/// Whether `m` is claude's OWN model-picker placeholder (`SystemMsg::
/// from_initialize`'s `models[0].value`, typically the literal string
/// `"default"` — labeled "Default"/"recommended" in the picker) rather than a
/// concrete, resolvable model id. ccteam cannot know which real model that
/// resolves to without a live turn, so this must never reach `--model` or be
/// displayed as if it were a resolved name — see the status seed in
/// [`ClaudeStreamJsonAdapter::start_thread`] and the filter below.
fn is_model_placeholder(m: &str) -> bool {
    m.trim().eq_ignore_ascii_case("default")
}

/// The last-known model for a session, from its persisted `status.json` (the
/// status tap records every `/model` switch + the API model). The gateway uses
/// this so a daemon-restart resume re-spawns at the model the user actually set
/// (`/model opus[1m]`), not the role default — `--resume` otherwise reverts to
/// claude's default model (the snapshot carries no live `set_model` state).
/// `None` for a never-run session (a fresh `/new` keeps using the role model).
pub fn persisted_session_model(project_dir: &Path, sid: &str) -> Option<String> {
    read_status_file(project_dir, sid)
        .and_then(|s| s.model)
        // Real model ids never contain `<`/`>`; reject a placeholder like
        // `<synthetic>` (legacy / never-resolved), and reject claude's own
        // unresolved "default" picker label (defense-in-depth — the status
        // seed already withholds it, but `--model default` would be a
        // meaningless, ambiguous request if one ever slipped through).
        .filter(|m| {
            let m = m.trim();
            !m.is_empty() && !m.contains('<') && !m.contains('>') && !is_model_placeholder(m)
        })
}

/// Read at most the trailing `max_bytes` of a file as a UTF-8 string (lossy).
/// Bounds a `goal_status` scan so a huge transcript can't stall the statusline;
/// a partial first line (when we seek mid-file) just fails to parse and is
/// skipped by the caller.
fn read_transcript_tail(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > max_bytes {
        f.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The session's current `/goal`, read from the Claude transcript. Claude
/// writes the active goal as a `{type:"attachment", attachment:{type:
/// "goal_status", condition, met}}` record on the user turn — it is NOT on the
/// stream-json stdout stream and has no control_request (both verified by live
/// probe), so the only read path is the transcript jsonl (the same file the TUI
/// adapter tails — reading it is the blessed path, not terminal scraping).
/// Returns the LAST such record (the current goal); `None` when no goal is set,
/// it was cleared (empty condition), or the transcript is absent.
fn read_latest_goal_status(cwd: &Path, uuid: &str) -> Option<crate::GoalStatus> {
    let path = anthropic_project_dir(cwd)?.join(format!("{uuid}.jsonl"));
    let body = read_transcript_tail(&path, 8 * 1024 * 1024)?;
    parse_latest_goal_status(&body)
}

/// Scan transcript jsonl lines for the LAST `goal_status` attachment. A later
/// `/goal clear` (or an empty condition) resets it to `None`. Pure (no fs) so
/// it is unit-testable; `read_latest_goal_status` wraps it with the file read.
///
/// COST DISCIPLINE (2026-08-02): the tail handed here is up to 8 MB and this
/// runs on every statusline read — per live session on the team graph, so a
/// naive full parse is tens of millions of JSON parses per page refresh. Two
/// properties keep it cheap without changing the answer:
/// - scan from the END and return the first hit (the LAST record — identical
///   result, but an active goal is found within a few lines);
/// - `contains` pre-filter before `serde_json` (a goal record is rare, and
///   substring scanning is orders of magnitude cheaper than parsing).
fn parse_latest_goal_status(body: &str) -> Option<crate::GoalStatus> {
    for line in body.lines().rev() {
        // Cheap reject: only lines that literally mention the marker can match.
        if !line.contains("goal_status") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(att) = v.get("attachment") else {
            continue;
        };
        if att.get("type").and_then(|t| t.as_str()) != Some("goal_status") {
            continue;
        }
        let condition = att
            .get("condition")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let met = att.get("met").and_then(|m| m.as_bool()).unwrap_or(false);
        // A cleared goal carries an empty condition → no active goal. This is
        // the LAST record, so it decides on its own — nothing earlier can
        // revive a cleared goal.
        return if condition.trim().is_empty() {
            None
        } else {
            Some(crate::GoalStatus { condition, met })
        };
    }
    None
}

/// Translate one outbound message and forward its events to the stream's
/// channel. `Err(())` means the consumer dropped the stream (stop).
async fn forward(
    translator: &mut StreamTranslator,
    tx: &mpsc::Sender<ThreadEvent>,
    out: Outbound,
) -> Result<(), ()> {
    if matches!(out, Outbound::Other) {
        return Ok(());
    }
    for ev in translator.ingest(out) {
        tx.send(ev).await.map_err(|_| ())?;
    }
    Ok(())
}

/// How long to wait for `system:init` before declaring the spawn failed.
/// claude startup (incl. auth) can be slow; tests shorten it via env.
fn init_timeout() -> Duration {
    std::env::var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(30))
}

impl ClaudeStreamJsonAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the HITL `can_use_tool` resolver at construction time
    /// (test-only builder pattern — production wiring happens post-
    /// construction via [`Self::set_resolver`], see the field doc).
    pub fn with_resolver(self, resolver: Arc<dyn CanUseToolResolver>) -> Self {
        self.set_resolver(resolver);
        self
    }

    /// Wire (or replace) the production HITL resolver AFTER construction.
    /// `&self` (not `&mut self`): the resolver lives behind a shared
    /// `Arc<StdMutex<..>>` cell so every clone of this adapter — including
    /// the singleton the gateway's `(vendor, protocol)` factory closure
    /// captured — sees the update (v0.8.22 P0-2: the daemon calls this once
    /// the gateway + pending registry + event sink are ready, closing the
    /// "stream-json hitl sessions silently deny with no resolver wired" gap).
    pub fn set_resolver(&self, resolver: Arc<dyn CanUseToolResolver>) {
        *self.resolver.lock().unwrap() = Some(resolver);
    }

    fn lookup(&self, identity: &str) -> Option<Arc<LiveSession>> {
        self.live.lock().unwrap().get(identity).cloned()
    }

    /// The slash-command table claude advertised at `system:init` for a
    /// live session (by vendor uuid / handle identity). The Wave 2 bridge
    /// gate keys known-vs-unknown commands off this; exposed now so the
    /// captured table has a reader and so tests can assert it.
    pub fn session_command_table(&self, identity: &str) -> Option<Vec<String>> {
        self.lookup(identity).map(|live| live.commands.clone())
    }

    /// The host-facet identity (`sid` / `vendor_uuid` / `host`) for a live
    /// session — the §七 ⑤ mapping record, surfaced for the gateway + tests.
    pub fn session_identity(&self, identity: &str) -> Option<SessionIdentity> {
        self.lookup(identity).map(|live| live.identity.clone())
    }

    /// True when claude has already filed a transcript jsonl for this uuid
    /// under the project's Anthropic dir — the signal to `--resume` rather
    /// than mint a fresh `--session-id`.
    fn session_jsonl_exists(cwd: &Path, uuid: &str) -> bool {
        anthropic_project_dir(cwd)
            .map(|d| d.join(format!("{uuid}.jsonl")).exists())
            .unwrap_or(false)
    }

    /// Spawn the child + perform the `initialize` handshake, shutting the
    /// transport down on any failure so a dead child never lingers.
    ///
    /// claude (stream-json) does **not** emit a `system:init` line until the
    /// first user turn, so waiting for `system:init` at spawn would hang
    /// forever (the daemon waits for init while claude waits for input). The
    /// capability handshake is the `initialize` control_request →
    /// `control_response` (what the VS Code extension / SDK do); we parse the
    /// slash-command table + model out of its response. `system:init` is still
    /// captured opportunistically by the reader when it arrives with the first
    /// turn (the bridge gate's command table is seeded from the handshake).
    async fn spawn_and_init(
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<(Arc<StreamJsonTransport>, protocol::SystemMsg), HarnessError> {
        let transport = StreamJsonTransport::connect_stdio(argv, env, cwd)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("stream-json connect: {e:#}")))?;
        Self::init_transport(transport).await
    }

    /// v0.9.0 W3 (F3, tech-design §0.4/§4.3) — the remote counterpart of
    /// [`Self::spawn_and_init`]: dial the satellite's `ccteam-exec.v1`
    /// bridge instead of spawning a local child, then run the EXACT SAME
    /// `initialize` handshake ([`Self::init_transport`]) over the resulting
    /// transport. The adapter's protocol logic downstream is unaware which
    /// path built the transport (the transport law).
    async fn spawn_and_init_remote(
        target: &crate::execution::remote_exec::RemoteExecTarget,
        exec_spec: crate::execution::remote_exec::ExecSpec,
    ) -> Result<(Arc<StreamJsonTransport>, protocol::SystemMsg), HarnessError> {
        let (reader, writer) = crate::execution::remote_exec::connect(target, exec_spec)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("ccteam-exec.v1 connect: {e:#}")))?;
        let transport = StreamJsonTransport::spawn_from_io(reader, writer, None);
        Self::init_transport(transport).await
    }

    /// Shared `initialize` control_request handshake — see
    /// [`Self::spawn_and_init`]'s doc for why this (not `system:init`) is
    /// the capability handshake stream-json uses.
    async fn init_transport(
        transport: StreamJsonTransport,
    ) -> Result<(Arc<StreamJsonTransport>, protocol::SystemMsg), HarnessError> {
        match transport
            .request_control("initialize", json!({}), init_timeout())
            .await
        {
            Ok(body) if body.subtype == "success" => Ok((
                Arc::new(transport),
                protocol::SystemMsg::from_initialize(&body),
            )),
            Ok(body) => {
                transport.shutdown().await;
                Err(HarnessError::SpawnFailed(format!(
                    "stream-json initialize rejected: {}",
                    body.error.unwrap_or_else(|| body.subtype.clone())
                )))
            }
            Err(e) => {
                transport.shutdown().await;
                Err(HarnessError::SpawnFailed(format!(
                    "stream-json init handshake: {e:#}"
                )))
            }
        }
    }

    /// v0.9.0 W3 — build the `ccteam-exec.v1` [`ExecSpec`](crate::execution::remote_exec::ExecSpec)
    /// for one spawn attempt: argv MINUS argv\[0\] (the satellite resolves
    /// its own `claude` binary — never trust a wire path), the `CCTEAM_*`
    /// env subset (allowlist — mirrors the satellite's own filter, belt +
    /// braces), and — when `ship_mcp` — the curated `mcp.json` body with
    /// the `{{DAEMON_URL}}` template token in place of a concrete URL (the
    /// satellite substitutes its own `daemon_url`; the main daemon never
    /// has to guess its own LAN-reachable address).
    fn build_exec_spec(
        ctx: &SpawnCtx,
        argv: Vec<String>,
        env: &[(String, String)],
        ship_mcp: bool,
        mcp_relpath: &Path,
    ) -> crate::execution::remote_exec::ExecSpec {
        let mut exec_spec = crate::execution::remote_exec::ExecSpec::new(
            "claude",
            ctx.remote
                .as_ref()
                .map(|target| target.wire_slug.clone())
                .unwrap_or_else(|| ctx.slug.clone()),
            ctx.sid.clone(),
            "stream-json",
        );
        exec_spec.args = argv.into_iter().skip(1).collect();
        for (k, v) in env {
            if k.starts_with("CCTEAM_") {
                exec_spec.env.insert(k.clone(), v.clone());
            }
        }
        if ship_mcp {
            let daemon_url_mcp = format!(
                "{}/mcp",
                crate::execution::remote_exec::ExecSpec::DAEMON_URL_TOKEN
            );
            let input = crate::execution::mcp_config::CuratedMcpInput {
                sid: &ctx.sid,
                secret: &ctx.secret,
                http_url: Some(&daemon_url_mcp),
            };
            let body = crate::execution::mcp_config::build_curated_mcp_json(&input);
            match serde_json::to_string_pretty(&body) {
                Ok(content) => exec_spec
                    .files
                    .push(crate::execution::remote_exec::ExecFile {
                        relpath: mcp_relpath.to_string_lossy().to_string(),
                        content,
                    }),
                Err(e) => tracing::warn!(
                    sid = %ctx.sid,
                    error = %e,
                    "claude-stream-json: serialize remote mcp.json failed; spawning without in-agent MCP"
                ),
            }
        }
        exec_spec
    }
}

#[async_trait]
impl HarnessAdapter for ClaudeStreamJsonAdapter {
    fn name(&self) -> &'static str {
        STREAM_JSON_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        // v0.8.11 E2 — pin-point isolate the official Telegram plugin (its
        // bot-token getUpdates poll structurally collides with ccteam's IM
        // gateway). Same managed layer the tmux path uses; only this one
        // plugin. A remote project's daemon-side `project_dir` is a DATA
        // HOME, not a vendor working tree: never materialize `.claude/`
        // there; the satellite's real working tree owns its settings.
        if ctx.remote.is_none() {
            crate::execution::claude_tui::ensure_telegram_plugin_disabled(&ctx.project_dir)?;
        }
        let bin = spawn_spec::claude_bin();
        // §七 ⑤ — stable per-(slug,sid) uuid: the stateless resume key.
        let uuid = spawn_spec::deterministic_session_uuid(&ctx.slug, &ctx.sid);
        // v0.9.0 W3 — a remote spawn's transcript lives on the SATELLITE,
        // unreachable via the local `session_jsonl_exists` fs check, so
        // ALWAYS attempt `--resume` first there: claude fails resume for an
        // unknown uuid (the satellite's very first spawn of this sid) and
        // the existing fallback-to-fresh path below catches it exactly like
        // a local resume failure — same contract, just optimistic instead
        // of fs-checked (tech-design §4.4: satellite `--resume` is how a
        // remote rebuild continues context, PRD F3.4).
        let resume = ctx.remote.is_some() || Self::session_jsonl_exists(&ctx.cwd, &uuid);

        // v0.8.24 C1 / v0.9.0 W3 — curated per-session MCP. `--mcp-config`
        // is a RELATIVE path (`.ccteam/chat/<sid>/mcp.json`, cwd = project
        // root) for BOTH local and remote spawns now (tech-design §4.3:
        // one argv shape; local behavior is unaffected since cwd ==
        // project_dir there too). Present ⇒ --mcp-config + strict; absent
        // ⇒ strict alone (strip ambient). Remote ALWAYS ships a fresh
        // mcp.json via `ExecSpec::files` (no local copy to fall back on,
        // and the secret is freshly minted per rebuild anyway) — gated on
        // a non-empty secret, mirroring the gateway's own gate for writing
        // the local copy.
        let mcp_present_locally =
            crate::execution::mcp_config::session_mcp_config_path(&ctx.project_dir, &ctx.sid)
                .exists();
        let mcp_relpath = PathBuf::from(".ccteam")
            .join("chat")
            .join(&ctx.sid)
            .join(crate::execution::mcp_config::MCP_CONFIG_FILENAME);
        let ship_mcp = if ctx.remote.is_some() {
            !ctx.secret.is_empty()
        } else {
            mcp_present_locally
        };
        let mcp_config_arg: Option<PathBuf> = ship_mcp.then(|| mcp_relpath.clone());
        let make_argv = |resume: bool| {
            spawn_spec::build_argv(
                &bin,
                &StreamJsonSpawnInput {
                    role: &spec.role,
                    session_uuid: &uuid,
                    resume,
                    model_id: ctx.model_id.as_deref(),
                    effort: ctx.effort.as_deref(),
                    permission_mode: ctx.permission_mode,
                    mcp_config_path: mcp_config_arg.as_deref(),
                },
            )
        };
        let env = spawn_spec::build_env(&spec.role, &ctx.slug, &ctx.secret, &ctx.sid);

        // Try the resume spawn first when a prior transcript exists (or,
        // remote, optimistically); on failure fall back to a fresh
        // `--session-id` spawn and emit a chat_session_reset with an
        // explicit reason (the honest context-loss signal — never silently
        // synthesize). The transport-building step differs (local child vs
        // `ccteam-exec.v1` WS bridge — tech-design §0.4: execution location
        // is a transport parameter); everything after this `match` is
        // identical for both.
        let (transport, init) = if let Some(remote) = ctx.remote.as_ref() {
            let build_spec = |resume: bool| {
                Self::build_exec_spec(ctx, make_argv(resume), &env, ship_mcp, &mcp_relpath)
            };
            match Self::spawn_and_init_remote(remote, build_spec(resume)).await {
                Ok(ok) => ok,
                Err(resume_err) if resume => {
                    tracing::warn!(
                        sid = %ctx.sid,
                        slug = %ctx.slug,
                        error = %resume_err,
                        "claude-stream-json: remote --resume spawn failed; falling back to fresh --session-id"
                    );
                    let fresh = Self::spawn_and_init_remote(remote, build_spec(false)).await?;
                    if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
                        let ev = build_chat_session_reset_event_with_reason(
                            &spec.role,
                            &ctx.sid,
                            "resume_failed_fallback_to_fresh",
                        );
                        if let Err(err) = append_event(&progress_path, &ev) {
                            tracing::warn!(error = %err, "claude-stream-json: append reset event failed");
                        }
                    }
                    fresh
                }
                Err(e) => return Err(e),
            }
        } else {
            match Self::spawn_and_init(&make_argv(resume), &env, &ctx.cwd).await {
                Ok(ok) => ok,
                Err(resume_err) if resume => {
                    tracing::warn!(
                        sid = %ctx.sid,
                        slug = %ctx.slug,
                        error = %resume_err,
                        "claude-stream-json: --resume spawn failed; falling back to fresh --session-id"
                    );
                    let fresh = Self::spawn_and_init(&make_argv(false), &env, &ctx.cwd).await?;
                    if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
                        let ev = build_chat_session_reset_event_with_reason(
                            &spec.role,
                            &ctx.sid,
                            "resume_failed_fallback_to_fresh",
                        );
                        if let Err(err) = append_event(&progress_path, &ev) {
                            tracing::warn!(error = %err, "claude-stream-json: append reset event failed");
                        }
                    }
                    fresh
                }
                Err(e) => return Err(e),
            }
        };

        let identity = SessionIdentity {
            sid: ctx.sid.clone(),
            vendor_uuid: uuid.clone(),
            host: "local".to_string(),
        };
        // Seed the live status with the `initialize` model (context unknown
        // until the first turn's `usage` lands). The status tap below keeps it
        // current; thread_status reads it. `models[0].value` is often the
        // literal string `"default"` — claude's OWN picker-menu placeholder
        // (`SystemMsg::from_initialize`: "Default" / "recommended"), not a
        // concrete resolved model id; ccteam genuinely does not know which
        // model that resolves to until a real turn runs. Surfacing "default"
        // verbatim in /sessions or /status would read as a real (wrong) model
        // name, so withhold it here (`None` = "not yet known", the SAME
        // convention every other not-yet-reported field already uses) —
        // `spawn_status_tap`'s `preserve_1m_tag` below overwrites it with the
        // REAL API-reported model as soon as the first assistant message lands.
        let status = Arc::new(StdMutex::new(ThreadStatus {
            model: init.model.clone().filter(|m| !is_model_placeholder(m)),
            context: None,
            // Effort is unknown until the first turn — the status tap reads the
            // vendor's runtime-resolved level via `get_settings`.
            effort: None,
            // Goal is read from the transcript on `thread_status`, not the tap.
            goal: None,
        }));
        // v0.8.20 `/status` — the live running-subagent/workflow list, kept
        // current by the SAME status tap from claude's `system:task_*` events.
        let running_tasks: Arc<StdMutex<Vec<crate::RunningTask>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let active_turn = Arc::new(AtomicBool::new(false));
        // Status tap (every session, not just hitl): watch the transport for
        // `assistant`/`result` messages and fold each one's `usage` (+ live
        // `message.model`) into `status`, so /sessions + the web statusline
        // show model + context% as the session burns context; ALSO reflect the
        // `system:task_*` lifecycle into `running_tasks` for `/status`.
        spawn_status_tap(
            Arc::clone(&transport),
            Arc::clone(&status),
            Arc::clone(&running_tasks),
            Arc::clone(&active_turn),
            ctx.project_dir.clone(),
            ctx.sid.clone(),
        );
        // HITL: only a hitl session (`--permission-prompt-tool stdio`) ever
        // receives `can_use_tool` reverse RPCs. Spawn the dispatcher that
        // resolves each via the wired resolver (→ IM approve/deny) and
        // replies with a control_response. A skip session never gets one,
        // so no dispatcher is needed.
        if ctx.permission_mode.is_hitl() {
            // Snapshot the CURRENT resolver at spawn time (not at adapter
            // construction time — see the field doc on why this is a
            // shared, lazily-wired cell).
            let resolver = self.resolver.lock().unwrap().clone();
            spawn_hitl_dispatcher(Arc::clone(&transport), ctx.sid.clone(), resolver);
        }
        // v0.8.19 — restore the 1M context window on resume. `build_argv`
        // stripped the `[1m]` tag from `--model` (claude rejects `…[1m]` and
        // would silently default to sonnet), so claude came up on the correct
        // BASE model. Re-request the persisted `[1m]` model via `set_model` —
        // the SAME control the live `/model` path uses, which DOES accept
        // `[1m]` — to put the resumed session back on 1M. Best-effort: on
        // failure the (correct) base model stands; never fail the spawn.
        if let Some(m) = ctx.model_id.as_deref() {
            if m.len() >= 4 && m[m.len() - 4..].eq_ignore_ascii_case("[1m]") {
                match transport
                    .request_control("set_model", json!({ "model": m }), init_timeout())
                    .await
                {
                    Ok(body) if body.subtype == "success" => {
                        if let Ok(mut s) = status.lock() {
                            s.model = Some(m.to_string());
                        }
                    }
                    Ok(body) => tracing::warn!(
                        sid = %ctx.sid, model = %m, why = ?body.error,
                        "claude-stream-json: post-resume set_model([1m]) rejected; base model stands"
                    ),
                    Err(e) => tracing::warn!(
                        sid = %ctx.sid, model = %m, error = %e,
                        "claude-stream-json: post-resume set_model([1m]) failed; base model stands"
                    ),
                }
            }
        }
        crate::model_catalog::record_vendor_models_best_effort(
            "claude",
            "claude initialize.models",
            init.models
                .iter()
                .filter(|model| !model.value.trim().is_empty())
                .map(|model| crate::model_catalog::CatalogModel {
                    id: model.value.clone(),
                    display_name: model.display_name.clone(),
                    efforts: model.efforts.clone(),
                })
                .collect(),
        );
        let live = LiveSession {
            identity: identity.clone(),
            transport,
            slug: ctx.slug.clone(),
            role: spec.role.clone(),
            project_dir: ctx.project_dir.clone(),
            cwd: ctx.cwd.clone(),
            commands: init.slash_commands.clone(),
            models: init.models.clone(),
            status,
            running_tasks,
            active_turn,
        };
        self.live
            .lock()
            .unwrap()
            .insert(uuid.clone(), Arc::new(live));

        tracing::info!(
            event = "stream_json_started",
            sid = %ctx.sid,
            slug = %ctx.slug,
            role = %spec.role,
            vendor_uuid = %uuid,
            resumed = resume,
            "claude-stream-json: session live"
        );

        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: uuid.clone(),
            started_at: Utc::now(),
            raw_extras: json!({
                "adapter": STREAM_JSON_ADAPTER_NAME,
                "protocol": "stream-json",
                "host": identity.host,
                "vendor_uuid": uuid,
                "sid": ctx.sid,
                "slug": ctx.slug,
                "role": spec.role,
                "project_dir": ctx.project_dir.to_string_lossy(),
                "cwd": ctx.cwd.to_string_lossy(),
            }),
        })
    }

    /// A stream-json child can exit out from under a held handle (crash / OOM /
    /// long idle): the [`LiveSession`] then lingers in the registry with a
    /// CLOSED transport. Liveness = a live session whose transport has not
    /// signalled close. A missing entry (idle-released / post-`close_thread`) is
    /// also "not live" → the gateway resumes via the resume-aware `start_thread`.
    /// Deliberately does NOT gate on `is_initialized()`: claude emits
    /// `system:init` only on the FIRST turn, so a freshly-spawned-but-unturned
    /// session is live yet uninitialized.
    fn thread_is_live(&self, h: &ThreadHandle) -> bool {
        self.lookup(&h.identity)
            .map(|live| !live.transport.is_session_closed())
            .unwrap_or(false)
    }

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        if routing == TurnRouting::Queue {
            return Err(HarnessError::NotImplemented {
                reason: "claude stream-json does not expose a distinct queued-turn channel".into(),
            });
        }
        let Some(live) = self.lookup(&h.identity) else {
            // Registry miss = the session was idle-released / closed: nothing was
            // sent, so this is a recoverable ThreadDied (caller resumes + retries
            // once), not a hard SubmitFailed.
            return Err(HarnessError::ThreadDied(format!(
                "stream-json session not live: {} (needs resume)",
                h.identity
            )));
        };
        let text = match input {
            TurnInput::UserText(s) => s,
            TurnInput::Artifact(p) => {
                format!("Look at the file I just placed at {}", p.display())
            }
            TurnInput::Image(p) => {
                format!("Look at the image I just placed at {}", p.display())
            }
            TurnInput::ToolResult { call_id, content } => {
                let body = match content {
                    serde_json::Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                format!("Tool result for {call_id}: {body}")
            }
        };
        let was_active = live.active_turn.swap(true, Ordering::AcqRel);
        if let Err(error) = live
            .transport
            .send_line(protocol::user_text_line(&text))
            .await
        {
            live.active_turn.store(was_active, Ordering::Release);
            // Writer closed = the child exited mid-handoff (the probe→send
            // race): the line was NOT delivered, so it's a recoverable
            // ThreadDied the gateway resumes + retries once.
            return Err(HarnessError::ThreadDied(format!(
                "stream-json send: {error:#}"
            )));
        }

        // Synthesize a turn id (the pump keys turns.jsonl off its own seq;
        // this id is only for adapter-side correlation / logs).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let turn_id = TurnId::new(format!("turn-{nanos:x}"));
        if was_active {
            Ok(TurnSubmission::injected(turn_id))
        } else {
            Ok(TurnSubmission::started(turn_id))
        }
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let Some(live) = self.lookup(&h.identity) else {
            // No live session (resumed handle pre-spawn / unknown): empty
            // stream. The gateway resume path re-establishes via
            // start_thread, then re-subscribes.
            return Box::pin(futures::stream::empty());
        };
        let mut sub = live.transport.subscribe();
        let transport = Arc::clone(&live.transport);
        let (tx, rx) = mpsc::channel::<ThreadEvent>(64);
        tokio::spawn(async move {
            let mut translator = StreamTranslator::new();
            loop {
                tokio::select! {
                    msg = sub.recv() => match msg {
                        Ok(out) => {
                            if forward(&mut translator, &tx, out).await.is_err() {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n, "claude-stream-json: events subscriber lagged");
                        }
                        // The transport was dropped — emit the in-flight signal.
                        Err(broadcast::error::RecvError::Closed) => {
                            if let Some(ev) = translator.on_close() {
                                let _ = tx.send(ev).await;
                            }
                            return;
                        }
                    },
                    // The broadcast sender lives on the transport, so a dead
                    // child never yields `Closed` here — the explicit close
                    // signal does. Drain any buffered messages first (so a
                    // final answer emitted just before EOF isn't lost), then —
                    // if a turn was still in flight — emit the honest
                    // in-flight-loss signal before ending the stream (E3).
                    _ = transport.wait_closed() => {
                        while let Ok(out) = sub.try_recv() {
                            if forward(&mut translator, &tx, out).await.is_err() {
                                return;
                            }
                        }
                        if let Some(ev) = translator.on_close() {
                            let _ = tx.send(ev).await;
                        }
                        return;
                    }
                }
            }
        });
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        }))
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        // A live session for this uuid (idle wake within one daemon
        // lifetime) → hand back a handle pointing at it. Otherwise we
        // cannot rebuild without the SpawnCtx (cwd/role): the gateway falls
        // back to `start_thread`, which IS resume-aware (deterministic uuid
        // + jsonl-presence → `--resume`).
        if let Some(live) = self.lookup(persistent_id) {
            if live.transport.is_initialized() {
                return Ok(ThreadHandle {
                    vendor: AgentVendor::Claude,
                    mode: ExecutionMode::Chat,
                    identity: persistent_id.to_string(),
                    started_at: Utc::now(),
                    raw_extras: json!({
                        "adapter": STREAM_JSON_ADAPTER_NAME,
                        "protocol": "stream-json",
                        "host": live.identity.host,
                        "vendor_uuid": persistent_id,
                        "sid": live.identity.sid,
                        "slug": live.slug,
                        "role": live.role,
                        "project_dir": live.project_dir.to_string_lossy(),
                        "cwd": live.cwd.to_string_lossy(),
                    }),
                });
            }
        }
        Err(HarnessError::NotImplemented {
            reason: format!(
                "stream-json resume of {persistent_id} needs the SpawnCtx; \
                 caller must invoke start_thread (resume-aware via the \
                 deterministic per-sid uuid + --resume)"
            ),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let live = self.live.lock().unwrap().remove(&h.identity);
        if let Some(live) = live {
            live.transport.shutdown().await;
        }
        Ok(())
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        // Bridge gate (PRD E1): classify against the live init command
        // table. ccteam's own IM commands never reach here — the gateway
        // intercepts them before `handle_directive`.
        let commands = self
            .lookup(&h.identity)
            .map(|live| live.commands.clone())
            .unwrap_or_default();
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();
        // `/model <id>` IS driveable in stream-json — the TUI picker has no
        // headless form, but the SDK control channel does (`set_model`). Handle
        // it BEFORE the bridge gate so it never falls into a DIALOG reject or a
        // verbatim passthrough. Empty arg → a usage hint (no pane to open a
        // picker on). Real-vendor `set_model` support is confirmed at smoke; an
        // unsupported build returns an error subtype → an honest refusal here.
        // `/effort <level>` — set the reasoning effort. There is NO live
        // `set_effort` control, but `apply_flag_settings` merges `{effortLevel}`
        // into the runtime flagSettings layer and updates the active config
        // IMMEDIATELY — no restart, no context loss (the mechanism iOS/remote
        // clients use; verified empirically). Also persist to settings.local.json
        // so the level survives an idle-release / `--resume`.
        if name == "effort" {
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "effort: no live stream-json session for this handle".into(),
                ));
            };
            let Some(level) = normalize_effort(&d.args) else {
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!(
                        "用法: /effort <{}>（reasoning effort，live 生效）",
                        EFFORT_LEVELS.join("|")
                    ),
                });
            };
            return match apply_flag_settings_live(&live, json!({ "effortLevel": level })).await {
                Ok(()) => {
                    // Live now → persist for resume + reflect truthfully.
                    let _ = set_effort_level(&live.cwd, &level);
                    if let Ok(mut s) = live.status.lock() {
                        s.effort = Some(level.clone());
                    }
                    Ok(DirectiveOutcome::Done {
                        receipt: format!("已切换 effort → {level}（live 生效）"),
                    })
                }
                Err(why) => Ok(DirectiveOutcome::Rejected {
                    reason: format!("/effort 切换失败: {why}"),
                }),
            };
        } else if name == "set" {
            // Generic live-config escape hatch: `/set <key> <value>` merges one
            // setting into the active config via `apply_flag_settings` (the same
            // runtime-settings hook `/effort` uses). Value = JSON if it parses,
            // else a bare string. Fenced off the safety/HITL boundary.
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "set: no live stream-json session for this handle".into(),
                ));
            };
            let args = d.args.trim();
            let mut it = args.splitn(2, char::is_whitespace);
            let key = it.next().unwrap_or("").trim();
            let raw = it.next().unwrap_or("").trim();
            if key.is_empty() || raw.is_empty() {
                return Ok(DirectiveOutcome::Rejected {
                    reason: "用法: /set <settings-key> <value>（live 应用一个 Claude 设置，如 /set effortLevel xhigh）".into(),
                });
            }
            if SET_PROTECTED_KEYS.contains(&key) {
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!("/set 不允许改 `{key}`（安全/HITL 边界，受保护）"),
                });
            }
            let value: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
            return match apply_flag_settings_live(&live, json!({ key: value.clone() })).await {
                Ok(()) => Ok(DirectiveOutcome::Done {
                    receipt: format!("已 live 应用: {key} = {value}"),
                }),
                Err(why) => Ok(DirectiveOutcome::Rejected {
                    reason: format!("/set {key} 失败: {why}"),
                }),
            };
        } else if name == "model" {
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "set_model: no live stream-json session for this handle".into(),
                ));
            };
            // Resolve the effective `<model> [effort]` arg. Three forms collapse
            // here: (1) a picker re-entry — `d.choice` carries the picked option
            // id (`"<value> <effort>"` or bare `"<value>"`, built by
            // `claude_model_options`), which `split_model_effort` parses exactly;
            // (2) an explicit `/model <id> [effort]`; (3) a bare `/model` (no
            // args, no choice) → offer the picker built strictly from the REAL
            // captured model list. The gateway re-enters with the ORIGINAL
            // directive (name=model, args="") + `.choice` set, so a re-entry has
            // empty args but a present `choice`.
            let picked = d.choice.as_ref().and_then(|c| {
                c.ids
                    .first()
                    .cloned()
                    .or_else(|| c.free_text.clone().filter(|s| !s.trim().is_empty()))
            });
            let arg = match picked {
                Some(p) => p,
                None => d.args.trim().to_string(),
            };
            if arg.is_empty() {
                // Bare `/model` → a NeedsChoice picker (one option per
                // model×effort), built ONLY from the captured init.models. If the
                // list is empty (older claude that sent no `models`, or capture
                // failed) fall back to the usage-text rejection — never an empty
                // picker.
                let options = claude_model_options(&live.models);
                if options.is_empty() {
                    return Ok(DirectiveOutcome::Rejected {
                        reason:
                            "用法: /model <model-id> [effort]（直接给 model id；可选 effort=low|medium|high|xhigh|max，live 生效）"
                                .into(),
                    });
                }
                return Ok(DirectiveOutcome::NeedsChoice(claude_choice_prompt(
                    "Choose a model + reasoning effort:",
                    options,
                )));
            }
            // "<model> [effort]" — effort is the trailing token iff a valid level
            // (so `/model opus[1m] xhigh` splits but `/model opus-4-8` doesn't).
            let (model, effort) = split_model_effort(&arg);
            let body = live
                .transport
                .request_control("set_model", json!({ "model": model }), init_timeout())
                .await
                .map_err(|e| HarnessError::SubmitFailed(format!("set_model 失败: {e}")))?;
            if body.subtype != "success" {
                let why = body
                    .error
                    .unwrap_or_else(|| "vendor rejected set_model".into());
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!("/model 切换失败: {why}"),
                });
            }
            // v0.8.19 — persist the live /model to status.json IMMEDIATELY so it
            // survives a daemon restart even before the next turn's status-tap
            // write (else resume reads the stale prior model). Resume strips the
            // `[1m]` for `--model` + re-requests 1M via set_model, so persisting
            // the user-typed form here is safe.
            let model_snap = if let Ok(mut s) = live.status.lock() {
                s.model = Some(model.clone());
                Some(s.clone())
            } else {
                None
            };
            if let Some(snap) = model_snap {
                write_status_file(&live.cwd, &live.identity.sid, &snap);
            }
            // Optional effort rides along — now also LIVE via apply_flag_settings.
            let mut receipt = format!("已切换 model → {model}（live）");
            if let Some(level) = effort {
                match apply_flag_settings_live(&live, json!({ "effortLevel": level })).await {
                    Ok(()) => {
                        let _ = set_effort_level(&live.cwd, &level);
                        if let Ok(mut s) = live.status.lock() {
                            s.effort = Some(level.clone());
                        }
                        receipt.push_str(&format!("；effort → {level}（live）"));
                    }
                    Err(why) => receipt.push_str(&format!("；effort 切换失败: {why}")),
                }
            }
            return Ok(DirectiveOutcome::Done { receipt });
        }
        match bridge::classify_slash(&name, &commands) {
            SlashClass::Reject => Ok(DirectiveOutcome::Rejected {
                reason: bridge::reject_reason(&name),
            }),
            SlashClass::Passthrough => {
                // Known prompt/local (incl. /compact /clear /context) OR
                // unknown → forward verbatim as user text.
                let line = if d.args.trim().is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {}", d.args.trim())
                };
                let turn = self.submit_turn(h, TurnInput::UserText(line)).await?;
                Ok(DirectiveOutcome::Turn(turn))
            }
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // Live model + context-window usage, kept current by the per-session
        // status tap ([`spawn_status_tap`]) folding each turn's `usage`. A live
        // session WITH context is authoritative; otherwise fall back to the
        // persisted snapshot ([`status_json_path`]) so a released / resumed
        // session (idle-release, daemon restart — spawn-on-demand) still shows
        // its statusline, the same durability the TUI gets from the transcript.
        let live = self
            .lookup(&h.identity)
            .map(|l| l.status.lock().unwrap().clone());
        let live_ready = live.as_ref().map(|s| s.context.is_some()).unwrap_or(false);
        let mut status = if live_ready {
            live.unwrap()
        } else {
            // `read_status_file` is sync fs — off the runtime (see the goal read
            // below for why this matters on this code path).
            let persisted = match h
                .raw_extras
                .get("project_dir")
                .and_then(|v| v.as_str())
                .zip(h.raw_extras.get("sid").and_then(|v| v.as_str()))
            {
                Some((pd, sid)) => {
                    let (pd, sid) = (PathBuf::from(pd), sid.to_string());
                    tokio::task::spawn_blocking(move || read_status_file(&pd, &sid))
                        .await
                        .unwrap_or(None)
                }
                None => None,
            };
            match (live, persisted) {
                // Live (model from init, no turn yet) + persisted context → show
                // the live model with the last-known context.
                (Some(l), Some(p)) => ThreadStatus {
                    model: l.model.or(p.model),
                    context: p.context,
                    effort: l.effort.or(p.effort),
                    goal: None,
                },
                (Some(l), None) => l,
                (None, Some(p)) => p,
                (None, None) => ThreadStatus::default(),
            }
        };
        // The `/goal` is NOT on the stream-json stream and has no control_request
        // (probed) — read the current one from the transcript (cwd + the vendor
        // uuid name the file). Overrides any goal from a persisted snapshot.
        //
        // OFF THE RUNTIME (2026-08-02): this is SYNC fs — it seeks and reads up
        // to 8 MB, then scans it. Doing that inside an `async fn` parks a Tokio
        // worker thread, and this call is fanned out per live session by the
        // team graph, so enough live sessions starve the runtime and EVERY
        // endpoint the daemon serves (plus every vendor event pump) stalls
        // behind it. `spawn_blocking` keeps the cost on the blocking pool where
        // it belongs.
        if let Some(cwd) = h.raw_extras.get("cwd").and_then(|v| v.as_str()) {
            let (cwd, uuid) = (PathBuf::from(cwd), h.identity.clone());
            status.goal = tokio::task::spawn_blocking(move || read_latest_goal_status(&cwd, &uuid))
                .await
                .unwrap_or(None);
        }
        Ok(status)
    }

    async fn account_usage(&self, h: &ThreadHandle) -> Option<crate::AccountUsage> {
        // Account-level usage (5h / weekly / credits) — query `get_usage` on this
        // live session's transport. It is account-scoped, so the gateway calls it
        // on any ONE live claude session to build the `/status` header.
        let live = self.lookup(&h.identity)?;
        get_account_usage(&live.transport).await
    }

    async fn running_tasks(&self, h: &ThreadHandle) -> Vec<crate::RunningTask> {
        // The session's currently-running subagent/workflow tasks, reflected
        // from claude's `system:task_*` lifecycle by the status tap. A snapshot
        // clone (cheap; the list is a handful of entries at most).
        match self.lookup(&h.identity) {
            Some(live) => live
                .running_tasks
                .lock()
                .map(|t| t.clone())
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Interrupt the in-flight turn via the bidirectional `interrupt`
    /// control_request. Because the transport is full-duplex NDJSON, this line
    /// is written to claude's stdin and answered OUT-OF-BAND — it reaches
    /// claude even while a turn is streaming tools, which is the whole point
    /// (the interrupt must NOT queue behind the running turn). The session is
    /// left fully live: no `close_thread`, no map removal, no pump abort — only
    /// the current turn stops, so a following `/model` etc. still works on the
    /// same context.
    async fn interrupt_turn(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let Some(live) = self.lookup(&h.identity) else {
            return Err(HarnessError::SubmitFailed(format!(
                "interrupt: no live stream-json session for {} (nothing to interrupt)",
                h.identity
            )));
        };
        let body = live
            .transport
            .request_control("interrupt", json!({}), init_timeout())
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("interrupt control_request: {e:#}")))?;
        if body.subtype != "success" {
            return Err(HarnessError::SubmitFailed(format!(
                "interrupt rejected: {}",
                body.error.unwrap_or_else(|| body.subtype.clone())
            )));
        }
        Ok(())
    }

    /// Claude's title surface is its transcript's `custom-title` entry (the
    /// SDK `renameSession` contract), NOT a control_request — the stream-json
    /// control channel has no title subtype. Writing the file also means a
    /// rename works on a stopped session, so the whole push lives in the
    /// shared vendor helper; the live map only supplies a fresher uuid.
    async fn set_session_title(
        &self,
        target: &crate::SessionTitleTarget,
        title: &str,
    ) -> Result<crate::TitleSync, HarnessError> {
        let mut target = target.clone();
        if let Some(live) = target
            .thread
            .as_ref()
            .and_then(|t| self.lookup(&t.identity))
        {
            target.vendor_uuid = live.identity.vendor_uuid.clone();
        }
        Ok(crate::execution::vendor_title::push_claude_custom_title(
            &target, title,
        ))
    }
}

#[cfg(test)]
mod effort_tests {
    use super::protocol::SystemMsg;
    use super::{
        claude_model_options, is_model_placeholder, normalize_effort, parse_latest_goal_status,
        persisted_session_model, preserve_1m_tag, reflect_task_event, set_effort_level,
        split_model_effort, task_outlives_turn, write_status_file, ClaudeModelOption,
        EFFORT_LEVELS,
    };
    use crate::ThreadStatus;
    use std::sync::Mutex;

    /// `"default"` (claude's own picker-menu label, any case/whitespace) is a
    /// placeholder, not a real model id; every other string — including a
    /// concrete id that merely CONTAINS "default" as a substring — is not.
    #[test]
    fn is_model_placeholder_matches_only_the_bare_default_label() {
        assert!(is_model_placeholder("default"));
        assert!(is_model_placeholder("Default"));
        assert!(is_model_placeholder("  default  "));
        assert!(!is_model_placeholder("claude-opus-4-8[1m]"));
        assert!(!is_model_placeholder("default-ish"));
        assert!(!is_model_placeholder(""));
    }

    /// THE FIX — `persisted_session_model` must never resolve to claude's own
    /// unresolved "default" placeholder (defense-in-depth for the spawn-time
    /// seed filter): a `status.json` snapshot recording it reads back as
    /// `None`, exactly like the existing `<synthetic>` placeholder guard, so
    /// neither can reach a real `--model` respawn request.
    #[test]
    fn persisted_session_model_rejects_the_default_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_status_file(
            dir.path(),
            "s1",
            &ThreadStatus {
                model: Some("default".to_string()),
                context: None,
                effort: None,
                goal: None,
            },
        );
        assert_eq!(persisted_session_model(dir.path(), "s1"), None);

        // A concrete id round-trips normally.
        write_status_file(
            dir.path(),
            "s2",
            &ThreadStatus {
                model: Some("claude-opus-4-8[1m]".to_string()),
                context: None,
                effort: None,
                goal: None,
            },
        );
        assert_eq!(
            persisted_session_model(dir.path(), "s2"),
            Some("claude-opus-4-8[1m]".to_string())
        );
    }

    /// Build a `system:task_*` line with the given subtype + task id.
    fn task_sys(subtype: &str, task_id: &str) -> SystemMsg {
        SystemMsg {
            subtype: subtype.to_string(),
            task_id: task_id.to_string(),
            ..Default::default()
        }
    }

    /// `/status` running-subagent tracking REFLECTS claude's own task lifecycle
    /// (never folds): `task_started` adds, a terminal `task_updated`/
    /// `task_notification` removes, duplicates are idempotent, non-task system
    /// lines are ignored.
    #[test]
    fn reflect_task_event_mirrors_claude_task_lifecycle() {
        let tasks = Mutex::new(Vec::new());
        // task_started adds, carrying kind/description from the event.
        let mut started = task_sys("task_started", "t1");
        started.subagent_type = "code-reviewer".into();
        started.description = "review auth".into();
        started.task_type = "local_agent".into();
        reflect_task_event(&tasks, &started);
        // A duplicate task_started for the same id must NOT double-insert.
        reflect_task_event(&tasks, &task_sys("task_started", "t1"));
        {
            let g = tasks.lock().unwrap();
            assert_eq!(g.len(), 1);
            assert_eq!(g[0].kind, "code-reviewer");
            assert_eq!(g[0].description, "review auth");
            assert_eq!(g[0].task_type, "local_agent");
        }
        // A second task.
        reflect_task_event(&tasks, &task_sys("task_started", "t2"));
        assert_eq!(tasks.lock().unwrap().len(), 2);
        // A NON-terminal patch keeps the task running.
        let mut running = task_sys("task_updated", "t1");
        running.patch = Some(serde_json::json!({"status": "in_progress"}));
        reflect_task_event(&tasks, &running);
        assert_eq!(
            tasks.lock().unwrap().len(),
            2,
            "non-terminal patch keeps it"
        );
        // A terminal patch removes t1; t2 remains.
        let mut done = task_sys("task_updated", "t1");
        done.patch = Some(serde_json::json!({"status": "completed", "end_time": 1}));
        reflect_task_event(&tasks, &done);
        {
            let g = tasks.lock().unwrap();
            assert_eq!(g.len(), 1, "completed patch removes t1");
            assert_eq!(g[0].task_id, "t2");
        }
        // task_notification{status:completed} removes t2.
        let mut note = task_sys("task_notification", "t2");
        note.status = "completed".into();
        reflect_task_event(&tasks, &note);
        assert!(tasks.lock().unwrap().is_empty());
        // A non-task system line (init) is ignored (no panic, no insert).
        reflect_task_event(&tasks, &task_sys("init", ""));
        assert!(tasks.lock().unwrap().is_empty());
    }

    /// The turn-end safety net evicts turn-scoped tasks (subagents) but keeps
    /// background workflows (`local_workflow`), which outlive the spawning
    /// turn; the workflow still leaves the list on its OWN terminal event.
    #[test]
    fn turn_end_safety_net_keeps_background_workflows() {
        let tasks = Mutex::new(Vec::new());
        let mut agent = task_sys("task_started", "a1");
        agent.subagent_type = "general-purpose".into();
        agent.task_type = "local_agent".into();
        reflect_task_event(&tasks, &agent);
        let mut wf = task_sys("task_started", "w1");
        wf.description = "audit the codebase".into();
        wf.task_type = "local_workflow".into();
        reflect_task_event(&tasks, &wf);
        // What the TurnResult arm applies: retain only tasks that outlive a turn.
        tasks.lock().unwrap().retain(task_outlives_turn);
        {
            let g = tasks.lock().unwrap();
            assert_eq!(g.len(), 1, "subagent evicted, workflow retained");
            assert_eq!(g[0].task_id, "w1");
        }
        // The workflow's own terminal notification still removes it.
        let mut note = task_sys("task_notification", "w1");
        note.status = "completed".into();
        reflect_task_event(&tasks, &note);
        assert!(tasks.lock().unwrap().is_empty());
    }

    /// Background SHELLS (`local_bash` — Bash `run_in_background` + Monitor
    /// watches; probed live 2026-07-22) outlive the spawning turn exactly like
    /// workflows: the turn ends, the task keeps running, and its own terminal
    /// event arrives later. Before this vocabulary fix the turn-end net
    /// evicted them, so an idle session showed no trace of an in-flight
    /// `make test` — the exact observability gap reported from the field.
    #[test]
    fn turn_end_safety_net_keeps_background_shells() {
        let tasks = Mutex::new(Vec::new());
        let mut sh = task_sys("task_started", "b1");
        sh.description = "make test".into();
        sh.task_type = "local_bash".into();
        reflect_task_event(&tasks, &sh);
        // What the TurnResult arm applies at turn end.
        tasks.lock().unwrap().retain(task_outlives_turn);
        {
            let g = tasks.lock().unwrap();
            assert_eq!(g.len(), 1, "background shell survives turn end");
            assert_eq!(g[0].task_id, "b1");
        }
        // Its own terminal notification still removes it.
        let mut note = task_sys("task_notification", "b1");
        note.status = "completed".into();
        reflect_task_event(&tasks, &note);
        assert!(tasks.lock().unwrap().is_empty());
    }

    /// TaskStop closes a task with `task_notification{status:"stopped"}`, and
    /// `task_updated` patches carry the internal terminal state `killed`. Both
    /// must evict: a background workflow survives turn-end by design, so
    /// missing either kill-word left a TaskStop'd workflow displayed as
    /// "running" forever (observed as a `/status` zombie aging past 14h).
    #[test]
    fn stopped_and_killed_are_terminal_statuses() {
        let tasks = Mutex::new(Vec::new());
        let mut wf = task_sys("task_started", "w1");
        wf.task_type = "local_workflow".into();
        reflect_task_event(&tasks, &wf);
        let mut note = task_sys("task_notification", "w1");
        note.status = "stopped".into();
        reflect_task_event(&tasks, &note);
        assert!(
            tasks.lock().unwrap().is_empty(),
            "task_notification stopped must evict"
        );
        let mut wf2 = task_sys("task_started", "w2");
        wf2.task_type = "local_workflow".into();
        reflect_task_event(&tasks, &wf2);
        let mut patch = task_sys("task_updated", "w2");
        patch.patch = Some(serde_json::json!({"status": "killed"}));
        reflect_task_event(&tasks, &patch);
        assert!(
            tasks.lock().unwrap().is_empty(),
            "task_updated killed must evict"
        );
    }

    #[test]
    fn parse_latest_goal_status_takes_the_last_record_and_honors_clear() {
        // No goal record → None.
        assert!(parse_latest_goal_status(r#"{"type":"user"}"#).is_none());
        // One active goal.
        let one = r#"{"type":"assistant"}
{"type":"attachment","attachment":{"type":"goal_status","met":false,"condition":"ship payments"}}"#;
        let g = parse_latest_goal_status(one).unwrap();
        assert_eq!(g.condition, "ship payments");
        assert!(!g.met);
        // The LAST record wins (met flips true).
        let two = format!(
            "{one}\n{}",
            r#"{"type":"attachment","attachment":{"type":"goal_status","met":true,"condition":"ship payments"}}"#
        );
        assert!(parse_latest_goal_status(&two).unwrap().met);
        // A trailing clear (empty condition) → no active goal.
        let cleared = format!(
            "{one}\n{}",
            r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"condition":""}}"#
        );
        assert!(parse_latest_goal_status(&cleared).is_none());
        // Half-flushed / non-JSON lines are skipped, not fatal.
        assert!(parse_latest_goal_status("{partial\n{\"x\":1}").is_none());
    }

    #[test]
    fn preserve_1m_tag_carries_user_intent_without_inventing_it() {
        // User set `opus[1m]`; the API id is bare → carry the tag over (so the
        // window heuristic keeps 1M and the statusline keeps showing [1m]).
        assert_eq!(
            preserve_1m_tag(Some("opus[1m]"), "claude-opus-4-8"),
            "claude-opus-4-8[1m]"
        );
        assert_eq!(
            preserve_1m_tag(Some("claude-opus-4-8[1m]"), "claude-opus-4-8"),
            "claude-opus-4-8[1m]"
        );
        // No [1m] in the current model → never invent one (a 200k model stays 200k).
        assert_eq!(
            preserve_1m_tag(Some("claude-sonnet-4-6"), "claude-sonnet-4-6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(preserve_1m_tag(None, "claude-opus-4-8"), "claude-opus-4-8");
        // API id already carries [1m] → don't double it.
        assert_eq!(
            preserve_1m_tag(Some("opus[1m]"), "claude-opus-4-8[1m]"),
            "claude-opus-4-8[1m]"
        );
    }

    #[test]
    fn normalize_effort_accepts_levels_case_insensitively_and_rejects_others() {
        for lvl in EFFORT_LEVELS {
            assert_eq!(normalize_effort(lvl).as_deref(), Some(*lvl));
        }
        assert_eq!(normalize_effort("XHigh").as_deref(), Some("xhigh"));
        assert_eq!(normalize_effort(" max ").as_deref(), Some("max"));
        assert_eq!(normalize_effort("turbo"), None);
        assert_eq!(normalize_effort("opus-4-8"), None);
        assert_eq!(normalize_effort(""), None);
    }

    #[test]
    fn split_model_effort_only_peels_a_valid_trailing_level() {
        assert_eq!(
            split_model_effort("opus[1m] xhigh"),
            ("opus[1m]".to_string(), Some("xhigh".to_string()))
        );
        assert_eq!(
            split_model_effort("claude-opus-4-8 max"),
            ("claude-opus-4-8".to_string(), Some("max".to_string()))
        );
        // No trailing level → the whole arg is the model (never mis-split).
        assert_eq!(
            split_model_effort("claude-opus-4-8"),
            ("claude-opus-4-8".to_string(), None)
        );
        assert_eq!(
            split_model_effort("opus[1m]"),
            ("opus[1m]".to_string(), None)
        );
    }

    #[test]
    fn claude_model_options_builds_picker_from_real_model_list() {
        // A realistic init.models slice (claude 2.1.187 live values): models
        // WITH an effort axis fan out to one option per (value, effort) with
        // `id = "<value> <effort>"`; haiku (no efforts) yields a single bare-id
        // option. The id is EXACTLY the `/model <id> [effort]` arg form
        // `split_model_effort` parses on the picker re-entry.
        let models = vec![
            ClaudeModelOption {
                value: "default".to_string(),
                display_name: Some("Default".to_string()),
                efforts: ["low", "medium", "high", "xhigh", "max"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
            ClaudeModelOption {
                value: "opus[1m]".to_string(),
                display_name: None,
                efforts: vec!["low".to_string(), "max".to_string()],
            },
            ClaudeModelOption {
                value: "haiku".to_string(),
                display_name: None,
                efforts: vec![],
            },
        ];
        let opts = claude_model_options(&models);
        // 5 (default) + 2 (opus[1m]) + 1 (haiku) = 8 options, in source order.
        assert_eq!(opts.len(), 8);

        // First effort-bearing model: `<value> <effort>` id + `<value> (<effort>)` label.
        assert_eq!(opts[0].id, "default low");
        assert_eq!(opts[0].label, "default (low)");
        assert_eq!(opts[4].id, "default max");

        // opus[1m] keeps its `[1m]` tag verbatim in the id (set_model accepts it).
        assert_eq!(opts[5].id, "opus[1m] low");
        assert_eq!(opts[6].id, "opus[1m] max");
        assert_eq!(opts[6].label, "opus[1m] (max)");

        // haiku has NO effort → a single bare-id option (id == label == value),
        // so the re-entry arg is just `haiku` (split_model_effort → (haiku, None)).
        assert_eq!(opts[7].id, "haiku");
        assert_eq!(opts[7].label, "haiku");

        // Every effort-bearing id round-trips through split_model_effort back to
        // its (value, effort) — the contract the set_model arm relies on.
        assert_eq!(
            split_model_effort(&opts[6].id),
            ("opus[1m]".to_string(), Some("max".to_string()))
        );

        // Empty model list → no options (the arm then falls back to usage text).
        assert!(claude_model_options(&[]).is_empty());
    }

    #[test]
    fn set_effort_level_writes_effortlevel_preserving_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        // Pre-existing sibling key must survive the effortLevel write.
        std::fs::write(&settings, r#"{"enabledPlugins":{"x@y":true}}"#).unwrap();

        set_effort_level(dir.path(), "xhigh").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["effortLevel"], "xhigh");
        assert_eq!(v["enabledPlugins"]["x@y"], true, "sibling preserved");

        // Idempotent overwrite of the same key.
        set_effort_level(dir.path(), "high").unwrap();
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v2["effortLevel"], "high");
        assert_eq!(v2["enabledPlugins"]["x@y"], true);
    }

    #[test]
    fn set_effort_level_creates_settings_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        set_effort_level(dir.path(), "medium").unwrap();
        let body = std::fs::read_to_string(dir.path().join(".claude").join("settings.local.json"))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["effortLevel"], "medium");
    }
}

#[cfg(test)]
mod effort_shape_tests {
    use super::extract_effort_from_settings;
    use serde_json::json;

    /// The `get_settings` effort location has moved across CLI versions; the
    /// extractor must read every known shape. The `effective.effortLevel`
    /// fixture is a captured wire response from the live 2.1.212 CLI (the
    /// regression: `/status` showed `—` for effort because only the retired
    /// `applied.effort` path was read).
    #[test]
    fn extracts_effort_from_old_and_new_settings_shapes() {
        // Pre-2.1.2xx: applied.effort.
        let old = json!({"applied": {"effort": "high"}});
        assert_eq!(extract_effort_from_settings(&old).as_deref(), Some("high"));

        // 2.1.212: effective.effortLevel (captured shape; sibling keys elided).
        let new = json!({"effective": {"model": "claude-fable-5[1m]", "effortLevel": "xhigh"}});
        assert_eq!(extract_effort_from_settings(&new).as_deref(), Some("xhigh"));

        // Defensive: effective.effort.
        let alt = json!({"effective": {"effort": "low"}});
        assert_eq!(extract_effort_from_settings(&alt).as_deref(), Some("low"));

        // Absent / empty → None (statusline shows `—`, never fabricated).
        assert_eq!(extract_effort_from_settings(&json!({})), None);
        assert_eq!(
            extract_effort_from_settings(&json!({"effective": {"effortLevel": "  "}})),
            None
        );
    }
}
