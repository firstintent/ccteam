//! Final-only translate: ACP notifications + prompt response → ThreadEvent.
//!
//! Client-started turn-end SoT = matching `session/prompt` JSON-RPC response.
//! A vendor that admits an idle control message can also self-start a turn;
//! that exceptional turn is opened by its first content update and finalized
//! by its own boundary notification. Buffer only `agent_message_chunk`; drop
//! thoughts and `isReplay` frames from the final answer, but emit throttled
//! mid-stream liveness events so the gateway silence watchdog sees long work.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Notify;

use super::protocol::{
    content_text, cost_from_usage_update, is_replay, is_turn_boundary,
    stop_reason_from_prompt_result, usage_from_prompt_result, AvailableCommand,
};
use super::transport::{AcpWriteBarrier, Notification};
use crate::{
    ContextSource, ContextUsage, ThreadErrorEvent, ThreadEvent, ThreadItem, ThreadItemDetails,
    ThreadStatus, UnifiedTokenUsage,
};

/// Min gap between liveness `ThreadEvent`s for message/thought chunks.
/// Chunks arrive many times per second; the watchdog only needs a periodic
/// pulse. First chunk of a streak always emits (interval elapsed).
const LIVENESS_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// Per-turn final-text buffer for either a client- or vendor-started turn.
#[derive(Debug, Clone, Default)]
pub struct TurnBuffer {
    pub turn_id: String,
    pub text: String,
}

#[derive(Debug)]
struct InjectionGateState {
    accepting: bool,
    pending: usize,
}

/// Per-turn coordinator for native ACP interjections.
///
/// A submit reserves the active turn while holding `SessionTranslateState`'s
/// lock. Once `session/prompt` resolves, the runner seals this gate under the
/// same lock and waits for every reservation to finish before it finalizes the
/// turn or starts a queued successor. This prevents a late interjection from
/// crossing a vendor turn boundary.
#[derive(Debug)]
pub struct AcpInjectionGate {
    state: StdMutex<InjectionGateState>,
    drained: Notify,
}

impl Default for AcpInjectionGate {
    fn default() -> Self {
        Self {
            state: StdMutex::new(InjectionGateState {
                accepting: true,
                pending: 0,
            }),
            drained: Notify::new(),
        }
    }
}

impl AcpInjectionGate {
    pub fn reserve(self: &Arc<Self>) -> Option<AcpInjectionReservation> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.accepting {
            return None;
        }
        state.pending += 1;
        Some(AcpInjectionReservation {
            gate: Some(Arc::clone(self)),
        })
    }

    pub fn seal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.accepting = false;
        if state.pending == 0 {
            self.drained.notify_one();
        }
    }

    pub async fn wait_drained(&self) {
        loop {
            if self.state.lock().unwrap_or_else(|e| e.into_inner()).pending == 0 {
                return;
            }
            self.drained.notified().await;
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = state.pending.saturating_sub(1);
        if state.pending == 0 {
            self.drained.notify_one();
        }
    }
}

/// RAII reservation so cancellation of a timed-out submit can never strand
/// the turn runner behind an unreleased injection gate.
#[derive(Debug)]
pub struct AcpInjectionReservation {
    gate: Option<Arc<AcpInjectionGate>>,
}

impl Drop for AcpInjectionReservation {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }
}

/// Shared live state for one ACP session.
#[derive(Debug, Default)]
pub struct SessionTranslateState {
    /// Client-started `session/prompt` turn, owned/finalized by the runner.
    pub buffer: Option<TurnBuffer>,
    /// Opt-in for vendors such as Grok that can admit an interjection while
    /// idle and then emit a turn without a matching `session/prompt` request.
    /// Kimi/OpenCode leave this false, preserving their existing behavior.
    pub capture_vendor_started_turns: bool,
    /// The client-started prompt's boundary was observed, so later chunks must
    /// never be appended to its sealed buffer while its RPC response races in.
    pub prompt_boundary_seen: bool,
    /// A turn opened by vendor content rather than by `session/prompt`.
    pub vendor_started_buffer: Option<TurnBuffer>,
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
    /// Released once the active `session/prompt` request has entered the ACP
    /// transport's FIFO writer. Native interjection requests wait on it so a
    /// back-to-back message can never overtake the prompt it is steering.
    pub prompt_sent: Option<Arc<AcpWriteBarrier>>,
    /// Registration gate for native interjections targeting this turn.
    pub injection_gate: Option<Arc<AcpInjectionGate>>,
    pub available_commands: Vec<AvailableCommand>,
    pub model: Option<String>,
    pub window_tokens: Option<u64>,
    pub used_tokens: Option<u64>,
    /// Which channel filled [`Self::used_tokens`]. Kept next to the number so
    /// [`Self::context_usage`] can hand provenance to the single render point
    /// instead of every vendor adapter re-deciding it.
    pub used_source: ContextSource,
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
    /// Resolve this session's context usage — the ONE place every ACP vendor
    /// answers `thread_status`'s context question from.
    ///
    /// A known window with no reported occupancy yields `used_tokens: None`
    /// (rendered `"— / 500k (usage unknown)"`), **never** `0`: a freshly
    /// resumed session has an empty in-memory counter and a full context, and
    /// claiming `0%` there is a lie, not a placeholder. Both halves unknown
    /// yields `None` so the statusline omits the fragment entirely.
    pub fn context_usage(&self) -> Option<ContextUsage> {
        match (self.used_tokens, self.window_tokens) {
            (None, None) => None,
            (used, window) => Some(ContextUsage {
                used_tokens: used,
                window_tokens: window.unwrap_or(0),
                source: if used.is_some() {
                    self.used_source
                } else {
                    ContextSource::Unknown
                },
            }),
        }
    }

    /// Record occupancy together with the channel that produced it.
    pub fn set_used_tokens(&mut self, used: u64, source: ContextSource) {
        self.used_tokens = Some(used);
        self.used_source = source;
    }

    /// The session's queryable status. Every ACP vendor answers
    /// `thread_status` with exactly this — the shape is protocol-level, not
    /// vendor-level, and duplicating it per adapter is how the three copies
    /// of the occupancy bug got in.
    pub fn thread_status(&self) -> ThreadStatus {
        ThreadStatus {
            model: self.model.clone(),
            context: self.context_usage(),
            effort: self.effort.clone(),
            goal: None,
        }
    }

    /// Seed a freshly (re)connected session from its persisted snapshot.
    ///
    /// The handshake is authoritative for what it actually reports — model,
    /// effort and window come from the vendor's live catalog — so this only
    /// fills what the handshake left empty. Occupancy is always taken from
    /// the snapshot because ACP has no way to ask for it at connect time; it
    /// was true at the session's last turn boundary and nothing has run
    /// since, so it is the honest starting value, carried with the
    /// provenance it was recorded under (never upgraded).
    pub fn seed_from_snapshot(&mut self, snapshot: &ThreadStatus) {
        if self.model.is_none() {
            self.model.clone_from(&snapshot.model);
        }
        if self.effort.is_none() {
            self.effort.clone_from(&snapshot.effort);
        }
        let Some(ctx) = snapshot.context else {
            return;
        };
        if self.window_tokens.is_none() && ctx.window_tokens > 0 {
            self.window_tokens = Some(ctx.window_tokens);
        }
        if let (None, Some(used)) = (self.used_tokens, ctx.used_tokens) {
            self.used_tokens = Some(used);
            self.used_source = ctx.source;
        }
    }

    pub fn begin_turn(&mut self, turn_id: impl Into<String>, done: Arc<Notify>) {
        self.begin_turn_with_prompt_barrier(turn_id, done, None);
    }

    pub fn begin_turn_with_prompt_barrier(
        &mut self,
        turn_id: impl Into<String>,
        done: Arc<Notify>,
        prompt_sent: Option<Arc<AcpWriteBarrier>>,
    ) {
        self.buffer = Some(TurnBuffer {
            turn_id: turn_id.into(),
            text: String::new(),
        });
        self.turn_done = Some(done);
        self.prompt_sent = prompt_sent;
        self.injection_gate = Some(Arc::new(AcpInjectionGate::default()));
        self.prompt_boundary_seen = false;
        // Fresh turn → first thought/message chunk should emit immediately.
        self.last_liveness_at = None;
    }

    fn append_message(&mut self, chunk: &str, vendor_started: bool) {
        let target = if vendor_started {
            self.vendor_started_buffer.as_mut()
        } else {
            self.buffer.as_mut()
        };
        if let Some(b) = target {
            b.text.push_str(chunk);
        }
    }

    fn begin_vendor_started_turn(&mut self) -> Option<ThreadEvent> {
        if self.vendor_started_buffer.is_some() {
            return None;
        }
        let turn_id = super::turn_runner::next_acp_turn_id();
        self.vendor_started_buffer = Some(TurnBuffer {
            turn_id: turn_id.clone(),
            text: String::new(),
        });
        self.last_liveness_at = None;
        Some(ThreadEvent::TurnStarted { turn_id })
    }

    /// Signal (once) that the turn boundary was reached.
    pub fn signal_turn_done(&mut self) {
        if let Some(done) = self.turn_done.take() {
            done.notify_one();
        }
    }

    pub fn take_buffer(&mut self) -> Option<TurnBuffer> {
        self.prompt_sent = None;
        self.injection_gate = None;
        self.prompt_boundary_seen = false;
        self.buffer.take()
    }
}

/// Apply one notification. Client-started final messages wait for the prompt
/// response; an opted-in vendor-started turn finalizes on its own boundary.
pub fn apply_notification(state: &mut SessionTranslateState, n: &Notification) -> Vec<ThreadEvent> {
    // Drop full replay frames from session/load (isReplay covers top-level and
    // nested `update._meta.isReplay`).
    if is_replay(&n.params) {
        return Vec::new();
    }

    // A vendor-started turn has no prompt response, so its own FIFO boundary is
    // authoritative and must emit a normal canonical completion here.
    if is_turn_boundary(&n.method, &n.params) {
        if state.vendor_started_buffer.is_some() {
            return finalize_vendor_started_turn(state);
        }
        // The prompt buffer stays present until its JSON-RPC result carries
        // usage/model. Mark it sealed now so a newly self-started answer cannot
        // tear onto the old text during that response race.
        if state.buffer.is_some() {
            state.prompt_boundary_seen = true;
        }
        state.signal_turn_done();
        return Vec::new();
    }

    // Standard session/update path (`session/update` or `_x.ai/session/update`).
    if n.method == "session/update" || n.method.ends_with("session/update") {
        let vendor_started = state.capture_vendor_started_turns
            && (state.vendor_started_buffer.is_some()
                || state.prompt_boundary_seen
                || state.buffer.is_none())
            && session_update_has_turn_content(&n.params);
        let mut events = Vec::new();
        if vendor_started {
            if let Some(started) = state.begin_vendor_started_turn() {
                events.push(started);
            }
        }
        events.extend(apply_session_update(state, &n.params, vendor_started));
        return events;
    }

    // Grok acknowledgement for `_x.ai/interject`. It confirms control-plane
    // admission only; content and any vendor-started boundary arrive through
    // their own notifications.
    if n.method.ends_with("session/interjection") {
        return Vec::new();
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

fn apply_session_update(
    state: &mut SessionTranslateState,
    params: &Value,
    vendor_started: bool,
) -> Vec<ThreadEvent> {
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
                state.append_message(&text, vendor_started);
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
                // Authoritative: the vendor states occupancy outright.
                state.set_used_tokens(used, ContextSource::Reported);
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
        // Kimi (and any configOptions vendor) republishes the WHOLE snapshot
        // whenever model / thinking / mode changes — from our own
        // `session/set_config_option` or from the user's own TUI. Re-plucking it
        // is what keeps the statusline's model · effort honest mid-session;
        // dropping the frame is why a switched effort used to read as the
        // handshake value forever.
        "config_option_update" => {
            let info = super::protocol::pluck_model_info(&update);
            if let Some(model) = info.model {
                state.model = Some(model);
            }
            // An absent axis means the current model has none — report nothing
            // rather than a stale level from the previous model.
            state.effort = info.effort;
            Vec::new()
        }
        "plan" | "current_mode_update" | "session_info_update" => Vec::new(),
        _ => {
            if !kind.is_empty() && state.warned_methods.insert(format!("update:{kind}")) {
                tracing::warn!(kind, "acp: skipping unknown sessionUpdate kind (warn-once)");
            }
            Vec::new()
        }
    }
}

fn session_update_has_turn_content(params: &Value) -> bool {
    let update = params.get("update").unwrap_or(params);
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            extract_chunk_text(update).is_some_and(|text| !text.is_empty())
        }
        "tool_call" | "tool_call_update" => true,
        _ => false,
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
        .vendor_started_buffer
        .as_ref()
        .or(state.buffer.as_ref())
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

fn finalize_vendor_started_turn(state: &mut SessionTranslateState) -> Vec<ThreadEvent> {
    let Some(buf) = state.vendor_started_buffer.take() else {
        return Vec::new();
    };
    state.last_liveness_at = None;
    let turn_id = buf.turn_id;
    vec![
        ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: format!("{turn_id}-msg"),
                details: ThreadItemDetails::AgentMessage(buf.text),
            },
        },
        ThreadEvent::TurnCompleted {
            turn_id,
            usage: UnifiedTokenUsage::default(),
            model: state.model.clone(),
        },
    ]
}

/// Finalize a turn from the `session/prompt` response (authoritative).
///
/// The response's `stopReason` decides whether this is an ANSWER or a
/// FAILURE. Both shapes still deliver whatever the vendor managed to produce
/// (never drop paid-for output), but a non-clean reason ends the turn with
/// [`ThreadEvent::TurnFailed`] so the failure reaches the user, `turns.jsonl`
/// (`outcome:"failed"`) and the delegation parent instead of masquerading as
/// the final reply.
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
    // Per-turn token total ≈ occupancy, because a vendor reporting it here
    // counts the WHOLE prompt (system + history) as this turn's input (grok,
    // verified on the wire). A `0` is never occupancy — it is what a turn that
    // made no model call reports (a vendor-local slash command, an aborted
    // turn), and folding it in would blank a session that is really at 80%.
    if let Some(total) = result
        .pointer("/_meta/totalTokens")
        .or_else(|| result.pointer("/usage/totalTokens"))
        .and_then(|v| v.as_u64())
        .filter(|t| *t > 0)
    {
        state.set_used_tokens(total, ContextSource::Derived);
    }
    // Signal barrier if turn_completed never arrived (OpenCode has no such notif).
    state.signal_turn_done();
    let buf = state.take_buffer();
    let turn_id = buf
        .as_ref()
        .map(|b| b.turn_id.clone())
        .unwrap_or_else(|| "unknown".into());
    let text = buf.map(|b| b.text).unwrap_or_default();
    let stop = stop_reason_from_prompt_result(result);
    let terminal_model = model.or_else(|| state.model.clone());
    let mut out = Vec::new();
    if let Some(message) = stop.failure_message() {
        // Partial output first — it was produced and paid for, and it is often
        // the only clue about how far the turn got. Empty text is dropped
        // downstream, so an unconditional push stays correct.
        out.push(ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: format!("{turn_id}-msg"),
                details: ThreadItemDetails::AgentMessage(text),
            },
        });
        // TurnFailed is the terminal event (same convention as
        // claude_stream_json's `is_failure` path): it clears the gateway's
        // in-flight marker, flushes the delegation boundary with
        // `vendor_error`, and no TurnCompleted follows.
        out.push(ThreadEvent::TurnFailed {
            turn_id,
            err: ThreadErrorEvent {
                kind: format!("stop_reason:{}", stop.wire()),
                message,
            },
            usage,
            model: terminal_model,
        });
        return out;
    }
    out.push(ThreadEvent::ItemCompleted {
        item: ThreadItem {
            id: format!("{turn_id}-msg"),
            details: ThreadItemDetails::AgentMessage(text),
        },
    });
    out.push(ThreadEvent::TurnCompleted {
        turn_id,
        usage,
        model: terminal_model,
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
        usage: UnifiedTokenUsage::default(),
        model: state.model.clone(),
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

    /// A resumed ACP session knows its window (from the vendor's model
    /// catalog) but not its occupancy (an in-memory counter that restarts
    /// empty). It must say "unknown", not "0%" — a grok session sitting at
    /// 80% used to render `0 / 500k (0%)` after every daemon restart.
    #[test]
    fn window_without_occupancy_is_unknown_not_zero() {
        let st = SessionTranslateState {
            window_tokens: Some(500_000),
            ..Default::default()
        };
        let ctx = st
            .context_usage()
            .expect("window alone is still reportable");
        assert_eq!(ctx.used_tokens, None);
        assert_eq!(ctx.window_tokens, 500_000);
        assert_eq!(ctx.source, ContextSource::Unknown);
        assert_eq!(ctx.render(), "— / 500k (usage unknown)");

        // Nothing at all → no fragment on the statusline.
        assert!(SessionTranslateState::default().context_usage().is_none());
    }

    /// `config_option_update` is how a configOptions vendor (kimi) announces
    /// every model / thinking change — from our own `set_config_option` or
    /// from the user's own TUI picker. Dropping the frame (what ccteam did)
    /// froze the statusline at the handshake values for the rest of the
    /// session.
    #[test]
    fn config_option_update_refreshes_model_and_effort() {
        let mut st = SessionTranslateState {
            model: Some("kimi-code/k3".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "config_option_update",
                        "configOptions": [
                            {"id":"model","category":"model","currentValue":"kimi-code/k3-256k",
                             "options":[{"value":"kimi-code/k3-256k","name":"K3-256k"}]},
                            {"id":"thinking","category":"thought_level","currentValue":"max",
                             "options":[{"value":"low"},{"value":"high"},{"value":"max"}]}
                        ]
                    }
                }),
            },
        );
        assert_eq!(st.model.as_deref(), Some("kimi-code/k3-256k"));
        assert_eq!(st.effort.as_deref(), Some("max"));

        // Switching to a model with no thinking axis drops the option from the
        // snapshot — report nothing, not the previous model's level.
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "config_option_update",
                        "configOptions": [
                            {"id":"model","category":"model","currentValue":"kimi-code/plain",
                             "options":[{"value":"kimi-code/plain","name":"Plain"}]},
                            {"id":"mode","category":"mode","currentValue":"default",
                             "options":[{"value":"default"}]}
                        ]
                    }
                }),
            },
        );
        assert_eq!(st.model.as_deref(), Some("kimi-code/plain"));
        assert_eq!(st.effort, None);
    }

    /// `usage_update` is the vendor stating occupancy; the prompt result's
    /// per-turn total is our own inference. Both fill the same slot, so the
    /// slot has to remember which one spoke.
    #[test]
    fn occupancy_carries_the_channel_that_reported_it() {
        let mut st = SessionTranslateState::default();
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {"sessionUpdate": "usage_update", "used": 4_000, "size": 128_000}
                }),
            },
        );
        let ctx = st.context_usage().unwrap();
        assert_eq!(ctx.used_tokens, Some(4_000));
        assert_eq!(ctx.source, ContextSource::Reported);

        let mut st = SessionTranslateState::default();
        st.begin_turn("t1", Arc::new(Notify::new()));
        finalize_from_prompt_result(
            &mut st,
            &json!({"stopReason": "end_turn", "_meta": {"totalTokens": 17_580}}),
        );
        assert_eq!(st.context_usage().unwrap().source, ContextSource::Derived);
    }

    /// A turn that made no model call reports `totalTokens: 0` (verified on a
    /// live grok binary: a vendor-local slash command answers in 3ms with a
    /// zero total). Folding that in would blank a session that is really at
    /// 80% — the last real measurement must survive.
    #[test]
    fn zero_total_tokens_never_overwrites_real_occupancy() {
        let mut st = SessionTranslateState {
            window_tokens: Some(500_000),
            ..Default::default()
        };
        st.begin_turn("t1", Arc::new(Notify::new()));
        finalize_from_prompt_result(
            &mut st,
            &json!({"stopReason": "end_turn", "_meta": {"totalTokens": 17_580}}),
        );
        assert_eq!(st.context_usage().unwrap().used_tokens, Some(17_580));

        st.begin_turn("t2", Arc::new(Notify::new()));
        finalize_from_prompt_result(
            &mut st,
            &json!({"stopReason": "end_turn", "_meta": {"totalTokens": 0}}),
        );
        assert_eq!(
            st.context_usage().unwrap().used_tokens,
            Some(17_580),
            "a no-model-call turn must not blank the last real measurement"
        );
    }

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

    /// Buffer some partial text, then finalize with `reason`.
    fn finalize_with_stop_reason(reason: &str) -> Vec<ThreadEvent> {
        let mut st = SessionTranslateState::default();
        st.begin_turn("t-stop", Arc::new(Notify::new()));
        apply_notification(
            &mut st,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"先读 state"}
                    }
                }),
            },
        );
        let events = finalize_from_prompt_result(
            &mut st,
            &json!({
                "stopReason": reason,
                "_meta": {
                    "inputTokens": 70,
                    "outputTokens": 30,
                    "modelId": "grok-4.5"
                }
            }),
        );
        assert!(
            st.buffer.is_none(),
            "the turn buffer must be released whatever the outcome"
        );
        events
    }

    #[test]
    fn non_clean_stop_reason_delivers_partial_text_then_fails_the_turn() {
        // The s172 shape: a vendor ends a turn abnormally after emitting a
        // mid-turn preamble. Pre-fix this finalized as TurnCompleted and the
        // preamble was delivered as if it were the final answer.
        for (reason, want_kind) in [
            ("refusal", "stop_reason:refusal"),
            ("max_tokens", "stop_reason:max_tokens"),
            ("max_turn_requests", "stop_reason:max_turn_requests"),
            ("something_new", "stop_reason:something_new"),
        ] {
            let events = finalize_with_stop_reason(reason);
            assert_eq!(events.len(), 2, "{reason}: partial text + failure");
            match &events[0] {
                ThreadEvent::ItemCompleted { item } => match &item.details {
                    // Never drop output the user already paid for.
                    ThreadItemDetails::AgentMessage(t) => assert_eq!(t, "先读 state"),
                    other => panic!("{reason}: unexpected {other:?}"),
                },
                other => panic!("{reason}: unexpected {other:?}"),
            }
            match &events[1] {
                ThreadEvent::TurnFailed {
                    turn_id,
                    err,
                    usage,
                    model,
                } => {
                    assert_eq!(turn_id, "t-stop");
                    assert_eq!(err.kind, want_kind);
                    assert!(err.message.contains(reason), "{reason}: {}", err.message);
                    assert_eq!(usage.input_tokens, 70, "{reason}");
                    assert_eq!(usage.output_tokens, 30, "{reason}");
                    assert_eq!(model.as_deref(), Some("grok-4.5"), "{reason}");
                }
                other => panic!("{reason}: must be TurnFailed, got {other:?}"),
            }
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, ThreadEvent::TurnCompleted { .. })),
                "{reason}: a failed turn must not also report completion"
            );
        }
    }

    #[test]
    fn clean_stop_reasons_still_complete_normally() {
        // end_turn, cancelled and an absent field must all keep the answer
        // path — a `/stop` is not a defect and an omitted field is not either.
        for reason in ["end_turn", "cancelled"] {
            let events = finalize_with_stop_reason(reason);
            assert_eq!(events.len(), 2, "{reason}");
            assert!(
                matches!(&events[1], ThreadEvent::TurnCompleted { .. }),
                "{reason}: must complete, got {:?}",
                events[1]
            );
        }
        let mut st = SessionTranslateState::default();
        st.begin_turn("t-bare", Arc::new(Notify::new()));
        let events = finalize_from_prompt_result(&mut st, &json!({}));
        assert!(matches!(&events[1], ThreadEvent::TurnCompleted { .. }));
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

    #[test]
    fn grok_interjection_ack_is_known_control_plane_noise() {
        let mut state = SessionTranslateState::default();
        let out = apply_notification(
            &mut state,
            &Notification {
                method: "_x.ai/session/interjection".into(),
                params: json!({
                    "sessionId": "s1",
                    "text": "new direction"
                }),
            },
        );
        assert!(out.is_empty());
        assert!(state.warned_methods.is_empty());
    }

    #[test]
    fn vendor_started_content_after_prompt_boundary_is_distinct_and_finalizes() {
        let mut state = SessionTranslateState {
            capture_vendor_started_turns: true,
            ..Default::default()
        };
        state.begin_turn("client-turn", Arc::new(Notify::new()));
        apply_notification(
            &mut state,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"old answer"}
                    }
                }),
            },
        );
        apply_notification(
            &mut state,
            &Notification {
                method: "_x.ai/session/prompt_complete".into(),
                params: json!({}),
            },
        );

        let opened = apply_notification(
            &mut state,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"self started"}
                    }
                }),
            },
        );
        let synthetic_id = opened
            .iter()
            .find_map(|event| match event {
                ThreadEvent::TurnStarted { turn_id } => Some(turn_id.clone()),
                _ => None,
            })
            .expect("content opens a synthetic turn");
        assert_eq!(state.buffer.as_ref().unwrap().text, "old answer");
        assert_eq!(
            state.vendor_started_buffer.as_ref().unwrap().text,
            "self started"
        );

        let synthetic_done = apply_notification(
            &mut state,
            &Notification {
                method: "_x.ai/session/prompt_complete".into(),
                params: json!({}),
            },
        );
        assert!(matches!(
            &synthetic_done[0],
            ThreadEvent::ItemCompleted { item }
                if matches!(&item.details, ThreadItemDetails::AgentMessage(text) if text == "self started")
        ));
        assert!(matches!(
            &synthetic_done[1],
            ThreadEvent::TurnCompleted { turn_id, .. } if turn_id == &synthetic_id
        ));

        let client_done = finalize_from_prompt_result(&mut state, &json!({}));
        assert!(matches!(
            &client_done[0],
            ThreadEvent::ItemCompleted { item }
                if matches!(&item.details, ThreadItemDetails::AgentMessage(text) if text == "old answer")
        ));
        assert!(matches!(
            &client_done[1],
            ThreadEvent::TurnCompleted { turn_id, .. } if turn_id == "client-turn"
        ));
    }

    #[test]
    fn bufferless_content_is_inert_without_vendor_started_opt_in() {
        let mut state = SessionTranslateState::default();
        let events = apply_notification(
            &mut state,
            &Notification {
                method: "session/update".into(),
                params: json!({
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type":"text","text":"unowned"}
                    }
                }),
            },
        );
        assert!(events
            .iter()
            .all(|event| !matches!(event, ThreadEvent::TurnStarted { .. })));
        assert!(state.vendor_started_buffer.is_none());
    }
}
