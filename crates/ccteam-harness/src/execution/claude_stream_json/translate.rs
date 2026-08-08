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
//!   forwards to IM as a reply.
//! - A turn **failure** is [`ThreadEvent::TurnFailed`] (the pump forwards
//!   `err.message` verbatim → the honest in-flight-loss / error signal).
//! - Tool-use / thinking blocks become `ItemStarted{ToolCall}` /
//!   `ItemUpdated{Reasoning}` — progress-fold fodder only (the pump drops
//!   their text), so they never masquerade as the answer.
//!
//! Pure + synchronous: [`StreamTranslator::ingest`] takes one parsed
//! [`Outbound`] and returns the events it produced. The transport's
//! `events()` task owns one translator and drives it.

use serde_json::Value;

use super::protocol::{MessageEnvelope, Outbound, ResultMsg};
use crate::{ThreadErrorEvent, ThreadEvent, ThreadItem, ThreadItemDetails, UnifiedTokenUsage};

/// Per-session translation state. One per live stream-json session.
#[derive(Debug, Default)]
pub struct StreamTranslator {
    /// Monotonic per-session turn counter (synthesizes turn ids; the pump
    /// keys turns.jsonl off its OWN seq, so these need only be unique).
    turn_seq: u64,
    /// `Some` while a turn is in flight (between first assistant block and
    /// its `result`).
    active_turn: Option<String>,
    /// Accumulated assistant text for the active turn — the fallback final
    /// text when `result.result` is empty.
    acc_text: String,
    /// Item-id counter for tool/reasoning items within a turn.
    item_seq: u64,
    /// Canonical model id (`message.model`) of the active turn's latest
    /// assistant message — the deterministic per-turn cost source. The
    /// `result` line carries no model, so we carry it forward from the
    /// assistant block(s). A turn can mix models (e.g. a sonnet sub-turn);
    /// the LAST assistant model wins for the turn's headline cost — the
    /// transcript path prices the finer per-message split.
    turn_model: Option<String>,
}

impl StreamTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one outbound message, returning the neutral events it
    /// produced (possibly empty).
    pub fn ingest(&mut self, out: Outbound) -> Vec<ThreadEvent> {
        match out {
            Outbound::Assistant(env) => self.on_assistant(env),
            Outbound::TurnResult(r) => self.on_result(r),
            // `user` replay echoes, init, control frames, partials: no
            // neutral event (transcript authority + HITL handled elsewhere).
            Outbound::User(_)
            | Outbound::System(_)
            | Outbound::ControlRequest(_)
            | Outbound::ControlResponse(_)
            | Outbound::Other => Vec::new(),
        }
    }

    fn ensure_turn_started(&mut self, out: &mut Vec<ThreadEvent>) {
        if self.active_turn.is_none() {
            self.turn_seq += 1;
            let id = format!("sj-{}", self.turn_seq);
            self.active_turn = Some(id.clone());
            self.acc_text.clear();
            self.item_seq = 0;
            self.turn_model = None;
            out.push(ThreadEvent::TurnStarted { turn_id: id });
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
        let model = self.turn_model.take();
        self.active_turn
            .take()
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
        if !text.is_empty() {
            if !self.acc_text.is_empty() {
                self.acc_text.push('\n');
            }
            self.acc_text.push_str(&text);
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
        let turn_id = self
            .active_turn
            .take()
            .unwrap_or_else(|| "sj-0".to_string());
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
            return out;
        }

        // Success: final text = result.result if non-empty, else the
        // accumulated assistant text. Emit the answer FIRST (so the pump
        // finalizes the turn's progress epoch before the boundary event),
        // then TurnCompleted with usage.
        let final_text = r
            .result
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| std::mem::take(&mut self.acc_text));
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
        });
        self.acc_text.clear();
        out
    }
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
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
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
                ThreadEvent::TurnStarted { turn_id } => Some(turn_id.clone()),
                _ => None,
            })
        };
        assert_ne!(id(&a), id(&b));
    }
}
