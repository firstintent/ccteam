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
(Claude Code / Codex / Grok / OpenCode / Kimi, on this machine or a registered satellite host) and ccteam does the identity, \
routing, delivery, guardrails, cost ledger, and team observability underneath.\n\n\
ORCHESTRATION (important): when the user asks you to call / use / delegate to another agent (e.g. \"call codex\", \
\"use grok to search\", \"spawn a reviewer\"), use the `session_*` tools — `session_spawn` starts a session (pick \
`vendor`: `claude` / `codex` / `grok` / `opencode` / `kimi`, optionally \
`model` / `role`, and pass the first `task` in the same call); its execution host is inherited from the project \
binding. `session_dispatch` sends follow-up tasks \
(async with a completion notification, or `wait_seconds` to block inline), `session_collect` reads its output \
(`tail:true` for the final answer), `session_list` shows the delegation tree, `session_stop` ends it. Do NOT shell \
out to vendor CLIs (`codex exec`, `claude -p`, …) for this: a raw CLI run has no session id, no transcript, no cost \
tracking, no completion notification, and is invisible to the team — it bypasses the bridge. The tools work both \
from ccteam-spawned sessions (per-session principal) and from a plain local session running inside a registered \
project (same-user admin fallback; `session_spawn` then targets the project of your working directory, or an \
explicit `project`).\n\n\
CHAT ROUTING: ccteam routes IM (Telegram / web) chats to you and back. \
An inbound chat message may arrive wrapped in a `<channel source=\"…\" chat_id=\"…\" user=\"…\" message_id=\"…\">` tag.\n\n\
ATTACHMENTS (important): if a `<channel …>` tag carries an `image_path=\"/abs/path\"` attribute, immediately `Read` that file — \
it is an image the user attached (often an error screenshot) and is essential context. If it carries a `file_path=\"/abs/path\"` \
attribute, `Read` that file too. Further attachments may appear in the body as `[attachment image_path=\"…\"]` / \
`[attachment file_path=\"…\"]` lines — `Read` each of those as well. Do this BEFORE you answer; the user expects you to have \
looked at the file they sent.";

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

/// Single source of truth for the MCP tool surface (v0.9 T1):
/// `status` (1) + `screenshot` (1) + `chat_send_file` (1) +
/// session (5) = **8 total**.
pub fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = vec![
        json!({
            "name": "status",
            "description": "Discovery + health: registered projects, daemon health, today's cost/budget, and your project's bound-host vendor panel — which of claude / codex / grok / opencode / kimi are installed (version, auth state) + advisory model catalog + routing notes. Call this first to learn what you can spawn.",
            "inputSchema": object_schema(&[]),
        }),
        json!({
            "name": "screenshot",
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
        "name": "chat_send_file",
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
            "name": "session_spawn",
            "description": "Spawn an agent session — `vendor`: `claude` (default) | `codex` | `grok` | `opencode` | `kimi` — in YOUR OWN project; always mints a NEW `s{n}` sid. `grok` = fast live web/X search; `claude`/`codex` = coding agents for repo work; `status` shows per-host availability. Pass `task` to dispatch the first task in the same call — identical semantics to session_dispatch (async + ONE completion notification when the child's turn completes and it goes idle; see `wait_seconds`/`notify`); the response then adds `turn_id` + `status`, plus `result_text`/`elapsed_seconds`/ledger `cost_usd`/`tokens_total` when waited to completion. Instruct children to answer tersely with a structured summary and no code or diff dumps, because answers beyond the return cap are truncated. Auth: your per-session `(sid, secret)` principal — you can only spawn into your own project; the execution host follows the project binding. Returns `{sid, vendor_session_id (vendor-native resume key, may be empty), host, ...}`. Read output later with session_collect{sid, tail:true}.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "Optional work-role (must exist as `.claude/agents/<role>.md`). Omit or pass \"\" for a roleless session (bare vendor reads the project CLAUDE.md/AGENTS.md)." },
                    "vendor": {
                        "type": "string",
                        "enum": ["claude", "codex", "grok", "opencode", "kimi"],
                        "description": "Harness vendor (lowercase). Default `claude`."
                    },
                    "model": { "type": "string", "description": "Optional explicit model id; overrides the role's `model:` frontmatter. Omitted/empty → vendor default." },
                    "effort": { "type": "string", "description": "Optional reasoning-effort token (vendor-specific value set). Ignored for grok (undocumented value set)." },
                    "protocol": {
                        "type": "string",
                        "enum": ["stream-json", "acp"],
                        "description": "Session channel. `stream-json` (default) for claude/codex; `acp` for grok/opencode/kimi (forced). `terminal` is not available to agents."
                    },
                    "project": { "type": "string", "description": "Target project slug — honored only for admin / local main-session callers (a session-principal caller always spawns into its OWN project). Local callers default to the project resolved from the working directory." },
                    "permission_mode": {
                        "type": "string",
                        "enum": ["skip", "hitl"],
                        "description": "Permission posture (default `skip`). `hitl` (human-in-the-loop) makes a non-allowlist tool call pop an approve/deny prompt to the bound IM chat; allowlist/auto-allowed tools never prompt."
                    },
                    "title": { "type": "string", "description": "Optional short label (≤80 chars) for the ledger / team visualization only — NEVER sent to the agent or concatenated into any prompt." },
                    "task": { "type": "string", "description": "Optional FIRST task — dispatched to the fresh child in the same call, exactly like session_dispatch{sid, task} (verbatim user turn, no injection). Omit to spawn only." },
                    "wait_seconds": { "type": "integer", "description": "With `task`: request 0–600 seconds (default 0 = async); effective inline wait is capped at 240s. Use inline wait for health probes/short tasks; keep long/repo tasks async with completion notification. Pending/timeout never cancels the child." },
                    "notify": { "type": ["string", "boolean"], "description": "With `task`: `final` (default) wakes you ONCE, when the child's vendor turn completes and it goes idle (interim narration stays in the ledger); `all` wakes you on every assistant message (debug firehose); `off` = ledger-only (poll session_collect yourself). Booleans still parse: true→final, false→off." },
                    "idempotency_key": { "type": "string", "description": "Optional client key. A retry with the same key (per-project, within ~1h) replays the ORIGINAL spawn (same sid + same dispatch outcome, zero side effects) instead of creating a second session — safe against MCP-client timeouts. In-memory only: a daemon restart forgets keys." }
                },
                "required": [],
            }),
        }),
        json!({
            "name": "session_dispatch",
            "description": "Dispatch a task to a session by `sid` (from session_spawn / session_list); the target must run in YOUR OWN project. `task` is forwarded VERBATIM as a user turn (NO system-prompt injection). Async by default: ONE completion notification when the child's vendor turn completes and it goes idle — mid-turn narration never notifies; the notification says the child is idle, so you know when to dispatch the next step (`notify` changes this, `wait_seconds` blocks inline). Completion returns `{status:\"completed\", result_text, elapsed_seconds, cost_usd?, tokens_total?}` (child's session ledger); timeout returns `{status:\"pending\"}` and never cancels the child. Instruct children to answer tersely with a structured summary and no code or diff dumps, because answers beyond the return cap are truncated. Dispatch to yourself or an ancestor is rejected (cycle). Explicit dispatch, never a proactive kill.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "sid": { "type": "string", "description": "Gateway session id (`s{n}`) from session_spawn / session_list." },
                    "task": { "type": "string", "description": "Task / instruction text, forwarded verbatim as a user turn." },
                    "wait_seconds": { "type": "integer", "description": "Request 0–600 seconds (default 0 = async); effective inline wait is capped at 240s. Use inline wait for health probes/short tasks; keep long/repo tasks async with completion notification. Pending/timeout never cancels the child." },
                    "notify": { "type": ["string", "boolean"], "description": "`final` (default) wakes you ONCE, when the child's vendor turn completes and it goes idle — the notification explicitly says the child is idle and waiting, so you always know when to dispatch the next step. `all` wakes you on every assistant message (debug firehose); `off` = ledger-only (poll session_collect yourself). Booleans still parse: true→final, false→off." },
                    "title": { "type": "string", "description": "Optional short label (≤80 chars) for the notification / ledger only — NEVER concatenated into the task or any prompt." },
                    "idempotency_key": { "type": "string", "description": "Optional client key. A retry with the same key (per-target-child, within ~1h) replays the ORIGINAL dispatch (same turn) instead of double-dispatching. In-memory only: a daemon restart forgets keys." }
                },
                "required": ["sid", "task"],
            }),
        }),
        json!({
            "name": "session_collect",
            "description": "Collect (poll) a session's transcript by `sid`. Authenticated by your `(sid, secret)` principal; the target `sid` must run in YOUR OWN project (cross-project collect is rejected). Tails `<project>/.ccteam/chat/<sid>/turns.jsonl` (the ccteam-owned mirror, keyed by sid so parallel sessions never bleed) and returns assistant-side turns plus the child's `vendor_session_id` (native resume key), `activity` (`working` = mid-turn / `idle` = turn done / `stale` / `stuck` — poll on `working`, read on `idle`), and the accrued ledger (`cost_usd` when priced, `tokens_total` always when the vendor reports usage). Pass `since` (a turn_id you already saw) to return only turns AFTER it — the polling cursor for incremental results. Default paging is OLDEST-first (page forward with `cursor`); pass `tail:true` for the NEWEST `n` turns instead (the \"just give me the final answer\" shape). Returns an empty `turns` array when the target hasn't answered yet.",
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
            "description": "List the gateway's live sessions (the same `s{n}` namespace session_spawn allocates), most recently active first, capped at `limit` (default 30; `truncated`/`total` say when the cap bit). Authenticated by your `(sid, secret)` principal. Each row carries `sid`, `project`, `vendor`, `activity` (`working` = mid-turn / `idle` / `stale` / `stuck` — the honest busy signal), `last_active`, plus — when set — `role`, `current`, `waiting_approval` (hitl blocked on a human), the delegation `parent_sid`/`delegation_depth`, non-local `host`, `cost_usd`, `tokens_total` (raw token ledger, present even for vendors with no USD price table), and `title` (null/empty fields are omitted). The response also includes a `tree` field (roots → children by `parent_sid`, over the filtered set) so you can see the delegation topology. Filter with `project` / `activity` to keep the listing small. Use this to find a `sid` to dispatch to or collect from.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Only list sessions of this project slug." },
                    "activity": { "type": "string", "enum": ["working", "idle", "stale", "stuck", "all"], "description": "Only list sessions with this activity state (default `all`)." },
                    "limit": { "type": "integer", "description": "Max rows returned, most recently active first (default 30, clamped to 1–500)." }
                },
                "required": [],
            }),
        }),
        json!({
            "name": "session_stop",
            "description": "Stop a session by `sid` (deregister + close it). Authenticated by your `(sid, secret)` principal; the target `sid` must run in YOUR OWN project (cross-project stop is rejected). This is an EXPLICIT command, NOT a proactive kill — it never file-purges the transcript, so a later session_collect of an already-recorded `turns.jsonl` still works until cleanup. An unknown sid is an error.",
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
        "status" => Ok(text_content(tool_ls(paths)?)),
        "screenshot" => Ok(text_content(tool_screenshot(paths, &args)?)),
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

fn text_content(body: String) -> Vec<Value> {
    vec![json!({ "type": "text", "text": body })]
}

/// Base `status` JSON body (projects + orchestrator + daemon health). The
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
    // V0.4.0 F60: active_count was derived from `phase_state == InFlight`;
    // with the phase state machine deleted F66 will recompute this from
    // `state.sessions` (live agent count).
    let active_count = 0usize;
    let arr: Vec<Value> = projects
        .iter()
        .filter(|project| visible(&project.state))
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
        "chat_send_file",
        "screenshot",
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
    fn screenshot_tool_definition_present_with_optional_lines() {
        let tools = tool_definitions();
        let s = tools
            .iter()
            .find(|t| t["name"] == "screenshot")
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
            assert!(description.contains("answer tersely with a structured summary"));
            assert!(description.contains("no code or diff dumps"));
        }
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
    /// paragraph. The five vendor names live in the FIRST sentence.
    #[test]
    fn session_spawn_description_front_loads_all_vendors() {
        let defs = session_tool_definitions();
        let spawn = defs.iter().find(|t| t["name"] == "session_spawn").unwrap();
        let head: String = spawn["description"]
            .as_str()
            .unwrap()
            .chars()
            .take(140)
            .collect();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi"] {
            assert!(
                head.contains(vendor),
                "vendor `{vendor}` must appear in the first 140 chars (discoverability): {head}"
            );
        }
    }

    /// MCP-DX-1 — `status` is the discovery surface (vendor availability per
    /// host); its description and the server instructions must say so, and the
    /// instructions must list ALL five harnesses (Kimi was missing).
    #[test]
    fn status_description_and_instructions_advertise_the_vendor_axis() {
        assert!(CCTEAM_MCP_INSTRUCTIONS.contains("Kimi"));
        for vendor in ["`claude`", "`codex`", "`grok`", "`opencode`", "`kimi`"] {
            assert!(
                CCTEAM_MCP_INSTRUCTIONS.contains(vendor),
                "instructions must enumerate {vendor}"
            );
        }
        let defs = tool_definitions();
        let status = defs.iter().find(|t| t["name"] == "status").unwrap();
        let description = status["description"].as_str().unwrap();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi"] {
            assert!(
                description.contains(vendor),
                "status description must enumerate `{vendor}`"
            );
        }
        assert!(description.contains("vendor panel"));
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

        // v0.9.0 W1 (G1) — vendor enum lists all FIVE harnesses.
        let vendors: Vec<&str> = props["vendor"]["enum"]
            .as_array()
            .expect("vendor has an enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(vendors, vec!["claude", "codex", "grok", "opencode", "kimi"]);

        // v0.9.0 W1 (G1) — new facets are present.
        for key in ["model", "effort", "protocol", "title"] {
            assert!(
                props[key].is_object(),
                "session_spawn schema must carry `{key}`"
            );
        }
        assert!(props.get("host").is_none());

        // protocol enum = stream-json | acp ONLY — terminal is NEVER exposed.
        let protos: Vec<&str> = props["protocol"]["enum"]
            .as_array()
            .expect("protocol has an enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(protos, vec!["stream-json", "acp"]);
        assert!(
            !protos.contains(&"terminal"),
            "terminal must not be exposed to agents"
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
                "name": "screenshot",
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
            parsed["orchestrator"]["daemon_health"]["status"], "unreachable",
            "status must annotate daemon health when daemon is down"
        );
    }
}
