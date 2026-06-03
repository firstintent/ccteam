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
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// V0.3 M5.0 (F45 promote): write-helper logic lives in
// `ccteam_core::actions`; the wrappers below stay thin (args parse +
// JSON encode + delegate). Web-side consumers (`crates/ccteam-web`)
// pick up the same import line in M5.3 (`docs/versions/v0-3/dev-plan.md` §5
// #4.3) — at which point `git grep -nE 'use ccteam_core::actions'`
// returns ≥ 2 hits.
use ccteam_core::actions;
use ccteam_core::{
    bootstrap_project, check_daemon_health, cost_summary, pick_unused_slug, render_screenshot,
    CcteamPaths, DaemonHealth, SendOptions,
};
use ccteam_flow::MAX_CONCURRENT_PROJECTS;

use crate::commands::{collect_projects, collect_recent_events, run_peek, run_show, OutputFormat};
// V0.4.0 F65 — 7 new workflow tools. Schemas + handlers live in a
// dedicated module so `mcp_serve.rs` stays focused on the M2.5 protocol
// surface.
use crate::mcp_workflow_tools;
// V0.6.0 Wave 1 (F108 / F111 / F112) — chat / advise tool stubs +
// CCTEAM_DISABLE_TOOLS group filter. Wave 2/3 fills the chat / advise
// dispatch handlers; Wave 1 lands stubs so the tool surface shape +
// group disable env are usable end-to-end.
use crate::{mcp_admin_tools, mcp_advise_tools, mcp_chat_tools, mcp_tool_groups};

/// Stable MCP protocol version this server speaks. Newer client versions
/// downgrade gracefully because we never advertise capabilities we don't
/// implement.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// How often to poll for orphan / idle-timeout shutdown conditions.
/// Cheap (one `getppid()` + an `Instant::elapsed()`); 30s keeps the
/// overhead negligible while still catching the parent-died case
/// within one tick.
const MCP_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// No JSON-RPC traffic for this long ⇒ the parent isn't using us,
/// suicide so we don't pile up. Defaults to 5 minutes; the
/// `CCTEAM_MCP_IDLE_TIMEOUT_SECS` env var overrides for tests.
const MCP_DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

fn mcp_idle_timeout() -> Duration {
    std::env::var("CCTEAM_MCP_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(MCP_DEFAULT_IDLE_TIMEOUT)
}

#[cfg(unix)]
fn current_ppid() -> i32 {
    // SAFETY: getppid is always-safe; returns this process's parent
    // PID. Cast pid_t (i32 on Linux, i32 on macOS) to i32.
    unsafe { libc::getppid() as i32 }
}
#[cfg(not(unix))]
fn current_ppid() -> i32 {
    0
}

/// Server identity advertised in `initialize`.
const SERVER_NAME: &str = "ccteam";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// V0.8.4 P2a — server `instructions` surfaced to the agent on
/// `initialize`. This is the **load-bearing** Read convention: a bare
/// `claude` session does NOT auto-`Read` an attachment path — it must be
/// told to. (The official telegram plugin teaches this exact convention
/// via its own MCP server instructions; ccteam owns its channel, so it
/// teaches its own.) Without this, inbound images are silently unseen.
const CCTEAM_MCP_INSTRUCTIONS: &str = "ccteam routes IM (Telegram / web) chats to you and back. \
An inbound chat message may arrive wrapped in a `<channel source=\"…\" chat_id=\"…\" user=\"…\" message_id=\"…\">` tag.\n\n\
ATTACHMENTS (important): if a `<channel …>` tag carries an `image_path=\"/abs/path\"` attribute, immediately `Read` that file — \
it is an image the user attached (often an error screenshot) and is essential context. If it carries a `file_path=\"/abs/path\"` \
attribute, `Read` that file too. Further attachments may appear in the body as `[attachment image_path=\"…\"]` / \
`[attachment file_path=\"…\"]` lines — `Read` each of those as well. Do this BEFORE you answer; the user expects you to have \
looked at the file they sent.";

/// Run `ccteam mcp-serve`. Reads JSON-RPC requests one per line from
/// stdin; writes responses one per line to stdout.
///
/// Exits the process when ANY of:
/// - stdin closes (parent disconnect, normal MCP shutdown)
/// - SIGTERM (kernel via [`set_pdeathsig_sigterm`] when parent dies, or
///   explicit `kill -TERM`)
/// - SIGINT (Ctrl-C in interactive testing)
/// - parent reparented (getppid changed → original parent died but
///   PR_SET_PDEATHSIG signal didn't reach us, e.g. when an
///   intermediate shell shielded it)
/// - idle for [`MCP_DEFAULT_IDLE_TIMEOUT`] (no JSON-RPC traffic; parent
///   is alive but stopped talking to us — a stale zombie session)
///
/// The orphan and idle arms are belt-and-suspenders on top of stdin
/// EOF + `PR_SET_PDEATHSIG`. On WSL and some claude-spawn paths
/// neither of those fires reliably, so without these belts mcp-serve
/// processes pile up one-per-bg-job and exhaust file descriptors.
///
/// Why `std::process::exit(0)` and not `return Ok(())`: returning
/// drops the tokio runtime, which then tries to join every spawned
/// task — including the `tokio::io::stdin()` reader, which sits on a
/// blocking-thread-pool syscall that can't be cancelled. The result
/// is the process parks in `futex_wait` and needs SIGKILL to die.
/// `exit(0)` skips runtime drop and unwinds via libc, which is the
/// idiomatic shutdown for a stateless protocol adapter that owns no
/// reverse-side state. Originally caught in the V0.4.1 round-2
/// deploy-verify (host SIGTERM left mcp-serve in Sl/Ssl).
pub async fn run_mcp_serve(paths: CcteamPaths) -> Result<()> {
    set_pdeathsig_sigterm();
    let original_ppid = current_ppid();
    let idle_timeout = mcp_idle_timeout();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    let mut sigterm = signal_stream(SignalKind::terminate());
    let mut sigint = signal_stream(SignalKind::interrupt());

    let mut health_ticker = tokio::time::interval(MCP_HEALTH_CHECK_INTERVAL);
    health_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick fires immediately; swallow it so the orphan / idle
    // checks don't run before we've had a chance to do any work.
    health_ticker.tick().await;

    let mut last_activity = Instant::now();

    loop {
        let line = tokio::select! {
            line = reader.next_line() => match line.context("read stdin from MCP client")? {
                Some(l) => {
                    last_activity = Instant::now();
                    l
                }
                None => {
                    tracing::info!("mcp-serve: stdin EOF; exiting");
                    std::process::exit(0);
                }
            },
            _ = signal_recv(&mut sigterm) => {
                tracing::info!("mcp-serve: SIGTERM (parent exited or explicit stop); exiting");
                std::process::exit(0);
            }
            _ = signal_recv(&mut sigint) => {
                tracing::info!("mcp-serve: SIGINT; exiting");
                std::process::exit(0);
            }
            _ = health_ticker.tick() => {
                if should_exit_for_orphan(original_ppid) {
                    tracing::info!(original_ppid, current_ppid = current_ppid(),
                        "mcp-serve: parent reparented (orphan); exiting");
                    std::process::exit(0);
                }
                if last_activity.elapsed() >= idle_timeout {
                    tracing::info!(
                        idle_secs = last_activity.elapsed().as_secs(),
                        timeout_secs = idle_timeout.as_secs(),
                        "mcp-serve: idle timeout reached; exiting"
                    );
                    std::process::exit(0);
                }
                continue;
            }
        };
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
}

/// Returns true if our parent process has changed since startup —
/// strong signal that the original parent died and we got reparented
/// to init or a subreaper. On Unix we trust `getppid()`; on non-Unix
/// we always return false (no equivalent).
///
/// `original_ppid == 0` (the non-unix default) short-circuits this
/// check to off so the function is a no-op on platforms where we
/// can't observe parent identity.
fn should_exit_for_orphan(original_ppid: i32) -> bool {
    if original_ppid == 0 {
        return false;
    }
    let now = current_ppid();
    now != original_ppid || now == 1
}

/// On Linux ask the kernel to send us SIGTERM the moment our parent
/// process exits. This guarantees mcp-serve doesn't get orphaned and
/// pile up after a Claude Code session closes — even when stdin EOF
/// isn't propagated (some daemon-spawn paths inherit /dev/null or a
/// keep-alive descriptor). No-op on non-Linux platforms.
#[cfg(target_os = "linux")]
fn set_pdeathsig_sigterm() {
    // SAFETY: prctl is a thin syscall wrapper; PR_SET_PDEATHSIG is
    // documented to take a signal number in arg2 and ignore the rest.
    // SIGTERM (15) is portable.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
    }
}

#[cfg(not(target_os = "linux"))]
fn set_pdeathsig_sigterm() {}

#[cfg(unix)]
type SigStream = tokio::signal::unix::Signal;
#[cfg(not(unix))]
type SigStream = ();

#[cfg(unix)]
use tokio::signal::unix::SignalKind;
#[cfg(not(unix))]
struct SignalKind;
#[cfg(not(unix))]
impl SignalKind {
    fn terminate() -> Self {
        Self
    }
    fn interrupt() -> Self {
        Self
    }
}

#[cfg(unix)]
fn signal_stream(kind: SignalKind) -> SigStream {
    tokio::signal::unix::signal(kind).expect("install unix signal handler")
}
#[cfg(not(unix))]
fn signal_stream(_: SignalKind) -> SigStream {}

#[cfg(unix)]
async fn signal_recv(s: &mut SigStream) {
    let _ = s.recv().await;
}
#[cfg(not(unix))]
async fn signal_recv(_: &mut SigStream) {
    // On non-unix there's no signal arm; future never resolves.
    std::future::pending::<()>().await;
}

/// Dispatch a single JSON-RPC message. Returns `Some(response)` for
/// requests (which carry an `id`) and `None` for notifications.
pub(crate) async fn handle_request(paths: &CcteamPaths, req: &Value) -> Option<Value> {
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
        // P2a — teach the agent the inbound-attachment Read convention.
        "instructions": CCTEAM_MCP_INSTRUCTIONS,
    })
}

fn tools_list_response() -> Value {
    // V0.6.0 F111 — honour `CCTEAM_DISABLE_TOOLS` group enum
    // (`workflow`, `chat`, `advise`, `screenshot`, `admin`). The
    // filter runs on every `tools/list` so users can toggle groups
    // without restarting `ccteam mcp-serve`.
    let disabled = mcp_tool_groups::disabled_groups_from_env();
    let tools = mcp_tool_groups::filter_by_disabled(tool_definitions(), &disabled);
    json!({ "tools": tools })
}

/// Single source of truth for the MCP tool surface. M2.5 shipped 9
/// tools; V0.2.2 F38 added `ccteam__screenshot` → 10; V0.4.0 F65 adds
/// 7 workflow-control tools (`spawn_agent` / `stop_agent` /
/// `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` /
/// `get_artifact_summary`) → 17. V0.6.0 Wave 1 (F111) adds 5 chat
/// stubs + 2 advise stubs → 24. V0.6.1 F128 adds 2 admin mutators
/// (`admin_change_persona` + `admin_add_tool`) → 26. V0.6.5 F146
/// swaps `chat_lifecycle` STUB for real `chat_register_bot` +
/// `chat_unregister_bot` (no deprecated alias — CLAUDE.md §五 #4)
/// → **27 total**. All tools carry a group sub-prefix (`admin_`,
/// `workflow_`, `chat_`, `advise_`) except `ccteam__screenshot`
/// which keeps its single-member-group name for V0.5 muscle memory.
/// Schemas mirror interfaces.md §12.2.
pub(crate) fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = vec![
        // Read-only inspection.
        json!({
            "name": "ccteam__admin_ls",
            "description": "List every ccteam project under ~/projects/ with its current phase, state, cost, and stall level. Equivalent to `ccteam ls --format json`.",
            "inputSchema": object_schema(&[]),
        }),
        json!({
            "name": "ccteam__workflow_show",
            "description": "Return the full state.json + recent progress events + artifact list for a single project. Equivalent to `ccteam show <slug> --format json`.",
            "inputSchema": object_schema(&[("slug", "string", "Project slug, as listed by ccteam__admin_ls.")]),
        }),
        json!({
            "name": "ccteam__workflow_peek",
            "description": "Capture the current tmux pane contents for a project session (one-shot, no attach). Useful when you want to see what claude is showing without interrupting.",
            "inputSchema": object_schema(&[("slug", "string", "Project slug.")]),
        }),
        json!({
            "name": "ccteam__workflow_progress",
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
            "name": "ccteam__workflow_new",
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
            "name": "ccteam__workflow_pause",
            "description": "Pause auto-dispatch for one project (sets user_pause_pending). Does not kill the tmux session.",
            "inputSchema": object_schema(&[("slug", "string", "Project slug.")]),
        }),
        json!({
            "name": "ccteam__workflow_resume",
            "description": "Resume an escalated / paused project (clears user_pause_pending and archives any escalation.md so the daemon's next tick re-dispatches the current phase).",
            "inputSchema": object_schema(&[("slug", "string", "Project slug.")]),
        }),
        // M2.5 new pair.
        json!({
            "name": "ccteam__workflow_send_to_session",
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
            "name": "ccteam__workflow_inject_decision",
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
        // requirement); mirrors `ccteam__workflow_peek` semantics.
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
    // V0.6.0 Wave 1 (F108 / F112) — append chat (6) + advise (2)
    // tools. V0.6.5 F146 / F147 turned all 6 chat stubs into real
    // implementations; V0.6.5 F152 / F153 turned both advise stubs
    // into real implementations against the `ccteam_core::advise`
    // entry points (Claude + Codex parallel one-shot advisors +
    // per-vendor budget ledger).
    tools.extend(mcp_chat_tools::chat_tool_definitions());
    tools.extend(mcp_advise_tools::advise_tool_definitions());
    // V0.6.1 F128 — `admin_change_persona` + `admin_add_tool` real
    // tools land here. The pre-existing `admin_ls` stays inline above
    // (it's the V0.5 read-only entry; the two F128 mutators move the
    // admin group from 1 → 3 tools).
    tools.extend(mcp_admin_tools::admin_tool_definitions());
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
        "ccteam__admin_ls" => Ok(text_content(tool_ls(paths)?)),
        "ccteam__workflow_show" => Ok(text_content(tool_show(paths, &args)?)),
        "ccteam__workflow_peek" => Ok(text_content(tool_peek(paths, &args)?)),
        "ccteam__workflow_progress" => Ok(text_content(tool_progress(paths, &args)?)),
        "ccteam__workflow_new" => Ok(text_content(tool_new(paths, &args)?)),
        "ccteam__workflow_pause" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_pause(paths, &args)?))
        }
        "ccteam__workflow_resume" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_resume(paths, &args)?))
        }
        "ccteam__workflow_send_to_session" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_send_to_session(paths, &args)?))
        }
        "ccteam__workflow_inject_decision" => {
            require_healthy_daemon(paths)?;
            Ok(text_content(tool_inject_decision(paths, &args)?))
        }
        "ccteam__screenshot" => Ok(text_content(tool_screenshot(paths, &args)?)),
        // V0.8.4 P2b — `chat_send_file` is a LIVE tool: it needs the
        // daemon's gateway-event sink (which this stdio process doesn't
        // have). Forward it over the existing `mcp.sock` to the daemon,
        // injecting the agent's ambient identity. (The daemon-side socket
        // handler intercepts it before `handle_request`, so it never loops
        // back into this branch.)
        "ccteam__chat_send_file" => forward_chat_send_file(paths, &args).await,
        // V0.4.0 F65 — route the 7 workflow tools through the
        // dedicated dispatcher. Mutating tools (spawn / stop / signal /
        // set_parallelism / trigger_gate) gate on a reachable gateway
        // daemon so a dead control plane surfaces immediately rather
        // than silently swallowing writes that no one consumes.
        // Read-only tools (observe_agents / get_artifact_summary) stay
        // daemon-independent so the meta-agent can inspect a stopped
        // project.
        other => {
            if mcp_workflow_tools::requires_daemon(other) {
                require_healthy_daemon(paths)?;
            }
            // V0.6.1 F128 — admin mutator tools (`change_persona` /
            // `add_tool`) gate on live daemon, mirroring the
            // workflow mutators above. `admin_ls` stays inline and
            // is read-only so it does not gate.
            if mcp_admin_tools::requires_daemon(other) {
                require_healthy_daemon(paths)?;
            }
            if let Some(body) = mcp_workflow_tools::dispatch(paths, other, &args)? {
                return Ok(text_content(body));
            }
            // V0.6.5 F146 — chat group: real register / unregister /
            // list_bots dispatchers + 3 F147-pending stubs. The
            // dispatcher returns Ok(None) for tools that aren't ours
            // so the fall-through to `unknown tool` below is preserved
            // for genuine typos. None of the chat tools currently gate
            // on a live daemon (file-system control plane only).
            if let Some(body) = mcp_chat_tools::dispatch(paths, other, &args)? {
                return Ok(text_content(body));
            }
            // V0.6.5 F152 + F153 — advise group: real `advise_vote` +
            // `advise_parallel` (Claude + Codex parallel one-shot
            // advisors + verdict synthesis / N-of-N). Async dispatch
            // because the underlying advisor calls spawn tokio
            // subprocesses; ledger gates the spend pre-fan-out.
            if let Some(body) = mcp_advise_tools::dispatch(paths, other, &args).await? {
                return Ok(text_content(body));
            }
            // V0.6.1 F128 — admin mutators (after the daemon gate
            // above).
            if let Some(body) = mcp_admin_tools::dispatch(paths, other, &args)? {
                return Ok(text_content(body));
            }
            Err(anyhow!("unknown tool: {other}"))
        }
    }
}

/// V0.8.4 P2b — forward a `chat_send_file` call to the daemon's
/// `mcp.sock`. The agent's identity is ambient (`CCTEAM_CHAT_SLUG` /
/// `CCTEAM_CHAT_ROLE`, injected at spawn); we inject it into the args so
/// the daemon can resolve the home chat. Returns a structured (non-fatal)
/// error content if we're not in a chat session or the daemon is down.
async fn forward_chat_send_file(paths: &CcteamPaths, args: &Value) -> Result<Vec<Value>> {
    let slug = std::env::var("CCTEAM_CHAT_SLUG").unwrap_or_default();
    let role = std::env::var("CCTEAM_CHAT_ROLE").unwrap_or_default();
    if slug.is_empty() || role.is_empty() {
        return Ok(text_content(
            "chat_send_file: not in a ccteam chat session (CCTEAM_CHAT_SLUG/ROLE unset)"
                .to_string(),
        ));
    }
    let mut fwd_args = args.clone();
    if let Some(obj) = fwd_args.as_object_mut() {
        obj.insert("slug".to_string(), json!(slug));
        obj.insert("role".to_string(), json!(role));
    }
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "ccteam__chat_send_file", "arguments": fwd_args },
    });
    let socket = ccteam_core::daemon_socket_path(paths);
    match forward_to_socket(&socket, &req).await {
        Ok(resp) => {
            let text = resp
                .pointer("/result/content/0/text")
                .and_then(|t| t.as_str())
                .unwrap_or("chat_send_file: delivered")
                .to_string();
            Ok(text_content(text))
        }
        Err(err) => Ok(text_content(format!(
            "chat_send_file failed: daemon mcp.sock unreachable ({err})"
        ))),
    }
}

/// Open a one-shot connection to the daemon `mcp.sock`, send one
/// JSON-RPC line, and read one response line back.
#[cfg(unix)]
async fn forward_to_socket(socket: &std::path::Path, req: &Value) -> Result<Value> {
    use tokio::io::AsyncWriteExt as _;
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    let mut lines = BufReader::new(reader).lines();
    let resp = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("mcp.sock closed before responding"))?;
    Ok(serde_json::from_str(&resp)?)
}

#[cfg(not(unix))]
async fn forward_to_socket(_socket: &std::path::Path, _req: &Value) -> Result<Value> {
    Err(anyhow!("mcp.sock forwarding is unix-only"))
}

/// Fail-loud gate for action tools that need a reachable gateway daemon.
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
            // V0.4.6 F91 — cost_used_usd is now sourced from
            // cost_summary (progress.jsonl-derived) rather than the
            // frozen state field. F90 will surface the new
            // cost_24h / cost_active fields here too; for now we
            // keep the legacy `cost_used_usd` JSON key but populate
            // it from `cost_total_usd` so the MCP shape is stable.
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

/// Stable JSON shape for daemon health: `status` is one of
/// `healthy|unreachable`; `message` is the human-readable describe().
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
    // V0.4.2 F73: register the new project in config.yaml so the
    // daemon roster (which reads `config.yaml::projects[]` first)
    // picks it up immediately. Without this, mcp-created projects
    // are only visible via the legacy fs-walk fallback.
    ccteam_core::upsert_project_in_config(
        &paths.root,
        ccteam_core::ProjectEntry {
            slug: slug.clone(),
            path: project_dir.clone(),
            team: team.clone(),
            installed_at: chrono::Utc::now(),
        },
    )
    .with_context(|| {
        format!(
            "upsert project `{slug}` into {}",
            ccteam_core::ccteam_config_path(&paths.root).display(),
        )
    })?;
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
        // 10. V0.4.0 F65 adds 7 workflow tools → 17. V0.6.0 Wave 1
        // (F111) adds 5 chat stubs + 2 advise stubs → 24. V0.6.1 F128
        // adds 2 admin mutators (`change_persona` + `add_tool`) → 26.
        // V0.6.5 F146 chat group net +1 (removed `chat_lifecycle`,
        // added `chat_register_bot` + `chat_unregister_bot`) → 27.
        // V0.8.4 P2b adds `chat_send_file` → 28.
        // Bump this when a new tool lands.
        assert_eq!(tool_definitions().len(), 28);
    }

    #[test]
    fn tool_definitions_have_unique_names_and_object_schemas() {
        let tools = tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 28, "tool names must be unique");
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

    #[cfg(unix)]
    fn fake_daemon(paths: &CcteamPaths) -> std::os::unix::net::UnixListener {
        let socket = ccteam_core::daemon_socket_path(paths);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::os::unix::net::UnixListener::bind(socket).unwrap()
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
        // P2a — the inbound-attachment Read convention must be taught via
        // the server `instructions`, or a bare claude won't Read images.
        let instructions = resp["result"]["instructions"].as_str().unwrap();
        assert!(
            instructions.contains("image_path"),
            "instructions: {instructions}"
        );
        assert!(instructions.contains("file_path"));
        assert!(instructions.contains("Read"));
        assert!(instructions.contains("<channel"));
    }

    /// V0.8.4 P2b — the stdio→daemon bridge: `forward_to_socket` writes one
    /// JSON-RPC line to a unix socket and reads one line back.
    #[cfg(unix)]
    #[tokio::test]
    async fn forward_to_socket_round_trips_one_line() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("mcp.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            // Confirm the forwarded request shape, then canned-respond.
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: Value = serde_json::from_str(&req_line).unwrap();
            assert_eq!(req["params"]["name"], "ccteam__chat_send_file");
            let resp = json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "content": [{"type":"text","text":"delivered: queued"}], "isError": false }
            });
            let mut line = serde_json::to_string(&resp).unwrap();
            line.push('\n');
            writer.write_all(line.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        });

        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "ccteam__chat_send_file", "arguments": { "path": "/x.png" } }
        });
        let resp = forward_to_socket(&socket, &req).await.unwrap();
        assert_eq!(
            resp.pointer("/result/content/0/text")
                .unwrap()
                .as_str()
                .unwrap(),
            "delivered: queued"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handle_tools_list_returns_full_tool_set() {
        // M2.5: 9 tools. V0.2.2 F38: +1 (`ccteam__screenshot`) → 10.
        // V0.4.0 F65: +7 workflow tools → 17. V0.6.0 Wave 1 (F111):
        // +5 chat stubs +2 advise stubs → 24. V0.6.1 F128: +2 admin
        // mutators (`change_persona` + `add_tool`) → 26. V0.6.5 F146:
        // chat group went from 5 → 6 (removed `chat_lifecycle`,
        // added `chat_register_bot` + `chat_unregister_bot`) → 27.
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
        assert_eq!(tools.len(), 28);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__screenshot"));
        assert!(names.contains(&"ccteam__chat_send_file"));
        // V0.4.0 F65 — spot-check one of the new tools is in the list.
        assert!(names.contains(&"ccteam__workflow_spawn_agent"));
        assert!(names.contains(&"ccteam__workflow_get_artifact_summary"));
        // V0.6.0 Wave 1 — chat / advise stubs present.
        assert!(names.contains(&"ccteam__chat_send_input"));
        assert!(names.contains(&"ccteam__advise_vote"));
        // V0.6.5 F146 — chat register / unregister land here, and the
        // old `chat_lifecycle` is gone (no deprecated alias).
        assert!(names.contains(&"ccteam__chat_register_bot"));
        assert!(names.contains(&"ccteam__chat_unregister_bot"));
        assert!(!names.contains(&"ccteam__chat_lifecycle"));
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
            "params": { "name": "ccteam__admin_ls", "arguments": {} }
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
        let _daemon = fake_daemon(&paths);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "ccteam__workflow_send_to_session",
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
        let _daemon = fake_daemon(&paths);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "ccteam__workflow_inject_decision",
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
        // No MCP socket listener → daemon considered down.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 70,
            "method": "tools/call",
            "params": {
                "name": "ccteam__workflow_send_to_session",
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
        for tool in ["ccteam__workflow_pause", "ccteam__workflow_resume"] {
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
            "params": { "name": "ccteam__admin_ls", "arguments": {} }
        });
        let resp = handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(
            parsed["orchestrator"]["daemon_health"]["status"], "unreachable",
            "ls must annotate daemon health when daemon is down"
        );
    }

    #[test]
    fn should_exit_for_orphan_returns_true_on_ppid_change() {
        // Simulate "we started under ppid=12345; now getppid() returns
        // something else" by passing an impossible original ppid.
        // current_ppid() returns this test process's real ppid, which
        // will never equal 12345 → orphan.
        assert!(should_exit_for_orphan(12345));
    }

    #[test]
    fn should_exit_for_orphan_is_noop_when_original_ppid_zero() {
        // Non-unix builds set original_ppid=0 ⇒ orphan check disabled.
        assert!(!should_exit_for_orphan(0));
    }

    #[test]
    fn should_exit_for_orphan_returns_false_when_ppid_unchanged() {
        let ppid = current_ppid();
        if ppid == 0 {
            // Non-unix path; nothing to assert.
            return;
        }
        // current ppid == original ppid AND not 1 ⇒ still attached.
        if ppid == 1 {
            // Test process happens to be PID 1 (e.g. inside a minimal
            // container); orphan check correctly reports orphan.
            assert!(should_exit_for_orphan(ppid));
        } else {
            assert!(!should_exit_for_orphan(ppid));
        }
    }

    #[test]
    fn mcp_idle_timeout_reads_env_then_falls_back_to_default() {
        // Single test exercises both the env-override path and the
        // default-on-missing path so the two halves don't race against
        // each other under `cargo test`'s parallel runner. No other
        // test touches CCTEAM_MCP_IDLE_TIMEOUT_SECS so this is the only
        // user of the var.
        let prev = std::env::var("CCTEAM_MCP_IDLE_TIMEOUT_SECS").ok();
        std::env::set_var("CCTEAM_MCP_IDLE_TIMEOUT_SECS", "7");
        assert_eq!(mcp_idle_timeout(), Duration::from_secs(7));
        std::env::remove_var("CCTEAM_MCP_IDLE_TIMEOUT_SECS");
        assert_eq!(mcp_idle_timeout(), MCP_DEFAULT_IDLE_TIMEOUT);
        if let Some(v) = prev {
            std::env::set_var("CCTEAM_MCP_IDLE_TIMEOUT_SECS", v);
        }
    }

    #[tokio::test]
    async fn handle_tools_call_inject_decision_rejects_unknown_kind() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // Reachable daemon socket so the health gate doesn't preempt
        // the unknown-kind validation we're testing here.
        let _daemon = fake_daemon(&paths);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "ccteam__workflow_inject_decision",
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
