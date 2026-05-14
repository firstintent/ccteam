//! M2.5: `ccteam mcp-serve` — stdio MCP server.
//!
//! Exposes the ccteam control surface as 9 MCP tools so the user's
//! daily-driver Claude Code session (and the meta-agent session) can
//! drive ccteam without shelling out to the CLI. Architecture and
//! red lines:
//!
//! - **Hand-rolled JSON-RPC 2.0** over stdio (line-delimited messages).
//!   Per the M2.5 brief, `rmcp` was the alternative; the in-process
//!   protocol surface is small enough that adding a crate dep buys
//!   little — `initialize` / `tools/list` / `tools/call` is the whole
//!   spec we exercise.
//! - **No LLM in-process** (Symphony anti-pattern, tech-design §3.1).
//!   The MCP server is a thin protocol adapter; every tool routes to
//!   an existing `commands.rs` function.
//! - Two consumers: the user's daily-driver claude (`~/.claude.json`
//!   `mcpServers` entry, written by `ccteam doctor --install-mcp`)
//!   and the meta-agent session (project-local `.mcp.json` —
//!   meta-agent prefers MCP, falls back to Bash).
//!
//! Wire format: each side sends one JSON object per line, terminated
//! by `\n`. Notifications (no `id`) get no reply. Errors follow the
//! JSON-RPC 2.0 error object shape (interfaces §12).

use std::io::Write;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// V0.3 M5.0 (F45 promote): write-helper logic lives in
// `ccteam_core::actions`; the wrappers below stay thin (args parse +
// JSON encode + delegate). Web-side consumers (`crates/ccteam-web`)
// pick up the same import line in M5.3 (`docs/v0-3/dev-plan.md` §5
// #4.3) — at which point `git grep -nE 'use ccteam_core::actions'`
// returns ≥ 2 hits.
use ccteam_core::actions;
use ccteam_core::{
    bootstrap_project, check_daemon_health, pick_unused_slug, render_screenshot, CcteamPaths,
    DaemonHealth, SendOptions,
};

use crate::commands::{collect_projects, collect_recent_events, run_peek, run_show, OutputFormat};
// V0.4.0 F65 — 7 new workflow tools. Schemas + handlers live in a
// dedicated module so `mcp_serve.rs` stays focused on the M2.5 protocol
// surface.
use crate::mcp_workflow_tools;

/// Stable MCP protocol version this server speaks. Newer client versions
/// downgrade gracefully because we never advertise capabilities we don't
/// implement.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server identity advertised in `initialize`.
const SERVER_NAME: &str = "ccteam";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run `ccteam mcp-serve`. Reads JSON-RPC requests one per line from
/// stdin; writes responses one per line to stdout. Returns when stdin
/// closes (clean shutdown when the parent disconnects).
pub async fn run_mcp_serve(paths: CcteamPaths) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader
        .next_line()
        .await
        .context("read stdin from MCP client")?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                let err_obj = json_rpc_error(None, -32700, &format!("parse error: {err}"));
                write_message(&mut stdout, &err_obj).await?;
                continue;
            }
        };
        let resp = handle_request(&paths, &req).await;
        if let Some(response) = resp {
            write_message(&mut stdout, &response).await?;
        }
    }
    Ok(())
}

/// Dispatch a single JSON-RPC message. Returns `Some(response)` for
/// requests (which carry an `id`) and `None` for notifications.
async fn handle_request(paths: &CcteamPaths, req: &Value) -> Option<Value> {
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
    })
}

fn tools_list_response() -> Value {
    json!({ "tools": tool_definitions() })
}

/// Single source of truth for the MCP tool surface. M2.5 shipped 9
/// tools; V0.2.2 F38 added `ccteam__screenshot` → 10; V0.4.0 F65 adds
/// 7 workflow-control tools (`spawn_agent` / `stop_agent` /
/// `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` /
/// `get_artifact_summary`) → 17 total. Schemas mirror interfaces.md
/// §12.2.
fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = vec![
        // Read-only inspection.
        json!({
            "name": "ccteam__ls",
            "description": "List every ccteam project under ~/projects/ with its current phase, state, cost, and stall level. Equivalent to `ccteam ls --format json`.",
            "inputSchema": object_schema(&[]),
        }),
        json!({
            "name": "ccteam__show",
            "description": "Return the full state.json + recent progress events + artifact list for a single project. Equivalent to `ccteam show <slug> --format json`.",
            "inputSchema": object_schema(&[("slug", "string", "Project slug, as listed by ccteam__ls.")]),
        }),
        json!({
            "name": "ccteam__peek",
            "description": "Capture the current tmux pane contents for a project session (one-shot, no attach). Useful when you want to see what claude is showing without interrupting.",
            "inputSchema": object_schema(&[("slug", "string", "Project slug.")]),
        }),
        json!({
            "name": "ccteam__progress",
            "description": "Return the last N progress.jsonl events for a project. Default N=50.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "last_n": { "type": "integer", "description": "How many events to return (default 50)." }
                },
                "required": ["slug"],
            }),
        }),
        // Lifecycle.
        json!({
            "name": "ccteam__new",
            "description": "Create a new ccteam project from a free-text request. Returns the assigned slug and project directory. Defaults team=dev (M3 will accept other teams).",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Free-text user request." },
                    "team": { "type": "string", "description": "Team name (default 'dev')." }
                },
                "required": ["prompt"],
            }),
        }),
        json!({
            "name": "ccteam__pause",
            "description": "Pause auto-dispatch for one project (sets user_pause_pending). Does not kill the tmux session.",
            "inputSchema": object_schema(&[("slug", "string", "Project slug.")]),
        }),
        json!({
            "name": "ccteam__resume",
            "description": "Resume an escalated / paused project (clears user_pause_pending and archives any escalation.md so the daemon's next tick re-dispatches the current phase).",
            "inputSchema": object_schema(&[("slug", "string", "Project slug.")]),
        }),
        // M2.5 new pair.
        json!({
            "name": "ccteam__send_to_session",
            "description": "Atomically write a markdown message into a session's .ccteam/inbox/. The orchestrator's next inotify wake delivers it via tmux send-keys. Use for free-form NL into project or meta-agent sessions.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Project slug OR meta session slug (e.g. 'meta-rob')." },
                    "content_type": { "type": "string", "description": "Content type: text|markdown (default 'text')." },
                    "body": { "type": "string", "description": "Message body (NL/markdown)." }
                },
                "required": ["session", "body"],
            }),
        }),
        json!({
            "name": "ccteam__inject_decision",
            "description": "Inject a structured ESCALATE-style decision into a project session's inbox. Used when the meta-agent has resolved a clarify/escalation and wants the project session to act on the resolution.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "escalate_kind": {
                        "type": "string",
                        "description": "One of: revert_to_phase | need_user_input | abort | insufficient_clarification | phase_done_pending. See interfaces §4.1.1."
                    },
                    "args": { "type": "object", "description": "Per-kind args. revert_to_phase needs target_phase; all kinds accept reason." }
                },
                "required": ["slug", "escalate_kind"],
            }),
        }),
        // V0.2.2 F38 — terminal screenshot. Read-only (no daemon
        // requirement); mirrors `ccteam__peek` semantics.
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
    // V0.4.0 F65 — append the 7 workflow-control tools.
    tools.extend(mcp_workflow_tools::workflow_tool_definitions());
    tools
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

/// Dispatch `tools/call` to the right tool implementation, returning
/// the MCP `content` array.
async fn call_tool(paths: &CcteamPaths, params: &Value) -> Result<Vec<Value>> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call missing `name`"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "ccteam__ls" => Ok(text_content(tool_ls(paths)?)),
        "ccteam__show" => Ok(text_content(tool_show(paths, &args)?)),
        "ccteam__peek" => Ok(text_content(tool_peek(paths, &args)?)),
        "ccteam__progress" => Ok(text_content(tool_progress(paths, &args)?)),
        "ccteam__new" => Ok(text_content(tool_new(paths, &args)?)),
        "ccteam__pause" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_pause(paths, &args)?))
        }
        "ccteam__resume" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_resume(paths, &args)?))
        }
        "ccteam__send_to_session" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_send_to_session(paths, &args)?))
        }
        "ccteam__inject_decision" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_inject_decision(paths, &args)?))
        }
        "ccteam__screenshot" => Ok(text_content(tool_screenshot(paths, &args)?)),
        // V0.4.0 F65 — route the 7 workflow tools through the
        // dedicated dispatcher. Mutating tools (spawn / stop / signal /
        // set_parallelism / trigger_gate) gate on a live daemon
        // heartbeat so a dead orchestrator surfaces immediately rather
        // than silently swallowing marker writes that no one consumes.
        // Read-only tools (observe_agents / get_artifact_summary) stay
        // daemon-independent so the meta-agent can inspect a stopped
        // project.
        other => {
            if mcp_workflow_tools::requires_daemon(other) {
                require_healthy_daemon(paths)?;
            }
            match mcp_workflow_tools::dispatch(paths, other, &args)? {
                Some(body) => Ok(text_content(body)),
                None => Err(anyhow!("unknown tool: {other}")),
            }
        }
    }
}

/// M0.23.1 + M0.23.3 fail-loud gate for action tools that need a live
/// orchestrator (state-mutating tools where a dead daemon means the
/// effect would silently never reach the project session). Pure stat
/// against the heartbeat file — no IPC.
fn require_healthy_daemon(paths: &CcteamPaths) -> Result<()> {
    let health = check_daemon_health(paths);
    if !health.is_healthy() {
        return Err(anyhow!(health.describe()));
    }
    Ok(())
}

fn text_content(body: String) -> Vec<Value> {
    vec![json!({ "type": "text", "text": body })]
}

// -------------- Tool implementations --------------

fn tool_ls(paths: &CcteamPaths) -> Result<String> {
    let projects = collect_projects(paths)?;
    // V0.4.0 F60: active_count was derived from `phase_state == InFlight`;
    // with the phase state machine deleted F66 will recompute this from
    // `state.sessions` (live agent count).
    let active_count = 0usize;
    let arr: Vec<Value> = projects
        .iter()
        .map(|p| {
            json!({
                "slug": p.state.slug,
                "team": p.state.team,
                "current_phase": p.state.current_phase,
                "phase_state": match p.state.phase_state {
                    ccteam_core::PhaseState::Idle => "idle",
                    ccteam_core::PhaseState::Done => "done",
                },
                "cost_used_usd": p.state.cost_used_usd,
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
            "max_concurrent": ccteam_core::MAX_CONCURRENT_PROJECTS,
            "daemon_health": daemon_health_json(&health),
        },
    });
    Ok(serde_json::to_string_pretty(&body)?)
}

/// Stable JSON shape for daemon health: `status` is one of
/// `healthy|no_heartbeat|stale`; `message` is the human-readable
/// describe(); `age_secs` carries heartbeat age when available.
fn daemon_health_json(health: &DaemonHealth) -> Value {
    match health {
        DaemonHealth::Healthy { age_secs } => json!({
            "status": "healthy",
            "age_secs": age_secs,
            "message": health.describe(),
        }),
        DaemonHealth::NoHeartbeat => json!({
            "status": "no_heartbeat",
            "message": health.describe(),
        }),
        DaemonHealth::Stale { age_secs } => json!({
            "status": "stale",
            "age_secs": age_secs,
            "message": health.describe(),
        }),
    }
}

fn tool_show(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    run_show(paths, &slug, OutputFormat::Json)
}

fn tool_peek(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    run_peek(paths, &slug)
}

fn tool_progress(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let last_n = args.get("last_n").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let events = collect_recent_events(paths, &slug, last_n)?;
    Ok(serde_json::to_string_pretty(&json!({ "events": events }))?)
}

fn tool_new(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let prompt = arg_string(args, "prompt")?;
    let team = args
        .get("team")
        .and_then(|v| v.as_str())
        .unwrap_or("dev")
        .to_string();
    if prompt.trim().is_empty() {
        return Err(anyhow!("prompt must be non-empty"));
    }
    let slug = pick_unused_slug(paths, &prompt, &team)?;
    let project_dir = bootstrap_project(paths, &slug, &prompt, &team)?;
    Ok(serde_json::to_string_pretty(&json!({
        "slug": slug,
        "workspace": project_dir.to_string_lossy(),
    }))?)
}

fn tool_pause(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    actions::pause(paths, &slug)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "user_pause_pending": true,
    }))?)
}

fn tool_resume(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    actions::resume(paths, &slug)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
    }))?)
}

fn tool_send_to_session(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let session = arg_string(args, "session")?;
    let body = arg_string(args, "body")?;
    let content_type = args
        .get("content_type")
        .and_then(|v| v.as_str())
        .unwrap_or("text")
        .to_string();
    // Wrapper keeps `source = "ccteam-mcp"` for backward compat with
    // retro / silence-classifier consumers that grep on `source`.
    let opts = SendOptions {
        source: "ccteam-mcp".into(),
        source_user: "mcp".into(),
        content_type,
    };
    let result = actions::send_to_session_with(paths, &session, &body, &opts)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "session": session,
        "inbox_file": result.inbox_file,
    }))?)
}

fn tool_inject_decision(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_string(args, "slug")?;
    let kind = arg_string(args, "escalate_kind")?;
    let kind_norm = kind.to_lowercase();
    let inner_args = args.get("args").cloned().unwrap_or(json!({}));
    let reason = inner_args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("(no reason provided)");
    let target_phase = inner_args.get("target_phase").and_then(|v| v.as_str());

    // Build a structured NL message that the in-session claude can
    // recognize as a meta-agent decision. The body re-uses the
    // ESCALATE grammar shape (interfaces §4.1.1) so claude can map
    // the decision back to its own routing logic.
    let body = match kind_norm.as_str() {
        "revert_to_phase" => {
            let phase = target_phase.ok_or_else(|| {
                anyhow!("revert_to_phase requires args.target_phase")
            })?;
            format!(
                "**META-AGENT DECISION**: revert to phase `{phase}`.\n\n原因:{reason}\n\n请回到 `{phase}` 阶段继续。",
            )
        }
        "need_user_input" => format!(
            "**META-AGENT DECISION**: clarification provided.\n\n{reason}\n\n请基于上述信息继续当前 phase。",
        ),
        "abort" => format!(
            "**META-AGENT DECISION**: abort this project.\n\n原因:{reason}\n\n请停止后续工作,本次结束输出 `ESCALATE: ABORT — {reason}`。",
        ),
        "insufficient_clarification" => format!(
            "**META-AGENT DECISION**: accept best-effort artifact.\n\n{reason}\n\n请基于现有产物继续到下一 phase,无需追加 CLARIFY 轮次。",
        ),
        "phase_done_pending" => format!(
            "**META-AGENT DECISION**: deferred decisions resolved.\n\n{reason}\n\n请将之前 PHASE_DONE_PENDING 的子任务标记完成,推进到下一 phase。",
        ),
        other => return Err(anyhow!(
            "unknown escalate_kind `{other}`; valid: revert_to_phase | need_user_input | abort | insufficient_clarification | phase_done_pending",
        )),
    };

    // Reuse send_to_session's inbox-write path so the orchestrator's
    // existing inotify+send-keys delivery picks it up.
    tool_send_to_session(
        paths,
        &json!({
            "session": slug,
            "content_type": "markdown",
            "body": body,
        }),
    )
}

/// V0.2.2 F38: render a PNG screenshot of the project's pane and
/// return the absolute path. `lines` defaults to 50 when omitted.
/// Returns `{ok:true, path}` on success and `{ok:false, reason}` on
/// graceful degrade (tmux missing, font failed, etc.) — never
/// `Err()` for those paths so callers can attach the reason in NL.
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

// -------------- Helpers --------------

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

async fn write_message(stdout: &mut tokio::io::Stdout, msg: &Value) -> Result<()> {
    let mut line = serde_json::to_string(msg).context("serialize MCP message")?;
    line.push('\n');
    stdout
        .write_all(line.as_bytes())
        .await
        .context("write MCP message to stdout")?;
    stdout.flush().await.context("flush MCP stdout")?;
    Ok(())
}

/// `ccteam doctor --install-mcp`: register the ccteam MCP server in
/// `~/.claude.json` so any new claude session can call ccteam tools
/// without per-project setup.
///
/// Strategy: read `~/.claude.json`, ensure `mcpServers.ccteam` points
/// at the running binary's absolute path, write back atomically. The
/// trust-marking path in `projects::pre_trust_project` follows the same
/// pattern, so the operator never sees a "Trust this folder?" prompt
/// for the ccteam MCP server itself.
pub fn install_mcp_into(claude_json: &std::path::Path, ccteam_bin: &std::path::Path) -> Result<()> {
    let bin = ccteam_bin
        .to_str()
        .ok_or_else(|| anyhow!("ccteam binary path not valid UTF-8"))?;
    let mut root = if claude_json.exists() {
        let bytes = std::fs::read(claude_json)
            .with_context(|| format!("read {}", claude_json.display()))?;
        if bytes.is_empty() {
            serde_json::Map::new()
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            }
        }
    } else {
        serde_json::Map::new()
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let map = match servers {
        Value::Object(m) => m,
        _ => {
            *servers = Value::Object(serde_json::Map::new());
            servers.as_object_mut().unwrap()
        }
    };
    map.insert(
        "ccteam".into(),
        json!({
            "command": bin,
            "args": ["mcp-serve"],
            "env": {},
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    if let Some(parent) = claude_json.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut tmp_os = claude_json.as_os_str().to_owned();
    tmp_os.push(".ccteam-mcp.tmp");
    let tmp = std::path::PathBuf::from(tmp_os);
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(body.as_bytes())?;
    }
    std::fs::rename(&tmp, claude_json)
        .with_context(|| format!("rename {} → {}", tmp.display(), claude_json.display()))?;
    Ok(())
}

/// Production path: locate `~/.claude.json` and the running binary,
/// then call `install_mcp_into`.
///
/// V0.2.1 F26: honors `CLAUDE_CONFIG_HOME` via
/// [`ccteam_core::projects::resolve_claude_json_path`] so e2e harnesses
/// get the same redirection sibling installers (`--install-skill`,
/// `--install-memory-bridge`) already honor through `user_claude_dir()`.
pub fn install_mcp() -> Result<std::path::PathBuf> {
    let claude_json = ccteam_core::projects::resolve_claude_json_path()?;
    let bin = ccteam_core::current_ccteam_bin()?;
    install_mcp_into(&claude_json, &bin)?;
    Ok(claude_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_count_matches_spec() {
        // M2.5 brief: 9 tools. V0.2.2 F38 adds `ccteam__screenshot` →
        // 10. V0.4.0 F65 adds 7 workflow tools → 17 total. Bump this
        // when a new tool lands.
        assert_eq!(tool_definitions().len(), 17);
    }

    #[test]
    fn tool_definitions_have_unique_names_and_object_schemas() {
        let tools = tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 17, "tool names must be unique");
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
        // `slug` required, `lines` optional.
        assert_eq!(req, vec!["slug"]);
        assert_eq!(s["inputSchema"]["properties"]["lines"]["type"], "integer");
    }

    #[test]
    fn install_mcp_into_writes_command_args_env_for_ccteam_server() {
        // MCP server name is `ccteam` (the binary name and the server
        // name match again post-F44).
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        let ccteam_bin = std::path::PathBuf::from("/usr/local/bin/ccteam");
        install_mcp_into(&claude_json, &ccteam_bin).unwrap();
        let body = std::fs::read_to_string(&claude_json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["mcpServers"]["ccteam"]["command"],
            "/usr/local/bin/ccteam"
        );
        assert_eq!(v["mcpServers"]["ccteam"]["args"][0], "mcp-serve");
    }

    #[test]
    fn install_mcp_into_preserves_other_top_level_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            r#"{"userID": "rob", "mcpServers": {"playwright": {"command": "npx"}}}"#,
        )
        .unwrap();
        install_mcp_into(&claude_json, &std::path::PathBuf::from("/x/ccteam")).unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["userID"], "rob");
        assert_eq!(v["mcpServers"]["playwright"]["command"], "npx");
        assert_eq!(v["mcpServers"]["ccteam"]["command"], "/x/ccteam");
    }

    // V0.3 M5.0: `next_inbox_seq` body lives in
    // `ccteam_core::actions::next_inbox_seq`; coverage moved with it
    // (see `crates/ccteam-core/src/actions.rs` test module).

    #[test]
    fn json_rpc_error_includes_id_and_envelope() {
        let e = json_rpc_error(Some(json!(7)), -32601, "method not found: foo");
        assert_eq!(e["jsonrpc"], "2.0");
        assert_eq!(e["id"], 7);
        assert_eq!(e["error"]["code"], -32601);
        assert!(e["error"]["message"].as_str().unwrap().contains("foo"));
    }

    fn ensure_isolation() {
        ccteam_core::disable_tool_surface_bootstrap_for_tests();
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
    }

    #[tokio::test]
    async fn handle_tools_list_returns_full_tool_set() {
        // M2.5: 9 tools. V0.2.2 F38: +1 (`ccteam__screenshot`) → 10.
        // V0.4.0 F65: +7 workflow tools → 17.
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
        assert_eq!(tools.len(), 17);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__screenshot"));
        // V0.4.0 F65 — spot-check one of the new tools is in the list.
        assert!(names.contains(&"ccteam__spawn_agent"));
        assert!(names.contains(&"ccteam__get_artifact_summary"));
    }

    #[tokio::test]
    async fn handle_tools_call_screenshot_degrades_when_session_missing() {
        // No tmux session for this slug → the tool returns ok=false
        // with a reason, NOT isError=true. (Mirrors `peek` semantics:
        // read-only, daemon-independent.)
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
        // Graceful degrade lands as a normal result (not isError).
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
        // Notifications carry no `id`, must not produce a response.
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
            "params": { "name": "ccteam__ls", "arguments": {} }
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
    async fn handle_tools_call_send_to_session_writes_inbox_file() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        // M0.23.1: action tools require a live daemon heartbeat.
        ccteam_core::write_heartbeat(&paths).unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "ccteam__send_to_session",
                "arguments": { "session": "demo", "body": "hello from MCP" }
            }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let inbox = paths.project_ccteam_dir("demo").join("inbox");
        let entries: Vec<_> = std::fs::read_dir(&inbox).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "send_to_session must write exactly one inbox file"
        );
    }

    #[tokio::test]
    async fn handle_tools_call_inject_decision_writes_revert_payload() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        ccteam_core::write_heartbeat(&paths).unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "ccteam__inject_decision",
                "arguments": {
                    "slug": "demo",
                    "escalate_kind": "revert_to_phase",
                    "args": { "target_phase": "plan-eng", "reason": "选型有问题" }
                }
            }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let inbox = paths.project_ccteam_dir("demo").join("inbox");
        let entries: Vec<_> = std::fs::read_dir(&inbox).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("revert to phase `plan-eng`"));
        assert!(body.contains("选型有问题"));
    }

    #[tokio::test]
    async fn send_to_session_fails_loud_when_daemon_down() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        // No heartbeat written → daemon considered down.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 70,
            "method": "tools/call",
            "params": {
                "name": "ccteam__send_to_session",
                "arguments": { "session": "demo", "body": "ignored" }
            }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("daemon"), "got: {text}");
        // And no inbox entry was created (fail-loud, not silent ack).
        let inbox = paths.project_ccteam_dir("demo").join("inbox");
        if inbox.exists() {
            let entries: Vec<_> = std::fs::read_dir(&inbox).unwrap().collect();
            assert_eq!(entries.len(), 0, "fail-loud must not write inbox");
        }
    }

    #[tokio::test]
    async fn pause_and_resume_fail_loud_when_daemon_down() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        for tool in ["ccteam__pause", "ccteam__resume"] {
            let req = json!({
                "jsonrpc": "2.0",
                "id": 71,
                "method": "tools/call",
                "params": {
                    "name": tool,
                    "arguments": { "slug": "demo" }
                }
            });
            let resp = handle_request(&paths, &req).await.unwrap();
            assert_eq!(resp["result"]["isError"], true, "{tool}");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("daemon"), "{tool}: {text}");
        }
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
            "params": { "name": "ccteam__ls", "arguments": {} }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(
            parsed["orchestrator"]["daemon_health"]["status"], "no_heartbeat",
            "ls must annotate daemon health when daemon is down"
        );
    }

    #[tokio::test]
    async fn handle_tools_call_inject_decision_rejects_unknown_kind() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // Heartbeat present so the daemon-health gate doesn't preempt
        // the unknown-kind validation we're testing here.
        ccteam_core::write_heartbeat(&paths).unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "ccteam__inject_decision",
                "arguments": { "slug": "demo", "escalate_kind": "fly_to_mars" }
            }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("fly_to_mars"));
    }
}

// silence unused import in some test configurations
#[cfg(not(test))]
const _: fn() = || {};
