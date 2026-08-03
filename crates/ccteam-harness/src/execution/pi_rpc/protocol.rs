//! Minimal wire model for the stable `pi --mode rpc` JSONL protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiResponse {
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub context_window: u64,
}

impl PiModel {
    pub fn canonical_id(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSessionState {
    #[serde(default)]
    pub model: Option<PiModel>,
    pub thinking_level: String,
    #[serde(default)]
    pub is_streaming: bool,
    #[serde(default)]
    pub session_file: Option<String>,
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PiAvailableModels {
    pub models: Vec<PiModel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PiThinkingLevels {
    pub levels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSessionStats {
    pub session_file: String,
    pub session_id: String,
    #[serde(default)]
    pub context_usage: Option<PiContextUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiContextUsage {
    pub tokens: Option<u64>,
    pub context_window: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(default)]
    pub cost: PiUsageCost,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct PiUsageCost {
    #[serde(default)]
    pub total: f64,
}

#[derive(Debug, Clone)]
pub enum PiEvent {
    AgentStart,
    AgentEnd { will_retry: bool },
    AgentSettled,
    TurnStart,
    TurnEnd,
    MessageEnd { message: Value },
    CompactionEnd { usage: Option<PiUsage> },
    AutoRetryStart,
    AutoRetryEnd { success: bool },
    ToolExecutionStart { name: String, args: Value },
    ToolExecutionEnd { name: String, is_error: bool },
    ExtensionUiRequest(PiExtensionUiRequest),
    ExtensionError { event: String, error: String },
    Activity,
}

#[derive(Debug, Clone)]
pub struct PiExtensionUiRequest {
    pub id: String,
    pub method: String,
    pub payload: Value,
}

/// Parse only fields the adapter consumes. Unknown event types are returned as
/// `Ok(None)` so the transport can warn once and continue.
pub fn parse_event(value: Value) -> Result<Option<PiEvent>, String> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "Pi record missing string type".to_string())?;
    let event = match kind {
        "agent_start" => PiEvent::AgentStart,
        "agent_end" => PiEvent::AgentEnd {
            will_retry: value
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "agent_settled" => PiEvent::AgentSettled,
        "turn_start" => PiEvent::TurnStart,
        "turn_end" => PiEvent::TurnEnd,
        "message_end" => PiEvent::MessageEnd {
            message: value
                .get("message")
                .cloned()
                .ok_or_else(|| "Pi message_end missing message".to_string())?,
        },
        "compaction_end" => {
            let usage = value
                .pointer("/result/usage")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid Pi compaction usage: {error}"))?;
            PiEvent::CompactionEnd { usage }
        }
        "auto_retry_start" => PiEvent::AutoRetryStart,
        "auto_retry_end" => PiEvent::AutoRetryEnd {
            success: value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "tool_execution_start" => PiEvent::ToolExecutionStart {
            name: value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
            args: value.get("args").cloned().unwrap_or(Value::Null),
        },
        "tool_execution_end" => PiEvent::ToolExecutionEnd {
            name: value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
            is_error: value
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "extension_ui_request" => PiEvent::ExtensionUiRequest(PiExtensionUiRequest {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "Pi extension UI request missing string id".to_string())?
                .to_string(),
            method: value
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| "Pi extension UI request missing string method".to_string())?
                .to_string(),
            payload: value,
        }),
        "extension_error" => PiEvent::ExtensionError {
            event: value
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown extension error")
                .to_string(),
        },
        "message_start"
        | "message_update"
        | "tool_execution_update"
        | "queue_update"
        | "compaction_start"
        | "summarization_retry_scheduled"
        | "summarization_retry_attempt_start"
        | "summarization_retry_finished"
        | "bash_execution_update" => PiEvent::Activity,
        _ => return Ok(None),
    };
    Ok(Some(event))
}

pub fn response_data<T: for<'de> Deserialize<'de>>(response: PiResponse) -> Result<T, String> {
    if !response.success {
        return Err(response
            .error
            .unwrap_or_else(|| format!("Pi {} request rejected", response.command)));
    }
    serde_json::from_value(response.data.unwrap_or(Value::Null))
        .map_err(|error| format!("invalid Pi {} response: {error}", response.command))
}
