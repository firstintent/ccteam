//! Seam ③ (PRD §七) — translate the stream-json [`Outbound`] sequence into
//! the vendor-neutral [`ThreadEvent`] stream the gateway pump already
//! consumes (the SAME contract `claude_tui`'s transcript tail emits, so
//! `spawn_event_pump` — the live daemon's only turns/progress writer —
//! needs zero changes).
//!
//! ## The contract this honors (gateway `async_event_text`)
//!
//! - The turn's **answer** is emitted exactly once as
//!   [`ThreadEvent::ItemCompleted`] carrying
//!   [`ThreadItemDetails::AgentMessage`] — the only event the pump
//!   forwards to IM as a reply. The answer is EVERY top-level assistant
//!   text block of the turn, in order — not just the last one. Claude's
//!   `result.result` carries only the final text block, so a reply the
//!   model wrote before its next tool call (typically an answer to a
//!   human who spoke mid-turn) would otherwise vanish from turns.jsonl,
//!   the IM reply and the delegation notification (issue #192).
//!   Subagent blocks (`parent_tool_use_id` set) are never the answer.
//! - The turn's **conclusion** — the text after the last tool call, which
//!   is what `result.result` carries — rides [`ThreadEvent::TurnCompleted`]
//!   separately, so a bounded excerpt of the answer (the completion
//!   notification a parent wakes up to) can show the receipt the model
//!   wrote last instead of the head of its narration (issue #196). Omitted
//!   when it IS the whole answer (a single-block turn).
//! - A turn **failure** is [`ThreadEvent::TurnFailed`] (the pump forwards
//!   `err.message` verbatim → the honest in-flight-loss / error signal).
//! - Tool-use / thinking blocks become `ItemStarted{ToolCall}` /
//!   `ItemUpdated{Reasoning}` — progress-fold fodder only (the pump drops
//!   their text), so they never masquerade as the answer.
//!
//! Pure + synchronous: [`StreamTranslator::ingest`] takes one parsed
//! [`Outbound`] and returns the events it produced. The transport's
//! `events()` task owns one translator and drives it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::protocol::{MessageEnvelope, Outbound, ResultMsg, SystemMsg};
use crate::{
    ThreadErrorEvent, ThreadEvent, ThreadItem, ThreadItemDetails, TurnContinuation, TurnOpening,
    UnifiedTokenUsage,
};

/// The session's EXECUTION-turn identity, shared by the submit path and this
/// translator.
///
/// stream-json used to run two disjoint id spaces: `submit_turn` handed the
/// caller a `turn-<nanos>` receipt while the event stream reported `sj-N`, so
/// nothing downstream could say which submission a `TurnCompleted` belonged to
/// — the gateway's own turn-origin bookkeeping never matched either, and a
/// dispatcher could not be told which of its queued tasks had just answered
/// (issue #201). One id, minted by whoever delivers the line and reported by
/// the turn it opens, is what makes a request correlatable end to end.
///
/// Ids embed the process clock so a line parked across a daemon restart keeps
/// an identity nothing in the next life can collide with — that is what lets a
/// restart reconcile rebind an outstanding request by identity instead of
/// guessing from order.
#[derive(Debug, Default)]
pub struct TurnIdentity {
    seq: u64,
    /// Reserved by a delivered line that is about to open a turn.
    pending: Option<String>,
    /// The turn the translator currently has in flight.
    active: Option<String>,
    /// A line ccteam wrote INTO a running turn is not read there: claude shows
    /// it to the model as a queued-command preview and then RE-RUNS it as the
    /// prompt of the next turn. Until that turn opens, the boundary of the turn
    /// the line joined answers nothing it was dispatched for (GitHub #199).
    ///
    /// A flag rather than a count, deliberately. Nothing in the protocol says
    /// how claude batches two queued lines — one replay turn or two — and a
    /// counter that guessed wrong would leave a request bound to a turn that
    /// never opens. One expected continuation, cleared by the first turn the
    /// vendor opens by itself, cannot strand anybody.
    replay_pending: bool,
}

impl TurnIdentity {
    /// A fresh, process-unique execution-turn id.
    pub fn mint(&mut self) -> String {
        self.seq += 1;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("sj-{nanos:x}-{:x}", self.seq)
    }

    /// Claim `id` for the turn the next delivered line opens.
    pub fn reserve(&mut self, id: &str) {
        self.pending = Some(id.to_string());
    }

    /// Undo a reservation whose line never reached the child.
    pub fn clear_reservation(&mut self, id: &str) {
        if self.pending.as_deref() == Some(id) {
            self.pending = None;
        }
    }

    /// The turn a line written RIGHT NOW would belong to: the one in flight,
    /// else the one a delivered line has already reserved.
    pub fn current(&self) -> Option<String> {
        self.active.clone().or_else(|| self.pending.clone())
    }

    /// Record that a line was written into a turn already in flight, so the
    /// boundary of THAT turn is known not to be the line's answer.
    pub fn note_injected(&mut self) {
        self.replay_pending = true;
    }

    /// Is claude still holding a line ccteam injected, to re-run as its next
    /// prompt? Read at the boundary: while it is true the boundary settles
    /// nothing.
    pub fn has_pending_replay(&self) -> bool {
        self.replay_pending
    }

    /// Open a turn: the reserved id when a delivery claimed one, else a fresh
    /// id (a turn the vendor started on its own). The second half of the
    /// answer is WHO opened it, which is what decides whether the previous
    /// turn's bindings move onto this one.
    pub fn open(&mut self) -> (String, TurnOpening) {
        match self.pending.take() {
            Some(id) => {
                self.active = Some(id.clone());
                (id, TurnOpening::Submitted)
            }
            None => {
                // Nobody reserved this turn, so ccteam did not ask for it: it
                // is claude re-running a queued line or answering its own
                // `<task-notification>`. Either way one expected continuation
                // has now arrived.
                self.replay_pending = false;
                let id = self.mint();
                self.active = Some(id.clone());
                (id, TurnOpening::VendorContinuation)
            }
        }
    }

    /// Close the turn in flight.
    pub fn close(&mut self) {
        self.active = None;
    }

    /// The stream is gone: nothing will replay anything. Kept separate from
    /// [`Self::close`], which runs at every ordinary boundary — that is exactly
    /// where a pending replay still matters.
    pub fn forget_replay(&mut self) {
        self.replay_pending = false;
    }

    /// Take a reservation that will never open a turn (the stream closed).
    pub fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }
}

/// Per-session translation state. One per live stream-json session.
#[derive(Debug, Default)]
pub struct StreamTranslator {
    /// Execution-turn identity, shared with the submit path so a submission
    /// receipt names the turn that will actually report the answer.
    identity: Arc<Mutex<TurnIdentity>>,
    /// `Some` while a turn is in flight (between first assistant block and
    /// its `result`) — a local mirror of the shared cell, so item ids stay
    /// correlatable without holding the lock.
    active_turn: Option<String>,
    /// Every top-level assistant text block of the active turn, in stream
    /// order — this IS the turn's answer (`result.result` only repeats the
    /// last block; see the module doc / issue #192).
    acc_text: String,
    /// The last top-level text block of the active turn — the conclusion's
    /// fallback when `result.result` is empty (see the module doc).
    last_text: Option<String>,
    /// Item-id counter for tool/reasoning items within a turn.
    item_seq: u64,
    /// Canonical model id (`message.model`) of the active turn's latest
    /// assistant message — the deterministic per-turn cost source. The
    /// `result` line carries no model, so we carry it forward from the
    /// assistant block(s). A turn can mix models (e.g. a sonnet sub-turn);
    /// the LAST assistant model wins for the turn's headline cost — the
    /// transcript path prices the finer per-message split.
    turn_model: Option<String>,
    /// Ids from the latest `system:background_tasks_changed` snapshot — the
    /// vendor's FULL list of what it currently runs in the background, re-sent
    /// on every change. Non-empty at a `result` means claude will wake its own
    /// model again when one of them finishes, so that `result` is not the end
    /// of anything (GitHub #198).
    ///
    /// Tracked HERE rather than read off the session's shared task mirror: the
    /// tap and this translator are two independent subscribers of one
    /// broadcast, so the tap may not have folded the snapshot that precedes a
    /// `result` by the time the translator reaches it. A subscriber's own
    /// stream is ordered; another subscriber's progress is not.
    background_tasks: HashSet<String>,
}

impl StreamTranslator {
    /// A standalone translator that owns its own identity space (unit tests,
    /// and any consumer without a live session behind it).
    pub fn new() -> Self {
        Self::default()
    }

    /// A translator sharing the live session's execution-turn identity, so the
    /// id a submission was given is the id its turn reports.
    pub fn attached(identity: Arc<Mutex<TurnIdentity>>) -> Self {
        Self {
            identity,
            ..Self::default()
        }
    }

    /// Mint through the shared cell, tolerating a poisoned lock by falling
    /// back to a locally unique id rather than panicking the pump.
    fn open_turn_id(&mut self) -> (String, TurnOpening) {
        match self.identity.lock() {
            Ok(mut identity) => identity.open(),
            Err(poisoned) => poisoned.into_inner().open(),
        }
    }

    /// What the vendor still holds at this boundary — see [`TurnContinuation`].
    /// Two harness facts, no text: the background-task snapshot it last sent,
    /// and whether a line ccteam injected is still waiting to be re-run.
    fn continuation(&self) -> TurnContinuation {
        let replay_pending = match self.identity.lock() {
            Ok(identity) => identity.has_pending_replay(),
            Err(poisoned) => poisoned.into_inner().has_pending_replay(),
        };
        if self.background_tasks.is_empty() && !replay_pending {
            TurnContinuation::Settled
        } else {
            TurnContinuation::Pending
        }
    }

    fn close_turn_id(&mut self) {
        match self.identity.lock() {
            Ok(mut identity) => identity.close(),
            Err(poisoned) => poisoned.into_inner().close(),
        }
    }

    /// Ingest one outbound message, returning the neutral events it
    /// produced (possibly empty).
    pub fn ingest(&mut self, out: Outbound) -> Vec<ThreadEvent> {
        match out {
            Outbound::Assistant(env) => self.on_assistant(env),
            Outbound::TurnResult(r) => self.on_result(r),
            // A system line carries no neutral event, but the background-task
            // snapshot on it decides whether the next `result` ends anything.
            Outbound::System(sys) => {
                self.on_system(&sys);
                Vec::new()
            }
            // `user` replay echoes, control frames, partials: no neutral event
            // (transcript authority + HITL handled elsewhere). A user line the
            // vendor wrote ITSELF is not classified from its text — the turn it
            // opens is recognised by having reserved no id (see
            // [`TurnIdentity::open`]).
            Outbound::User(_)
            | Outbound::ControlRequest(_)
            | Outbound::ControlResponse(_)
            | Outbound::Other => Vec::new(),
        }
    }

    /// Adopt the vendor's latest background-task snapshot. Only the membership
    /// matters here (is anything running at all), so the ids are kept without
    /// interpretation — `/status`'s richer running-task list is the tap's job.
    fn on_system(&mut self, sys: &SystemMsg) {
        if sys.subtype != "background_tasks_changed" {
            return;
        }
        self.background_tasks = sys
            .tasks
            .iter()
            .filter(|task| !task.task_id.is_empty())
            .map(|task| task.task_id.clone())
            .collect();
    }

    fn ensure_turn_started(&mut self, out: &mut Vec<ThreadEvent>) {
        if self.active_turn.is_none() {
            let (id, opening) = self.open_turn_id();
            self.active_turn = Some(id.clone());
            self.acc_text.clear();
            self.last_text = None;
            self.item_seq = 0;
            self.turn_model = None;
            out.push(ThreadEvent::TurnStarted {
                turn_id: id,
                opening,
            });
        }
    }

    fn next_item_id(&mut self) -> String {
        self.item_seq += 1;
        format!(
            "sj-{}-{}",
            self.active_turn.as_deref().unwrap_or("0"),
            self.item_seq
        )
    }

    /// Called when the transport closes (child death / EOF). If a turn was
    /// in flight (started but no `result` arrived), synthesize a
    /// [`ThreadEvent::TurnFailed`] so the in-flight loss surfaces as a
    /// **human signal** (the pump forwards `err.message` to IM) instead of
    /// silence — the honest cost of the stream-json channel (PRD E3:
    /// stream-json doesn't survive a process interrupt; recovery is only to
    /// `--resume` granularity). Returns `None` when no turn was active (a
    /// clean idle close), so a graceful stop emits no spurious failure.
    pub fn on_close(&mut self) -> Option<ThreadEvent> {
        self.acc_text.clear();
        self.last_text = None;
        let model = self.turn_model.take();
        // A line delivered but never answered (the child died before its first
        // assistant block) has an id reserved and no turn in flight. Failing it
        // under THAT id is what lets the dispatcher that submitted it see its
        // own request fail instead of waiting forever (issue #201).
        self.background_tasks.clear();
        let reserved = match self.identity.lock() {
            Ok(mut identity) => {
                identity.close();
                identity.forget_replay();
                identity.take_pending()
            }
            Err(poisoned) => {
                let mut identity = poisoned.into_inner();
                identity.close();
                identity.forget_replay();
                identity.take_pending()
            }
        };
        self.active_turn
            .take()
            .or(reserved)
            .map(|turn_id| ThreadEvent::TurnFailed {
                turn_id,
                err: ThreadErrorEvent {
                    kind: "stream_closed_in_flight".to_string(),
                    message: "stream-json 会话在回合进行中断开,这一回合丢失了 \
                          (stream-json 通道不扛进程中断,只恢复到 --resume 粒度)。\
                          再发一条消息会自动 resume 续上下文。"
                        .to_string(),
                },
                usage: UnifiedTokenUsage::default(),
                model,
            })
    }

    fn on_assistant(&mut self, env: MessageEnvelope) -> Vec<ThreadEvent> {
        let mut out = Vec::new();
        self.ensure_turn_started(&mut out);
        // Capture this turn's canonical model id (`message.model`) for the
        // deterministic per-turn cost on the TurnCompleted boundary.
        if let Some(m) = env.message.get("model").and_then(|v| v.as_str()) {
            if !m.is_empty() {
                self.turn_model = Some(m.to_string());
            }
        }
        let (text, items) = extract_blocks(&env.message);
        // Subagent narration (`parent_tool_use_id` set) belongs to the
        // Task tool's private thread, never to this session's reply.
        if !text.is_empty() && env.parent_tool_use_id.is_none() {
            push_paragraph(&mut self.acc_text, &text);
            self.last_text = Some(text);
        }
        for ev in items {
            // Re-id with the translator's counter so item ids are stable
            // within the turn (the raw tool_use id is fine too, but this
            // keeps them grep-correlatable with the turn).
            match ev {
                BlockItem::Tool { name, args } => {
                    let id = self.next_item_id();
                    out.push(ThreadEvent::ItemStarted {
                        item: ThreadItem {
                            id,
                            details: ThreadItemDetails::ToolCall { name, args },
                        },
                    });
                }
                BlockItem::Reasoning(text) => {
                    let id = self.next_item_id();
                    out.push(ThreadEvent::ItemUpdated {
                        item: ThreadItem {
                            id,
                            details: ThreadItemDetails::Reasoning(text),
                        },
                    });
                }
            }
        }
        out
    }

    fn on_result(&mut self, r: ResultMsg) -> Vec<ThreadEvent> {
        let mut out = Vec::new();
        // A `result` can arrive without a preceding assistant block (a
        // pure error / empty turn) — still synthesize a turn id.
        self.ensure_turn_started(&mut out);
        // Read BEFORE the turn is closed: what the vendor still holds decides
        // whether this boundary answers anybody (GitHub #198/#199).
        let continuation = self.continuation();
        let turn_id = self.active_turn.take().unwrap_or_default();
        self.close_turn_id();
        let usage = r
            .usage
            .as_ref()
            .and_then(|u| serde_json::from_value::<UnifiedTokenUsage>(u.clone()).ok())
            .unwrap_or_default();
        let model = self.turn_model.take();

        if r.is_failure() {
            let message = r
                .result
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "claude turn ended with error ({})",
                        if r.subtype.is_empty() {
                            "unknown"
                        } else {
                            &r.subtype
                        }
                    )
                });
            out.push(ThreadEvent::TurnFailed {
                turn_id,
                err: ThreadErrorEvent {
                    kind: r.subtype.clone(),
                    message,
                },
                usage,
                model,
            });
            self.acc_text.clear();
            self.last_text = None;
            return out;
        }

        // Success: the answer = every top-level text block seen this turn
        // (issue #192 — `result.result` is only the LAST block, so a reply
        // written before a further tool call would be dropped). `result`
        // is still honoured as the source of truth for text the stream
        // never showed us (empty stream, or a tail the blocks lack). Emit
        // the answer FIRST (so the pump finalizes the turn's progress epoch
        // before the boundary event), then TurnCompleted with usage.
        let mut final_text = std::mem::take(&mut self.acc_text);
        let last_text = self.last_text.take();
        let result_text = r.result.as_deref().filter(|s| !s.is_empty());
        if let Some(tail) = result_text {
            if !final_text.trim_end().ends_with(tail.trim_end()) {
                push_paragraph(&mut final_text, tail);
            }
        }
        // The conclusion = the vendor's `result.result` (measured: the LAST
        // text block), else the last block the stream showed; carried only
        // when the answer holds more than it (issue #196).
        let conclusion = result_text
            .map(str::to_string)
            .or(last_text)
            .filter(|conclusion| conclusion.trim() != final_text.trim());
        if !final_text.is_empty() {
            let id = self.next_item_id();
            out.push(ThreadEvent::ItemCompleted {
                item: ThreadItem {
                    id,
                    details: ThreadItemDetails::AgentMessage(final_text),
                },
            });
        }
        out.push(ThreadEvent::TurnCompleted {
            turn_id,
            usage,
            model,
            conclusion,
            continuation,
        });
        self.acc_text.clear();
        out
    }
}

/// Append one text block as its own paragraph (blank-line separated, so
/// consecutive blocks read the way Claude's own transcript shows them).
fn push_paragraph(acc: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !acc.is_empty() {
        acc.push_str("\n\n");
    }
    acc.push_str(text);
}

/// The PUBLIC text of one Anthropic `Message` — the text blocks a transcript
/// would show, and nothing else. One definition of "what this session said",
/// shared by the turn accumulator above and by the status tap's in-flight
/// narration cell, so an interrupted turn's record can never widen to
/// something the transcript would not show.
pub(super) fn public_text(message: &Value) -> String {
    extract_blocks(message).0
}

/// One non-text content block worth surfacing as a progress item.
enum BlockItem {
    Tool { name: String, args: Value },
    Reasoning(String),
}

/// Pull `(concatenated text, progress items)` out of an Anthropic
/// `Message` object. Tolerant of a string-form `content` (collapses to one
/// text block) and of unknown block types (ignored).
fn extract_blocks(message: &Value) -> (String, Vec<BlockItem>) {
    let mut text = String::new();
    let mut items = Vec::new();

    let content = message.get("content");
    match content {
        Some(Value::String(s)) => return (s.clone(), items),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                let kind = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match kind {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            push_paragraph(&mut text, t);
                        }
                    }
                    "tool_use" => {
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let args = block.get("input").cloned().unwrap_or(Value::Null);
                        items.push(BlockItem::Tool { name, args });
                    }
                    "thinking" => {
                        if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                            items.push(BlockItem::Reasoning(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    (text, items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(content: Value) -> Outbound {
        Outbound::Assistant(MessageEnvelope {
            message: json!({"role": "assistant", "content": content}),
            session_id: "u-1".into(),
            parent_tool_use_id: None,
        })
    }

    fn result_ok(text: &str) -> Outbound {
        Outbound::TurnResult(ResultMsg {
            subtype: "success".into(),
            result: Some(text.into()),
            is_error: false,
            total_cost_usd: Some(0.01),
            usage: Some(json!({"input_tokens": 10, "output_tokens": 5})),
            session_id: "u-1".into(),
        })
    }

    fn answer_text(evs: &[ThreadEvent]) -> Option<String> {
        evs.iter().find_map(|e| match e {
            ThreadEvent::ItemCompleted { item } => match &item.details {
                ThreadItemDetails::AgentMessage(t) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        })
    }

    /// A `system:background_tasks_changed` snapshot carrying `ids`.
    fn background(ids: &[&str]) -> Outbound {
        Outbound::System(Box::new(SystemMsg {
            subtype: "background_tasks_changed".into(),
            tasks: ids
                .iter()
                .map(|id| super::super::protocol::BackgroundTaskRef {
                    task_id: (*id).into(),
                })
                .collect(),
            ..SystemMsg::default()
        }))
    }

    /// What the turn's boundary said the vendor still holds.
    fn continuation_of(evs: &[ThreadEvent]) -> Option<TurnContinuation> {
        evs.iter().find_map(|e| match e {
            ThreadEvent::TurnCompleted { continuation, .. } => Some(*continuation),
            _ => None,
        })
    }

    /// Who opened the turn these events belong to.
    fn opening_of(evs: &[ThreadEvent]) -> Option<TurnOpening> {
        evs.iter().find_map(|e| match e {
            ThreadEvent::TurnStarted { opening, .. } => Some(*opening),
            _ => None,
        })
    }

    /// The `conclusion` the turn's boundary event carried (`None` = the
    /// answer is its own conclusion).
    fn conclusion_of(evs: &[ThreadEvent]) -> Option<String> {
        evs.iter().find_map(|e| match e {
            ThreadEvent::TurnCompleted { conclusion, .. } => conclusion.clone(),
            _ => None,
        })
    }

    #[test]
    fn simple_turn_emits_started_answer_completed() {
        let mut t = StreamTranslator::new();
        let mut all = Vec::new();
        all.extend(t.ingest(assistant(json!([{"type": "text", "text": "hi there"}]))));
        all.extend(t.ingest(result_ok("hi there")));

        assert!(matches!(all.first(), Some(ThreadEvent::TurnStarted { .. })));
        assert_eq!(answer_text(&all).as_deref(), Some("hi there"));
        assert!(all
            .iter()
            .any(|e| matches!(e, ThreadEvent::TurnCompleted { .. })));
        // Answer (ItemCompleted) precedes the TurnCompleted boundary.
        let ans = all
            .iter()
            .position(|e| matches!(e, ThreadEvent::ItemCompleted { .. }))
            .unwrap();
        let done = all
            .iter()
            .position(|e| matches!(e, ThreadEvent::TurnCompleted { .. }))
            .unwrap();
        assert!(ans < done);
    }

    #[test]
    fn usage_is_parsed_into_turn_completed() {
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!([{"type": "text", "text": "x"}])));
        let evs = t.ingest(result_ok("x"));
        let usage = evs.iter().find_map(|e| match e {
            ThreadEvent::TurnCompleted { usage, .. } => Some(*usage),
            _ => None,
        });
        let usage = usage.expect("TurnCompleted");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
    }

    #[test]
    fn canonical_message_model_flows_to_turn_completed() {
        // The assistant message's `message.model` (canonical id) is carried
        // forward onto the TurnCompleted boundary for deterministic cost.
        let mut t = StreamTranslator::new();
        let env = Outbound::Assistant(MessageEnvelope {
            message: json!({
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [{"type": "text", "text": "x"}],
            }),
            session_id: "u-1".into(),
            parent_tool_use_id: None,
        });
        t.ingest(env);
        let evs = t.ingest(result_ok("x"));
        let model = evs.iter().find_map(|e| match e {
            ThreadEvent::TurnCompleted { model, .. } => model.clone(),
            _ => None,
        });
        assert_eq!(model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn turn_completed_model_is_none_without_message_model() {
        // No `message.model` anywhere in the turn → model is None (unpriced,
        // exposed — never a fabricated fallback).
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!([{"type": "text", "text": "x"}])));
        let evs = t.ingest(result_ok("x"));
        let tc = evs
            .iter()
            .find(|e| matches!(e, ThreadEvent::TurnCompleted { .. }))
            .expect("TurnCompleted");
        match tc {
            ThreadEvent::TurnCompleted { model, .. } => assert!(model.is_none()),
            _ => unreachable!(),
        }
    }

    #[test]
    fn tool_use_block_becomes_progress_item_not_answer() {
        let mut t = StreamTranslator::new();
        let evs = t.ingest(assistant(json!([
            {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}, "id": "tu-1"},
            {"type": "text", "text": "running ls"}
        ])));
        // The tool_use surfaces as ItemStarted{ToolCall}; the text is
        // accumulated (not yet an answer until result).
        assert!(evs.iter().any(|e| matches!(
            e,
            ThreadEvent::ItemStarted { item }
                if matches!(&item.details, ThreadItemDetails::ToolCall { name, .. } if name == "Bash")
        )));
        assert!(answer_text(&evs).is_none());
    }

    #[test]
    fn result_falls_back_to_accumulated_text() {
        let mut t = StreamTranslator::new();
        t.ingest(assistant(
            json!([{"type": "text", "text": "accumulated answer"}]),
        ));
        // result with empty `result` → fall back to accumulated.
        let evs = t.ingest(Outbound::TurnResult(ResultMsg {
            subtype: "success".into(),
            result: None,
            is_error: false,
            total_cost_usd: None,
            usage: None,
            session_id: "u-1".into(),
        }));
        assert_eq!(answer_text(&evs).as_deref(), Some("accumulated answer"));
    }

    #[test]
    fn mid_turn_text_blocks_are_all_part_of_the_answer() {
        // issue #192 — measured wire shape (claude 2.1.258): one `assistant`
        // event per content block; `result.result` = the LAST text block
        // only. A reply written before a further tool call must survive.
        let mut t = StreamTranslator::new();
        t.ingest(assistant(
            json!([{"type": "text", "text": "ALPHA report here."}]),
        ));
        t.ingest(assistant(json!([
            {"type": "tool_use", "name": "Bash", "input": {"command": "echo hi"}, "id": "tu-1"}
        ])));
        t.ingest(assistant(json!([{"type": "text", "text": "BETA done."}])));
        let evs = t.ingest(result_ok("BETA done."));
        assert_eq!(
            answer_text(&evs).as_deref(),
            Some("ALPHA report here.\n\nBETA done.")
        );
        // issue #196 — the boundary names the block after the last tool call
        // as the turn's conclusion, so an excerpt can prefer it.
        assert_eq!(conclusion_of(&evs).as_deref(), Some("BETA done."));
    }

    #[test]
    fn conclusion_falls_back_to_the_last_stream_block_without_result_text() {
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!([{"type": "text", "text": "narration"}])));
        t.ingest(assistant(json!([
            {"type": "tool_use", "name": "Bash", "input": {"command": "true"}, "id": "tu-1"}
        ])));
        t.ingest(assistant(json!([{"type": "text", "text": "the receipt"}])));
        let evs = t.ingest(result_ok(""));
        assert_eq!(
            answer_text(&evs).as_deref(),
            Some("narration\n\nthe receipt")
        );
        assert_eq!(conclusion_of(&evs).as_deref(), Some("the receipt"));
    }

    #[test]
    fn a_single_block_answer_carries_no_conclusion() {
        // The answer IS the conclusion: carrying it twice would only cost the
        // ledger a copy and the excerpt nothing.
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!([{"type": "text", "text": "hi there"}])));
        let evs = t.ingest(result_ok("hi there"));
        assert_eq!(answer_text(&evs).as_deref(), Some("hi there"));
        assert_eq!(conclusion_of(&evs), None);
    }

    #[test]
    fn result_text_unseen_in_the_stream_is_still_kept() {
        // `result.result` stays authoritative for anything the blocks never
        // carried (never dropped, never duplicated).
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!([{"type": "text", "text": "narration"}])));
        let evs = t.ingest(result_ok("final from result only"));
        assert_eq!(
            answer_text(&evs).as_deref(),
            Some("narration\n\nfinal from result only")
        );
        assert_eq!(
            conclusion_of(&evs).as_deref(),
            Some("final from result only")
        );
    }

    #[test]
    fn subagent_text_never_joins_the_answer() {
        let mut t = StreamTranslator::new();
        t.ingest(Outbound::Assistant(MessageEnvelope {
            message: json!({"role": "assistant",
                "content": [{"type": "text", "text": "subagent chatter"}]}),
            session_id: "u-1".into(),
            parent_tool_use_id: Some("tu-9".into()),
        }));
        t.ingest(assistant(json!([{"type": "text", "text": "top-level"}])));
        let evs = t.ingest(result_ok("top-level"));
        assert_eq!(answer_text(&evs).as_deref(), Some("top-level"));
        assert_eq!(
            conclusion_of(&evs),
            None,
            "subagent text is never the conclusion either"
        );
    }

    #[test]
    fn failure_result_emits_turn_failed_with_human_message() {
        let mut t = StreamTranslator::new();
        t.ingest(Outbound::Assistant(MessageEnvelope {
            message: json!({
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [{"type": "text", "text": "partial"}],
            }),
            session_id: "u-1".into(),
            parent_tool_use_id: None,
        }));
        let evs = t.ingest(Outbound::TurnResult(ResultMsg {
            subtype: "error_max_turns".into(),
            result: None,
            is_error: true,
            total_cost_usd: None,
            usage: Some(json!({"input_tokens": 40, "output_tokens": 8})),
            session_id: "u-1".into(),
        }));
        let failed = evs.iter().find_map(|e| match e {
            ThreadEvent::TurnFailed {
                err, usage, model, ..
            } => Some((err, usage, model)),
            _ => None,
        });
        let (err, usage, model) = failed.expect("TurnFailed");
        assert!(err.message.contains("error_max_turns"));
        assert_eq!(usage.input_tokens, 40);
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(model.as_deref(), Some("claude-opus-4-8"));
        assert!(answer_text(&evs).is_none());
    }

    #[test]
    fn string_form_content_collapses_to_text() {
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!("just a string")));
        let evs = t.ingest(Outbound::TurnResult(ResultMsg {
            subtype: "success".into(),
            result: None,
            is_error: false,
            total_cost_usd: None,
            usage: None,
            session_id: "u-1".into(),
        }));
        assert_eq!(answer_text(&evs).as_deref(), Some("just a string"));
    }

    #[test]
    fn on_close_with_in_flight_turn_emits_human_failure() {
        let mut t = StreamTranslator::new();
        // An assistant block starts the turn; no result arrives → in flight.
        t.ingest(assistant(json!([{"type": "text", "text": "partial"}])));
        match t.on_close() {
            Some(ThreadEvent::TurnFailed { err, .. }) => {
                assert!(err.message.contains("stream-json"));
                assert_eq!(err.kind, "stream_closed_in_flight");
            }
            other => panic!("expected TurnFailed on in-flight close, got {other:?}"),
        }
        // Idempotent: no active turn left → silent.
        assert!(t.on_close().is_none());
    }

    #[test]
    fn on_close_after_completed_turn_is_silent() {
        let mut t = StreamTranslator::new();
        assert!(t.on_close().is_none(), "no turn yet → silent");
        t.ingest(assistant(json!([{"type": "text", "text": "x"}])));
        let _ = t.ingest(result_ok("x"));
        // The turn completed (result arrived) → a clean idle close is silent.
        assert!(t.on_close().is_none());
    }

    #[test]
    fn two_turns_have_distinct_turn_ids() {
        let mut t = StreamTranslator::new();
        let a = t.ingest(assistant(json!([{"type": "text", "text": "one"}])));
        let _ = t.ingest(result_ok("one"));
        let b = t.ingest(assistant(json!([{"type": "text", "text": "two"}])));
        let _ = t.ingest(result_ok("two"));
        let id = |evs: &[ThreadEvent]| {
            evs.iter().find_map(|e| match e {
                ThreadEvent::TurnStarted { turn_id, .. } => Some(turn_id.clone()),
                _ => None,
            })
        };
        assert_ne!(id(&a), id(&b));
    }

    /// GitHub #198 — claude wakes its own model. A `result` that lands while
    /// its `background_tasks_changed` snapshot still names something is NOT the
    /// end of the task: the CLI will inject a `<task-notification>` when that
    /// task finishes and the model answers in a brand-new turn. Measured on a
    /// live session: one dispatched task ran seven turns over 47 minutes and
    /// the receipt was on the last one.
    ///
    /// Pre-fix this boundary was indistinguishable from a settled one, so the
    /// dispatcher was woken by the child's first checkpoint and its request was
    /// consumed — the real answer reached nobody.
    #[test]
    fn a_result_with_background_work_outstanding_settles_nothing() {
        let mut t = StreamTranslator::new();
        t.ingest(background(&["bg-1"]));
        t.ingest(assistant(
            json!([{"type": "text", "text": "kicked off a build"}]),
        ));
        let a = t.ingest(result_ok("kicked off a build"));
        assert_eq!(
            continuation_of(&a),
            Some(TurnContinuation::Pending),
            "a snapshot that still names a task means the vendor will be back"
        );

        // The task finishes: claude re-sends the snapshot, now empty, and wakes
        // itself. Nothing reserved that turn's id, so it is a continuation.
        t.ingest(background(&[]));
        let b = t.ingest(assistant(
            json!([{"type": "text", "text": "the build passed"}]),
        ));
        assert_eq!(opening_of(&b), Some(TurnOpening::VendorContinuation));
        let b = [b, t.ingest(result_ok("the build passed"))].concat();
        assert_eq!(
            continuation_of(&b),
            Some(TurnContinuation::Settled),
            "an empty snapshot and nothing to replay: THIS is the receipt"
        );
    }

    /// A turn ccteam asked for is never mistaken for one the vendor started:
    /// the submit path reserves the id before the bytes go out, and a turn that
    /// opens on a reserved id is `Submitted`. Only that keeps a continuation
    /// from adopting the bindings of an unrelated turn.
    #[test]
    fn a_reserved_turn_is_submitted_and_an_unreserved_one_is_a_continuation() {
        let identity = Arc::new(Mutex::new(TurnIdentity::default()));
        let mut t = StreamTranslator::attached(Arc::clone(&identity));
        let reserved = {
            let mut ids = identity.lock().unwrap();
            let id = ids.mint();
            ids.reserve(&id);
            id
        };
        let a = t.ingest(assistant(json!([{"type": "text", "text": "on it"}])));
        assert_eq!(opening_of(&a), Some(TurnOpening::Submitted));
        assert!(a.iter().any(|e| matches!(
            e,
            ThreadEvent::TurnStarted { turn_id, .. } if turn_id == &reserved
        )));
        t.ingest(result_ok("on it"));

        let b = t.ingest(assistant(json!([{"type": "text", "text": "and now this"}])));
        assert_eq!(opening_of(&b), Some(TurnOpening::VendorContinuation));
    }

    /// GitHub #199 — a line ccteam injects mid-turn is shown to the model as a
    /// queued command and then RE-RUN as the next prompt, so the turn it joined
    /// is not where it is answered. The joined turn's boundary therefore
    /// settles nothing, and the replay turn's does.
    #[test]
    fn an_injected_line_is_answered_by_the_replay_turn_not_the_one_it_joined() {
        let identity = Arc::new(Mutex::new(TurnIdentity::default()));
        let mut t = StreamTranslator::attached(Arc::clone(&identity));
        t.ingest(assistant(json!([{"type": "text", "text": "working"}])));
        // The submit path's mid-turn branch: the line joins the running turn
        // and claude is now holding it to re-run.
        identity.lock().unwrap().note_injected();
        let joined = t.ingest(result_ok("working"));
        assert_eq!(
            continuation_of(&joined),
            Some(TurnContinuation::Pending),
            "the injected line has not been read as a prompt yet"
        );

        let replay = t.ingest(assistant(json!([{"type": "text", "text": "done"}])));
        assert_eq!(opening_of(&replay), Some(TurnOpening::VendorContinuation));
        let replay = [replay, t.ingest(result_ok("done"))].concat();
        assert_eq!(continuation_of(&replay), Some(TurnContinuation::Settled));
    }

    /// The two facts are independent: an empty snapshot alone does not settle a
    /// turn that still owes a replay, and no pending replay does not settle a
    /// turn whose vendor is still running something.
    #[test]
    fn either_held_fact_alone_keeps_a_boundary_open() {
        let identity = Arc::new(Mutex::new(TurnIdentity::default()));
        let mut t = StreamTranslator::attached(Arc::clone(&identity));
        t.ingest(background(&[]));
        // Injected INTO the running turn, which is the only moment the submit
        // path can be holding a line claude will re-run: a flag set before any
        // turn is open belongs to the turn that opens next and is consumed by
        // it, exactly as `TurnIdentity::open` does.
        t.ingest(assistant(json!([{"type": "text", "text": "a"}])));
        identity.lock().unwrap().note_injected();
        assert_eq!(
            continuation_of(&t.ingest(result_ok("a"))),
            Some(TurnContinuation::Pending)
        );

        let mut t = StreamTranslator::new();
        t.ingest(background(&["bg-1"]));
        t.ingest(assistant(json!([{"type": "text", "text": "b"}])));
        assert_eq!(
            continuation_of(&t.ingest(result_ok("b"))),
            Some(TurnContinuation::Pending)
        );
    }

    /// An ordinary turn with nothing in the background settles — the common
    /// case must not have been made non-terminal by any of the above.
    #[test]
    fn a_plain_turn_settles() {
        let mut t = StreamTranslator::new();
        t.ingest(assistant(json!([{"type": "text", "text": "hi"}])));
        assert_eq!(
            continuation_of(&t.ingest(result_ok("hi"))),
            Some(TurnContinuation::Settled)
        );
        // …and a snapshot that empties again releases the hold.
        t.ingest(background(&["bg-1"]));
        t.ingest(assistant(json!([{"type": "text", "text": "x"}])));
        assert_eq!(
            continuation_of(&t.ingest(result_ok("x"))),
            Some(TurnContinuation::Pending)
        );
        t.ingest(background(&[]));
        t.ingest(assistant(json!([{"type": "text", "text": "y"}])));
        assert_eq!(
            continuation_of(&t.ingest(result_ok("y"))),
            Some(TurnContinuation::Settled)
        );
    }

    /// A stream that dies takes the expectation with it: nothing will replay a
    /// line into a child that is gone, so the next life must not open its first
    /// turn already believing it owes a continuation.
    #[test]
    fn a_closed_stream_forgets_what_it_was_holding() {
        let identity = Arc::new(Mutex::new(TurnIdentity::default()));
        let mut t = StreamTranslator::attached(Arc::clone(&identity));
        t.ingest(background(&["bg-1"]));
        identity.lock().unwrap().note_injected();
        t.ingest(assistant(json!([{"type": "text", "text": "working"}])));
        let _ = t.on_close();
        assert!(!identity.lock().unwrap().has_pending_replay());

        t.ingest(assistant(json!([{"type": "text", "text": "next life"}])));
        assert_eq!(
            continuation_of(&t.ingest(result_ok("next life"))),
            Some(TurnContinuation::Settled)
        );
    }
}
