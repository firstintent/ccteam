//! Transport-agnostic MCP protocol core (JSON-RPC value-in / value-out).
//!
//! Speaks `initialize` / `tools/list` / `tools/call` for the **local** tool
//! (`status`). Stateful tools (`chat_send_file`, `session_*`)
//! are listed in `tools/list` but only dispatched by [`super::dispatch::McpDispatch`]
//! (daemon socket / future HTTP). The stdio process in `ccteam-cli` forwards
//! those tools to the daemon over `mcp.sock` before falling through here.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ccteam_core::{check_daemon_health, collect_projects, CcteamPaths, DaemonHealth};

use super::groups;

/// Stable MCP protocol version this server speaks.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server identity advertised in `initialize`.
const SERVER_NAME: &str = "ccteam";
/// Workspace-synced version of this crate (same workspace version as the binary).
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Server `instructions` surfaced to the agent on `initialize`.
///
/// Two load-bearing conventions:
/// - **Orchestration-first**: when the user asks for another agent ("call
///   codex", "have claude review this"), the tracked path is `session_*` —
///   NOT shelling out to `codex exec` / `claude -p`, which bypasses the
///   ledger (no sid, no transcript, no cost, invisible to `session_list`).
///   The model only ever sees tool schemas + these instructions, so this is
///   where that steer lives.
/// - **Attachments**: a bare `claude` session does NOT auto-`Read` an
///   attachment path — it must be told to.
pub const CCTEAM_MCP_INSTRUCTIONS: &str = "ccteam is the local agent bridge: any session can hire other agent sessions \
(Claude Code / Codex / Grok / OpenCode / Kimi / Pi / DSH; Pi and DSH are local-only, the others may run on a registered satellite host) and ccteam does the identity, \
routing, delivery, guardrails, cost ledger, and team observability underneath.\n\n\
ORCHESTRATION (important): when the user asks you to call / use / delegate to another agent (e.g. \"call codex\", \
\"use grok to search\", \"spawn a reviewer\"), use the `session_*` tools — `session_spawn` starts a session (pick \
`vendor`: `claude` / `codex` / `grok` / `opencode` / `kimi` / `pi` / `dsh`, optionally \
`model` / `role`, and pass the first `task` in the same call); its execution host is inherited from the project \
binding. `session_dispatch` sends follow-up tasks \
(async with a completion notification for managed parent sessions, or `wait_seconds` to block inline), `session_collect` reads its output \
(`tail:true` for the final answer), `session_list` shows the delegation tree (`is_self` marks YOUR row), \
`session_stop` ends it. Do NOT shell \
out to vendor CLIs (`codex exec`, `claude -p`, …) for this: a raw CLI run has no session id, no transcript, no cost \
tracking, no completion notification, and is invisible to the team — it bypasses the bridge. The tools work both \
from ccteam-spawned sessions (per-session principal) and from a hand-started agent, which enrolls on its first \
call: if your first `session_*` call is refused for having no project, name one (`project: \"<slug>\"`) — the error \
lists the ones you can reach, and the first project you name is your workspace for the rest of the session. ccteam \
never guesses it from your working directory. A hand-started agent has no return transport for a completion \
notification, so async responses report `notify_deliverable:false`; poll `session_collect` (or use \
`wait_seconds`) instead.\n\n\
CHAT ROUTING: ccteam routes IM (Telegram / web) chats to you and back. \
An inbound chat message may arrive wrapped in a `<channel source=\"…\" chat_id=\"…\" user=\"…\" message_id=\"…\">` tag.\n\n\
ATTACHMENTS (important): if a `<channel …>` tag carries an `image_path=\"/abs/path\"` attribute, immediately `Read` that file — \
it is an image the user attached (often an error screenshot) and is essential context. If it carries a `file_path=\"/abs/path\"` \
attribute, `Read` that file too. Further attachments may appear in the body as `[attachment image_path=\"…\"]` / \
`[attachment file_path=\"…\"]` lines — `Read` each of those as well. Do this BEFORE you answer; the user expects you to have \
looked at the file they sent.";

/// Bare-name discovery beacon — a PURE ALIAS of `status` (same handler,
/// same response). Some MCP hosts strip descriptions and server
/// instructions from ambient context and surface tool NAMES only; in that
/// world nothing in `status` / `chat_send_file` / `session_*` says "grok"
/// or "codex", so "use grok to search" dies on first-turn discovery. This
/// name front-loads the owner-pinned discovery literal. Pi deliberately stays
/// in the `status`/`session_spawn` descriptions instead of renaming this tool.
pub const STATUS_BEACON_TOOL_NAME: &str = "grok_claude_codex_kimi";

/// Full tool names in the session group, registration order.
pub const SESSION_TOOL_NAMES: &[&str] = &[
    "session_spawn",
    "session_dispatch",
    "session_collect",
    "session_list",
    "session_stop",
];

/// True if `name` is one of the `session_*` tools.
pub fn is_session_tool(name: &str) -> bool {
    SESSION_TOOL_NAMES.contains(&name)
}

/// Dispatch a single JSON-RPC message. Returns `Some(response)` for
/// requests (which carry an `id`) and `None` for notifications.
///
/// `tools/call` only handles the **local** tool (`status`).
/// Unknown tools (including stateful `chat_send_file` / `session_*` when
/// reached without a prior intercept) return `isError: true`.
pub async fn handle_request(paths: &CcteamPaths, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no `id`) never get a reply.
    let is_notification = id.is_none();

    let result = match method {
        "initialize" => Ok(initialize_response(&params)),
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

/// Build the `initialize` result. Echo the client's
/// `params.protocolVersion` when present (MCP negotiation); otherwise
/// answer with [`MCP_PROTOCOL_VERSION`].
fn initialize_response(params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(MCP_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
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

/// Single source of truth for the MCP tool surface:
/// `status` (1) + its bare-name beacon alias (1) + `chat_send_file` (1) +
/// session (5) = **8 total**.
pub fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = vec![
        json!({
            "name": "status",
            "description": "Discovery + health: which of claude / codex / grok / opencode / kimi / pi / dsh are installed on your project's host, plus per-vendor session_spawn recipes, daemon health, cost/budget, advisory models, and routing notes. Managed Pi sessions get the bridge; plain shell pi does not. Managed DSH sessions get the ccteam plugin; plain shell dsh needs @ccteam/ccteam-client.",
            "inputSchema": object_schema(&[]),
        }),
        json!({
            "name": STATUS_BEACON_TOOL_NAME,
            "description": "Alias of status (discovery beacon for hosts that surface tool names only). Which agents this machine can spawn — claude / codex / grok / kimi / opencode / pi / dsh — with install/auth state and per-vendor session_spawn recipes. Identical response to status.",
            "inputSchema": object_schema(&[]),
        }),
    ];
    tools.extend(chat_tool_definitions());
    tools.extend(session_tool_definitions());
    tools
}

/// Tool definitions for the chat group (`send_file` only after v0.9 T1).
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![json!({
        "name": "chat_send_file",
        "description": "Send a file (image or document) from disk back to YOUR own bound chat (Telegram / Lark / web) — a chat user cannot open a local path, so this is how a generated artifact (chart, report, photo) actually reaches them. Zero addressing params: the daemon resolves your home chat from your session identity. `path` must be on the daemon's filesystem. `kind` is inferred from the extension when omitted (png/jpg/jpeg/gif/webp → photo, else document). Delivery reuses the same outbound funnel as text replies (long-message split + durable ledger + failure echo).",
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
            "name": "session_spawn",
            "description": "Spawn an agent session — vendor: claude (default) | codex | grok | opencode | kimi | pi | dsh — in YOUR OWN project; always mints a NEW s{n} sid. Pass `task` to dispatch the first task in the same call and tell the child to answer tersely with a structured summary, never code/diff dumps. Spawn-only returns `{sid}`; async task returns `{sid,turn_id,status}`; inline completion adds `{turn,context_pct?,result_text,error_kind?,error?,cost_usd?}`. Completion notification starts `s<N> done · turn N · ctx N%`; `session_collect{sid, tail:true}` returns full text. A hand-started caller also gets `notify_deliverable:false` and must poll. Auth: your per-session principal; host follows the project binding; status shows availability.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "Optional work-role (must exist as `.claude/agents/<role>.md`). Omit or pass \"\" for a roleless session (bare vendor reads the project CLAUDE.md/AGENTS.md)." },
                    "vendor": {
                        "type": "string",
                        "enum": ["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"],
                        "description": "Harness vendor (lowercase). Default claude."
                    },
                    "model": { "type": "string", "description": "Optional explicit model id, passed to the vendor verbatim; overrides the role's `model:` frontmatter. Omitted → vendor default. `status` lists each installed vendor's observed ids." },
                    "effort": { "type": "string", "description": "Optional reasoning-effort token, passed to the vendor verbatim for EVERY vendor — the value set is vendor-specific and the vendor validates it (a bad token fails the spawn with its own error, it is never silently ignored). Omitted → vendor default. `status` lists each installed vendor's effort ladder." },
                    "mode": { "type": "string", "description": "Optional vendor session-mode token, validated by the vendor adapter. DSH only today: its agent preset — `standard` | `ptc` (alias `code`) | `minimal` | `creator` (alias `cordis`); omitted → DSH hires default to `standard`. Every other vendor refuses a non-empty mode." },
                    "project": { "type": "string", "description": "Target project slug. A managed session always spawns into its OWN project and may omit this. A hand-started (enrolled) caller names its workspace here on its first call — that choice sticks for the session, and `status` lists the slugs it can reach. Never inferred from a working directory." },
                    "permission_mode": {
                        "type": "string",
                        "enum": ["skip", "hitl"],
                        "description": "Permission posture (default `skip`). `hitl` (human-in-the-loop) makes a non-allowlist tool call pop an approve/deny prompt to the bound IM chat; allowlist/auto-allowed tools never prompt."
                    },
                    "title": { "type": "string", "description": "Optional short label (≤80 chars) for the ledger / team visualization only — NEVER sent to the agent or concatenated into any prompt." },
                    "task": { "type": "string", "description": "Optional FIRST task — dispatched to the fresh child in the same call, exactly like session_dispatch{sid, task} (verbatim user turn, no injection). Omit to spawn only." },
                    "wait_seconds": { "type": "integer", "description": "With `task`: request 0–600 seconds (default 0 = async); effective inline wait is capped at 240s. Use inline wait for health probes/short tasks; keep long/repo tasks async (managed parents get a notification; a hand-started agent polls collect). Pending/timeout never cancels the child." },
                    "notify": { "type": ["string", "boolean"], "description": "With `task`: for a managed parent, `final` (default) wakes it ONCE when the child's vendor turn ends; `all` wakes it on every assistant message of that task (debug firehose); `off` = ledger-only. Every mode covers THIS task only — the watch ends with it. A hand-started (enrolled) caller has no notification return transport: the response reports `notify_deliverable:false`; poll session_collect. Booleans still parse: true→final, false→off." },
                    "idempotency_key": { "type": "string", "description": "Optional client key. A retry with the same key (per-project, within ~1h) replays the ORIGINAL spawn (same sid + same dispatch outcome, zero side effects) instead of creating a second session — safe against MCP-client timeouts. In-memory only: a daemon restart forgets keys." },
                    "parent_sid": { "type": "string", "description": "Your OWN sid, when you are a plain local session ccteam mirrors in its ledger (session_list shows you). A managed session never needs this — its parent comes from its principal — but a plain one is anonymous to the bridge, so without it the child mounts as a root and the delegation tree loses the edge. Validated: an unknown sid is an error, not a silent root." }
                },
                "required": [],
            }),
        }),
        json!({
            "name": "session_dispatch",
            "description": "Dispatch a verbatim user task to a session in YOUR OWN project; tell the child to answer tersely with a structured summary, never code/diff dumps. Async returns `{turn_id,status}`; inline completion adds `{turn,context_pct?,result_text,error_kind?,error?,cost_usd?}`. Completion notification starts `s<N> done · turn N · ctx N%`; `session_collect{sid, tail:true}` returns full text. Timeout returns `pending` and never cancels. Self/ancestor cycles are rejected. A peer handoff arms no watch unless `notify` is explicit. Explicit dispatch, never a proactive kill.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "sid": { "type": "string", "description": "Gateway session id (`s{n}`) from session_spawn / session_list." },
                    "task": { "type": "string", "description": "Task / instruction text, forwarded verbatim as a user turn." },
                    "wait_seconds": { "type": "integer", "description": "Request 0–600 seconds (default 0 = async); effective inline wait is capped at 240s. Use inline wait for health probes/short tasks; keep long/repo tasks async (managed parents get a notification; a hand-started agent polls collect). Pending/timeout never cancels the child." },
                    "notify": { "type": ["string", "boolean"], "description": "For a managed parent, `final` (default) wakes it ONCE when the child's vendor turn ends; `all` wakes it on every assistant message of that task (debug firehose); `off` = ledger-only. Every mode covers THIS task only — the watch ends with it. For a target you did not delegate, the default subscribes you to nothing: name `notify` to opt in for that one task. A hand-started (enrolled) caller has no notification return transport: the response reports `notify_deliverable:false`; poll session_collect. Booleans still parse: true→final, false→off." },
                    "title": { "type": "string", "description": "Optional short label (≤80 chars) for the notification / ledger only — NEVER concatenated into the task or any prompt." },
                    "idempotency_key": { "type": "string", "description": "Optional client key. A retry with the same key (per-target-child, within ~1h) replays the ORIGINAL dispatch (same turn) instead of double-dispatching. In-memory only: a daemon restart forgets keys." }
                },
                "required": ["sid", "task"],
            }),
        }),
        json!({
            "name": "session_collect",
            "description": "Collect a session transcript in YOUR OWN project. Returns `{activity,context_pct?,cursor?,truncated?,turns:[{turn_id,content,outcome?,error_kind?,error?}],cost_usd?,tokens_total?}`, plus `residency` when ccteam holds no process for it: `released` (dispatch resumes it) or `stopped` (ended by its user). Pass `since` for turns after a cursor; default paging is oldest-first, `tail:true` selects the newest `n`. Empty `turns` means no answer yet.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "sid": { "type": "string", "description": "Gateway session id (`s{n}`) to collect from." },
                    "since": { "type": "string", "description": "Optional turn_id cursor — return only assistant turns recorded AFTER this id." },
                    "n": { "type": "integer", "description": "Max turns to return (default 20). Applied after the `since` cursor filter." },
                    "tail": { "type": "boolean", "description": "When true, return the NEWEST `n` turns (after the `since` filter) instead of the oldest — use to grab the final answer of a long transcript without paging." },
                    "max_chars": { "type": "integer", "description": "Maximum total characters across returned turn contents (default 10000; clamped to 500–50000). Longer contents retain a 70% head / 30% tail excerpt with an explicit ledger pointer." }
                },
                "required": ["sid"],
            }),
        }),
        json!({
            "name": "session_list",
            "description": "List sessions you can reach, most recently active first. Rows are `{sid,vendor,role?,title?,activity,residency?,context_pct?,parent_sid?,is_self?,waiting_approval?,host?,cost_usd?,tokens_total?}`; host is non-local only. `residency` appears only when ccteam holds no process: `released` sessions are idle-but-real and resume on your next dispatch, so reuse one instead of spawning a duplicate. Filter with `project`/`activity`; cap with `limit` (default 30). `truncated:true`, `total`, and a hint appear only when capped. Pass `tree:true` to add delegation topology over the filtered rows. Use `is_self` to find your own sid.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Only list sessions of this project slug." },
                    "activity": { "type": "string", "enum": ["working", "idle", "stale", "stuck", "all"], "description": "Only list sessions with this activity state (default `all`)." },
                    "limit": { "type": "integer", "description": "Max rows returned, most recently active first (default 30, clamped to 1–500)." },
                    "tree": { "type": "boolean", "description": "When true, include delegation topology over the filtered rows (default false)." }
                },
                "required": [],
            }),
        }),
        json!({
            "name": "session_stop",
            "description": "Stop a session by `sid`; returns `{sid,stopped:true}`. The target must run in YOUR OWN project. This is an EXPLICIT command, never a proactive kill, and it does not purge the transcript; session_collect can still read recorded turns. Unknown sid is an error.",
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

/// Local-only `tools/call` dispatch (`status` + its beacon alias).
async fn call_tool(paths: &CcteamPaths, params: &Value) -> Result<Vec<Value>> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call missing `name`"))?;
    if name == "status" || name == STATUS_BEACON_TOOL_NAME {
        return Ok(text_content(tool_ls(paths)?));
    }
    Err(anyhow!("unknown tool: {name}"))
}

fn text_content(body: String) -> Vec<Value> {
    vec![json!({ "type": "text", "text": body })]
}

/// Base `status` JSON body (slim projects + daemon health). The
/// daemon-aware dispatch path reuses this verbatim, then appends the vendor
/// panel + routing notes (see [`super::dispatch`]).
pub(crate) fn tool_ls(paths: &CcteamPaths) -> Result<String> {
    tool_ls_matching(paths, |_| true)
}

/// Tenant-scoped base `status` body. This is intentionally the same renderer
/// as [`tool_ls`], with only the shared owner-policy filter applied.
pub(crate) fn tool_ls_for_user(paths: &CcteamPaths, user_id: &str) -> Result<String> {
    tool_ls_matching(paths, |state| {
        ccteam_core::identity::can_see_owner(user_id, false, state.owner.as_deref())
    })
}

fn tool_ls_matching(
    paths: &CcteamPaths,
    mut visible: impl FnMut(&ccteam_core::ProjectState) -> bool,
) -> Result<String> {
    let projects = collect_projects(paths)?;
    let projection = crate::progress_projection::ProgressProjection::new(paths.clone());
    let arr: Vec<Value> = projects
        .iter()
        .filter(|project| visible(&project.state))
        .map(|p| {
            let cost = projection.project_snapshot(&p.state.slug).cost;
            json!({
                "slug": p.state.slug,
                "cost_24h_usd": cost.cost_24h_usd,
            })
        })
        .collect();
    let health = check_daemon_health(paths);
    let body = json!({
        "projects": arr,
        "daemon": daemon_health_json(&health),
    });
    Ok(serde_json::to_string_pretty(&body)?)
}

fn daemon_health_json(health: &DaemonHealth) -> Value {
    match health {
        DaemonHealth::Healthy { .. } => json!({
            "status": "healthy",
            "message": health.describe(),
        }),
        DaemonHealth::Unreachable { .. } => json!({
            "status": "unreachable",
            "message": health.describe(),
        }),
    }
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

    /// Exact set of MCP tool names (8 tools; `screenshot` culled 2026-07-26
    /// as tmux-era legacy, the bare-name status beacon alias added the same
    /// day — owner-ordered both).
    const EXPECTED_TOOL_NAMES: &[&str] = &[
        "chat_send_file",
        "grok_claude_codex_kimi",
        "session_collect",
        "session_dispatch",
        "session_list",
        "session_spawn",
        "session_stop",
        "status",
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
            // Wire names are BARE: the MCP client namespaces by server key
            // (`mcp__ccteam__session_spawn`), so a baked-in `ccteam__`
            // prefix would render as `mcp__ccteam__ccteam__session_spawn`.
            assert!(
                !tool["name"].as_str().unwrap().starts_with("ccteam__"),
                "wire tool name must not embed the server prefix: {}",
                tool["name"]
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn one_chat_tool_registered_send_file() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "chat_send_file");
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

    /// Group prefix + schema shape for the whole session group. Ported here
    /// from the deleted stdio forwarder, which owned the only copy: the
    /// invariant is about the wire surface, not about any transport.
    #[test]
    fn all_session_tools_carry_session_prefix() {
        for t in session_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("session_"),
                "session tool name must start with session_: {n}"
            );
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn collect_schema_exposes_character_budget_and_delegation_prompts_are_terse() {
        let defs = session_tool_definitions();
        let collect = defs
            .iter()
            .find(|t| t["name"] == "session_collect")
            .unwrap();
        assert_eq!(
            collect["inputSchema"]["properties"]["max_chars"]["type"],
            "integer"
        );
        for name in ["session_spawn", "session_dispatch"] {
            let description = defs.iter().find(|t| t["name"] == name).unwrap()["description"]
                .as_str()
                .unwrap();
            assert!(description.contains("s<N> done · turn N · ctx N%"));
            assert!(description.contains("session_collect{sid, tail:true}"));
            assert!(description.contains("structured summary"));
            assert!(description.contains("never code/diff dumps"));
        }
        let list = defs.iter().find(|t| t["name"] == "session_list").unwrap();
        assert_eq!(list["inputSchema"]["properties"]["tree"]["type"], "boolean");
    }

    #[test]
    fn inline_wait_descriptions_explain_ceiling_without_changing_schema_shape() {
        let defs = session_tool_definitions();
        for name in ["session_spawn", "session_dispatch"] {
            let definition = defs.iter().find(|tool| tool["name"] == name).unwrap();
            let wait = &definition["inputSchema"]["properties"]["wait_seconds"];
            assert_eq!(wait["type"], "integer");
            assert!(wait.get("minimum").is_none(), "{name}: no schema minimum");
            assert!(wait.get("maximum").is_none(), "{name}: no schema maximum");
            let description = wait["description"].as_str().unwrap();
            for expected in [
                "0–600",
                "240s",
                "health probes/short tasks",
                "long/repo tasks",
                "never cancels",
            ] {
                assert!(
                    description.contains(expected),
                    "{name}: wait description must mention `{expected}`"
                );
            }
        }
    }

    /// MCP-DX-1 — external-agent feedback: callers searching for "grok" (or
    /// any vendor keyword) must hit the spawn tool without reading a 500-char
    /// paragraph. The vendor names live in the FIRST sentence.
    ///
    /// MCP-DX-2 hardening: vendor keywords must be PLAIN TEXT. A host-side
    /// keyword matcher tokenizes the description — backtick-wrapped `grok`
    /// was measured to miss, so `session_spawn` was undiscoverable by the
    /// very keyword it advertises (the plain-text `status` description
    /// matched fine, which confirmed the diagnosis).
    #[test]
    fn session_spawn_description_front_loads_all_vendors() {
        let defs = session_tool_definitions();
        let spawn = defs.iter().find(|t| t["name"] == "session_spawn").unwrap();
        let description = spawn["description"].as_str().unwrap();
        let head: String = description.chars().take(140).collect();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"] {
            assert!(
                head.contains(vendor),
                "vendor `{vendor}` must appear in the first 140 chars (discoverability): {head}"
            );
            assert!(
                !description.contains(&format!("`{vendor}`")),
                "vendor keyword {vendor} must be plain text in the spawn description \
                 (backticks defeat host keyword matchers)"
            );
        }
    }

    /// MCP-BEACON-1 — the bare-name beacon is a PURE alias: listed next to
    /// `status`, same handler, byte-identical response. Its NAME is the
    /// contract — owner-pinned independently of `AgentVendor::ALL` — and it
    /// must survive the `mcp__ccteam__` client prefix under the 64-char cap.
    #[tokio::test]
    async fn status_beacon_is_a_pure_alias_with_owner_pinned_literal_name() {
        assert_eq!(STATUS_BEACON_TOOL_NAME, "grok_claude_codex_kimi");
        assert!(
            "mcp__ccteam__".len() + STATUS_BEACON_TOOL_NAME.len() <= 64,
            "beacon name must fit the 64-char tool-name cap with the client prefix"
        );

        // Listed, admin-grouped, schema-identical to status.
        let defs = tool_definitions();
        let beacon = defs
            .iter()
            .find(|t| t["name"] == STATUS_BEACON_TOOL_NAME)
            .expect("beacon listed");
        let status = defs.iter().find(|t| t["name"] == "status").unwrap();
        assert_eq!(beacon["inputSchema"], status["inputSchema"]);

        // Pure alias: tools/call returns the same body as status.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let call = |name: &str| {
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": { "name": name, "arguments": {} }
            })
        };
        let via_status = handle_request(&paths, &call("status")).await.unwrap();
        let via_beacon = handle_request(&paths, &call(STATUS_BEACON_TOOL_NAME))
            .await
            .unwrap();
        assert_eq!(via_status["result"], via_beacon["result"]);
        assert_eq!(via_beacon["result"]["isError"], false);

        let old = handle_request(
            &paths,
            &call(concat!("claude_codex_grok_kimi_", "opencode_status")),
        )
        .await
        .unwrap();
        assert_eq!(old["result"]["isError"], true);
        assert!(old["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    /// MCP-DX-1 — `status` is the discovery surface (vendor availability per
    /// host); its description and the server instructions must say so, and the
    /// instructions must list every reachable harness.
    #[test]
    fn status_description_and_instructions_advertise_the_vendor_axis() {
        assert!(CCTEAM_MCP_INSTRUCTIONS.contains("Kimi"));
        for vendor in [
            "`claude`",
            "`codex`",
            "`grok`",
            "`opencode`",
            "`kimi`",
            "`pi`",
            "`dsh`",
        ] {
            assert!(
                CCTEAM_MCP_INSTRUCTIONS.contains(vendor),
                "instructions must enumerate {vendor}"
            );
        }
        let defs = tool_definitions();
        let status = defs.iter().find(|t| t["name"] == "status").unwrap();
        let description = status["description"].as_str().unwrap();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"] {
            assert!(
                description.contains(vendor),
                "status description must enumerate `{vendor}`"
            );
        }
        assert!(description.contains("installed on your project's host"));
    }

    #[test]
    fn cto_scheduling_tools_present_in_canonical_set() {
        for needed in [
            "session_spawn",
            "session_dispatch",
            "session_collect",
            "session_list",
            "session_stop",
        ] {
            assert!(
                SESSION_TOOL_NAMES.contains(&needed),
                "the session_* scheduling tools depend on the `{needed}` tool"
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
    fn session_spawn_schema_carries_full_facet_set() {
        let spawn = session_tool_definitions()
            .into_iter()
            .find(|t| t["name"] == "session_spawn")
            .expect("session_spawn defined");
        let props = &spawn["inputSchema"]["properties"];

        // permission_mode enum unchanged.
        let pm: Vec<&str> = props["permission_mode"]["enum"]
            .as_array()
            .expect("permission_mode has an enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(pm, vec!["skip", "hitl"]);

        // Vendor enum lists every reachable harness.
        let vendors: Vec<&str> = props["vendor"]["enum"]
            .as_array()
            .expect("vendor has an enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            vendors,
            vec!["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"]
        );

        // v0.9.0 W1 (G1) — new facets are present.
        for key in ["model", "effort", "mode", "title"] {
            assert!(
                props[key].is_object(),
                "session_spawn schema must carry `{key}`"
            );
        }
        assert!(props.get("host").is_none());
        assert!(
            props.get("protocol").is_none(),
            "session_spawn schema must not carry removed `protocol`"
        );

        // role is now OPTIONAL (roleless is a first-class form) → required = [].
        let required: Vec<&str> = spawn["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            required.is_empty(),
            "role is optional; required must be empty"
        );
    }

    #[test]
    fn is_session_tool_recognizes_group_and_rejects_others() {
        assert!(is_session_tool("session_spawn"));
        assert!(is_session_tool("session_stop"));
        assert!(!is_session_tool("chat_register_bot"));
        assert!(!is_session_tool("session_bogus"));
        // Pre-rename prefixed wire names are gone — no compat alias.
        assert!(!is_session_tool("ccteam__session_spawn"));
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
        // Orchestration-first steer: the tracked path is session_*, not a
        // raw vendor-CLI shell-out.
        assert!(instructions.contains("session_spawn"));
        assert!(instructions.contains("codex exec"));
    }

    #[tokio::test]
    async fn handle_initialize_defaults_protocol_version_when_client_omits() {
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
        assert_eq!(
            resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION,
            "missing client protocolVersion must fall back to the server const"
        );
    }

    #[tokio::test]
    async fn handle_initialize_echoes_client_protocol_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let client_ver = "2025-03-26";
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": client_ver }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(
            resp["result"]["protocolVersion"], client_ver,
            "initialize must echo the client's protocolVersion when present"
        );
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
            // 2026-07-26 cull: tmux-era pane screenshot (web route stays).
            "screenshot",
            // Pre-rename prefixed wire names (client namespaces by server
            // key; the baked-in prefix rendered as mcp__ccteam__ccteam__*).
            "ccteam__status",
            "ccteam__screenshot",
            "ccteam__chat_send_file",
            "ccteam__session_spawn",
            "ccteam__session_dispatch",
            "ccteam__session_collect",
            "ccteam__session_list",
            "ccteam__session_stop",
        ] {
            assert!(!names.contains(&gone), "culled tool present: {gone}");
        }
    }

    #[test]
    fn tenant_status_base_contains_only_owned_projects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        for (slug, owner) in [
            ("alice", "user:ualice"),
            ("bob", "user:ubob"),
            ("admin", "user:web-api"),
        ] {
            let dir = paths.projects_root.join(slug);
            std::fs::create_dir_all(dir.join(".ccteam")).unwrap();
            let mut state = ccteam_core::ProjectState::initial(slug.to_string());
            state.owner = Some(owner.to_string());
            state.save(&CcteamPaths::project_state_in(&dir)).unwrap();
            ccteam_core::config::upsert_project(
                &paths.root,
                ccteam_core::ProjectEntry {
                    slug: slug.to_string(),
                    path: dir,
                    host: ccteam_core::LOCAL_HOST.to_string(),
                    remote_slug: None,
                    remote_path: None,
                    team: "dev".into(),
                    installed_at: chrono::Utc::now(),
                },
            )
            .unwrap();
        }

        let body: Value = serde_json::from_str(&tool_ls_for_user(&paths, "ualice").unwrap())
            .expect("tenant status base is JSON");
        let slugs: Vec<&str> = body["projects"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|project| project["slug"].as_str())
            .collect();
        assert_eq!(slugs, vec!["alice"]);
    }

    #[test]
    fn status_base_has_slim_exact_key_sets_for_admin_and_tenant() {
        use std::collections::BTreeSet;

        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let slug = "slim";
        let dir = paths.projects_root.join(slug);
        std::fs::create_dir_all(dir.join(".ccteam")).unwrap();
        let mut state = ccteam_core::ProjectState::initial(slug.to_string());
        state.owner = Some("user:ualice".to_string());
        state.save(&CcteamPaths::project_state_in(&dir)).unwrap();
        ccteam_core::config::upsert_project(
            &paths.root,
            ccteam_core::ProjectEntry {
                slug: slug.to_string(),
                path: dir,
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        for body in [
            tool_ls(&paths).unwrap(),
            tool_ls_for_user(&paths, "ualice").unwrap(),
        ] {
            let body: Value = serde_json::from_str(&body).unwrap();
            let top: BTreeSet<_> = body
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(top, BTreeSet::from(["daemon", "projects"]));
            let project = body["projects"].as_array().unwrap().first().unwrap();
            let project_keys: BTreeSet<_> = project
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(project_keys, BTreeSet::from(["cost_24h_usd", "slug"]));
            let daemon_keys: BTreeSet<_> = body["daemon"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(daemon_keys, BTreeSet::from(["message", "status"]));
            for dead in [
                "team",
                "current_phase",
                "phase_state",
                "cost_used_usd",
                "cost_active_usd",
                "tmux_session",
                "age_seconds",
                "orchestrator",
                "socket",
                "reason",
            ] {
                assert!(
                    !body.to_string().contains(&format!("\"{dead}\"")),
                    "{dead} reappeared: {body}"
                );
            }
        }
    }

    /// 2026-07-26 cull — `screenshot` (tmux-era pane render) is no longer an
    /// MCP tool; a call must fail as UNKNOWN, not degrade gracefully.
    #[tokio::test]
    async fn handle_tools_call_screenshot_is_unknown_after_cull() {
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
                "name": "screenshot",
                "arguments": { "slug": "no-such-slug-xyz", "lines": 5 }
            }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("unknown tool: screenshot"),
            "expected unknown-tool error, got: {text}"
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
            "params": { "name": "status", "arguments": {} }
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
            "params": { "name": "status", "arguments": {} }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(
            parsed["daemon"]["status"], "unreachable",
            "status must annotate daemon health when daemon is down"
        );
    }
}
