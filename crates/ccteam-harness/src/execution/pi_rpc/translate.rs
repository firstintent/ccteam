//! Pi session-level event translation. `agent_settled` is the sole terminal.

use serde_json::Value;

use super::protocol::{PiEvent, PiUsage};
use crate::{ThreadErrorEvent, ThreadEvent, ThreadItem, ThreadItemDetails, UnifiedTokenUsage};

#[derive(Debug, Default)]
pub struct TranslateOutput {
    pub events: Vec<ThreadEvent>,
    pub settled: bool,
}

#[derive(Debug, Default)]
pub struct PiTurnTranslator {
    active: Option<TurnBuffer>,
}

#[derive(Debug)]
struct TurnBuffer {
    turn_id: String,
    started: bool,
    item_seq: u64,
    usage: UnifiedTokenUsage,
    model: Option<String>,
    terminal: Option<TerminalMessage>,
    protocol_error: Option<String>,
}

#[derive(Debug, Clone)]
struct TerminalMessage {
    stop_reason: String,
    error_message: Option<String>,
    text: Option<String>,
    has_tool_call: bool,
}

impl PiTurnTranslator {
    pub fn begin(&mut self, turn_id: String) -> Result<(), String> {
        if self.active.is_some() {
            return Err("Pi canonical turn already active".to_string());
        }
        self.active = Some(TurnBuffer {
            turn_id,
            started: false,
            item_seq: 0,
            usage: UnifiedTokenUsage::default(),
            model: None,
            terminal: None,
            protocol_error: None,
        });
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.active = None;
    }

    pub fn active_turn_id(&self) -> Option<&str> {
        self.active.as_ref().map(|turn| turn.turn_id.as_str())
    }

    pub fn ingest(&mut self, event: PiEvent) -> TranslateOutput {
        let Some(active) = self.active.as_mut() else {
            return TranslateOutput::default();
        };
        match event {
            PiEvent::AgentStart if !active.started => {
                active.started = true;
                TranslateOutput {
                    events: vec![ThreadEvent::TurnStarted {
                        turn_id: active.turn_id.clone(),
                    }],
                    settled: false,
                }
            }
            PiEvent::MessageEnd { message } => {
                if let Err(error) = active.ingest_message(&message) {
                    // `agent_settled` remains the sole normal terminal; a bad
                    // message poisons the pending outcome instead of ending it.
                    active.protocol_error = Some(error);
                }
                TranslateOutput::default()
            }
            PiEvent::CompactionEnd { usage } => {
                if let Some(usage) = usage {
                    active.add_usage(usage);
                }
                TranslateOutput::default()
            }
            PiEvent::ToolExecutionStart { name, args } => {
                active.item_seq += 1;
                TranslateOutput {
                    events: vec![ThreadEvent::ItemStarted {
                        item: ThreadItem {
                            id: format!("{}-tool-{}", active.turn_id, active.item_seq),
                            details: ThreadItemDetails::ToolCall { name, args },
                        },
                    }],
                    settled: false,
                }
            }
            PiEvent::ToolExecutionEnd { name, is_error } => {
                active.item_seq += 1;
                TranslateOutput {
                    events: vec![ThreadEvent::ItemCompleted {
                        item: ThreadItem {
                            id: format!("{}-tool-{}", active.turn_id, active.item_seq),
                            details: ThreadItemDetails::CommandExecution {
                                cmd: name,
                                status: if is_error { "failed" } else { "completed" }.to_string(),
                            },
                        },
                    }],
                    settled: false,
                }
            }
            PiEvent::AgentSettled => self.settle(),
            PiEvent::AgentStart
            | PiEvent::AgentEnd { .. }
            | PiEvent::TurnStart
            | PiEvent::TurnEnd
            | PiEvent::AutoRetryStart
            | PiEvent::AutoRetryEnd { .. }
            | PiEvent::ExtensionUiRequest(_)
            | PiEvent::ExtensionError { .. }
            | PiEvent::Activity => TranslateOutput::default(),
        }
    }

    pub fn transport_failed(&mut self, message: String) -> TranslateOutput {
        if self.active.is_none() {
            return TranslateOutput::default();
        }
        self.fail_active("protocol", message)
    }

    fn settle(&mut self) -> TranslateOutput {
        let Some(mut active) = self.active.take() else {
            return TranslateOutput::default();
        };
        if let Some(error) = active.protocol_error.take() {
            return terminal_failure(active, "protocol", &error);
        }
        let Some(terminal) = active.terminal.take() else {
            return terminal_failure(
                active,
                "protocol",
                "Pi settled without a terminal assistant message",
            );
        };
        match terminal.stop_reason.as_str() {
            "stop" => {
                let Some(text) = terminal.text.filter(|text| !text.trim().is_empty()) else {
                    return terminal_failure(
                        active,
                        "protocol",
                        "Pi stop message had no final assistant text",
                    );
                };
                active.item_seq += 1;
                TranslateOutput {
                    events: vec![
                        ThreadEvent::ItemCompleted {
                            item: ThreadItem {
                                id: format!("{}-agent-{}", active.turn_id, active.item_seq),
                                details: ThreadItemDetails::AgentMessage(text),
                            },
                        },
                        ThreadEvent::TurnCompleted {
                            turn_id: active.turn_id,
                            usage: active.usage,
                            model: active.model,
                        },
                    ],
                    settled: true,
                }
            }
            "toolUse" => {
                let mut events = Vec::new();
                if !terminal.has_tool_call {
                    if let Some(text) = terminal.text.filter(|text| !text.trim().is_empty()) {
                        active.item_seq += 1;
                        events.push(ThreadEvent::ItemCompleted {
                            item: ThreadItem {
                                id: format!("{}-agent-{}", active.turn_id, active.item_seq),
                                details: ThreadItemDetails::AgentMessage(text),
                            },
                        });
                    }
                }
                events.push(ThreadEvent::TurnCompleted {
                    turn_id: active.turn_id,
                    usage: active.usage,
                    model: active.model,
                });
                TranslateOutput {
                    events,
                    settled: true,
                }
            }
            "length" => {
                terminal_failure(active, "max_tokens", "Pi exhausted the output token window")
            }
            "error" => terminal_failure(
                active,
                "vendor_error",
                terminal
                    .error_message
                    .as_deref()
                    .unwrap_or("Pi provider returned an error"),
            ),
            "aborted" => terminal_failure(
                active,
                "aborted",
                terminal
                    .error_message
                    .as_deref()
                    .unwrap_or("Pi turn aborted"),
            ),
            unknown => terminal_failure(
                active,
                "protocol",
                &format!("unknown Pi terminal stop reason `{unknown}`"),
            ),
        }
    }

    fn fail_active(&mut self, kind: &str, message: String) -> TranslateOutput {
        let active = self.active.take().expect("active checked by caller");
        terminal_failure(active, kind, &message)
    }
}

impl TurnBuffer {
    fn ingest_message(&mut self, message: &Value) -> Result<(), String> {
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let usage: PiUsage =
                    serde_json::from_value(message.get("usage").cloned().unwrap_or(Value::Null))
                        .map_err(|error| format!("invalid Pi assistant usage: {error}"))?;
                self.add_usage(usage);
                let provider = required_string(message, "provider")?;
                let model = message
                    .get("responseModel")
                    .and_then(Value::as_str)
                    .or_else(|| message.get("model").and_then(Value::as_str))
                    .ok_or_else(|| "Pi assistant message missing model".to_string())?;
                self.model = Some(format!("{provider}/{model}"));
                let stop_reason = required_string(message, "stopReason")?;
                if stop_reason == "pending" {
                    return Ok(());
                }
                let content = message
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "Pi assistant content is not an array".to_string())?;
                let text = content
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("");
                let has_tool_call = content
                    .iter()
                    .any(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"));
                self.terminal = Some(TerminalMessage {
                    stop_reason,
                    error_message: message
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    text: (!text.is_empty()).then_some(text),
                    has_tool_call,
                });
            }
            Some("toolResult") => {
                if let Some(usage) = message.get("usage") {
                    let usage: PiUsage = serde_json::from_value(usage.clone())
                        .map_err(|error| format!("invalid Pi tool-result usage: {error}"))?;
                    self.add_usage(usage);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn add_usage(&mut self, usage: PiUsage) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output);
        self.usage.cached_input_tokens = self
            .usage
            .cached_input_tokens
            .saturating_add(usage.cache_read);
        self.usage.cache_creation_input_tokens = Some(
            self.usage
                .cache_creation_input_tokens
                .unwrap_or(0)
                .saturating_add(usage.cache_write),
        );
        self.usage.reported_cost_usd =
            Some(self.usage.reported_cost_usd.unwrap_or(0.0) + usage.cost.total);
        // Pi documents `reasoning` as a subset of `output`; intentionally do
        // not populate reasoning_output_tokens or it would be billed twice.
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Pi message missing string {field}"))
}

fn terminal_failure(active: TurnBuffer, kind: &str, message: &str) -> TranslateOutput {
    TranslateOutput {
        events: vec![ThreadEvent::TurnFailed {
            turn_id: active.turn_id,
            err: ThreadErrorEvent {
                kind: kind.to_string(),
                message: message.to_string(),
            },
            usage: active.usage,
            model: active.model,
        }],
        settled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u64, output: u64, cost: f64, reasoning: u64) -> Value {
        json!({
            "input": input, "output": output, "cacheRead": 2,
            "cacheWrite": 3, "reasoning": reasoning, "totalTokens": input + output + 5,
            "cost": {"total": cost}
        })
    }

    fn assistant(reason: &str, text: &str, tool: bool, usage_value: Value) -> PiEvent {
        let mut content = vec![json!({"type":"text", "text":text})];
        if tool {
            content.push(json!({"type":"toolCall", "id":"call-1", "name":"bash", "arguments":{}}));
        }
        PiEvent::MessageEnd {
            message: json!({
                "role":"assistant", "content":content, "provider":"anthropic",
                "model":"requested", "responseModel":"resolved", "usage":usage_value,
                "stopReason":reason, "errorMessage":"boom"
            }),
        }
    }

    #[test]
    fn low_level_boundaries_and_retry_do_not_complete_before_settled() {
        let mut translator = PiTurnTranslator::default();
        translator.begin("t1".into()).unwrap();
        assert_eq!(translator.ingest(PiEvent::AgentStart).events.len(), 1);
        for _ in 0..3 {
            assert!(translator.ingest(PiEvent::TurnEnd).events.is_empty());
        }
        assert!(translator
            .ingest(PiEvent::AgentEnd { will_retry: true })
            .events
            .is_empty());
        translator.ingest(assistant("error", "retry", false, usage(1, 1, 0.1, 1)));
        translator.ingest(assistant("stop", "final", false, usage(2, 2, 0.2, 1)));
        let output = translator.ingest(PiEvent::AgentSettled);
        assert!(output.settled);
        assert_eq!(output.events.len(), 2);
        assert!(matches!(
            output.events[1],
            ThreadEvent::TurnCompleted { .. }
        ));
    }

    #[test]
    fn tool_preamble_is_never_the_final_answer() {
        let mut translator = PiTurnTranslator::default();
        translator.begin("t2".into()).unwrap();
        translator.ingest(assistant(
            "toolUse",
            "I will run it",
            true,
            usage(1, 1, 0.1, 0),
        ));
        let output = translator.ingest(PiEvent::AgentSettled);
        assert_eq!(output.events.len(), 1);
        assert!(matches!(
            output.events[0],
            ThreadEvent::TurnCompleted { .. }
        ));
    }

    #[test]
    fn malformed_message_waits_for_agent_settled_before_failing() {
        let mut translator = PiTurnTranslator::default();
        translator.begin("bad".into()).unwrap();
        let early = translator.ingest(PiEvent::MessageEnd {
            message: json!({"role":"assistant", "content":[]}),
        });
        assert!(!early.settled);
        assert!(early.events.is_empty());
        let settled = translator.ingest(PiEvent::AgentSettled);
        assert!(matches!(
            settled.events.last(),
            Some(ThreadEvent::TurnFailed { err, .. }) if err.kind == "protocol"
        ));
    }

    #[test]
    fn all_terminal_stop_reasons_fail_closed() {
        for (reason, expected) in [
            ("stop", None),
            ("toolUse", None),
            ("length", Some("max_tokens")),
            ("error", Some("vendor_error")),
            ("aborted", Some("aborted")),
            ("future-reason", Some("protocol")),
        ] {
            let mut translator = PiTurnTranslator::default();
            translator.begin(format!("t-{reason}")).unwrap();
            translator.ingest(assistant(
                reason,
                "answer",
                reason == "toolUse",
                usage(1, 1, 0.1, 1),
            ));
            let output = translator.ingest(PiEvent::AgentSettled);
            match expected {
                None => assert!(matches!(
                    output.events.last(),
                    Some(ThreadEvent::TurnCompleted { .. })
                )),
                Some(kind) => match output.events.last().unwrap() {
                    ThreadEvent::TurnFailed { err, usage, .. } => {
                        assert_eq!(err.kind, kind);
                        assert_eq!(usage.input_tokens, 1);
                        assert_eq!(usage.output_tokens, 1);
                        assert_eq!(usage.reasoning_output_tokens, None);
                        assert_eq!(usage.reported_cost_usd, Some(0.1));
                    }
                    other => panic!("unexpected terminal: {other:?}"),
                },
            }
        }
        let mut missing = PiTurnTranslator::default();
        missing.begin("missing".into()).unwrap();
        assert!(matches!(
            missing.ingest(PiEvent::AgentSettled).events.last(),
            Some(ThreadEvent::TurnFailed { err, .. }) if err.kind == "protocol"
        ));
    }

    #[test]
    fn aggregates_assistant_tool_and_compaction_without_reasoning_double_count() {
        let mut translator = PiTurnTranslator::default();
        translator.begin("usage".into()).unwrap();
        translator.ingest(assistant("stop", "answer", false, usage(10, 20, 0.4, 9)));
        translator.ingest(PiEvent::MessageEnd {
            message: json!({"role":"toolResult", "usage": usage(3, 4, 0.2, 2)}),
        });
        translator.ingest(PiEvent::CompactionEnd {
            usage: serde_json::from_value(usage(5, 6, 0.3, 1)).ok(),
        });
        let output = translator.ingest(PiEvent::AgentSettled);
        let ThreadEvent::TurnCompleted { usage, model, .. } = output.events.last().unwrap() else {
            panic!("expected completion");
        };
        assert_eq!(usage.input_tokens, 18);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.cached_input_tokens, 6);
        assert_eq!(usage.cache_creation_input_tokens, Some(9));
        assert_eq!(usage.reasoning_output_tokens, None);
        assert!((usage.reported_cost_usd.unwrap() - 0.9).abs() < 1e-9);
        assert_eq!(model.as_deref(), Some("anthropic/resolved"));
    }
}
