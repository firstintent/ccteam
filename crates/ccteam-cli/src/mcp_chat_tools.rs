//! V0.6.0 Wave 1 — `ccteam__chat_*` MCP tool **stubs**.
//!
//! These 5 tools back the chat workflow surface (mode 3 — `claude -p
//! --resume <sid>` + stream-json + JSON-mailbox-trigger; see
//! `docs/v0-6-0/prd.md` F108). Wave 1 registers the schemas and a
//! `NotImplemented` dispatcher so:
//!
//! - the tool list shape locked here doesn't bikeshed in Wave 2
//! - `CCTEAM_DISABLE_TOOLS=chat` actually has a `chat` group to hide
//! - downstream skill / agent prompts can compile against the final
//!   tool names without waiting for the runtime
//!
//! Wave 2 (F108 / F114 / F117) fills the dispatch handlers.

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

/// Tool definitions for the 5 chat stubs. Merged into the top-level
/// `tool_definitions()` in `mcp_serve.rs`. Schemas mirror the
/// expected Wave 2 surface so users / agent prompts can pre-wire.
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__chat_send_input",
            "description": "V0.6.0 Wave 2 (F108) STUB — send a user-NL turn to a chat-mode bot session. Writes <project>/.ccteam/chat/<bot>/inbox/msg-<ts>-NNN.md + a short tmux send-keys trigger (`read your mailbox`). Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id (registered via ccteam-creator)." },
                    "input": { "type": "string", "description": "User NL / markdown body." }
                },
                "required": ["slug", "bot", "input"],
            }),
        }),
        json!({
            "name": "ccteam__chat_lifecycle",
            "description": "V0.6.0 Wave 2 (F108) STUB — lifecycle ops for a chat bot session: `start` (spawn `claude -p --resume <sid>` long process), `stop` (graceful close), `reset` (drop ccteam-owned turns.jsonl + restart with fresh sid). Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id." },
                    "op": {
                        "type": "string",
                        "enum": ["start", "stop", "reset"],
                        "description": "Lifecycle operation."
                    }
                },
                "required": ["slug", "bot", "op"],
            }),
        }),
        json!({
            "name": "ccteam__chat_session_reset",
            "description": "V0.6.0 Wave 2 (F108) STUB — force-reset a single chat session: cancel in-flight turn, archive current turns.jsonl, spin a fresh `claude -p` session under the same bot identity. Use when the bot is wedged but you want to keep persona / history snapshot. Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id." },
                    "preserve_history": {
                        "type": "boolean",
                        "description": "If true, copy current turns.jsonl into archived/ before reset. Default true."
                    }
                },
                "required": ["slug", "bot"],
            }),
        }),
        json!({
            "name": "ccteam__chat_list_bots",
            "description": "V0.6.0 Wave 2 (F108) STUB — list every chat bot persona registered under a chat-mode project, with status (running / paused / dead) and last-turn timestamp. Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." }
                },
                "required": ["slug"],
            }),
        }),
        json!({
            "name": "ccteam__chat_show_turn_log",
            "description": "V0.6.0 Wave 2 (F108) STUB — return the last N turns from a bot's ccteam-owned <project>/.ccteam/chat/<bot>/turns.jsonl. Default N=20. Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Chat-workflow project slug." },
                    "bot": { "type": "string", "description": "Bot persona id." },
                    "last_n": {
                        "type": "integer",
                        "description": "How many turns to return (default 20)."
                    }
                },
                "required": ["slug", "bot"],
            }),
        }),
    ]
}

/// Dispatch a `ccteam__chat_*` tool. Wave 1 returns `Ok(Some(...))`
/// with a JSON body containing `ok: false, error: "NotImplemented"`
/// so MCP clients see a graceful error rather than a transport
/// failure. `Ok(None)` means the name is not one of ours — caller
/// falls through to the next dispatcher.
pub fn dispatch(name: &str, _args: &Value) -> Result<Option<String>> {
    let matched = matches!(
        name,
        "ccteam__chat_send_input"
            | "ccteam__chat_lifecycle"
            | "ccteam__chat_session_reset"
            | "ccteam__chat_list_bots"
            | "ccteam__chat_show_turn_log"
    );
    if !matched {
        return Ok(None);
    }
    let body = json!({
        "ok": false,
        "error": "NotImplemented",
        "tool": name,
        "wave": "Wave 2 (V0.6.0 F108)",
        "note": "ccteam__chat_* tools are stubs in V0.6.0 Wave 1. Wave 2 lands the dispatch handlers behind this surface.",
    });
    Ok(Some(serde_json::to_string_pretty(&body)?))
}

/// Convenience: bail with a parse error if `name` looks like one of
/// ours but doesn't match. Reserved for future arg-validation
/// helpers; unused in Wave 1.
#[allow(dead_code)]
fn _placeholder_unused_to_silence_unused_warnings_during_wave1() -> Result<Map<String, Value>> {
    Err(anyhow!("placeholder"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_chat_tools_registered() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__chat_send_input"));
        assert!(names.contains(&"ccteam__chat_lifecycle"));
        assert!(names.contains(&"ccteam__chat_session_reset"));
        assert!(names.contains(&"ccteam__chat_list_bots"));
        assert!(names.contains(&"ccteam__chat_show_turn_log"));
    }

    #[test]
    fn dispatch_returns_not_implemented_body() {
        let body = dispatch("ccteam__chat_send_input", &json!({}))
            .unwrap()
            .expect("matched our tool");
        assert!(body.contains("NotImplemented"));
        assert!(body.contains("Wave 2"));
    }

    #[test]
    fn dispatch_returns_none_for_foreign_tools() {
        assert!(dispatch("ccteam__workflow_ls", &json!({})).unwrap().is_none());
        assert!(dispatch("ccteam__advise_vote", &json!({})).unwrap().is_none());
    }

    #[test]
    fn all_chat_tools_carry_chat_prefix() {
        for t in chat_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("ccteam__chat_"),
                "chat tool name must start with ccteam__chat_: {n}"
            );
        }
    }
}
