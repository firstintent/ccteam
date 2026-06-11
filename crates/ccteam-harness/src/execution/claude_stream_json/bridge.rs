//! Wave 2 — the stream-json **slash bridge gate** + **HITL** (`can_use_tool`)
//! types.
//!
//! ## Slash policy = bridge posture (PRD E1, not VS Code full passthrough)
//!
//! Per `docs/research/cc-stream-json-protocol.md` §3, claude's `system:init`
//! advertises only the safe (prompt/local) command set — dialog/local-jsx
//! commands are filtered out by the CLI. So the gate is:
//! - in the live init table OR an always-safe red-line command →
//!   Passthrough (forward verbatim as user text);
//! - else a curated dialog/panel command → Reject with a human message;
//! - else unknown → Passthrough as text (the bridge never leaks an
//!   "Unknown skill" reply to the IM user).
//!
//! ccteam's own IM commands (`/pair /cd /use /new /role @handle`) never
//! reach the adapter — the gateway intercepts them before `handle_directive`.

use async_trait::async_trait;
use serde_json::Value;

use super::protocol::ControlRequestMsg;

/// Red-line commands that ALWAYS pass through, independent of the init
/// table (the `/compact /clear /context` transparency red line). `/new` is
/// a ccteam gateway command and never reaches the adapter.
pub const ALWAYS_PASSTHROUGH: &[&str] = &["compact", "clear", "context"];

/// Curated local-jsx (dialog/panel) commands with no headless equivalent.
/// `system:init` already filters these out of its table; the list lets the
/// bridge give a precise human refusal instead of leaking "Unknown skill".
pub const DIALOG_COMMANDS: &[&str] = &[
    "model",
    "config",
    "agents",
    "permissions",
    "mcp",
    "hooks",
    "ide",
    "login",
    "logout",
    "theme",
    "vault",
    "privacy-settings",
    "output-style",
    "terminal-setup",
    "sandbox-toggle",
    "rate-limit-options",
    "help",
    "plan",
    "statusline",
];

/// The bridge's verdict for one slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashClass {
    /// Forward verbatim as user text (known prompt/local OR unknown).
    Passthrough,
    /// Refuse with a human message (known dialog/panel, undriveable here).
    Reject,
}

/// Classify a vendor slash command for the bridge gate.
pub fn classify_slash(name: &str, command_table: &[String]) -> SlashClass {
    let name = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if ALWAYS_PASSTHROUGH.contains(&name.as_str()) {
        return SlashClass::Passthrough;
    }
    let in_table = command_table
        .iter()
        .any(|c| c.trim_start_matches('/').eq_ignore_ascii_case(&name));
    if in_table {
        return SlashClass::Passthrough;
    }
    if DIALOG_COMMANDS.contains(&name.as_str()) {
        return SlashClass::Reject;
    }
    SlashClass::Passthrough
}

/// Human-readable refusal for a rejected dialog command.
pub fn reject_reason(name: &str) -> String {
    format!(
        "/{name} 是 Claude 的交互面板命令，在 stream-json 聊天通道里没有等价操作 \
         —— 用 web Settings 调整，或 `/role` 切换 persona。"
    )
}

/// A parsed `can_use_tool` permission request (CLI → client reverse RPC).
#[derive(Debug, Clone)]
pub struct CanUseToolReq {
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: Option<String>,
}

/// Parse a control_request into a `can_use_tool` ask (`None` for any other
/// control subtype).
pub fn parse_can_use_tool(msg: &ControlRequestMsg) -> Option<CanUseToolReq> {
    if msg.subtype() != Some("can_use_tool") {
        return None;
    }
    Some(CanUseToolReq {
        request_id: msg.request_id.clone(),
        tool_name: msg
            .request
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        input: msg.request.get("input").cloned().unwrap_or(Value::Null),
        tool_use_id: msg
            .request
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from),
    })
}

/// A HITL decision. `deny` blocks ONLY this tool call — the turn continues
/// (red line: never kill a turn).
#[derive(Debug, Clone)]
pub struct ApprovalDecision {
    pub allow: bool,
    pub message: String,
}

impl ApprovalDecision {
    pub fn allow() -> Self {
        Self {
            allow: true,
            message: String::new(),
        }
    }
    pub fn deny(message: impl Into<String>) -> Self {
        Self {
            allow: false,
            message: message.into(),
        }
    }
}

/// Resolver the adapter consults for a HITL tool approval. The production
/// impl (wired by the gateway in Wave 3) routes to the daemon's existing
/// `permission/ask` → IM [同意][拒绝]; tests use a deterministic stub.
#[async_trait]
pub trait CanUseToolResolver: Send + Sync {
    /// Decide a tool-use approval for the session identified by `sid`.
    async fn resolve(&self, sid: &str, req: &CanUseToolReq) -> ApprovalDecision;
}

/// A [`CanUseToolResolver`] backed by a synchronous closure — convenient
/// for tests and simple policies whose decision needs no `await`.
pub struct FnResolver<F>(pub F);

#[async_trait]
impl<F> CanUseToolResolver for FnResolver<F>
where
    F: Fn(&str, &CanUseToolReq) -> ApprovalDecision + Send + Sync,
{
    async fn resolve(&self, sid: &str, req: &CanUseToolReq) -> ApprovalDecision {
        (self.0)(sid, req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table() -> Vec<String> {
        vec!["compact".into(), "context".into(), "review".into()]
    }

    #[test]
    fn known_table_command_passes_through() {
        assert_eq!(classify_slash("review", &table()), SlashClass::Passthrough);
        assert_eq!(classify_slash("/review", &table()), SlashClass::Passthrough);
    }

    #[test]
    fn redline_commands_always_pass_even_if_table_empty() {
        for c in ["compact", "clear", "context"] {
            assert_eq!(classify_slash(c, &[]), SlashClass::Passthrough, "{c}");
        }
    }

    #[test]
    fn dialog_commands_reject() {
        for c in ["model", "mcp", "login", "permissions", "help"] {
            assert_eq!(classify_slash(c, &table()), SlashClass::Reject, "{c}");
        }
    }

    #[test]
    fn unknown_command_passes_as_text() {
        // The bridge never leaks "Unknown skill"; unknowns become text.
        assert_eq!(classify_slash("shrug", &table()), SlashClass::Passthrough);
        assert_eq!(
            classify_slash("frobnicate", &table()),
            SlashClass::Passthrough
        );
    }

    #[test]
    fn table_membership_beats_stale_dialog_list() {
        // If claude advertises a command we (staleley) list as dialog, the
        // live table wins → passthrough.
        let t = vec!["model".to_string()];
        assert_eq!(classify_slash("model", &t), SlashClass::Passthrough);
    }

    #[test]
    fn parse_can_use_tool_extracts_fields() {
        let msg = ControlRequestMsg {
            request_id: "req-9".into(),
            request: json!({
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "ls -la"},
                "tool_use_id": "tu-3"
            }),
        };
        let req = parse_can_use_tool(&msg).unwrap();
        assert_eq!(req.request_id, "req-9");
        assert_eq!(req.tool_name, "Bash");
        assert_eq!(req.input["command"], "ls -la");
        assert_eq!(req.tool_use_id.as_deref(), Some("tu-3"));
    }

    #[test]
    fn parse_can_use_tool_ignores_other_subtypes() {
        let msg = ControlRequestMsg {
            request_id: "req-1".into(),
            request: json!({"subtype": "something_else"}),
        };
        assert!(parse_can_use_tool(&msg).is_none());
    }
}
