//! Final-only translate: ACP notifications + prompt response → ThreadEvent.
//!
//! Turn-end SoT = matching `session/prompt` JSON-RPC **response** (not
//! `turn_completed` notifications). Buffer only `agent_message_chunk`;
//! drop thoughts and `isReplay` frames from the **final answer**, but emit
//! throttled mid-stream liveness events so the gateway silence watchdog
//! sees that a long think / long draft is still alive (ACP vendors otherwise
//! surface zero `ThreadEvent`s between tool calls and turn end).

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Notify;

use super::protocol::{
    content_text, cost_from_usage_update, is_replay, is_turn_boundary, usage_from_prompt_result,
    AvailableCommand,
};
use super::transport::Notification;
use crate::{ThreadErrorEvent, ThreadEvent, ThreadItem, ThreadItemDetails};

/// Min gap between liveness `ThreadEvent`s for message/thought chunks.
/// Chunks arrive many times per second; the watchdog only needs a periodic
/// pulse. First chunk of a streak always emits (interval elapsed).
const LIVENESS_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// Per-turn buffer while a prompt is in flight.
#[derive(Debug, Clone, Default)]
pub struct TurnBuffer {
    pub turn_id: String,
    pub text: String,
}

/// Shared live state for one ACP session.
#[derive(Debug, Default)]
pub struct SessionTranslateState {
    pub buffer: Option<TurnBuffer>,
    /// Turns steered in while `buffer` is occupied, `(turn_id, text)`, FIFO.
    /// ACP `session/prompt` is a single-turn RPC and `buffer` holds exactly one
    /// turn, so a second concurrent prompt would clobber it. The gateway (like
    /// Claude/Codex) fires a fresh `submit_turn` the instant a user steers
    /// mid-turn; instead of hard-rejecting, we queue here and the turn runner
    /// drains FIFO — matching Grok/OpenCode native prompt-queue semantics.
    pub pending: VecDeque<(String, String)>,
    /// Fired once by the dispatcher when it processes this turn's boundary
    /// (`turn_completed` / `prompt_complete`), which is FIFO-ordered after
    /// every `agent_message_chunk`. The submit task awaits this before
    /// finalizing so no trailing chunk is lost to the buffer/finalize race.
    pub turn_done: Option<Arc<Notify>>,
    pub available_commands: Vec<AvailableCommand>,
    pub model: Option<String>,
    pub window_tokens: Option<u64>,
    pub used_tokens: Option<u64>,
    pub effort: Option<String>,
    /// Session-cumulative USD from the latest non-zero `usage_update.cost`.
    pub session_cost_usd: Option<f64>,
    /// Previous session-cumulative USD (for per-turn delta).
    pub prev_session_cost_usd: Option<f64>,
    /// Methods we already warn-once skipped.
    pub warned_methods: HashSet<String>,
    /// Last time we emitted a message/thought liveness event (throttled).
    /// `pub` so vendor adapters can construct the state with struct update
    /// syntax (`..Default::default()`); not part of the public API surface.
    pub last_liveness_at: Option<Instant>,
}

impl SessionTranslateState {
    pub fn begin_turn(&mut self, turn_id: impl Into<String>, done: Arc<Notify>) {
        self.buffer = Some(TurnBuffer {
            turn_id: turn_id.into(),
            text: String::new(),
        });
        self.turn_done = Some(done);
        // Fresh turn → first thought/message chunk should emit immediately.
        self.last_liveness_at = None;
    }

    pub fn append_message(&mut self, chunk: &str) {
        if let Some(b) = self.buffer.as_mut() {
            b.text.push_str(chunk);
        }
    }

    /// Signal (once) that the turn boundary was reached.
    fn signal_turn_done(&mut self) {
        if let Some(done) = self.turn_done.take() {
            done.notify_one();
        }
    }

    pub fn take_buffer(&mut self) -> Option<TurnBuffer> {
        self.buffer.take()
    }
}

/// Apply one notification. Returns optional mid-stream events (tools).
/// Does **not** emit final agent message — that waits for the prompt response.
pub fn apply_notification(state: &mut SessionTranslateState, n: &Notification) -> Vec<ThreadEvent> {
    // Drop full replay frames from session/load (isReplay covers top-level and
    // nested `update._meta.isReplay`).
    if is_replay(&n.params) {
        return Vec::new();
    }

    // Turn boundary (FIFO-ordered after every chunk) → release the finalize
    // barrier. Must run before the vendor-noise skip below.
    if is_turn_boundary(&n.method, &n.params) {
        state.signal_turn_done();
        return Vec::new();
    }

    // Standard session/update path (`session/update` or `_x.ai/session/update`).
    if n.method == "session/update" || n.method.ends_with("session/update") {
        return apply_session_update(state, &n.params);
    }

    // Grok live model switch: `_x.ai/session_notification` with
    // `sessionUpdate: model_changed` (also after `session/set_model`).
    if n.method.ends_with("session_notification") {
        apply_model_changed(state, &n.params);
        return Vec::new();
    }

    // Everything else (`_x.ai/*` push noise, unknown methods): warn once, skip.
    if state.warned_methods.insert(n.method.clone()) {
        tracing::warn!(
            method = %n.method,
            "acp: skipping unknown notification method (warn-once)"
        );
    }
    Vec::new()
}

/// Apply `model_changed` from a session_notification / session/update payload.
fn apply_model_changed(state: &mut SessionTranslateState, params: &Value) {
    let update = params
        .get("update")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if kind != "model_changed" {
        return;
    }
    if let Some(model) = update
        .get("model_id")
        .or_else(|| update.get("modelId"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        state.model = Some(model.to_string());
    }
    if let Some(effort) = update
        .get("reasoning_effort")
        .or_else(|| update.get("reasoningEffort"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        state.effort = Some(effort.to_string());
    }
}

fn apply_session_update(state: &mut SessionTranslateState, params: &Value) -> Vec<ThreadEvent> {
    let update = params
        .get("update")
        .cloned()
        .unwrap_or_else(|| params.clone());
    // session update type may be `sessionUpdate` or `type`.
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match kind {
        "agent_message_chunk" => {
            let text = extract_chunk_text(&update).unwrap_or_default();
            if !text.is_empty() {
                state.append_message(&text);
            }
            // Final answer still waits for the prompt response (buffer only).
            // Emit a throttled ItemUpdated so the gateway activity counter
            // moves during long drafts — without this, a multi-minute pure
            // generation looks "silent" and the 300s watchdog false-alarms.
            maybe_liveness_event(state, "msg", ThreadItemDetails::AgentMessage(text))
        }
        "agent_thought_chunk" => {
            // Thoughts never enter final text; still pulse liveness (Grok can
            // think for many minutes with zero tool calls).
            let text = extract_chunk_text(&update).unwrap_or_default();
            maybe_liveness_event(state, "thought", ThreadItemDetails::Reasoning(text))
        }
        "user_message_chunk" => {
            // User echo ignored (and not a liveness signal for the agent turn).
            Vec::new()
        }
        "available_commands_update" => {
            if let Some(cmds) = update.get("availableCommands").and_then(|v| v.as_array()) {
                state.available_commands = cmds
                    .iter()
                    .filter_map(|c| {
                        let name = c.get("name")?.as_str()?.to_string();
                        Some(AvailableCommand {
                            name,
                            description: c
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string(),
                            input: c.get("input").cloned(),
                        })
                    })
                    .collect();
            }
            Vec::new()
        }
        "tool_call" | "tool_call_update" => {
            // Optional progress items; do not affect final text.
            let name = update
                .get("title")
                .or_else(|| update.get("toolName"))
                .or_else(|| update.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();
            let id = update
                .get("toolCallId")
                .or_else(|| update.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();
            let status = update.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let item = ThreadItem {
                id,
                details: ThreadItemDetails::ToolCall {
                    name,
                    args: update.get("rawInput").cloned().unwrap_or(Value::Null),
                },
            };
            if kind == "tool_call" && status != "completed" {
                vec![ThreadEvent::ItemStarted { item }]
            } else if status == "completed" {
                vec![ThreadEvent::ItemCompleted { item }]
            } else {
                vec![ThreadEvent::ItemUpdated { item }]
            }
        }
        // OpenCode: context occupancy + session-cumulative USD.
        "usage_update" => {
            if let Some(used) = update.get("used").and_then(|v| v.as_u64()) {
                state.used_tokens = Some(used);
            }
            if let Some(size) = update.get("size").and_then(|v| v.as_u64()) {
                state.window_tokens = Some(size);
            }
            if let Some(amount) = cost_from_usage_update(&update) {
                // Keep previous so finalize can delta for per-turn cost.
                state.prev_session_cost_usd = state.session_cost_usd;
                state.session_cost_usd = Some(amount);
            }
            Vec::new()
        }
        "model_changed" => {
            apply_model_changed(state, params);
            Vec::new()
        }
        "plan" | "current_mode_update" | "config_option_update" | "session_info_update" => {
            Vec::new()
        }
        _ => {
            if !kind.is_empty() && state.warned_methods.insert(format!("update:{kind}")) {
                tracing::warn!(kind, "acp: skipping unknown sessionUpdate kind (warn-once)");
            }
            Vec::new()
        }
    }
}

fn extract_chunk_text(update: &Value) -> Option<String> {
    if let Some(content) = update.get("content") {
        return content_text(content);
    }
    // Some wires put text at top level.
    update
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// Throttled mid-stream pulse for the silence watchdog / progress fold.
/// Does **not** affect the final answer (still buffer + prompt response).
fn maybe_liveness_event(
    state: &mut SessionTranslateState,
    kind: &str,
    details: ThreadItemDetails,
) -> Vec<ThreadEvent> {
    let now = Instant::now();
    if state
        .last_liveness_at
        .is_some_and(|at| now.duration_since(at) < LIVENESS_MIN_INTERVAL)
    {
        return Vec::new();
    }
    state.last_liveness_at = Some(now);
    let turn_id = state
        .buffer
        .as_ref()
        .map(|b| b.turn_id.as_str())
        .unwrap_or("pending");
    vec![ThreadEvent::ItemUpdated {
        item: ThreadItem {
            // Stable per-kind id so ProgressFold updates one card, not a flood.
            id: format!("{turn_id}-live-{kind}"),
            details,
        },
    }]
}

/// Finalize a turn from the `session/prompt` response (authoritative).
pub fn finalize_from_prompt_result(
    state: &mut SessionTranslateState,
    result: &Value,
) -> Vec<ThreadEvent> {
    let (mut usage, model) = usage_from_prompt_result(result);
    // OpenCode: attach per-turn reported USD as delta of session-cumulative
    // cost from usage_update (or full amount on first turn). Zero/missing → None.
    if let Some(curr) = state.session_cost_usd {
        let prev = state.prev_session_cost_usd.unwrap_or(0.0);
        let delta = curr - prev;
        if delta > 0.0 {
            usage.reported_cost_usd = Some(delta);
        } else if curr > 0.0 && state.prev_session_cost_usd.is_none() {
            usage.reported_cost_usd = Some(curr);
        }
        // Advance baseline so the next turn deltas correctly.
        state.prev_session_cost_usd = Some(curr);
    }
    if let Some(m) = model.clone() {
        state.model = Some(m);
    }
    if let Some(total) = result
        .pointer("/_meta/totalTokens")
        .or_else(|| result.pointer("/usage/totalTokens"))
        .and_then(|v| v.as_u64())
    {
        state.used_tokens = Some(total);
    }
    // Signal barrier if turn_completed never arrived (OpenCode has no such notif).
    state.signal_turn_done();
    let buf = state.take_buffer();
    let turn_id = buf
        .as_ref()
        .map(|b| b.turn_id.clone())
        .unwrap_or_else(|| "unknown".into());
    let text = buf.map(|b| b.text).unwrap_or_default();
    let mut out = Vec::new();
    out.push(ThreadEvent::ItemCompleted {
        item: ThreadItem {
            id: format!("{turn_id}-msg"),
            details: ThreadItemDetails::AgentMessage(text),
        },
    });
    out.push(ThreadEvent::TurnCompleted {
        turn_id,
        usage,
        model: model.or_else(|| state.model.clone()),
    });
    out
}

/// Fail mid-turn (child death / RPC error).
pub fn fail_turn(state: &mut SessionTranslateState, message: &str) -> Vec<ThreadEvent> {
    let buf = state.take_buffer();
    let turn_id = buf.map(|b| b.turn_id).unwrap_or_else(|| "unknown".into());
    vec![ThreadEvent::TurnFailed {
        turn_id,
        err: ThreadErrorEvent {
            kind: "transport".into(),
            message: message.to_string(),
        },
    }]
}

/// Helper: lock + apply notification into shared state, returning events.
pub fn apply_notification_shared(
    state: &Arc<StdMutex<SessionTranslateState>>,
    n: &Notification,
) -> Vec<ThreadEvent> {
    let Ok(mut guard) = state.lock() else {
        return Vec::new();
    };
    apply_notification(&mut guard, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use serde_json::json;

    #[test]
    fn buffers_message_not_thought_and_finalizes_once() {
        let mut st = SessionTranslateState::default();
        st.begin_turn("t1", Arc::new(Notify::new()));
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type":"text","text":"thinking..."}
                    }
                }),
            },
        );
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"Hello "}
                    }
                }),
            },
        );
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"world"}
                    }
                }),
            },
        );
        // Replay must not pollute.
        apply_notification(
            &mut st,
            &Notification {
                method: "_x.ai/session/update".into(),
                params: json!({
                    "_meta": {"isReplay": true},
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"REPLAY"}
                    }
                }),
            },
        );
        let events = finalize_from_prompt_result(
            &mut st,
            &json!({
                "stopReason":"end_turn",
                "_meta":{"inputTokens":10,"outputTokens":2,"cachedReadTokens":0,"reasoningTokens":1,"modelId":"grok-4.5","totalTokens":12}
            }),
        );
        assert_eq!(events.len(), 2);
        match &events[0] {
            ThreadEvent::ItemCompleted { item } => match &item.details {
                ThreadItemDetails::AgentMessage(t) => assert_eq!(t, "Hello world"),
                other => panic!("unexpected {other:?}"),
            },
            other => panic!("unexpected {other:?}"),
        }
        match &events[1] {
            ThreadEvent::TurnCompleted { usage, model, .. } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(usage.reasoning_output_tokens, Some(1));
                assert_eq!(model.as_deref(), Some("grok-4.5"));
            }
            other => panic!("unexpected {other:?}"),
        }
        // usage type sanity
        let _: crate::UnifiedTokenUsage = match &events[1] {
            ThreadEvent::TurnCompleted { usage, .. } => *usage,
            _ => unreachable!(),
        };
    }

    #[test]
    fn message_and_thought_chunks_emit_throttled_liveness_events() {
        let mut st = SessionTranslateState::default();
        st.begin_turn("t1", Arc::new(Notify::new()));
        let thought = apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type":"text","text":"thinking hard"}
                    }
                }),
            },
        );
        assert_eq!(thought.len(), 1, "first thought chunk must pulse liveness");
        match &thought[0] {
            ThreadEvent::ItemUpdated { item } => match &item.details {
                ThreadItemDetails::Reasoning(t) => assert!(t.contains("thinking")),
                other => panic!("expected Reasoning, got {other:?}"),
            },
            other => panic!("expected ItemUpdated, got {other:?}"),
        }
        // Immediate second thought is throttled (same Instant window).
        let thought2 = apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type":"text","text":"more thought"}
                    }
                }),
            },
        );
        assert!(
            thought2.is_empty(),
            "second thought within throttle window must not flood"
        );

        // Force throttle window open for the message-chunk arm.
        st.last_liveness_at =
            Some(Instant::now() - LIVENESS_MIN_INTERVAL - Duration::from_millis(1));
        let msg = apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"Hello"}
                    }
                }),
            },
        );
        assert_eq!(msg.len(), 1, "message chunk after throttle must pulse");
        match &msg[0] {
            ThreadEvent::ItemUpdated { item } => match &item.details {
                ThreadItemDetails::AgentMessage(t) => assert_eq!(t, "Hello"),
                other => panic!("expected AgentMessage, got {other:?}"),
            },
            other => panic!("expected ItemUpdated, got {other:?}"),
        }
        // Buffer still accumulates for the final answer.
        assert_eq!(st.buffer.as_ref().unwrap().text, "Hello");
    }

    #[test]
    fn turn_boundary_notification_fires_finalize_barrier() {
        let mut st = SessionTranslateState::default();
        let done = Arc::new(Notify::new());
        st.begin_turn("t1", done.clone());
        // A `turn_completed` update must release the barrier (and not be
        // treated as an unknown vendor notification).
        let out = apply_notification(
            &mut st,
            &Notification {
                method: "_x.ai/session_notification".into(),
                params: json!({ "update": { "sessionUpdate": "turn_completed" } }),
            },
        );
        assert!(out.is_empty());
        // notify_one before notified() stores a permit → this returns Ready.
        assert!(
            done.notified().now_or_never().is_some(),
            "turn boundary must have signalled the barrier"
        );
    }
}
