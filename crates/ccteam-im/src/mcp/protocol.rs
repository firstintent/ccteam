//! Transport-agnostic MCP protocol core (JSON-RPC value-in / value-out).
//!
//! Speaks `initialize` / `tools/list` / `tools/call` for the **local** tool
//! (`status`). Stateful tools (`chat_send_file`, `agent*`) are listed in
//! `tools/list` but only dispatched by [`super::dispatch::McpDispatch`]
//! (daemon socket / HTTP).
//!
//! **Menu, not manual.** Every byte here is charged to every session's first
//! turn, so a schema says WHAT a parameter is in one line and nothing else:
//! why, edges and failure semantics live in the server's error bodies (only
//! the caller who trips one pays), in `status{detail}` (only the caller who
//! asks pays), and in `docs/orchestration.md` (a human reads it once).

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ccteam_core::{check_daemon_health, collect_projects, CcteamPaths, DaemonHealth};

use super::face::{self, FaceIdentity, ToolFace};

/// The MCP protocol version this server speaks — also the answer to a client
/// that asks for something unknown.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Every protocol revision this server will answer in. `initialize` echoes a
/// client version from this set and otherwise answers [`MCP_PROTOCOL_VERSION`]
/// (the spec's own negotiation: never an error, always a version the client
/// can decide to accept or drop).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Server identity advertised in `initialize`.
const SERVER_NAME: &str = "ccteam";
/// Workspace-synced version of this crate (same workspace version as the binary).
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bare-name discovery beacon — a PURE ALIAS of `status` (same handler,
/// same brief response). Some MCP hosts strip descriptions and server
/// instructions from ambient context and surface tool NAMES only; in that
/// world nothing in `status` / `chat_send_file` / `agent*` says "grok"
/// or "codex", so "use grok to search" dies on first-turn discovery. This
/// name front-loads the owner-pinned discovery literal.
pub const STATUS_BEACON_TOOL_NAME: &str = "grok_claude_codex_kimi";

/// The session group's wire names, registration order.
pub const SESSION_TOOL_NAMES: &[&str] = &["agent", "agent_read", "agent_stop"];

/// True if `name` is one of the session tools.
pub fn is_session_tool(name: &str) -> bool {
    SESSION_TOOL_NAMES.contains(&name)
}

/// True if `name` is a tool this server registers AT ALL (any face).
///
/// A front door that refuses a call for a reason of its own — no workspace
/// named, no chat bound — must answer an unknown NAME with the standard
/// unknown-tool error instead: a 487-byte lecture about naming a project is a
/// wrong answer to `bogus`, and it taught a caller that its typo was a
/// permission problem (2026-08-31). Kept honest by
/// `known_tool_set_matches_the_definitions`.
pub fn is_known_tool(name: &str) -> bool {
    matches!(name, "status" | "chat_send_file")
        || name == STATUS_BEACON_TOOL_NAME
        || is_session_tool(name)
}

// ── instructions ────────────────────────────────────────────────────────────

/// Always: what ccteam is. One sentence, because the tool schemas already say
/// what it does.
const INSTRUCTIONS_BASE: &str =
    "ccteam bridges agent sessions and does identity, routing, delivery, cost and \
observability underneath.";

/// Only for a face that can hire. The one steer that cannot live in a schema:
/// the alternative (`codex exec`, `claude -p`) is a different tool entirely,
/// so no `agent` description can be read at the moment the model reaches for
/// it.
const INSTRUCTIONS_ORCH: &str =
    "When asked to call/use/delegate to another agent (claude/codex/grok/opencode/kimi/pi/dsh), \
use `agent` — never shell out to a vendor CLI (`codex exec`, `claude -p`): a raw run has no sid, \
no transcript, no cost, and is invisible to the team. `agent{task,vendor}` hires; \
`agent{task,sid}` follows up; `wait` returns the answer inline, otherwise a completion \
notification arrives when the task's turn ends.";

/// Only when a chat can reach this session.
const INSTRUCTIONS_CHAT: &str =
    "Inbound chat messages arrive wrapped in a `<channel …>` envelope; your reply goes back to \
that chat.";

/// Always. A bare vendor session does not open an attachment path on its own,
/// and the owner sends screenshots.
const INSTRUCTIONS_ATTACH: &str =
    "If a `<channel …>` tag or an `[attachment …]` line carries `image_path=` / `file_path=`, \
Read those files before answering — they are what the user sent.";

/// Compose `initialize.instructions` for one caller.
///
/// Cross-tool policy (which no single schema can carry) plus identity FACTS —
/// never behaviour instructions for the agent itself. That distinction is the
/// no-prompt-injection red line: ccteam may say "you are s5 in project x",
/// never "work like this".
pub fn instructions_for(face: &ToolFace) -> String {
    let mut parts: Vec<String> = vec![INSTRUCTIONS_BASE.to_string()];
    if face.orchestrates {
        parts.push(INSTRUCTIONS_ORCH.to_string());
    }
    if let Some(FaceIdentity::EnrolledUnbound { reachable }) = &face.identity {
        let reach = if reachable.is_empty() {
            "— none is registered for this credential's owner yet".to_string()
        } else {
            format!("— reachable: {}", reachable.join(", "))
        };
        parts.push(format!(
            "You are a hand-started agent with no project yet: name one on your first `agent` \
             call (`project:\"<slug>\"`) {reach}. The first project you name is your workspace \
             for the rest of this session; ccteam never guesses it from a working directory. \
             Notifications cannot be pushed to you; agent_read{{sid,wait}} awaits a turn instead."
        ));
    }
    if face.chat_capable {
        parts.push(INSTRUCTIONS_CHAT.to_string());
    }
    parts.push(INSTRUCTIONS_ATTACH.to_string());
    if let Some(FaceIdentity::Session {
        sid,
        slug,
        depth_capped,
        no_tools,
        pushable,
    }) = &face.identity
    {
        if *no_tools {
            parts.push(format!(
                "You are {sid} in project {slug} (no ccteam tools)."
            ));
        } else if *depth_capped {
            parts.push(format!(
                "You are {sid} in project {slug}. This session is at the delegation depth cap \
                 and cannot hire agents."
            ));
        } else if *pushable {
            // A session that CAN hire is told what happens after it does. The
            // fact is stated, never inferred: `notify_deliverable:false` is a
            // per-call deviation marker, and a session that had to read its
            // absence as "I am hand-started" got it wrong (2026-08-31).
            parts.push(format!(
                "You are {sid} in project {slug}. Completion notifications from your hires \
                 arrive here."
            ));
        } else {
            parts.push(format!(
                "You are {sid} in project {slug} (client-run: notifications cannot be pushed to \
                 you; agent_read{{sid,wait}} awaits a turn instead)."
            ));
        }
    }
    parts.join("\n\n")
}

// ── request dispatch ────────────────────────────────────────────────────────

/// Dispatch a single JSON-RPC message. Returns `Some(response)` for
/// requests (which carry an `id`) and `None` for notifications.
///
/// `tools/call` only handles the **local** tool (`status`). Unknown tools
/// (including stateful `chat_send_file` / `agent*` when reached without a
/// prior intercept) return `isError: true`.
pub async fn handle_request(paths: &CcteamPaths, req: &Value, face: &ToolFace) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no `id`) never get a reply.
    let is_notification = id.is_none();

    let result = match method {
        "initialize" => Ok(initialize_response(&params, face)),
        "notifications/initialized" => return None,
        "tools/list" => Ok(tools_list_response(face)),
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

/// Build the `initialize` result. A client `protocolVersion` this server
/// speaks is echoed; anything else (including an absent one) answers
/// [`MCP_PROTOCOL_VERSION`] and lets the client decide — the spec's own
/// negotiation, never an error.
pub fn initialize_response(params: &Value, face: &ToolFace) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
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
        "instructions": instructions_for(face),
    })
}

/// Build the `tools/list` result for `face`.
pub fn tools_list_response(face: &ToolFace) -> Value {
    json!({ "tools": face::tools_for(face) })
}

// ── tool surface ────────────────────────────────────────────────────────────

fn schema(props: Value, required: &[&str]) -> Value {
    let mut schema = json!({ "type": "object", "properties": props });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// Single source of truth for the MCP tool surface: `status` (1) + its
/// bare-name beacon alias (1) + `chat_send_file` (1) + session (3) = **6**.
///
/// Registration order is the listing order, and the two discovery tools come
/// first on purpose: a host that truncates a tool list truncates the tail.
pub fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = vec![
        json!({
            "name": "status",
            "description": "Which agents this project's host can hire and what the team spent today. Brief by default; `detail` adds model ids + effort ladders (models), install/auth/budget per vendor (vendors), your routing notes (routing), or everything (full).",
            "inputSchema": schema(json!({
                "detail": { "type": "string", "enum": ["brief", "models", "vendors", "routing", "full"], "description": "Default brief." }
            }), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": STATUS_BEACON_TOOL_NAME,
            "description": "Alias of `status` (brief): which agents — grok, claude, codex, kimi, opencode, pi, dsh — this machine can hire.",
            "inputSchema": schema(json!({}), &[]),
            "annotations": { "readOnlyHint": true },
        }),
    ];
    tools.extend(chat_tool_definitions());
    tools.extend(session_tool_definitions());
    tools
}

/// Tool definitions for the chat group (`send_file` only).
pub fn chat_tool_definitions() -> Vec<Value> {
    vec![json!({
        "name": "chat_send_file",
        "description": "Send a local file (image or document) to your own bound chat — a chat user cannot open a path.",
        "inputSchema": schema(json!({
            "path": { "type": "string", "description": "Absolute path on the daemon's filesystem." },
            "caption": { "type": "string", "description": "Optional caption." },
            "kind": { "type": "string", "enum": ["photo", "document"], "description": "Default: images → photo, else document." }
        }), &["path"]),
        "annotations": { "destructiveHint": false },
    })]
}

/// Tool definitions for the session group (`agent` / `agent_read` /
/// `agent_stop`).
///
/// `agent` is one tool because "hire somebody" and "give the one I have
/// another task" are the same act with one parameter of difference (`sid`);
/// two tools meant writing five shared parameters twice, which is an
/// implementation detail leaking into the caller's context.
pub fn session_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "agent",
            "description": "Hire an agent (claude, codex, grok, opencode, kimi, pi, dsh) or task one you already have. No `sid` → spawn a new session and dispatch `task` to it; with `sid` → follow up on that session. `wait` returns the answer inline; 0 (default) is async: one completion notification when the task's turn ends, or `agent_read{sid,wait}` when the reply says notify_deliverable:false. Tell children to answer tersely, never to dump code or diffs.",
            "inputSchema": schema(json!({
                "task": { "type": "string", "description": "Task text, forwarded verbatim as a user turn." },
                "sid": { "type": "string", "description": "Existing session to task; omit to hire a new one." },
                "vendor": {
                    "type": "string",
                    "enum": ["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"],
                    "description": "Harness for a new session (default claude)."
                },
                "wait": { "type": "integer", "description": "Seconds to block inline, 0-240 (default 0 = async)." },
                "model": { "type": "string", "description": "Model id, passed to the vendor verbatim." },
                "effort": { "type": "string", "description": "Reasoning-effort token, passed verbatim to the vendor." },
                "role": { "type": "string", "description": "Work-role `.claude/agents/<role>.md`; omit for roleless." },
                "project": { "type": "string", "description": "Workspace slug. Required on an enrolled client's first call." },
                "title": { "type": "string", "description": "Ledger label (<=80 chars); never sent to the agent." },
                "notify": {
                    "type": "string",
                    "enum": ["final", "brief", "off"],
                    "description": "Turn-end wake: final (2000-char excerpt, default), brief (500), off."
                },
                "tools": {
                    "type": "string",
                    "enum": ["full", "read", "none"],
                    "description": "New session's ccteam tool face (default full)."
                },
                "mode": { "type": "string", "description": "Vendor session mode. DSH only: standard|ptc|minimal|creator." },
                "permission_mode": {
                    "type": "string",
                    "enum": ["skip", "hitl"],
                    "description": "hitl asks your chat to approve tool calls (default skip)."
                },
                "idempotency_key": { "type": "string", "description": "Retry key: a retry replays the original call (~1h)." },
                "parent_sid": { "type": "string", "description": "Your own sid when ccteam does not manage you." }
            }), &["task"]),
            "annotations": { "destructiveHint": false },
        }),
        json!({
            "name": "agent_read",
            "description": "Read the team. No `sid` → roster of sessions you can reach, most recently active first; reuse a `released` row via `agent{sid}` instead of hiring a twin. With `sid` → that session's transcript, newest first unless `since` pages forward; empty means no answer yet.",
            "inputSchema": schema(json!({
                "sid": { "type": "string", "description": "Read this session's transcript instead of the roster." },
                "n": { "type": "integer", "description": "Max rows/turns (default 10, max 500)." },
                "tail": { "type": "boolean", "description": "With `sid`: newest first (default true unless `since`)." },
                "since": { "type": "string", "description": "With `sid`: only turns after this turn_id cursor." },
                "max_chars": { "type": "integer", "description": "With `sid`: char budget across returned turns (default 4000)." },
                "wait": { "type": "integer", "description": "With `sid`: seconds to wait for an in-flight turn to end, 0-240 (default 0)." },
                "project": { "type": "string", "description": "Roster filter: this project slug only." },
                "activity": {
                    "type": "string",
                    "enum": ["working", "idle", "stale", "stuck", "all"],
                    "description": "Roster filter (default all)."
                },
                "tree": { "type": "boolean", "description": "Roster: add delegation topology over the returned rows." }
            }), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "agent_stop",
            "description": "Stop a session you delegated. Explicit command, never a proactive kill; `agent_read{sid}` still reads its transcript.",
            "inputSchema": schema(json!({
                "sid": { "type": "string", "description": "Session to stop." }
            }), &["sid"]),
            "annotations": { "destructiveHint": true },
        }),
    ]
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

/// Daemonless `status` body: the projects this reader can see + daemon health.
/// The daemon-aware path in [`super::dispatch`] builds the real tiered body;
/// this is what a reader with no gateway can honestly answer.
pub(crate) fn tool_ls(paths: &CcteamPaths) -> Result<String> {
    let body = json!({
        "projects": status_project_rows(paths, |_| true),
        "daemon": daemon_health_json(&check_daemon_health(paths)),
    });
    Ok(serde_json::to_string(&body)?)
}

/// `[{slug, cost_24h_usd}]` for every project `visible` accepts.
pub(crate) fn status_project_rows(
    paths: &CcteamPaths,
    mut visible: impl FnMut(&ccteam_core::ProjectState) -> bool,
) -> Vec<Value> {
    let Ok(projects) = collect_projects(paths) else {
        return Vec::new();
    };
    let projection = crate::progress_projection::ProgressProjection::new(paths.clone());
    projects
        .iter()
        .filter(|project| visible(&project.state))
        .map(|p| {
            let cost = projection.project_snapshot(&p.state.slug).cost;
            json!({
                "slug": p.state.slug,
                "cost_24h_usd": cost.cost_24h_usd,
            })
        })
        .collect()
}

pub(crate) fn daemon_health_json(health: &DaemonHealth) -> Value {
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
    use crate::mcp::face::FaceIdentity;

    /// Exact set of MCP tool names (6 tools after the 2026-08-31 merge:
    /// spawn+dispatch → `agent`, list+collect → `agent_read`).
    const EXPECTED_TOOL_NAMES: &[&str] = &[
        "agent",
        "agent_read",
        "agent_stop",
        "chat_send_file",
        "grok_claude_codex_kimi",
        "status",
    ];

    fn paths(tmp: &tempfile::TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    fn compact_len(value: &Value) -> usize {
        serde_json::to_string(value).unwrap().len()
    }

    // ── the byte gates (the point of the whole surface) ────────────────────

    /// G1 — the ambient tax an ORCHESTRATOR pays on its first turn. Every
    /// byte here is charged to every session before it has done anything, so
    /// the budget is a hard gate, not a guideline.
    #[test]
    fn full_face_tools_list_fits_byte_budget() {
        let body = tools_list_response(&ToolFace::full());
        let bytes = compact_len(&body);
        assert!(
            bytes <= 5000,
            "full tools/list is {bytes} B; budget is 5000 B"
        );
    }

    /// The static instruction paragraphs (no identity, no reachable list) are
    /// what every caller pays. Dynamic lines are per-caller and tiny.
    #[test]
    fn instructions_static_budget() {
        let text = instructions_for(&ToolFace::full());
        assert!(
            text.len() <= 1100,
            "static instructions are {} B; budget is 1100 B: {text}",
            text.len()
        );
        assert!(text.contains(INSTRUCTIONS_BASE));
        assert!(text.contains("never shell out to a vendor CLI"));
        assert!(text.contains("<channel"));
        assert!(text.contains("image_path="));
    }

    /// A leaf worker — read-only face, no chat — is the most common session
    /// in a team, and its whole ambient bill is capped. The budget went
    /// 2000 → 2200 B when `status` joined the read face: a leaf that can see
    /// which agents exist and what the project has spent is worth ~470 B of a
    /// bill that is still a fifth of an orchestrator's.
    #[test]
    fn leaf_ambient_cost_stays_inside_its_budget() {
        let face = ToolFace {
            tools: vec!["status", "agent_read"],
            orchestrates: false,
            chat_capable: false,
            identity: Some(FaceIdentity::Session {
                sid: "s1394".into(),
                slug: "ccteam-src".into(),
                depth_capped: true,
                no_tools: false,
                pushable: true,
            }),
        };
        let ambient = compact_len(&tools_list_response(&face)) + instructions_for(&face).len();
        assert!(ambient <= 2200, "leaf ambient cost is {ambient} B");
    }

    #[test]
    fn no_tools_face_lists_nothing_and_says_so() {
        let face = ToolFace {
            tools: vec![],
            orchestrates: false,
            chat_capable: false,
            identity: Some(FaceIdentity::Session {
                sid: "s1394".into(),
                slug: "ccteam-src".into(),
                depth_capped: false,
                no_tools: true,
                pushable: true,
            }),
        };
        assert_eq!(tools_list_response(&face), json!({ "tools": [] }));
        let text = instructions_for(&face);
        assert_eq!(
            text,
            format!(
                "{INSTRUCTIONS_BASE}\n\n{INSTRUCTIONS_ATTACH}\n\nYou are s1394 in project ccteam-src (no ccteam tools)."
            )
        );
    }

    #[test]
    fn depth_capped_identity_states_the_cap_without_the_orchestration_policy() {
        let face = ToolFace {
            tools: vec!["agent_read"],
            orchestrates: false,
            chat_capable: false,
            identity: Some(FaceIdentity::Session {
                sid: "s7".into(),
                slug: "alpha".into(),
                depth_capped: true,
                no_tools: false,
                pushable: true,
            }),
        };
        let text = instructions_for(&face);
        assert!(text.contains("You are s7 in project alpha."));
        assert!(text.contains("at the delegation depth cap and cannot hire agents"));
        assert!(!text.contains("never shell out to a vendor CLI"));
    }

    /// D2 — a session that can hire is TOLD what happens after it does, in a
    /// plain sentence. The three states are verbatim contract: a managed
    /// session, a client-run one, and (below) an unbound enrolled binding.
    #[test]
    fn a_hiring_session_is_told_where_its_notifications_land() {
        let face = |pushable: bool| ToolFace {
            tools: vec!["status", "agent", "agent_read"],
            orchestrates: true,
            chat_capable: false,
            identity: Some(FaceIdentity::Session {
                sid: if pushable {
                    "s42".into()
                } else {
                    "s900".into()
                },
                slug: "cct".into(),
                depth_capped: false,
                no_tools: false,
                pushable,
            }),
        };
        let managed = instructions_for(&face(true));
        assert!(
            managed.ends_with(
                "You are s42 in project cct. Completion notifications from your hires arrive here."
            ),
            "{managed}"
        );
        let client_run = instructions_for(&face(false));
        assert!(
            client_run.ends_with(
                "You are s900 in project cct (client-run: notifications cannot be pushed to you; \
                 agent_read{sid,wait} awaits a turn instead)."
            ),
            "{client_run}"
        );
        // A face that cannot hire says neither: delivery is meaningless to it.
        for (depth_capped, no_tools) in [(true, false), (false, true)] {
            let text = instructions_for(&ToolFace {
                tools: vec![],
                orchestrates: false,
                chat_capable: false,
                identity: Some(FaceIdentity::Session {
                    sid: "s7".into(),
                    slug: "cct".into(),
                    depth_capped,
                    no_tools,
                    pushable: true,
                }),
            });
            assert!(!text.contains("notifications"), "{text}");
            assert!(!text.contains("Completion notifications"), "{text}");
        }
    }

    /// [`is_known_tool`] is what a front door checks before lecturing a caller
    /// about anything, so it must never drift from the registered set.
    #[test]
    fn known_tool_set_matches_the_definitions() {
        for tool in tool_definitions() {
            let name = tool["name"].as_str().unwrap();
            assert!(is_known_tool(name), "{name} is registered but unknown");
        }
        for stranger in [
            "bogus",
            "session_spawn",
            "agent_bogus",
            "",
            "ccteam__status",
        ] {
            assert!(!is_known_tool(stranger), "{stranger} must not be known");
        }
    }

    #[test]
    fn enrolled_unbound_instructions_list_reachable_projects() {
        let face = ToolFace {
            tools: vec!["agent"],
            orchestrates: true,
            chat_capable: false,
            identity: Some(FaceIdentity::EnrolledUnbound {
                reachable: vec!["alpha".into(), "beta".into()],
            }),
        };
        let text = instructions_for(&face);
        assert!(text.contains("reachable: alpha, beta"));
        assert!(text.contains("never guesses it from a working directory"));
        assert!(
            text.contains(
                "Notifications cannot be pushed to you; agent_read{sid,wait} awaits a turn instead."
            ),
            "{text}"
        );
        assert!(!text.contains("<channel …> envelope; your reply"));

        let empty = ToolFace {
            identity: Some(FaceIdentity::EnrolledUnbound { reachable: vec![] }),
            ..face
        };
        assert!(
            instructions_for(&empty).contains("none is registered for this credential's owner yet")
        );
    }

    // ── surface shape ──────────────────────────────────────────────────────

    #[test]
    fn tool_definitions_count_matches_spec() {
        assert_eq!(tool_definitions().len(), 6);
        assert_eq!(tool_definitions().len(), EXPECTED_TOOL_NAMES.len());
    }

    /// Listing ORDER is part of the contract: a host that truncates a tool
    /// list truncates the tail, so the two discovery tools come first. (JSON
    /// arrays preserve order; object keys do not — serde_json is built without
    /// `preserve_order`, so `properties` serializes alphabetically.)
    #[test]
    fn tool_definitions_registration_order_is_stable() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec![
                "status",
                STATUS_BEACON_TOOL_NAME,
                "chat_send_file",
                "agent",
                "agent_read",
                "agent_stop",
            ]
        );
    }

    /// Enum parameters declare `"type":"string"` + `"enum"` — never a union
    /// type. `notify` still PARSES the boolean form at runtime, but the schema
    /// advertises the strings only (smaller, and one shape for every client).
    #[test]
    fn enum_parameters_declare_a_string_type() {
        for tool in tool_definitions() {
            let props = tool["inputSchema"]["properties"].as_object().unwrap();
            for (param, spec) in props {
                if spec.get("enum").is_none() {
                    continue;
                }
                assert_eq!(
                    spec["type"], "string",
                    "{}.{param} must declare a plain string type",
                    tool["name"]
                );
            }
        }
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
        assert_eq!(names.len(), 6, "tool names must be unique");
        for tool in &tools {
            // Wire names are BARE: the MCP client namespaces by server key
            // (`mcp__ccteam__agent`), so a baked-in `ccteam__` prefix would
            // render as `mcp__ccteam__ccteam__agent`.
            assert!(
                !tool["name"].as_str().unwrap().starts_with("ccteam__"),
                "wire tool name must not embed the server prefix: {}",
                tool["name"]
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    /// Annotations let a host reason about a tool before calling it (and are
    /// only honest once version negotiation is right — 2025-03-26+).
    #[test]
    fn every_tool_declares_its_annotation() {
        let expect = [
            ("status", "readOnlyHint", true),
            (STATUS_BEACON_TOOL_NAME, "readOnlyHint", true),
            ("chat_send_file", "destructiveHint", false),
            ("agent", "destructiveHint", false),
            ("agent_read", "readOnlyHint", true),
            ("agent_stop", "destructiveHint", true),
        ];
        let tools = tool_definitions();
        for (name, key, value) in expect {
            let tool = tools.iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["annotations"][key], json!(value), "{name}.{key}");
        }
    }

    /// Every parameter description is ONE sentence ending in a period: the
    /// schema is a menu, and prose is what made the old surface a manual.
    #[test]
    fn parameter_descriptions_are_one_line_and_terminated() {
        for tool in tool_definitions() {
            let props = tool["inputSchema"]["properties"].as_object().unwrap();
            for (param, spec) in props {
                let description = spec["description"].as_str().unwrap_or_default();
                assert!(
                    !description.is_empty(),
                    "{}.{param} needs a description",
                    tool["name"]
                );
                assert!(
                    !description.contains('\n'),
                    "{}.{param} must be one line",
                    tool["name"]
                );
                assert!(
                    description.ends_with('.'),
                    "{}.{param} must end with a period: {description}",
                    tool["name"]
                );
            }
        }
    }

    #[test]
    fn one_chat_tool_registered_send_file() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "chat_send_file");
    }

    #[test]
    fn three_session_tools_registered_with_correct_names() {
        let tools = session_tool_definitions();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for needed in SESSION_TOOL_NAMES {
            assert!(names.contains(needed), "missing {needed}");
        }
    }

    /// [`SESSION_TOOL_NAMES`] is what [`is_session_tool`] answers from and
    /// what the face builds on, so it must never drift from the definitions.
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
    fn all_session_tools_carry_the_agent_prefix() {
        for t in session_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n == "agent" || n.starts_with("agent_"),
                "session tool name must be `agent` or `agent_*`: {n}"
            );
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    /// The one call that both hires and follows up needs `task` and nothing
    /// else; a spawn-only form was deleted (nobody used it in A2A).
    #[test]
    fn agent_requires_only_task_and_carries_the_full_facet_set() {
        let agent = session_tool_definitions()
            .into_iter()
            .find(|t| t["name"] == "agent")
            .expect("agent defined");
        assert_eq!(agent["inputSchema"]["required"], json!(["task"]));
        let props = &agent["inputSchema"]["properties"];
        for key in [
            "sid",
            "vendor",
            "wait",
            "model",
            "effort",
            "role",
            "project",
            "title",
            "notify",
            "tools",
            "mode",
            "permission_mode",
            "idempotency_key",
            "parent_sid",
        ] {
            assert!(props[key].is_object(), "agent schema must carry `{key}`");
        }
        // Removed facets never come back as schema.
        assert!(props.get("host").is_none());
        assert!(props.get("protocol").is_none());
        assert!(props.get("wait_seconds").is_none());
        let vendors: Vec<&str> = props["vendor"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            vendors,
            vec!["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"]
        );
        let notify: Vec<&str> = props["notify"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(notify, vec!["final", "brief", "off"]);
        let face: Vec<&str> = props["tools"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(face, vec!["full", "read", "none"]);
    }

    #[test]
    fn agent_read_carries_both_branches_and_no_renamed_params() {
        let read = session_tool_definitions()
            .into_iter()
            .find(|t| t["name"] == "agent_read")
            .unwrap();
        let props = &read["inputSchema"]["properties"];
        for key in [
            "sid",
            "n",
            "tail",
            "since",
            "max_chars",
            "wait",
            "project",
            "activity",
            "tree",
        ] {
            assert!(props[key].is_object(), "agent_read must carry `{key}`");
        }
        assert!(props.get("limit").is_none(), "`limit` was renamed to `n`");
        assert!(read["inputSchema"].get("required").is_none());
    }

    /// MCP-DX-1/2 — a caller searching for "grok" (or any vendor keyword) must
    /// hit the hiring tool without reading a paragraph, and the keywords must
    /// be PLAIN TEXT: a host-side matcher tokenizes the description, and
    /// backtick-wrapped `grok` was measured to miss.
    #[test]
    fn agent_description_front_loads_all_vendors() {
        let defs = session_tool_definitions();
        let agent = defs.iter().find(|t| t["name"] == "agent").unwrap();
        let description = agent["description"].as_str().unwrap();
        let head: String = description.chars().take(140).collect();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"] {
            assert!(
                head.contains(vendor),
                "vendor `{vendor}` must appear in the first 140 chars (discoverability): {head}"
            );
            assert!(
                !description.contains(&format!("`{vendor}`")),
                "vendor keyword {vendor} must be plain text in the agent description \
                 (backticks defeat host keyword matchers)"
            );
        }
    }

    /// MCP-BEACON-1 — the bare-name beacon is a PURE alias: listed next to
    /// `status`, same handler, byte-identical response. Its NAME is the
    /// contract, and it must survive the `mcp__ccteam__` client prefix under
    /// the 64-char cap.
    #[tokio::test]
    async fn status_beacon_is_a_pure_alias_with_owner_pinned_literal_name() {
        assert_eq!(STATUS_BEACON_TOOL_NAME, "grok_claude_codex_kimi");
        assert!(
            "mcp__ccteam__".len() + STATUS_BEACON_TOOL_NAME.len() <= 64,
            "beacon name must fit the 64-char tool-name cap with the client prefix"
        );

        let defs = tool_definitions();
        let beacon = defs
            .iter()
            .find(|t| t["name"] == STATUS_BEACON_TOOL_NAME)
            .expect("beacon listed");
        // Vendors stay plain text here too: this tool exists FOR the hosts
        // that only see names and descriptions.
        let description = beacon["description"].as_str().unwrap();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"] {
            let head: String = description.chars().take(140).collect();
            assert!(head.contains(vendor), "beacon must name {vendor}: {head}");
        }
        assert!(beacon["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty());

        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        let call = |name: &str| {
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": { "name": name, "arguments": {} }
            })
        };
        let face = ToolFace::full();
        let via_status = handle_request(&paths, &call("status"), &face)
            .await
            .unwrap();
        let via_beacon = handle_request(&paths, &call(STATUS_BEACON_TOOL_NAME), &face)
            .await
            .unwrap();
        assert_eq!(via_status["result"], via_beacon["result"]);
        assert_eq!(via_beacon["result"]["isError"], false);
    }

    #[test]
    fn status_description_advertises_the_vendor_axis() {
        let defs = tool_definitions();
        let status = defs.iter().find(|t| t["name"] == "status").unwrap();
        let description = status["description"].as_str().unwrap();
        assert!(description.contains("hire"));
        for detail in ["models", "vendors", "routing", "full"] {
            assert!(
                description.contains(detail),
                "status description must name the `{detail}` detail"
            );
        }
    }

    #[test]
    fn is_session_tool_recognizes_group_and_rejects_others() {
        assert!(is_session_tool("agent"));
        assert!(is_session_tool("agent_read"));
        assert!(is_session_tool("agent_stop"));
        assert!(!is_session_tool("chat_send_file"));
        assert!(!is_session_tool("agent_bogus"));
        // Pre-rename wire names are gone — no compat alias.
        assert!(!is_session_tool("session_spawn"));
        assert!(!is_session_tool("session_list"));
    }

    // ── protocol correctness ───────────────────────────────────────────────

    #[test]
    fn json_rpc_error_includes_id_and_envelope() {
        let e = json_rpc_error(Some(json!(7)), -32601, "method not found: foo");
        assert_eq!(e["jsonrpc"], "2.0");
        assert_eq!(e["id"], 7);
        assert_eq!(e["error"]["code"], -32601);
        assert!(e["error"]["message"].as_str().unwrap().contains("foo"));
    }

    #[tokio::test]
    async fn initialize_negotiates_the_protocol_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        let face = ToolFace::full();
        let ask = |version: Option<&str>| {
            let params = match version {
                Some(v) => json!({ "protocolVersion": v }),
                None => json!({}),
            };
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params })
        };
        for known in SUPPORTED_PROTOCOL_VERSIONS {
            let resp = handle_request(&paths, &ask(Some(known)), &face)
                .await
                .unwrap();
            assert_eq!(
                resp["result"]["protocolVersion"], *known,
                "a supported client version is echoed"
            );
        }
        for unknown in [Some("9999-99-99"), Some("2020-01-01"), None] {
            let resp = handle_request(&paths, &ask(unknown), &face).await.unwrap();
            assert_eq!(
                resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION,
                "an unknown/absent version answers this server's own"
            );
        }
    }

    #[tokio::test]
    async fn handle_initialize_returns_tools_capability_and_instructions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = handle_request(&paths, &req, &ToolFace::full())
            .await
            .unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
        let instructions = resp["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("image_path"));
        assert!(instructions.contains("file_path"));
        assert!(instructions.contains("Read"));
        assert!(instructions.contains("<channel"));
        assert!(instructions.contains("`agent`"));
        assert!(instructions.contains("codex exec"));
    }

    #[tokio::test]
    async fn handle_tools_list_returns_the_face() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
        let resp = handle_request(&paths, &req, &ToolFace::full())
            .await
            .unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        let mut expected = EXPECTED_TOOL_NAMES.to_vec();
        expected.sort();
        assert_eq!(names, expected);
        for gone in [
            "session_spawn",
            "session_dispatch",
            "session_collect",
            "session_list",
            "session_stop",
            "screenshot",
            "ccteam__status",
            "ccteam__session_spawn",
        ] {
            assert!(!names.contains(&gone), "culled tool present: {gone}");
        }
    }

    #[tokio::test]
    async fn handle_notifications_initialized_returns_no_response() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        assert!(handle_request(&paths, &req, &ToolFace::full())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn handle_tools_call_unknown_tool_returns_iserror_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        for gone in ["screenshot", "session_spawn", "ccteam__no_such_tool"] {
            let req = json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": gone, "arguments": {} }
            });
            let resp = handle_request(&paths, &req, &ToolFace::full())
                .await
                .unwrap();
            assert_eq!(resp["result"]["isError"], true, "{gone}");
            assert!(resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool"));
        }
    }

    // ── daemonless status body ─────────────────────────────────────────────

    fn seed_project(paths: &CcteamPaths, slug: &str, owner: &str) {
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

    #[test]
    fn tenant_status_base_contains_only_owned_projects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        for (slug, owner) in [
            ("alice", "user:ualice"),
            ("bob", "user:ubob"),
            ("admin", "user:web-api"),
        ] {
            seed_project(&paths, slug, owner);
        }
        let rows = status_project_rows(&paths, |state| {
            ccteam_core::identity::can_see_owner("ualice", false, state.owner.as_deref())
        });
        let slugs: Vec<&str> = rows
            .iter()
            .filter_map(|project| project["slug"].as_str())
            .collect();
        assert_eq!(slugs, vec!["alice"]);
    }

    #[test]
    fn status_base_is_compact_with_slim_exact_key_sets() {
        use std::collections::BTreeSet;

        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        seed_project(&paths, "slim", "user:ualice");

        let raw = tool_ls(&paths).unwrap();
        // Compact: no pretty-printer indentation anywhere.
        assert!(!raw.contains("\n  "), "status base must be compact: {raw}");
        let body: Value = serde_json::from_str(&raw).unwrap();
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
    }

    #[tokio::test]
    async fn ls_succeeds_without_daemon_and_annotates_health() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = paths(&tmp);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 72,
            "method": "tools/call",
            "params": { "name": "status", "arguments": {} }
        });
        let resp = handle_request(&paths, &req, &ToolFace::full())
            .await
            .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
        assert_eq!(
            parsed["daemon"]["status"], "unreachable",
            "status must annotate daemon health when daemon is down"
        );
    }
}
