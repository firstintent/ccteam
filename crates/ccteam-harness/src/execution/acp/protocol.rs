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

/// The `stopReason` an ACP vendor reports on its `session/prompt` result —
/// the ONLY place the protocol says how a turn ended.
///
/// A `session/prompt` result is a *successful* JSON-RPC response even when the
/// turn did not produce an answer (refused, truncated, cancelled), so a client
/// that ignores this field reports every vendor outcome as a clean answer:
/// half-finished text lands in `turns.jsonl` as the final reply and a
/// delegation parent is told the task completed. Parsing it here (shared by
/// every ACP vendor, present and future) is what keeps the completion contract
/// honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpStopReason {
    /// The vendor finished normally. Also the verdict for an **absent** field:
    /// not every build emits one, and treating silence as failure would break
    /// working vendors.
    EndTurn,
    /// `session/cancel` took effect — an explicit stop, not a defect. Whatever
    /// the vendor produced first is still the answer.
    Cancelled,
    /// Output window exhausted → the answer is truncated, not complete.
    MaxTokens,
    /// Vendor-side turn-request budget exhausted mid-task.
    MaxTurnRequests,
    /// The vendor declined to answer (also where kimi maps its own
    /// `blocked` / content-filtered turns).
    Refusal,
    /// A reason this ccteam does not know, kept verbatim. Reported as a
    /// failure on purpose: an unrecognized terminal state must never be
    /// laundered into "answered".
    Other(String),
}

impl AcpStopReason {
    /// The wire spelling, for error kinds and logs.
    pub fn wire(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::Cancelled => "cancelled",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Other(raw) => raw.as_str(),
        }
    }

    /// Whether the turn may be finalized as an ordinary completed answer.
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::EndTurn | Self::Cancelled)
    }

    /// The honest sentence for a non-clean outcome (`None` when clean). Goes
    /// to the user verbatim and into `turns.jsonl`'s `error`, so it names the
    /// wire reason rather than paraphrasing it.
    pub fn failure_message(&self) -> Option<String> {
        match self {
            Self::EndTurn | Self::Cancelled => None,
            Self::MaxTokens => Some(
                "⚠️ vendor ended the turn at its output limit (stopReason=max_tokens) — \
                 the reply above is truncated, not a finished answer."
                    .into(),
            ),
            Self::MaxTurnRequests => Some(
                "⚠️ vendor ended the turn at its request budget \
                 (stopReason=max_turn_requests) — the task did not finish."
                    .into(),
            ),
            Self::Refusal => Some(
                "⚠️ vendor refused this turn (stopReason=refusal) — no answer was produced.".into(),
            ),
            Self::Other(raw) => Some(format!(
                "⚠️ vendor ended the turn with an unrecognized stopReason={raw} — \
                 treating it as a failure rather than an answer."
            )),
        }
    }
}

/// Read `stopReason` off a `session/prompt` result.
///
/// Tolerant by design: top-level or `_meta`, and case/separator-insensitive
/// (`end_turn` / `endTurn` / `END-TURN` all land on [`AcpStopReason::EndTurn`])
/// because ACP implementations differ on spelling. Absent or blank → `EndTurn`.
pub fn stop_reason_from_prompt_result(result: &Value) -> AcpStopReason {
    let raw = result
        .get("stopReason")
        .or_else(|| result.pointer("/_meta/stopReason"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw.is_empty() {
        return AcpStopReason::EndTurn;
    }
    let normalized: String = raw
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    match normalized.as_str() {
        "endturn" => AcpStopReason::EndTurn,
        "cancelled" | "canceled" => AcpStopReason::Cancelled,
        "maxtokens" => AcpStopReason::MaxTokens,
        "maxturnrequests" => AcpStopReason::MaxTurnRequests,
        "refusal" | "refused" => AcpStopReason::Refusal,
        _ => AcpStopReason::Other(raw.to_string()),
    }
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

/// One entry from Grok `session/new|load` `models.availableModels[]`.
/// Drives the bare-`/model` NeedsChoice picker; never a hardcoded catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpModelOption {
    /// Wire id for `session/set_model.modelId` (e.g. `grok-4.5`).
    pub model_id: String,
    /// Human label (`Grok 4.5`); empty when vendor omitted `name`.
    pub name: String,
    /// Optional short description for picker subtitles.
    pub description: String,
    /// Context window tokens from `_meta.totalContextTokens`.
    pub window: Option<u64>,
    /// Allowed reasoning efforts (`low`/`medium`/`high`, …); empty when the
    /// model has no effort axis (e.g. composer).
    pub efforts: Vec<String>,
}

/// Model + context window + reasoning effort from a `session/new` /
/// `session/load` result.
#[derive(Debug, Clone, Default)]
pub struct ModelInfo {
    pub model: Option<String>,
    pub window: Option<u64>,
    pub effort: Option<String>,
    /// Vendor-supplied catalog for the bare-`/model` picker.
    ///
    /// - **Grok**: `models.availableModels[]` (live, changes with CLI upgrades).
    /// - **OpenCode**: `configOptions[id=model].options[]` (+ shared effort
    ///   levels from `configOptions[id=effort].options[]`).
    ///
    /// Never a ccteam-hardcoded name list.
    pub available: Vec<AcpModelOption>,
}

/// Build bare-`/model` picker options from a vendor-captured catalog.
/// One option per model, or per (model × effort) when that entry has an
/// effort axis — the picked `id` is exactly the `/model <id> [effort]` form.
pub fn acp_model_picker_options(models: &[AcpModelOption]) -> Vec<crate::ChoiceOption> {
    let mut out = Vec::new();
    for m in models {
        let label_base = if m.name.trim().is_empty() {
            m.model_id.clone()
        } else {
            m.name.clone()
        };
        if m.efforts.is_empty() {
            out.push(crate::ChoiceOption {
                id: m.model_id.clone(),
                label: label_base,
            });
        } else {
            for e in &m.efforts {
                out.push(crate::ChoiceOption {
                    id: format!("{} {e}", m.model_id),
                    label: format!("{label_base} ({e})"),
                });
            }
        }
    }
    out
}

/// Union of effort tokens from a vendor catalog (used to split trailing
/// `/model <id> <effort>` args). Empty when no model advertises efforts.
pub fn known_efforts(models: &[AcpModelOption]) -> Vec<String> {
    let mut out = Vec::new();
    for m in models {
        for e in &m.efforts {
            if !out.iter().any(|x| x == e) {
                out.push(e.clone());
            }
        }
    }
    out
}

/// Split `/model` arg into `(modelId, effort?)`. The trailing whitespace
/// token is treated as effort **only** when it appears in `known_efforts`
/// (vendor-captured — never a hardcoded name list).
pub fn split_trailing_effort(arg: &str, known_efforts: &[String]) -> (String, Option<String>) {
    let arg = arg.trim();
    if known_efforts.is_empty() {
        return (arg.to_string(), None);
    }
    if let Some((head, tail)) = arg.rsplit_once(char::is_whitespace) {
        let t = tail.trim();
        let t_lower = t.to_ascii_lowercase();
        if let Some(matched) = known_efforts
            .iter()
            .find(|e| e.eq_ignore_ascii_case(&t_lower) || e.as_str() == t)
        {
            return (head.trim().to_string(), Some(matched.clone()));
        }
    }
    (arg.to_string(), None)
}

/// Pull model info from a `session/new` / `session/load` / `session/resume` result.
///
/// - **Grok**: `models.currentModelId` + full `availableModels` (+ `_meta`).
/// - **OpenCode**: `configOptions` with `id=model|effort` (current + options).
pub fn pluck_model_info(result: &Value) -> ModelInfo {
    // OpenCode path first: configOptions present without models block.
    if let Some(opts) = result.get("configOptions").and_then(|v| v.as_array()) {
        let model_entry = opts
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("model"));
        let effort_entry = opts
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("effort"));
        let model = model_entry
            .and_then(|o| o.get("currentValue"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let effort = effort_entry
            .and_then(|o| o.get("currentValue"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Shared effort axis (OpenCode models share one effort select).
        let effort_levels = effort_entry
            .and_then(|o| o.get("options"))
            .and_then(|a| a.as_array())
            .map(|arr| de_select_option_values(arr))
            .unwrap_or_default();
        let available = model_entry
            .and_then(|o| o.get("options"))
            .and_then(|a| a.as_array())
            .map(|arr| de_opencode_model_options(arr, &effort_levels))
            .unwrap_or_default();
        if model.is_some() || effort.is_some() || !available.is_empty() {
            return ModelInfo {
                model,
                window: None,
                effort,
                available,
            };
        }
    }

    let models = result.get("models");
    let model = models
        .and_then(|m| m.get("currentModelId"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let available = models
        .and_then(|m| m.get("availableModels"))
        .and_then(|a| a.as_array())
        .map(|arr| de_available_models(arr))
        .unwrap_or_default();
    let selected = available
        .iter()
        .find(|m| Some(m.model_id.as_str()) == model.as_deref())
        .or_else(|| available.first());
    let window = selected.and_then(|m| m.window);
    let effort = models
        .and_then(|m| m.get("availableModels"))
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            let want = model.as_deref();
            arr.iter()
                .find(|m| m.get("modelId").and_then(|v| v.as_str()) == want)
                .or_else(|| arr.first())
        })
        .and_then(|m| m.get("_meta"))
        .and_then(|meta| meta.get("reasoningEffort"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    ModelInfo {
        model,
        window,
        effort,
        available,
    }
}

/// Parse Grok `availableModels[]`. Each entry needs `modelId`; name /
/// description / efforts / window degrade to empty when missing.
fn de_available_models(arr: &[Value]) -> Vec<AcpModelOption> {
    arr.iter()
        .filter_map(|m| {
            let model_id = m.get("modelId")?.as_str()?.trim();
            if model_id.is_empty() {
                return None;
            }
            let meta = m.get("_meta");
            let window = meta
                .and_then(|meta| meta.get("totalContextTokens"))
                .and_then(|v| v.as_u64());
            let efforts = meta
                .and_then(|meta| meta.get("reasoningEfforts"))
                .map(de_reasoning_efforts)
                .unwrap_or_default();
            Some(AcpModelOption {
                model_id: model_id.to_string(),
                name: m
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: m
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                window,
                efforts,
            })
        })
        .collect()
}

/// `reasoningEfforts` is either `["high","medium"]` or
/// `[{id|value:"high",...}, …]` (live Grok 0.2.x).
fn de_reasoning_efforts(v: &Value) -> Vec<String> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    de_select_option_values(arr)
}

/// OpenCode `configOptions[].options[]` — each entry needs `value`.
fn de_opencode_model_options(arr: &[Value], shared_efforts: &[String]) -> Vec<AcpModelOption> {
    arr.iter()
        .filter_map(|o| {
            let model_id = o
                .get("value")
                .or_else(|| o.get("id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            let name = o
                .get("name")
                .or_else(|| o.get("label"))
                .and_then(|v| v.as_str())
                .unwrap_or(model_id)
                .to_string();
            Some(AcpModelOption {
                model_id: model_id.to_string(),
                name,
                description: o
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                window: None,
                // OpenCode effort is a separate select shared by all models.
                efforts: shared_efforts.to_vec(),
            })
        })
        .collect()
}

/// Select-option values from either bare strings or `{value|id}` objects.
fn de_select_option_values(arr: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for item in arr {
        if let Some(s) = item.as_str() {
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
            continue;
        }
        let id = item
            .get("value")
            .or_else(|| item.get("id"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(id) = id {
            out.push(id.to_string());
        }
    }
    out
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
    fn stop_reason_maps_every_spec_value_and_tolerates_spelling() {
        for (raw, want) in [
            ("end_turn", AcpStopReason::EndTurn),
            ("endTurn", AcpStopReason::EndTurn),
            ("END-TURN", AcpStopReason::EndTurn),
            ("cancelled", AcpStopReason::Cancelled),
            ("canceled", AcpStopReason::Cancelled),
            ("max_tokens", AcpStopReason::MaxTokens),
            ("maxTokens", AcpStopReason::MaxTokens),
            ("max_turn_requests", AcpStopReason::MaxTurnRequests),
            ("refusal", AcpStopReason::Refusal),
        ] {
            assert_eq!(
                stop_reason_from_prompt_result(&json!({ "stopReason": raw })),
                want,
                "stopReason {raw} must map to {want:?}"
            );
        }
    }

    #[test]
    fn absent_or_blank_stop_reason_stays_clean() {
        // Not every ACP build emits one; treating silence as failure would
        // break working vendors (grok/opencode omit it on some paths).
        for result in [
            json!({}),
            json!({ "stopReason": "  " }),
            json!({"usage":{}}),
        ] {
            let stop = stop_reason_from_prompt_result(&result);
            assert_eq!(stop, AcpStopReason::EndTurn);
            assert!(stop.is_clean());
            assert!(stop.failure_message().is_none());
        }
    }

    #[test]
    fn unknown_stop_reason_fails_loud_and_keeps_the_raw_value() {
        let stop = stop_reason_from_prompt_result(&json!({ "stopReason": "exploded" }));
        assert_eq!(stop, AcpStopReason::Other("exploded".into()));
        assert!(
            !stop.is_clean(),
            "an unknown terminal state is not an answer"
        );
        assert_eq!(stop.wire(), "exploded");
        assert!(stop
            .failure_message()
            .expect("unknown reason must report")
            .contains("exploded"));
    }

    #[test]
    fn cancelled_is_clean_but_truncating_reasons_are_not() {
        assert!(AcpStopReason::Cancelled.is_clean(), "/stop is not a defect");
        for stop in [
            AcpStopReason::MaxTokens,
            AcpStopReason::MaxTurnRequests,
            AcpStopReason::Refusal,
        ] {
            assert!(!stop.is_clean());
            let msg = stop.failure_message().expect("must carry a message");
            assert!(
                msg.contains(stop.wire()),
                "the message must name the wire reason: {msg}"
            );
        }
    }

    #[test]
    fn stop_reason_also_reads_from_meta() {
        assert_eq!(
            stop_reason_from_prompt_result(&json!({ "_meta": { "stopReason": "refusal" } })),
            AcpStopReason::Refusal
        );
    }

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
                {
                    "id":"model",
                    "currentValue":"tokenopen/gpt-5.5",
                    "options": [
                        {"value":"tokenopen/gpt-5.5","name":"GPT 5.5"},
                        {"value":"anthropic/claude-sonnet-4","name":"Sonnet 4"}
                    ]
                },
                {
                    "id":"effort",
                    "currentValue":"high",
                    "options": [
                        {"value":"low","name":"low"},
                        {"value":"high","name":"high"}
                    ]
                }
            ]
        });
        let info = pluck_model_info(&result);
        assert_eq!(info.model.as_deref(), Some("tokenopen/gpt-5.5"));
        assert_eq!(info.effort.as_deref(), Some("high"));
        assert_eq!(info.available.len(), 2);
        assert_eq!(info.available[0].model_id, "tokenopen/gpt-5.5");
        assert_eq!(info.available[0].name, "GPT 5.5");
        assert_eq!(info.available[0].efforts, vec!["low", "high"]);
        assert_eq!(info.available[1].model_id, "anthropic/claude-sonnet-4");
        // Picker expands model×effort from the vendor options only.
        let opts = acp_model_picker_options(&info.available);
        assert_eq!(opts.len(), 4);
        assert_eq!(opts[0].id, "tokenopen/gpt-5.5 low");
    }

    #[test]
    fn split_trailing_effort_uses_vendor_known_set_only() {
        let known = vec!["low".into(), "high".into()];
        assert_eq!(
            split_trailing_effort("tokenopen/gpt-5.5 low", &known),
            ("tokenopen/gpt-5.5".into(), Some("low".into()))
        );
        // Unknown trailing token is NOT stripped (avoids eating model suffixes).
        assert_eq!(
            split_trailing_effort("my-model turbo", &known),
            ("my-model turbo".into(), None)
        );
        // Empty known → never split.
        assert_eq!(split_trailing_effort("x low", &[]), ("x low".into(), None));
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
                    "name": "Grok 4.5",
                    "_meta": {
                        "totalContextTokens": 500000,
                        "reasoningEffort": "high",
                        "reasoningEfforts": [
                            {"id": "high", "value": "high"},
                            {"id": "medium", "value": "medium"},
                            "low"
                        ]
                    }
                }, {
                    "modelId": "grok-composer-2.5-fast",
                    "name": "Composer 2.5"
                }]
            }
        });
        let info = pluck_model_info(&result);
        assert_eq!(info.model.as_deref(), Some("grok-4.5"));
        assert_eq!(info.window, Some(500000));
        assert_eq!(info.effort.as_deref(), Some("high"));
        assert_eq!(info.available.len(), 2);
        assert_eq!(info.available[0].model_id, "grok-4.5");
        assert_eq!(info.available[0].name, "Grok 4.5");
        assert_eq!(info.available[0].efforts, vec!["high", "medium", "low"]);
        assert_eq!(info.available[1].model_id, "grok-composer-2.5-fast");
        assert!(info.available[1].efforts.is_empty());
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
