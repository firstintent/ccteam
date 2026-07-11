//! Thin ACP wire types shared by Grok Build and OpenCode.
//!
//! Grok fixtures pin CLI `0.2.93`; OpenCode fixtures pin release `1.17.17`.
//! Unknown fields are ignored.

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

/// Map `session/prompt` result → usage.
///
/// - **Grok**: fields live under `_meta` (`inputTokens`, `cachedReadTokens`, …).
/// - **OpenCode**: fields live under top-level `usage` (`inputTokens`,
///   `outputTokens`, `totalTokens`); cost is **not** here — it arrives via
///   `usage_update` session notification as session-cumulative USD.
pub fn usage_from_prompt_result(result: &Value) -> (crate::UnifiedTokenUsage, Option<String>) {
    let meta = result.get("_meta").cloned().unwrap_or(Value::Null);
    let usage_block = result.get("usage").cloned().unwrap_or(Value::Null);

    let input_tokens = usage_block
        .get("inputTokens")
        .or_else(|| meta.get("inputTokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage_block
        .get("outputTokens")
        .or_else(|| meta.get("outputTokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cached_input_tokens = usage_block
        .get("cachedInputTokens")
        .or_else(|| meta.get("cachedReadTokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reasoning_output_tokens = usage_block
        .get("reasoningTokens")
        .or_else(|| meta.get("reasoningTokens"))
        .and_then(|v| v.as_u64());

    let usage = crate::UnifiedTokenUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_creation_input_tokens: None,
        reasoning_output_tokens,
        reported_cost_usd: None, // filled by translate from usage_update delta
    };
    let model = meta
        .get("modelId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (usage, model)
}

/// Pull session-cumulative USD from a `usage_update` payload.
/// Returns `None` when missing or zero (upstream WIP cost=0 → UI "—").
pub fn cost_from_usage_update(update: &Value) -> Option<f64> {
    let amount = update
        .pointer("/cost/amount")
        .or_else(|| update.get("cost").and_then(|c| c.get("amount")))
        .and_then(|v| v.as_f64().or_else(|| v.as_u64().map(|u| u as f64)))?;
    if amount > 0.0 {
        Some(amount)
    } else {
        None
    }
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

/// Model + context window + reasoning effort from a `session/new` /
/// `session/load` result.
#[derive(Debug, Clone, Default)]
pub struct ModelInfo {
    pub model: Option<String>,
    pub window: Option<u64>,
    pub effort: Option<String>,
}

/// Pull model info from a `session/new` / `session/load` / `session/resume` result.
///
/// - **Grok**: `models.currentModelId` + availableModels `_meta`.
/// - **OpenCode**: `configOptions` with `id=model|effort` (`currentValue`).
pub fn pluck_model_info(result: &Value) -> ModelInfo {
    // OpenCode path first: configOptions present without models block.
    if let Some(opts) = result.get("configOptions").and_then(|v| v.as_array()) {
        let model = opts
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("model"))
            .and_then(|o| o.get("currentValue"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let effort = opts
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("effort"))
            .and_then(|o| o.get("currentValue"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if model.is_some() || effort.is_some() {
            return ModelInfo {
                model,
                window: None,
                effort,
            };
        }
    }

    let models = result.get("models");
    let model = models
        .and_then(|m| m.get("currentModelId"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let selected = models
        .and_then(|m| m.get("availableModels"))
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            let want = model.as_deref();
            arr.iter()
                .find(|m| m.get("modelId").and_then(|v| v.as_str()) == want)
                .or_else(|| arr.first())
        });
    let meta = selected.and_then(|m| m.get("_meta"));
    let window = meta
        .and_then(|meta| meta.get("totalContextTokens"))
        .and_then(|v| v.as_u64());
    let effort = meta
        .and_then(|meta| meta.get("reasoningEffort"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    ModelInfo {
        model,
        window,
        effort,
    }
}

/// True for a turn-completion signal — the FIFO-ordered `turn_completed`
/// update or a `prompt_complete` notification. Used as the barrier that
/// guarantees all `agent_message_chunk` frames were buffered before the
/// prompt response finalizes the turn.
pub fn is_turn_boundary(method: &str, params: &Value) -> bool {
    if method.ends_with("prompt_complete") {
        return true;
    }
    for scope in [params.get("update"), Some(params)].into_iter().flatten() {
        let kind = scope
            .get("sessionUpdate")
            .or_else(|| scope.get("type"))
            .and_then(|v| v.as_str());
        if kind == Some("turn_completed") {
            return true;
        }
    }
    false
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
        assert!(u.reported_cost_usd.is_none());
    }

    #[test]
    fn usage_maps_opencode_top_level_usage() {
        let result = json!({
            "stopReason": "end_turn",
            "usage": {"inputTokens": 9538, "outputTokens": 21, "totalTokens": 9559},
            "_meta": {}
        });
        let (u, m) = usage_from_prompt_result(&result);
        assert_eq!(u.input_tokens, 9538);
        assert_eq!(u.output_tokens, 21);
        assert!(m.is_none());
    }

    #[test]
    fn cost_from_usage_update_skips_zero() {
        assert!(cost_from_usage_update(&json!({"cost":{"amount":0,"currency":"USD"}})).is_none());
        assert_eq!(
            cost_from_usage_update(&json!({"cost":{"amount":0.012,"currency":"USD"}})),
            Some(0.012)
        );
    }

    #[test]
    fn model_info_from_opencode_config_options() {
        let result = json!({
            "sessionId": "ses_abc",
            "configOptions": [
                {"id":"model","currentValue":"tokenopen/gpt-5.5"},
                {"id":"effort","currentValue":"high"}
            ]
        });
        let info = pluck_model_info(&result);
        assert_eq!(info.model.as_deref(), Some("tokenopen/gpt-5.5"));
        assert_eq!(info.effort.as_deref(), Some("high"));
    }

    #[test]
    fn is_replay_detects_meta() {
        assert!(is_replay(&json!({"_meta":{"isReplay":true}})));
        assert!(!is_replay(&json!({"_meta":{"isReplay":false}})));
        assert!(!is_replay(&json!({})));
    }

    #[test]
    fn model_info_pulls_model_window_and_effort() {
        let result = json!({
            "sessionId": "abc",
            "models": {
                "currentModelId": "grok-4.5",
                "availableModels": [{
                    "modelId": "grok-4.5",
                    "_meta": { "totalContextTokens": 500000, "reasoningEffort": "high" }
                }]
            }
        });
        let info = pluck_model_info(&result);
        assert_eq!(info.model.as_deref(), Some("grok-4.5"));
        assert_eq!(info.window, Some(500000));
        assert_eq!(info.effort.as_deref(), Some("high"));
    }

    #[test]
    fn turn_boundary_matches_completed_and_prompt_complete() {
        assert!(is_turn_boundary(
            "_x.ai/session_notification",
            &json!({"update":{"sessionUpdate":"turn_completed"}})
        ));
        // fake shape: `type` at params top level.
        assert!(is_turn_boundary(
            "_x.ai/session_notification",
            &json!({"type":"turn_completed"})
        ));
        assert!(is_turn_boundary(
            "_x.ai/session/prompt_complete",
            &json!({})
        ));
        assert!(!is_turn_boundary(
            "session/update",
            &json!({"update":{"sessionUpdate":"agent_message_chunk"}})
        ));
    }
}
