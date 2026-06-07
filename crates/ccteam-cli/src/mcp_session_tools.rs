//! `ccteam__session_*` MCP tools (v0.8.7 W1 — cto scheduling, B-tier).
//!
//! These let the privileged `cto` role drive the **gateway session map**:
//! spawn a work-role session, dispatch a task to it, collect its result,
//! list live sessions, and stop one. They are the "give the cto real
//! scheduling" half of v0.8.7 Item A; v0.8.6 already shipped the `pub`
//! gateway primitives (`create_session_api` / `submit_to_sid` /
//! `session_views` / `stop_session`) this group wraps.
//!
//! Architecture (mirrors `chat_send_file`, `mcp_serve::forward_chat_send_file`):
//!
//! - The stdio MCP server (this module) does NOT own the gateway — the
//!   long-lived daemon does. So every `session_*` call is **forwarded over
//!   `~/.ccteam/run/mcp.sock`** to the daemon, which holds the
//!   `Arc<Mutex<Gateway>>`, enforces the privilege gate, and drives the
//!   session map. The daemon-side handler lives in `main.rs`
//!   (`is_session_tool_call` / `execute_session_tool`).
//! - The caller's identity is **ambient**: `CCTEAM_CHAT_SLUG` /
//!   `CCTEAM_CHAT_ROLE` / `CCTEAM_CHAT_SECRET` are injected into the tmux pane
//!   at spawn (`claude_tui::chat_spawn_env_owned`), inherited by this stdio
//!   process, and re-injected into the forwarded args here as `_caller_slug` /
//!   `_caller_role` / `_caller_secret` so the daemon can resolve "which
//!   project" + authenticate "this is really the cto session" by matching the
//!   secret. We overwrite any caller-supplied value (same plumbing as
//!   `chat_send_file`).
//!
//! Permission layering (defense-in-depth, NOT a hard boundary — see honest
//! scope below):
//!   1. `cto_role.md` frontmatter `tools:` grants the `mcp__ccteam__session_*`
//!      handles; work-role templates do NOT, so Claude's per-agent allow-list
//!      discourages a non-cto role from calling the tool. (Vendor caveat: MCP
//!      tools may bypass that allow-list depending on the CLI version, so this
//!      layer is best-effort, not load-bearing.)
//!   2. The daemon handler authenticates the forwarded `(role, secret)` pair
//!      against its `sid -> {role, secret}` session map and returns an MCP
//!      `isError` result on a non-cto role OR a missing/wrong secret. A cheap
//!      role pre-filter runs first so an obvious non-cto is denied even with
//!      the gateway down; the secret match is the security-relevant check.
//!
//! HONEST SCOPE (do not over-claim): under the current single-OS-uid
//! full-trust model there is NO hard boundary between agents. A same-uid
//! process can read another pane's `/proc/<pid>/environ`, its files, or ptrace
//! it, and thereby recover `CCTEAM_CHAT_SECRET`. The secret therefore only
//! RAISES THE BAR (stops the trivial "send `{_caller_role:"cto"}` over the
//! socket" forgery); it does NOT close the hole. Real per-agent isolation
//! requires a per-agent OS user or sandbox — tracked as v0.8.8-deferred.
//!
//! Red lines honored: this is the GATEWAY session map, NOT the deprecated
//! registry/supervisor (`chat_*`) machinery — the two are never mixed.
//! `dispatch`/`stop` are explicit cto commands (never proactive kill). No
//! prompt injection: role behavior stays in `.claude/agents/*.md`; we only
//! forward the task text as a user turn. `collect` is polled (MVP) — it tails
//! the child's `turns.jsonl`; push-back-as-turn is v0.8.8.

use anyhow::Result;
use serde_json::{json, Value};

use ccteam_core::paths::CcteamPaths;

/// Full tool names in this group, in registration order. Used by the
/// stdio dispatch + the daemon-side intercept predicate so both sides
/// agree on membership without duplicating string literals.
pub const SESSION_TOOL_NAMES: &[&str] = &[
    "ccteam__session_spawn",
    "ccteam__session_dispatch",
    "ccteam__session_collect",
    "ccteam__session_list",
    "ccteam__session_stop",
];

/// True if `name` is one of the `ccteam__session_*` tools.
pub fn is_session_tool(name: &str) -> bool {
    SESSION_TOOL_NAMES.contains(&name)
}

/// Tool definitions for the session group (total 5): spawn / dispatch /
/// collect / list / stop. Merged into the top-level `tool_definitions()`
/// in `mcp_serve.rs`.
pub fn session_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__session_spawn",
            "description": "Spawn a work-role session in the gateway and return its `s{n}` id. Privileged: only the `cto` role may call this (the daemon authenticates the caller's per-session secret; the cto agent's tool allow-list is a secondary discouragement). The new session runs `<role>` from `.claude/agents/<role>.md` and is ALWAYS created in the caller's OWN bound project (there is no project parameter — a cto bound to project A cannot spawn into another project). `vendor` defaults to `claude`. Always mints a NEW sid: a second spawn of the same role creates a SEPARATE session (its own pane + sid + independent transcript), so you can run several instances of one role in parallel. After spawning, drive it with session_dispatch and read its answer with session_collect.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "Work-role to spawn (must exist as `.claude/agents/<role>.md`)." },
                    "vendor": {
                        "type": "string",
                        "enum": ["claude", "codex"],
                        "description": "Harness vendor (lowercase). Default `claude`."
                    },
                    "permission_mode": {
                        "type": "string",
                        "enum": ["skip", "hitl"],
                        "description": "Permission posture (default `skip`). `hitl` (human-in-the-loop) drops the skip flag at spawn so a non-allowlist tool call pops an approve/deny prompt to the bound IM chat; allowlist/auto-allowed tools never prompt. Use for a supervised work-role."
                    }
                },
                "required": ["role"],
            }),
        }),
        json!({
            "name": "ccteam__session_dispatch",
            "description": "Dispatch a task (a user-turn) to a gateway session addressed by `sid` (e.g. `s2` from session_spawn). Privileged: cto only, and the `sid` must run in the caller's OWN project (cross-project dispatch is rejected). The `task` text is forwarded verbatim as a user turn to the child session's agent (NO system prompt injection). Returns the submitted turn id. The child runs asynchronously; poll session_collect to read its answer once the turn completes. This is an explicit dispatch, never a proactive kill.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "sid": { "type": "string", "description": "Gateway session id (`s{n}`) from session_spawn / session_list." },
                    "task": { "type": "string", "description": "Task / instruction text, forwarded verbatim as a user turn." }
                },
                "required": ["sid", "task"],
            }),
        }),
        json!({
            "name": "ccteam__session_collect",
            "description": "Collect (poll) a child session's transcript by `sid`. Privileged: cto only, and the `sid` must run in the caller's OWN project (cross-project collect is rejected). Tails `<project>/.ccteam/chat/<sid>/turns.jsonl` (the ccteam-owned mirror the child's answers are written to, keyed by sid so parallel same-role sessions never bleed) and returns assistant-side turns. Pass `since` (a turn_id you already saw) to return only turns AFTER it — the polling cursor for collecting incremental results. MVP = polled (push-back-as-turn, where the child's result is injected straight into cto's context, is v0.8.8). Returns an empty `turns` array when the child hasn't answered yet.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "sid": { "type": "string", "description": "Gateway session id (`s{n}`) to collect from." },
                    "since": { "type": "string", "description": "Optional turn_id cursor — return only assistant turns recorded AFTER this id." },
                    "n": { "type": "integer", "description": "Max turns to return (default 20). Applied after the `since` cursor filter." }
                },
                "required": ["sid"],
            }),
        }),
        json!({
            "name": "ccteam__session_list",
            "description": "List the gateway's live sessions (the same `s{n}` namespace session_spawn allocates). Privileged: cto only. Each row carries `sid`, `project`, `role`, `vendor`, `current`, `status`. Use this to find a `sid` to dispatch to or collect from.",
            "inputSchema": json!({
                "type": "object",
                "properties": {},
                "required": [],
            }),
        }),
        json!({
            "name": "ccteam__session_stop",
            "description": "Stop a gateway session by `sid` (deregister + close its pane). Privileged: cto only, and the `sid` must run in the caller's OWN project (cross-project stop is rejected). This is an EXPLICIT command (the cto deciding the work is done), NOT a proactive kill — it never file-purges the transcript, so a later session_collect of an already-collected `turns.jsonl` still works until cleanup. An unknown sid is an error.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "sid": { "type": "string", "description": "Gateway session id (`s{n}`) to stop." }
                },
                "required": ["sid"],
            }),
        }),
    ]
}

/// Stdio-side dispatch for a `ccteam__session_*` tool: inject the ambient
/// caller identity and forward to the daemon over `mcp.sock`. Returns
/// `Ok(None)` for tools that aren't ours so `call_tool` falls through.
///
/// The forwarded request reuses the EXACT shape the daemon's
/// `execute_session_tool` expects: a `tools/call` whose `arguments` carry the
/// original args plus `_caller_slug` / `_caller_role` / `_caller_secret`
/// overwritten from the env (caller-supplied values are ignored). The secret
/// is what the daemon authenticates; it only raises the bar (not a hard
/// boundary under a single-uid model — see the module-level honest scope).
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
    if slug.is_empty() || role.is_empty() {
        return Ok(vec![json!({
            "type": "text",
            "text": format!(
                "{name}: not in a ccteam chat session (CCTEAM_CHAT_SLUG/ROLE unset); session tools are only callable from a live session"
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
        // Every tool def has an object inputSchema.
        for t in session_tool_definitions() {
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn session_spawn_schema_carries_permission_mode_param() {
        // v0.8.7 W2 (DB.1) — session_spawn gains an optional permission_mode
        // param (a schema change to an EXISTING tool — tool count stays 17).
        let spawn = session_tool_definitions()
            .into_iter()
            .find(|t| t["name"] == "ccteam__session_spawn")
            .expect("session_spawn defined");
        let pm = &spawn["inputSchema"]["properties"]["permission_mode"];
        assert_eq!(pm["type"], "string");
        let en: Vec<&str> = pm["enum"]
            .as_array()
            .expect("permission_mode has an enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(en, vec!["skip", "hitl"]);
        // It is NOT required (default skip), so old callers keep working.
        let required: Vec<&str> = spawn["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(!required.contains(&"permission_mode"));
        assert_eq!(required, vec!["role"]);
    }

    #[test]
    fn is_session_tool_recognizes_group_and_rejects_others() {
        assert!(is_session_tool("ccteam__session_spawn"));
        assert!(is_session_tool("ccteam__session_stop"));
        assert!(!is_session_tool("ccteam__chat_send_input"));
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
        assert!(dispatch(&paths, "ccteam__chat_send_input", &json!({}))
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
        // No CCTEAM_CHAT_SLUG/ROLE in this test process's env (and we do
        // not set it — env-mutating cases live in integration tests). The
        // stdio forwarder must NOT error; it returns a structured message
        // so the agent sees a clear reason rather than a tool failure.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let prev_slug = std::env::var("CCTEAM_CHAT_SLUG").ok();
        let prev_role = std::env::var("CCTEAM_CHAT_ROLE").ok();
        std::env::remove_var("CCTEAM_CHAT_SLUG");
        std::env::remove_var("CCTEAM_CHAT_ROLE");
        let body = dispatch(&paths, "ccteam__session_list", &json!({}))
            .await
            .unwrap()
            .expect("session tool matched");
        assert!(
            body.contains("not in a ccteam chat session"),
            "expected soft-degrade message, got: {body}"
        );
        if let Some(v) = prev_slug {
            std::env::set_var("CCTEAM_CHAT_SLUG", v);
        }
        if let Some(v) = prev_role {
            std::env::set_var("CCTEAM_CHAT_ROLE", v);
        }
    }
}
