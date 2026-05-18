//! V0.6.0 Wave 1 — `ccteam__advise_*` MCP tool **stubs**.
//!
//! These 2 tools back the Codex + Claude parallel advisor surface
//! (F112 §A — `/ccteam-advise <hard question>` slash skill, Wave 3).
//! Wave 1 registers the schemas and a `NotImplemented` dispatcher so
//! the `advise` group exists for `CCTEAM_DISABLE_TOOLS` filtering
//! and downstream skills can pre-wire the tool names.

use anyhow::Result;
use serde_json::{json, Value};

/// Tool definitions for the 2 advise stubs. Merged into the top-level
/// `tool_definitions()` in `mcp_serve.rs`.
pub fn advise_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__advise_vote",
            "description": "V0.6.0 Wave 3 (F112 §A) STUB — ask Claude AND Codex the same hard question in parallel, then return both verdicts plus a vote synthesis (agreement / disagreement summary). Use for second-opinion / cross-vendor sanity checks. Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The hard question to put to both vendors."
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional context block prepended to each vendor's prompt."
                    },
                    "vendors": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["claude", "codex"] },
                        "description": "Vendors to consult. Default ['claude', 'codex']."
                    }
                },
                "required": ["question"],
            }),
        }),
        json!({
            "name": "ccteam__advise_parallel",
            "description": "V0.6.0 Wave 3 (F112 §A) STUB — fan out one prompt to N parallel advisor sessions (any mix of Claude + Codex vendors) and return all individual answers without vote synthesis. Use when you want raw N-of-N rather than a single combined verdict. Returns NotImplemented in Wave 1.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Prompt to fan out." },
                    "advisors": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "vendor": { "type": "string", "enum": ["claude", "codex"] },
                                "model": { "type": "string", "description": "Optional model override." },
                                "persona": { "type": "string", "description": "Optional persona id." }
                            },
                            "required": ["vendor"]
                        },
                        "description": "Advisor configs to fan out to (2-8 typical)."
                    }
                },
                "required": ["prompt", "advisors"],
            }),
        }),
    ]
}

/// Dispatch a `ccteam__advise_*` tool. Wave 1 returns a graceful
/// `NotImplemented` body for matched names and `Ok(None)` otherwise.
pub fn dispatch(name: &str, _args: &Value) -> Result<Option<String>> {
    let matched = matches!(
        name,
        "ccteam__advise_vote" | "ccteam__advise_parallel"
    );
    if !matched {
        return Ok(None);
    }
    let body = json!({
        "ok": false,
        "error": "NotImplemented",
        "tool": name,
        "wave": "Wave 3 (V0.6.0 F112 §A)",
        "note": "ccteam__advise_* tools are stubs in V0.6.0 Wave 1. Wave 3 lands the dispatch handlers behind this surface.",
    });
    Ok(Some(serde_json::to_string_pretty(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_advise_tools_registered() {
        let tools = advise_tool_definitions();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__advise_vote"));
        assert!(names.contains(&"ccteam__advise_parallel"));
    }

    #[test]
    fn dispatch_returns_not_implemented_body() {
        let body = dispatch("ccteam__advise_vote", &json!({}))
            .unwrap()
            .expect("matched our tool");
        assert!(body.contains("NotImplemented"));
        assert!(body.contains("Wave 3"));
    }

    #[test]
    fn dispatch_returns_none_for_foreign_tools() {
        assert!(dispatch("ccteam__chat_send_input", &json!({}))
            .unwrap()
            .is_none());
    }

    #[test]
    fn all_advise_tools_carry_advise_prefix() {
        for t in advise_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("ccteam__advise_"),
                "advise tool name must start with ccteam__advise_: {n}"
            );
        }
    }
}
