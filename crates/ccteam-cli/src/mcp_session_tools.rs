//! `ccteam__session_*` MCP tools (v0.8.7 W1 — cto scheduling, B-tier).
//!
//! Tool **definitions** and the daemon-side gate live in [`ccteam_im::mcp`].
//! This module owns the **stdio-side forwarder**: inject ambient caller
//! identity and ship the call over `mcp.sock` to the daemon.

use anyhow::Result;
use serde_json::{json, Value};

use ccteam_core::paths::CcteamPaths;

pub use ccteam_im::mcp::is_session_tool;

/// Stdio-side dispatch for a `ccteam__session_*` tool: inject the ambient
/// caller identity and forward to the daemon over `mcp.sock`. Returns
/// `Ok(None)` for tools that aren't ours so `call_tool` falls through.
///
/// The forwarded request reuses the EXACT shape the daemon's
/// `execute_session_tool` expects: a `tools/call` whose `arguments` carry the
/// original args plus `_caller_slug` / `_caller_role` / `_caller_secret`
/// overwritten from the env (caller-supplied values are ignored). The secret
/// is what the daemon authenticates; it only raises the bar (not a hard
/// boundary under a single-uid model — see the module-level honest scope in
/// `ccteam_im::mcp::dispatch`).
pub async fn dispatch(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Option<String>> {
    if !is_session_tool(name) {
        return Ok(None);
    }
    let content = forward_session_tool(paths, name, args).await?;
    // The forwarder returns the MCP `content` array (a single text block);
    // unwrap it to the body string the group-dispatch contract expects.
    let body = content
        .first()
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    Ok(Some(body))
}

/// Forward one `session_*` tool call to the daemon, injecting the ambient
/// identity. Surfaces a structured (non-fatal) message when we're not in a
/// ccteam chat session or the daemon is unreachable, matching
/// `forward_chat_send_file`'s graceful-degrade contract.
async fn forward_session_tool(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Vec<Value>> {
    let slug = std::env::var("CCTEAM_CHAT_SLUG").unwrap_or_default();
    let role = std::env::var("CCTEAM_CHAT_ROLE").unwrap_or_default();
    // v0.8.7 review-fix (R-M1) — the per-session secret minted at spawn. The
    // daemon authenticates this `(role, secret)` pair against its session map
    // instead of trusting the plaintext role. Absent for a session spawned
    // before this change (restored, no secret) → the daemon fails the check
    // closed, which is the intended fail-safe.
    let secret = std::env::var("CCTEAM_CHAT_SECRET").unwrap_or_default();
    // v0.8.8 F1 — the caller (cto) session's own ccteam sid. Forwarded so the
    // daemon can resolve the SPECIFIC caller session post-dedup (`(slug, role)`
    // is no longer unique). Empty for a pre-F1 / restored pane.
    let caller_sid = std::env::var("CCTEAM_CHAT_SID").unwrap_or_default();
    // v0.9.0 W1 (F1) — the caller identity is now the `(sid, secret)` PRINCIPAL,
    // not `(slug, role)`. Require the principal env (a roleless session has an
    // empty ROLE but still a valid sid+secret); slug/role ride along as
    // audit labels the daemon overwrites from the resolved CallerCtx.
    if caller_sid.is_empty() || secret.is_empty() {
        return Ok(vec![json!({
            "type": "text",
            "text": format!(
                "{name}: not in a ccteam session (CCTEAM_CHAT_SID/SECRET unset); session tools are only callable from a live session with a principal"
            ),
        })]);
    }
    let mut fwd_args = args.clone();
    if let Some(obj) = fwd_args.as_object_mut() {
        // Overwrite (never trust) any caller-supplied identity. The secret is
        // the authenticated component; the slug/role are inputs the daemon
        // cross-checks against the secret's session.
        obj.insert("_caller_slug".to_string(), json!(slug));
        obj.insert("_caller_role".to_string(), json!(role));
        obj.insert("_caller_secret".to_string(), json!(secret));
        obj.insert("_caller_sid".to_string(), json!(caller_sid));
    }
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": fwd_args },
    });
    let socket = ccteam_core::daemon_socket_path(paths);
    match crate::mcp_serve::forward_to_socket(&socket, &req).await {
        Ok(resp) => crate::mcp_serve::forward_outcome(&resp),
        Err(err) => Ok(vec![json!({
            "type": "text",
            "text": format!("{name} failed: daemon mcp.sock unreachable ({err})"),
        })]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_im::mcp::{is_session_tool, session_tool_definitions, SESSION_TOOL_NAMES};

    #[test]
    fn five_session_tools_registered_with_correct_names() {
        let tools = session_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__session_spawn"));
        assert!(names.contains(&"ccteam__session_dispatch"));
        assert!(names.contains(&"ccteam__session_collect"));
        assert!(names.contains(&"ccteam__session_list"));
        assert!(names.contains(&"ccteam__session_stop"));
    }

    /// Tripwire for the cto scheduling surface.
    #[test]
    fn cto_scheduling_tools_present_in_canonical_set() {
        for needed in [
            "ccteam__session_spawn",
            "ccteam__session_dispatch",
            "ccteam__session_collect",
            "ccteam__session_list",
            "ccteam__session_stop",
        ] {
            assert!(
                SESSION_TOOL_NAMES.contains(&needed),
                "the cto role depends on the `{needed}` scheduling tool (exposed \
                 to agents as `mcp__ccteam__{needed}`); if you removed or renamed \
                 it, update the daemon session handler + cto role — do not \
                 silently drop it"
            );
        }
    }

    #[test]
    fn definitions_match_the_name_constant() {
        let defs = session_tool_definitions();
        let mut def_names: Vec<&str> = defs.iter().map(|t| t["name"].as_str().unwrap()).collect();
        def_names.sort();
        let mut const_names: Vec<&str> = SESSION_TOOL_NAMES.to_vec();
        const_names.sort();
        assert_eq!(
            def_names, const_names,
            "SESSION_TOOL_NAMES must stay in lockstep with session_tool_definitions()"
        );
    }

    #[test]
    fn all_session_tools_carry_session_prefix() {
        for t in session_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("ccteam__session_"),
                "session tool name must start with ccteam__session_: {n}"
            );
        }
        for t in session_tool_definitions() {
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn session_spawn_schema_carries_full_facet_set() {
        let spawn = session_tool_definitions()
            .into_iter()
            .find(|t| t["name"] == "ccteam__session_spawn")
            .expect("session_spawn defined");
        let props = &spawn["inputSchema"]["properties"];
        // v0.9.0 W1 (G1) — vendor enum lists all FOUR harnesses.
        let vendors: Vec<&str> = props["vendor"]["enum"]
            .as_array()
            .expect("vendor enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(vendors, vec!["claude", "codex", "grok", "opencode"]);
        // New facets present; protocol enum excludes terminal.
        for key in ["model", "effort", "protocol", "host", "title"] {
            assert!(props[key].is_object(), "schema must carry `{key}`");
        }
        let protos: Vec<&str> = props["protocol"]["enum"]
            .as_array()
            .expect("protocol enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(protos, vec!["stream-json", "acp"]);
        // role is optional now → required is empty.
        let required = spawn["inputSchema"]["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "role is optional; required must be empty"
        );
    }

    #[test]
    fn is_session_tool_recognizes_group_and_rejects_others() {
        assert!(is_session_tool("ccteam__session_spawn"));
        assert!(is_session_tool("ccteam__session_stop"));
        assert!(!is_session_tool("ccteam__chat_register_bot"));
        assert!(!is_session_tool("ccteam__advise_vote"));
        assert!(!is_session_tool("ccteam__session_bogus"));
    }

    #[tokio::test]
    async fn dispatch_returns_none_for_foreign_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        assert!(dispatch(&paths, "ccteam__chat_register_bot", &json!({}))
            .await
            .unwrap()
            .is_none());
        assert!(dispatch(&paths, "ccteam__advise_vote", &json!({}))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn dispatch_without_ambient_identity_degrades_softly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let prev_sid = std::env::var("CCTEAM_CHAT_SID").ok();
        let prev_secret = std::env::var("CCTEAM_CHAT_SECRET").ok();
        std::env::remove_var("CCTEAM_CHAT_SID");
        std::env::remove_var("CCTEAM_CHAT_SECRET");
        let body = dispatch(&paths, "ccteam__session_list", &json!({}))
            .await
            .unwrap()
            .expect("session tool matched");
        assert!(
            body.contains("not in a ccteam session"),
            "expected soft-degrade message, got: {body}"
        );
        if let Some(v) = prev_sid {
            std::env::set_var("CCTEAM_CHAT_SID", v);
        }
        if let Some(v) = prev_secret {
            std::env::set_var("CCTEAM_CHAT_SECRET", v);
        }
    }
}
