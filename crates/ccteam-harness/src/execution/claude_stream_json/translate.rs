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
            self.last_text = None;
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
        self.last_text = None;
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
                ThreadEvent::TurnStarted { turn_id } => Some(turn_id.clone()),
                _ => None,
            })
        };
        assert_ne!(id(&a), id(&b));
    }
}
