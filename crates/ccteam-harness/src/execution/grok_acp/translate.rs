//! Final-only translate: ACP notifications + prompt response → ThreadEvent.
//!
//! Turn-end SoT = matching `session/prompt` JSON-RPC **response** (not
//! `turn_completed` notifications). Buffer only `agent_message_chunk`;
//! drop thoughts and `isReplay` frames.

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::Value;

use super::protocol::{content_text, is_replay, usage_from_prompt_result, AvailableCommand};
use super::transport::Notification;
use crate::{ThreadErrorEvent, ThreadEvent, ThreadItem, ThreadItemDetails};

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
    pub available_commands: Vec<AvailableCommand>,
    pub model: Option<String>,
    pub window_tokens: Option<u64>,
    pub used_tokens: Option<u64>,
    pub effort: Option<String>,
    /// Methods we already warn-once skipped.
    pub warned_methods: HashSet<String>,
}

impl SessionTranslateState {
    pub fn begin_turn(&mut self, turn_id: impl Into<String>) {
        self.buffer = Some(TurnBuffer {
            turn_id: turn_id.into(),
            text: String::new(),
        });
    }

    pub fn append_message(&mut self, chunk: &str) {
        if let Some(b) = self.buffer.as_mut() {
            b.text.push_str(chunk);
        }
    }

    pub fn take_buffer(&mut self) -> Option<TurnBuffer> {
        self.buffer.take()
    }
}

/// Apply one notification. Returns optional mid-stream events (tools).
/// Does **not** emit final agent message — that waits for the prompt response.
pub fn apply_notification(state: &mut SessionTranslateState, n: &Notification) -> Vec<ThreadEvent> {
    // Drop full replay frames from session/load.
    if is_replay(&n.params) {
        return Vec::new();
    }

    // Standard session/update path.
    if n.method == "session/update" || n.method.ends_with("session/update") {
        return apply_session_update(state, &n.params);
    }

    // Underscore-prefixed x.ai noise + other unknown methods: warn once.
    if n.method.starts_with("_x.ai/") || n.method.starts_with("x.ai/") {
        if state.warned_methods.insert(n.method.clone()) {
            tracing::warn!(
                method = %n.method,
                "grok_acp: skipping unknown vendor notification (warn-once)"
            );
        }
        // Still peek inside for isReplay already handled; some replays use
        // `_x.ai/session/update` with isReplay — already dropped above if params carry it.
        // If nested update exists without top-level isReplay, try nested.
        if let Some(update) = n.params.get("update") {
            if is_replay(&n.params)
                || update
                    .get("_meta")
                    .and_then(|m| m.get("isReplay"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            {
                return Vec::new();
            }
        }
        return Vec::new();
    }

    if state.warned_methods.insert(n.method.clone()) {
        tracing::warn!(
            method = %n.method,
            "grok_acp: skipping unknown notification method (warn-once)"
        );
    }
    Vec::new()
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
            if let Some(text) = extract_chunk_text(&update) {
                state.append_message(&text);
            }
            Vec::new()
        }
        "agent_thought_chunk" | "user_message_chunk" => {
            // Thoughts never enter final; user echo ignored.
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
        "plan" => Vec::new(),
        _ => {
            if !kind.is_empty() && state.warned_methods.insert(format!("update:{kind}")) {
                tracing::warn!(
                    kind,
                    "grok_acp: skipping unknown sessionUpdate kind (warn-once)"
                );
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

/// Finalize a turn from the `session/prompt` response (authoritative).
pub fn finalize_from_prompt_result(
    state: &mut SessionTranslateState,
    result: &Value,
) -> Vec<ThreadEvent> {
    let (usage, model) = usage_from_prompt_result(result);
    if let Some(m) = model.clone() {
        state.model = Some(m);
    }
    if let Some(total) = result
        .pointer("/_meta/totalTokens")
        .and_then(|v| v.as_u64())
    {
        state.used_tokens = Some(total);
    }
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
    use serde_json::json;

    #[test]
    fn buffers_message_not_thought_and_finalizes_once() {
        let mut st = SessionTranslateState::default();
        st.begin_turn("t1");
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
}
