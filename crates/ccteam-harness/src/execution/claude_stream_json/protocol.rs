//! NDJSON wire types for the Claude Code stream-json protocol.
//!
//! One JSON object per line, bidirectional over the child's stdio:
//! - **inbound** (ccteam → claude on stdin): `user` messages +
//!   `control_response` replies (HITL allow/deny).
//! - **outbound** (claude → ccteam on stdout): `system:init`, `assistant`
//!   / `user` (replay) messages, per-turn `result`, and server-initiated
//!   `control_request` (`can_use_tool` HITL).
//!
//! Wire facts verified against `docs/research/cc-stream-json-protocol.md`
//! (§2–§4) + the `references/alleycat` reference bridge. Bodies that
//! ccteam only forwards (the Anthropic `message` object, control request
//! payloads) stay `serde_json::Value` so a vendor field addition never
//! breaks parsing — only the fields ccteam actually reads are typed.

use serde::Deserialize;
use serde_json::{json, Value};

/// One outbound stdout line. Internally tagged on `type`; unknown /
/// presentation-only kinds (`stream_event`, `keep_alive`,
/// `rate_limit_event`, …) collapse to [`Outbound::Other`] and are dropped
/// by the translator (the final-only contract — partials are delta only).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outbound {
    /// Capability broadcast at startup (`subtype: "init"`): confirms the
    /// session id and ships the slash-command table.
    System(SystemMsg),
    /// An assistant turn message (full Anthropic `Message` with content
    /// blocks). Accumulated for fallback final text + tool-use progress.
    Assistant(MessageEnvelope),
    /// A user message echo (`--replay-user-messages`) or a tool_result
    /// turn. Carried for transcript authority; not forwarded as a reply.
    User(MessageEnvelope),
    /// Per-turn terminal message: final text + usage + cost + error flag.
    #[serde(rename = "result")]
    TurnResult(ResultMsg),
    /// Server-initiated reverse RPC (e.g. `can_use_tool` permission ask).
    ControlRequest(ControlRequestMsg),
    /// Reply to one of *our* control requests (correlated by `request_id`).
    ControlResponse(ControlResponseMsg),
    /// Everything else (partials, keep-alives, rate-limit notices) — dropped.
    #[serde(other)]
    Other,
}

/// Deserialize a slash-command table claude sends as EITHER a bare string
/// array OR an array of `{name,…}` objects (the `system:commands_changed`
/// form). Extracts each name; anything else is skipped. Tolerant by design — a
/// command-table line must never fail the whole transport parse (the bug this
/// fixes: `commands` objects parsed into `Vec<String>` → "expected a string").
fn de_command_names<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(d)?;
    Ok(v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.as_str()
                        .map(String::from)
                        .or_else(|| c.get("name").and_then(Value::as_str).map(String::from))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// One entry from claude's `initialize` `response.models[]` — the REAL,
/// vendor-supplied model list (claude has no `model/list` RPC; the
/// capability ships on the initialize control_response). `value` is exactly
/// the `/model <value>` id form; `efforts` is the model's
/// `supportedEffortLevels` (empty for a no-effort model like haiku). Only
/// these two fields are typed; everything else (displayName, description,
/// supportsEffort) is presentation we don't drive the picker from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeModelOption {
    /// The `/model <value>` id (e.g. `"opus[1m]"`, `"sonnet"`, `"haiku"`).
    pub value: String,
    /// `supportedEffortLevels` (e.g. `["low","medium","high","xhigh","max"]`);
    /// empty when the model has no effort axis (e.g. haiku).
    pub efforts: Vec<String>,
}

/// `system` message body. Only `subtype: "init"` is meaningful today.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SystemMsg {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub model: Option<String>,
    /// The REAL model list from the `initialize` control_response
    /// (`response.models[]`). Drives the bare-`/model` picker
    /// (`claude_model_options`) — one option per (value, effort). Only the
    /// `initialize` handshake populates this; a `system:init` stdout line
    /// leaves it empty (it carries no `models` array). Defaults to empty,
    /// which the model arm falls back from to the usage-text rejection.
    #[serde(skip)]
    pub models: Vec<ClaudeModelOption>,
    /// The slash-command table claude exposes for this session — names
    /// only (dialog/local-jsx commands are already filtered out by the
    /// CLI). The bridge gate (Wave 2) keys known-vs-unknown off this.
    /// claude sends `commands` as an array of OBJECTS (`{name,description,…}`),
    /// e.g. in a stdout `system:commands_changed` line — `de_command_names`
    /// accepts that (extracting `.name`) AND a bare string array, so neither
    /// form trips a parse failure.
    #[serde(default, alias = "commands", deserialize_with = "de_command_names")]
    pub slash_commands: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    // v0.8.20 /status — subagent/workflow task lifecycle. claude reports a
    // running task via `task_started`, mutates it via `task_updated{patch}`, and
    // signals completion via `task_notification{status}`. The status tap reflects
    // these into the session's running-task list. Only the fields the tracker
    // reads are typed; all default-absent so non-task system lines (init /
    // commands_changed) still parse unchanged.
    /// `task_*.task_id` — the stable id (add on `task_started`, remove on terminal).
    #[serde(default)]
    pub task_id: String,
    /// `task_started.subagent_type` (e.g. `general-purpose`, `code-reviewer`).
    #[serde(default)]
    pub subagent_type: String,
    /// `task_started.description` — the task's short label.
    #[serde(default)]
    pub description: String,
    /// `task_started.task_type` (e.g. `local_agent`).
    #[serde(default)]
    pub task_type: String,
    /// `task_notification.status` (terminal, e.g. `completed`/`failed`).
    #[serde(default)]
    pub status: String,
    /// `task_updated.patch` (`{status, end_time}`) — read `patch.status` for removal.
    #[serde(default)]
    pub patch: Option<Value>,
}

impl SystemMsg {
    pub fn is_init(&self) -> bool {
        self.subtype == "init"
    }

    /// Build the session's capability view from an `initialize`
    /// `control_response` body. claude (stream-json) does NOT emit a
    /// `system:init` line until the first user turn, so the spawn-time
    /// handshake uses the `initialize` control_request and parses its
    /// response here: `response.commands[].name` → the slash-command table,
    /// `response.models[0]` → the model id. Missing fields degrade to empty
    /// (the bridge gate then treats every slash as passthrough text).
    pub fn from_initialize(body: &ControlResponseBody) -> Self {
        let resp = body.response.as_ref();
        let slash_commands = resp
            .and_then(|v| v.get("commands"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.get("name").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let models_raw = resp
            .and_then(|v| v.get("models"))
            .and_then(|v| v.as_array());
        let model = models_raw
            .and_then(|arr| arr.first())
            .and_then(|m| {
                m.get("value")
                    .or_else(|| m.get("model"))
                    .or_else(|| m.get("id"))
                    .or_else(|| m.get("name"))
            })
            .and_then(Value::as_str)
            .map(String::from);
        let models = models_raw
            .map(|arr| de_model_options(arr))
            .unwrap_or_default();
        SystemMsg {
            subtype: "init".to_string(),
            session_id: String::new(),
            model,
            models,
            slash_commands,
            tools: Vec::new(),
            ..Default::default()
        }
    }
}

/// Parse the `initialize` `response.models[]` array into the REAL model
/// list. Defensive: each entry needs a `value` (the `/model <value>` id);
/// `supportedEffortLevels` is optional (absent/empty for a no-effort model
/// like haiku) and only string members are kept. Entries without a `value`
/// are skipped rather than failing the whole parse.
fn de_model_options(models: &[Value]) -> Vec<ClaudeModelOption> {
    models
        .iter()
        .filter_map(|m| {
            let value = m
                .get("value")
                .or_else(|| m.get("model"))
                .or_else(|| m.get("id"))
                .and_then(Value::as_str)?
                .to_string();
            let efforts = m
                .get("supportedEffortLevels")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(ClaudeModelOption { value, efforts })
        })
        .collect()
}

/// `assistant` / `user` envelope. `message` is the raw Anthropic `Message`
/// object; the translator plucks text + tool_use blocks from it.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageEnvelope {
    #[serde(default)]
    pub message: Value,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

/// Per-turn `result` body.
#[derive(Debug, Clone, Deserialize)]
pub struct ResultMsg {
    /// `"success"` | `"error_max_turns"` | `"error_during_execution"` | …
    #[serde(default)]
    pub subtype: String,
    /// Authoritative final assistant text for a success turn.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    /// Anthropic usage block → deserialized into [`crate::UnifiedTokenUsage`].
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub session_id: String,
}

impl ResultMsg {
    /// True when this result is a turn failure (error subtype or flag).
    pub fn is_failure(&self) -> bool {
        self.is_error || self.subtype.starts_with("error")
    }
}

/// A server-initiated `control_request` (CLI → client). The `request`
/// body stays `Value`; the HITL handler (Wave 2) parses `subtype` +
/// `can_use_tool` fields out of it.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlRequestMsg {
    pub request_id: String,
    #[serde(default)]
    pub request: Value,
}

impl ControlRequestMsg {
    /// The `request.subtype` (e.g. `"can_use_tool"`), if present.
    pub fn subtype(&self) -> Option<&str> {
        self.request.get("subtype").and_then(|v| v.as_str())
    }
}

/// A reply to one of *our* control requests. Per the wire quirk the
/// `request_id` is nested INSIDE `response`, not at the top level.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponseMsg {
    #[serde(default)]
    pub response: ControlResponseBody,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ControlResponseBody {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub response: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

// ── Inbound builders (ccteam → claude). Emitted as JSON strings rather
//    than a typed enum because the write side is a thin serialize. ──────

/// A `user` turn line: `{"type":"user","message":{"role":"user",
/// "content":[{"type":"text","text":"..."}]}}`. A single text block is the
/// canonical chat-turn shape (matches what claude's own TUI submits).
pub fn user_text_line(text: &str) -> String {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}],
        },
    })
    .to_string()
}

/// A `control_response` line replying to a `can_use_tool` request. `allow`
/// → `{"behavior":"allow","updatedInput":<input>}`; deny →
/// `{"behavior":"deny","message":<reason>}`. Deny blocks ONLY this tool
/// call — it does not interrupt the turn (red line: never kill a turn).
pub fn can_use_tool_response_line(
    request_id: &str,
    allow: bool,
    input: &Value,
    deny_message: &str,
) -> String {
    let inner = if allow {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        json!({"behavior": "deny", "message": deny_message})
    };
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": inner,
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_system_init_with_commands() {
        let line = r#"{"type":"system","subtype":"init","session_id":"u-1",
            "model":"claude-opus-4-8","slash_commands":["compact","context","clear"],
            "tools":["Bash","Edit"]}"#;
        let out: Outbound = serde_json::from_str(line).unwrap();
        match out {
            Outbound::System(s) => {
                assert!(s.is_init());
                assert_eq!(s.session_id, "u-1");
                assert!(s.slash_commands.contains(&"compact".to_string()));
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn parses_commands_changed_object_array_without_failing() {
        // Regression: claude sends `system:commands_changed` with `commands` as
        // an array of OBJECTS — it must parse (extracting names), not fail the
        // transport with "invalid type: map, expected a string".
        let line = r#"{"type":"system","subtype":"commands_changed","commands":[
            {"name":"goal","description":"Set a goal","argumentHint":""},
            {"name":"review","description":"Review a PR","argumentHint":""}]}"#;
        let out: Outbound = serde_json::from_str(line).expect("must parse, not error");
        match out {
            Outbound::System(s) => {
                assert_eq!(s.subtype, "commands_changed");
                assert_eq!(
                    s.slash_commands,
                    vec!["goal".to_string(), "review".to_string()]
                );
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[test]
    fn from_initialize_captures_real_model_list() {
        // The `initialize` control_response carries the REAL model list under
        // `response.models[]` (claude has no `model/list` RPC). Each entry's
        // `value` is the `/model <value>` id; `supportedEffortLevels` is the
        // effort axis (absent for haiku). `from_initialize` must capture all
        // of them — the bare-`/model` picker is built strictly from this.
        let body = ControlResponseBody {
            subtype: "success".to_string(),
            request_id: "1".to_string(),
            response: Some(json!({
                "commands": [{"name":"compact"}, {"name":"context"}],
                "models": [
                    {"value":"default","displayName":"Default",
                     "description":"Opus 4.8 1M, recommended","supportsEffort":true,
                     "supportedEffortLevels":["low","medium","high","xhigh","max"]},
                    {"value":"opus[1m]","displayName":"Opus","supportsEffort":true,
                     "supportedEffortLevels":["low","medium","high","xhigh","max"]},
                    {"value":"sonnet","displayName":"Sonnet","supportsEffort":true,
                     "supportedEffortLevels":["low","medium","high","xhigh","max"]},
                    {"value":"haiku","displayName":"Haiku","supportsEffort":false}
                ]
            })),
            error: None,
        };
        let s = SystemMsg::from_initialize(&body);
        // First model id is captured for the status seed.
        assert_eq!(s.model.as_deref(), Some("default"));
        assert!(s.slash_commands.contains(&"compact".to_string()));
        // Full list captured, in order, with efforts (haiku has none).
        assert_eq!(s.models.len(), 4);
        assert_eq!(s.models[0].value, "default");
        assert_eq!(
            s.models[0].efforts,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(s.models[1].value, "opus[1m]");
        assert_eq!(s.models[3].value, "haiku");
        assert!(s.models[3].efforts.is_empty());
    }

    #[test]
    fn parses_result_success_and_failure() {
        let ok = r#"{"type":"result","subtype":"success","result":"all done",
            "is_error":false,"total_cost_usd":0.012,
            "usage":{"input_tokens":100,"output_tokens":20}}"#;
        match serde_json::from_str::<Outbound>(ok).unwrap() {
            Outbound::TurnResult(r) => {
                assert!(!r.is_failure());
                assert_eq!(r.result.as_deref(), Some("all done"));
                let usage: crate::UnifiedTokenUsage =
                    serde_json::from_value(r.usage.unwrap()).unwrap();
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 20);
            }
            other => panic!("expected TurnResult, got {other:?}"),
        }

        let err = r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#;
        match serde_json::from_str::<Outbound>(err).unwrap() {
            Outbound::TurnResult(r) => assert!(r.is_failure()),
            other => panic!("expected TurnResult, got {other:?}"),
        }
    }

    #[test]
    fn parses_can_use_tool_control_request() {
        let line = r#"{"type":"control_request","request_id":"req-7",
            "request":{"subtype":"can_use_tool","tool_name":"Bash",
            "input":{"command":"ls"},"tool_use_id":"tu-1"}}"#;
        match serde_json::from_str::<Outbound>(line).unwrap() {
            Outbound::ControlRequest(c) => {
                assert_eq!(c.request_id, "req-7");
                assert_eq!(c.subtype(), Some("can_use_tool"));
                assert_eq!(c.request["tool_name"], "Bash");
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
    }

    #[test]
    fn unknown_kinds_collapse_to_other() {
        for line in [
            r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#,
            r#"{"type":"keep_alive"}"#,
            r#"{"type":"rate_limit_event","rate":99}"#,
        ] {
            assert!(matches!(
                serde_json::from_str::<Outbound>(line).unwrap(),
                Outbound::Other
            ));
        }
    }

    #[test]
    fn user_text_line_is_canonical_single_block() {
        let line = user_text_line("hello");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn can_use_tool_response_allow_and_deny_shapes() {
        let allow = can_use_tool_response_line("req-1", true, &json!({"command": "ls"}), "");
        let av: Value = serde_json::from_str(&allow).unwrap();
        assert_eq!(av["response"]["request_id"], "req-1");
        assert_eq!(av["response"]["response"]["behavior"], "allow");
        assert_eq!(av["response"]["response"]["updatedInput"]["command"], "ls");

        let deny = can_use_tool_response_line("req-2", false, &Value::Null, "nope");
        let dv: Value = serde_json::from_str(&deny).unwrap();
        assert_eq!(dv["response"]["response"]["behavior"], "deny");
        assert_eq!(dv["response"]["response"]["message"], "nope");
    }
}
