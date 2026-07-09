//! Thin ACP wire types for Grok Build (`grok agent stdio`).
//!
//! Fixtures pin grok CLI `0.2.93` (dev-plan §11). Unknown fields are ignored.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One entry from `session/update{available_commands_update}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input: Option<Value>,
}

/// Map `session/prompt` result `_meta` → usage fields (raw JSON).
pub fn usage_from_prompt_result(result: &Value) -> (crate::UnifiedTokenUsage, Option<String>) {
    let meta = result.get("_meta").cloned().unwrap_or(Value::Null);
    let usage = crate::UnifiedTokenUsage {
        input_tokens: meta
            .get("inputTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output_tokens: meta
            .get("outputTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cached_input_tokens: meta
            .get("cachedReadTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_creation_input_tokens: None,
        reasoning_output_tokens: meta.get("reasoningTokens").and_then(|v| v.as_u64()),
    };
    let model = meta
        .get("modelId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (usage, model)
}

/// Extract text from an ACP content block (`{type:"text", text:"…"}`) or a bare string.
pub fn content_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    content
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// Pull `sessionId` from `session/new` result.
pub fn pluck_session_id(result: &Value) -> Option<String> {
    result
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Pull `currentModelId` + window from `session/new` / `session/load` result.
pub fn pluck_model_and_window(result: &Value) -> (Option<String>, Option<u64>) {
    let models = result.get("models");
    let model = models
        .and_then(|m| m.get("currentModelId"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let window = models
        .and_then(|m| m.get("availableModels"))
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            let want = model.as_deref();
            arr.iter()
                .find(|m| m.get("modelId").and_then(|v| v.as_str()) == want)
                .or_else(|| arr.first())
        })
        .and_then(|m| m.get("_meta"))
        .and_then(|meta| meta.get("totalContextTokens"))
        .and_then(|v| v.as_u64());
    (model, window)
}

/// True when `_meta.isReplay == true` (session/load history replay).
pub fn is_replay(params: &Value) -> bool {
    params
        .pointer("/_meta/isReplay")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            params
                .get("update")
                .and_then(|u| u.pointer("/_meta/isReplay"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false)
        || params
            .get("_meta")
            .and_then(|m| m.get("isReplay"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_maps_section11_fields() {
        let result = json!({
            "stopReason": "end_turn",
            "_meta": {
                "inputTokens": 15587,
                "outputTokens": 29,
                "cachedReadTokens": 11136,
                "reasoningTokens": 23,
                "totalTokens": 15617,
                "modelId": "grok-4.5"
            }
        });
        let (u, m) = usage_from_prompt_result(&result);
        assert_eq!(u.input_tokens, 15587);
        assert_eq!(u.output_tokens, 29);
        assert_eq!(u.cached_input_tokens, 11136);
        assert_eq!(u.reasoning_output_tokens, Some(23));
        assert_eq!(m.as_deref(), Some("grok-4.5"));
    }

    #[test]
    fn is_replay_detects_meta() {
        assert!(is_replay(&json!({"_meta":{"isReplay":true}})));
        assert!(!is_replay(&json!({"_meta":{"isReplay":false}})));
        assert!(!is_replay(&json!({})));
    }
}
