//! Transport-agnostic MCP protocol core (JSON-RPC value-in / value-out).
//!
//! Speaks `initialize` / `tools/list` / `tools/call` for the **local** tools
//! (`status`, `screenshot`). Stateful tools (`chat_send_file`, `session_*`)
//! are listed in `tools/list` but only dispatched by [`super::dispatch::McpDispatch`]
//! (daemon socket / future HTTP). The stdio process in `ccteam-cli` forwards
//! those tools to the daemon over `mcp.sock` before falling through here.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ccteam_core::{
    check_daemon_health, collect_projects, cost_summary, render_screenshot, CcteamPaths,
    DaemonHealth,
};

use super::groups;

/// Stable MCP protocol version this server speaks.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server identity advertised in `initialize`.
const SERVER_NAME: &str = "ccteam";
/// Workspace-synced version of this crate (same workspace version as the binary).
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Must match `ccteam_flow::MAX_CONCURRENT_PROJECTS` (orchestrator hard cap).
/// Duplicated here so `ccteam-im` does not depend on `ccteam-flow` while the
/// `status` tool body shape stays identical.
const MAX_CONCURRENT_PROJECTS: usize = 3;

/// Server `instructions` surfaced to the agent on `initialize`.
///
/// Load-bearing Read convention: a bare `claude` session does NOT auto-`Read`
/// an attachment path — it must be told to.
pub const CCTEAM_MCP_INSTRUCTIONS: &str = "ccteam routes IM (Telegram / web) chats to you and back. \
An inbound chat message may arrive wrapped in a `<channel source=\"…\" chat_id=\"…\" user=\"…\" message_id=\"…\">` tag.\n\n\
ATTACHMENTS (important): if a `<channel …>` tag carries an `image_path=\"/abs/path\"` attribute, immediately `Read` that file — \
it is an image the user attached (often an error screenshot) and is essential context. If it carries a `file_path=\"/abs/path\"` \
attribute, `Read` that file too. Further attachments may appear in the body as `[attachment image_path=\"…\"]` / \
`[attachment file_path=\"…\"]` lines — `Read` each of those as well. Do this BEFORE you answer; the user expects you to have \
looked at the file they sent.";

/// Full tool names in the session group, registration order.
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

/// Dispatch a single JSON-RPC message. Returns `Some(response)` for
/// requests (which carry an `id`) and `None` for notifications.
///
/// `tools/call` only handles **local** tools (`status`, `screenshot`).
/// Unknown tools (including stateful `chat_send_file` / `session_*` when
/// reached without a prior intercept) return `isError: true`.
pub async fn handle_request(paths: &CcteamPaths, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no `id`) never get a reply.
    let is_notification = id.is_none();

    let result = match method {
        "initialize" => Ok(initialize_response()),
        "notifications/initialized" => return None,
        "tools/list" => Ok(tools_list_response()),
        "tools/call" => match call_tool(paths, &params).await {
            Ok(content) => Ok(json!({ "content": content, "isError": false })),
            Err(err) => {
                // tools/call errors return as a result with isError=true,
                // not as JSON-RPC error envelopes — that's the MCP
                // convention so the client can surface to the LLM.
                Ok(json!({
                    "content": [{ "type": "text", "text": format!("{err:#}") }],
                    "isError": true,
                }))
            }
        },
        other => Err(format!("method not found: {other}")),
    };

    if is_notification {
        return None;
    }
    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(msg) => json_rpc_error(id, -32601, &msg),
    })
}

fn initialize_response() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
        },
        "instructions": CCTEAM_MCP_INSTRUCTIONS,
    })
}

fn tools_list_response() -> Value {
    let disabled = groups::disabled_groups_from_env();
    let tools = groups::filter_by_disabled(tool_definitions(), &disabled);
    json!({ "tools": tools })
}

/// Single source of truth for the MCP tool surface (v0.9 T1):
/// `status` (1) + `screenshot` (1) + `chat_send_file` (1) +
/// session (5) = **8 total**.
pub fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = vec![
        json!({
            "name": "ccteam__status",
            "description": "daemon health + sessions + today's cost",
            "inputSchema": object_schema(&[]),
        }),
        json!({
            "name": "ccteam__screenshot",
            "description": "Render the current tmux pane of a project to a PNG under <project>/.ccteam/screenshots/<utc>.png. Pure Rust pipeline (vt100 → imageproc), no system deps. Returns the absolute path on success or a reason on graceful degrade. V0.2.2 F38.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "lines": {
                        "type": "integer",
                        "description": "Scrollback depth to capture (default 50)."
                    }
                },
                "required": ["slug"],
            }),
        }),
    ];
    tools.extend(chat_tool_definitions());
    tools.extend(session_tool_definitions());
    tools
}

/// Tool definitions for the chat group (`send_file` only after v0.9 T1).
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![json!({
        "name": "ccteam__chat_send_file",
        "description": "V0.8.4 P2b — send a file (image or document) from disk back to YOUR own bound chat (Telegram / Lark / web). Zero addressing params: your identity comes from the spawn-injected CCTEAM_CHAT_SLUG / CCTEAM_CHAT_ROLE env, and the daemon resolves your home chat from the registry. `path` must be on the daemon's filesystem (shared with you under tmux). `kind` is inferred from the extension when omitted (png/jpg/jpeg/gif/webp → photo, else document). To send a rendered screenshot, compose with `screenshot`: it returns a PNG path → pass that to chat_send_file. Delivery reuses the same outbound funnel as text replies (long-message split + durable ledger + failure echo).",
        "inputSchema": json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file on the daemon's filesystem." },
                "caption": { "type": "string", "description": "Optional caption sent with the file." },
                "kind": { "type": "string", "enum": ["photo", "document"], "description": "photo → sendPhoto (compressed image); document → sendDocument (any file). Inferred from the extension when omitted." }
            },
            "required": ["path"],
        }),
    })]
}

/// Tool definitions for the session group (spawn / dispatch / collect / list / stop).
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

fn object_schema(props: &[(&str, &str, &str)]) -> Value {
    let mut p = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, ty, desc) in props {
        p.insert((*name).into(), json!({ "type": ty, "description": desc }));
        required.push(*name);
    }
    json!({
        "type": "object",
        "properties": Value::Object(p),
        "required": required,
    })
}

/// Local-only `tools/call` dispatch (`status` + `screenshot`).
async fn call_tool(paths: &CcteamPaths, params: &Value) -> Result<Vec<Value>> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call missing `name`"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "ccteam__status" => Ok(text_content(tool_ls(paths)?)),
        "ccteam__screenshot" => Ok(text_content(tool_screenshot(paths, &args)?)),
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

fn text_content(body: String) -> Vec<Value> {
    vec![json!({ "type": "text", "text": body })]
}

fn tool_ls(paths: &CcteamPaths) -> Result<String> {
    let projects = collect_projects(paths)?;
    // V0.4.0 F60: active_count was derived from `phase_state == InFlight`;
    // with the phase state machine deleted F66 will recompute this from
    // `state.sessions` (live agent count).
    let active_count = 0usize;
    let arr: Vec<Value> = projects
        .iter()
        .map(|p| {
            let cost = cost_summary(&p.state.slug, &paths.progress_jsonl(&p.state.slug), paths)
                .unwrap_or_default();
            json!({
                "slug": p.state.slug,
                "team": p.state.team,
                "current_phase": p.state.current_phase,
                "phase_state": match p.state.phase_state {
                    ccteam_core::PhaseState::Idle => "idle",
                    ccteam_core::PhaseState::Done => "done",
                },
                "cost_used_usd": cost.cost_total_usd,
                "cost_24h_usd": cost.cost_24h_usd,
                "cost_active_usd": cost.cost_active_usd,
                "tmux_session": p.state.tmux_session,
                "age_seconds": p.age_seconds,
            })
        })
        .collect();
    let health = check_daemon_health(paths);
    let body = json!({
        "projects": arr,
        "orchestrator": {
            "active_count": active_count,
            "max_concurrent": MAX_CONCURRENT_PROJECTS,
            "daemon_health": daemon_health_json(&health),
        },
    });
    Ok(serde_json::to_string_pretty(&body)?)
}

fn daemon_health_json(health: &DaemonHealth) -> Value {
    match health {
        DaemonHealth::Healthy { socket } => json!({
            "status": "healthy",
            "socket": socket.display().to_string(),
            "message": health.describe(),
        }),
        DaemonHealth::Unreachable { socket, reason } => json!({
            "status": "unreachable",
            "socket": socket.display().to_string(),
            "reason": reason,
            "message": health.describe(),
        }),
    }
}

fn tool_screenshot(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    match render_screenshot(paths, &slug, None, lines)? {
        Some(path) => Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "slug": slug,
            "path": path.to_string_lossy(),
        }))?),
        None => Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "slug": slug,
            "reason": "screenshot rendering degraded; check daemon stderr for warn details \
                      (tmux missing, session not found, font failed, or IO failure)",
        }))?),
    }
}

fn arg_string(args: &Value, name: &str) -> Result<String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing required argument `{name}`"))
}

fn json_rpc_error(id: Option<Value>, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact set of MCP tool names after the v0.9 T1 cull (8 tools).
    const EXPECTED_TOOL_NAMES: &[&str] = &[
        "ccteam__chat_send_file",
        "ccteam__screenshot",
        "ccteam__session_collect",
        "ccteam__session_dispatch",
        "ccteam__session_list",
        "ccteam__session_spawn",
        "ccteam__session_stop",
        "ccteam__status",
    ];

    #[test]
    fn tool_definitions_count_matches_spec() {
        assert_eq!(tool_definitions().len(), 8);
        assert_eq!(tool_definitions().len(), EXPECTED_TOOL_NAMES.len());
    }

    #[test]
    fn tool_definitions_exact_set() {
        let tools = tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        let mut expected: Vec<&str> = EXPECTED_TOOL_NAMES.to_vec();
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn tool_definitions_have_unique_names_and_object_schemas() {
        let tools = tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8, "tool names must be unique");
        for tool in &tools {
            assert!(tool["name"].as_str().unwrap().starts_with("ccteam__"));
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn screenshot_tool_definition_present_with_optional_lines() {
        let tools = tool_definitions();
        let s = tools
            .iter()
            .find(|t| t["name"] == "ccteam__screenshot")
            .expect("screenshot tool registered");
        let req: Vec<&str> = s["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(req, vec!["slug"]);
        assert_eq!(s["inputSchema"]["properties"]["lines"]["type"], "integer");
    }

    #[test]
    fn one_chat_tool_registered_send_file() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "ccteam__chat_send_file");
    }

    #[test]
    fn five_session_tools_registered_with_correct_names() {
        let tools = session_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for needed in SESSION_TOOL_NAMES {
            assert!(names.contains(needed), "missing {needed}");
        }
    }

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
                "the cto role depends on the `{needed}` scheduling tool"
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
        assert_eq!(def_names, const_names);
    }

    #[test]
    fn session_spawn_schema_carries_permission_mode_param() {
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
        assert!(!is_session_tool("ccteam__chat_register_bot"));
        assert!(!is_session_tool("ccteam__session_bogus"));
    }

    #[test]
    fn json_rpc_error_includes_id_and_envelope() {
        let e = json_rpc_error(Some(json!(7)), -32601, "method not found: foo");
        assert_eq!(e["jsonrpc"], "2.0");
        assert_eq!(e["id"], 7);
        assert_eq!(e["error"]["code"], -32601);
        assert!(e["error"]["message"].as_str().unwrap().contains("foo"));
    }

    #[tokio::test]
    async fn handle_initialize_returns_tools_capability() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
        let instructions = resp["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("image_path"));
        assert!(instructions.contains("file_path"));
        assert!(instructions.contains("Read"));
        assert!(instructions.contains("<channel"));
    }

    #[tokio::test]
    async fn handle_tools_list_returns_full_tool_set() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 8);
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        let mut expected = EXPECTED_TOOL_NAMES.to_vec();
        expected.sort();
        assert_eq!(names, expected);
        for gone in [
            "ccteam__admin_ls",
            "ccteam__admin_change_persona",
            "ccteam__admin_add_tool",
            "ccteam__advise_vote",
            "ccteam__advise_parallel",
            "ccteam__chat_register_bot",
            "ccteam__chat_unregister_bot",
            "ccteam__chat_list_bots",
            "ccteam__chat_lifecycle",
            "ccteam__workflow_show",
        ] {
            assert!(!names.contains(&gone), "culled tool present: {gone}");
        }
    }

    #[tokio::test]
    async fn handle_tools_call_screenshot_degrades_when_session_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "ccteam__screenshot",
                "arguments": { "slug": "no-such-slug-xyz", "lines": 5 }
            }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("\"ok\": false"),
            "expected ok=false on graceful degrade, got: {text}"
        );
    }

    #[tokio::test]
    async fn handle_notifications_initialized_returns_no_response() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        assert!(handle_request(&paths, &req).await.is_none());
    }

    #[tokio::test]
    async fn handle_tools_call_ls_returns_empty_projects_array_for_fresh_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "ccteam__status", "arguments": {} }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn handle_tools_call_unknown_tool_returns_iserror_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "ccteam__no_such_tool", "arguments": {} }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn ls_succeeds_without_daemon_and_annotates_health() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 72,
            "method": "tools/call",
            "params": { "name": "ccteam__status", "arguments": {} }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(
            parsed["orchestrator"]["daemon_health"]["status"], "unreachable",
            "status must annotate daemon health when daemon is down"
        );
    }
}
