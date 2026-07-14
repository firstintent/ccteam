//! Daemon-side MCP dispatch: stateful intercepts + protocol-core fallback.
//!
//! Owns the live gateway / pending registry / event sink needed for
//! `interaction/ask`, `permission/ask`, `chat_send_file`, `session_*`, and
//! `ccteam/reload`. The thin socket (or future HTTP) loop only does
//! read-line → [`McpDispatch::dispatch`] → write-line.

use std::sync::Arc;

use ccteam_core::CcteamPaths;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::gateway::{Gateway, GatewayEvent};
use crate::pending::PendingInteractions;

use super::protocol;

/// Sender half of the gateway-event channel that the IM daemon consumes.
pub type GatewayEventSink = tokio::sync::mpsc::UnboundedSender<GatewayEvent>;

/// Shared pending-interaction registry (gateway + MCP handler both hold it).
pub type PendingRegistry = Arc<Mutex<PendingInteractions>>;

/// Shared gateway handle so `session_*` tools drive the in-memory session map.
pub type GatewayHandle = Arc<Mutex<Gateway>>;

/// Daemon-side MCP request dispatcher.
///
/// Fields are `Option` so a socket connection still works when IM / web
/// pieces are not wired (structured errors for stateful tools; protocol
/// core still serves `status` / `screenshot` / `tools/list`).
pub struct McpDispatch {
    /// ccteam path layout (home + projects root).
    pub paths: CcteamPaths,
    /// Outbound IM funnel (file sends + HITL buttons).
    pub sink: Option<GatewayEventSink>,
    /// Shared pending-interaction registry for External-origin prompts.
    pub pending: Option<PendingRegistry>,
    /// Live gateway session map.
    pub gateway: Option<GatewayHandle>,
}

/// Who is invoking the dispatch — decides how the privileged intercepts
/// authenticate (v0.9 T4 review fix).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum McpCaller {
    /// mcp.sock / stdio-forwarder path: `session_*` authenticates via the
    /// env-injected `_caller_role`/`_caller_secret` args (cto gate); the
    /// internal-bus methods (`interaction/ask`, `permission/ask`) are served.
    Ambient,
    /// HTTP `/mcp` behind a verified admin bearer — the owner's front door.
    /// `session_*` skips the cto role/secret gate (that gate exists to stop
    /// non-privileged *sessions*, not the authenticated owner); the
    /// internal-bus methods are NOT exposed (in-band/daemon-internal
    /// responsibility, not front-door API).
    Admin,
}

impl McpDispatch {
    /// Dispatch one JSON-RPC request on the ambient (mcp.sock / stdio) path.
    /// Wire-compatible with the historical `handle_mcp_socket_connection`.
    ///
    /// v0.9.1 main-session fallback: a LOCAL caller may present the admin web
    /// token (`_caller_admin_token` in the tool arguments — injected by the
    /// stdio forwarder when its env carries no `(sid, secret)` principal, i.e.
    /// the user's daily-driver Claude/Codex session that ccteam did not
    /// spawn). A matching token promotes the call to [`McpCaller::Admin`] —
    /// the same trust the HTTP `/mcp` admin bearer grants; the token file is
    /// `0600` under `~/.ccteam/secrets/`, so presenting it proves same-user
    /// file access, exactly like running the `ccteam` CLI. A missing or
    /// wrong token leaves the call on the fail-closed Ambient path. The arg
    /// is stripped either way so nothing downstream ever sees it.
    pub async fn dispatch(&self, req: Value) -> Option<Value> {
        let (req, caller) = self.promote_local_admin(req);
        self.dispatch_as(req, caller).await
    }

    /// Socket-only admin promotion (see [`Self::dispatch`]). Constant-time
    /// token compare; never logs the presented value.
    fn promote_local_admin(&self, mut req: Value) -> (Value, McpCaller) {
        let presented = match req
            .pointer_mut("/params/arguments")
            .and_then(|a| a.as_object_mut())
        {
            Some(args) => match args.remove("_caller_admin_token") {
                Some(v) => v.as_str().unwrap_or_default().to_string(),
                None => return (req, McpCaller::Ambient),
            },
            None => return (req, McpCaller::Ambient),
        };
        let expected = std::fs::read_to_string(self.paths.web_token_path())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !expected.is_empty() && ccteam_core::session_secret::ct_eq(&expected, &presented) {
            (req, McpCaller::Admin)
        } else {
            (req, McpCaller::Ambient)
        }
    }

    /// Dispatch one JSON-RPC request as `caller`. Order matches the historical
    /// `handle_mcp_socket_connection` intercept chain exactly:
    /// `interaction/ask` → `permission/ask` → `chat_send_file` →
    /// `session_*` → `ccteam/reload` → protocol core.
    pub async fn dispatch_as(&self, req: Value, caller: McpCaller) -> Option<Value> {
        if is_interaction_ask_call(&req) {
            if caller == McpCaller::Admin {
                return Some(internal_bus_not_exposed(&req));
            }
            Some(
                execute_interaction_ask(
                    &req,
                    self.sink.as_ref(),
                    self.pending.as_ref(),
                    self.gateway.as_ref(),
                )
                .await,
            )
        } else if is_permission_ask_call(&req) {
            if caller == McpCaller::Admin {
                return Some(internal_bus_not_exposed(&req));
            }
            Some(
                execute_permission_ask(
                    &req,
                    self.sink.as_ref(),
                    self.pending.as_ref(),
                    self.gateway.as_ref(),
                )
                .await,
            )
        } else if is_chat_send_file_call(&req) {
            Some(execute_chat_send_file(&req, self.sink.as_ref(), self.gateway.as_ref()).await)
        } else if is_session_tool_call(&req) {
            Some(execute_session_tool(&req, self.gateway.as_ref(), caller).await)
        } else if is_reload_call(&req) {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let ok = if let Some(gw) = self.gateway.as_ref() {
                gw.lock().await.request_im_reload()
            } else {
                false
            };
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "reloaded": ok },
            }))
        } else {
            protocol::handle_request(&self.paths, &req).await
        }
    }
}

/// v0.8.7 W1 — cap on how many child turns `session_collect` returns when the
/// caller doesn't pass `n`. Keeps a runaway transcript from flooding the
/// cto's context in one poll.
const SESSION_COLLECT_DEFAULT_N: usize = 20;

/// v0.8.5 D6 — how long the `interaction/ask` handler waits for the user to
/// answer before forgetting the prompt + reporting a timeout (the hook then
/// degrades to deny-with-reason). Matches the gateway pending TTL default.
const INTERACTION_ASK_TIMEOUT_SECS: u64 = 600;

/// v0.8.7 review-fix (R-L1) — a HITL `permission/ask` prompt gets a SHORTER
/// deadline than the 600s `interaction/ask`: a tool-approval blocks the
/// agent's whole turn, so a long park is worse than a fast fail-safe deny.
/// On lapse the hook still denies (fail-safe = deny). Env-overridable
/// (`CCTEAM_PERMISSION_PROMPT_TTL_SECS`) for ops + tests.
///
/// v0.8.22 P0-2 — delegates to [`crate::hitl::permission_prompt_timeout_secs`]
/// (the SAME knob the stream-json protocol's in-process HITL resolver reads),
/// so the terminal protocol's `permission/ask` hook and the stream-json
/// protocol's `can_use_tool` resolver can never drift on the TTL.
fn permission_prompt_timeout_secs() -> u64 {
    crate::hitl::permission_prompt_timeout_secs()
}

/// JSON-RPC `-32601` for the internal-bus methods (`interaction/ask`,
/// `permission/ask`) on the HTTP front door: those are in-band / daemon-
/// internal (mcp.sock) responsibilities, deliberately not front-door API
/// (tech-design v0.9 §1.1 — HITL stays on vendor-native channels).
fn internal_bus_not_exposed(req: &serde_json::Value) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("method not available on this transport: {method}"),
        },
    })
}

/// Monotonic id source so each `chat_send_file` gets a distinct durable
/// ledger row (avoids `{id}-0` collisions in `outbound.jsonl`).
static CHAT_SEND_FILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn is_chat_send_file_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req.pointer("/params/name").and_then(|n| n.as_str()) == Some("chat_send_file")
}

/// Resolve addressing, validate the file, and enqueue a `GatewayEvent`
/// onto the shared sink (the IM consumer does the actual `sendPhoto` /
/// `sendDocument`). Returns a tools/call-shaped JSON-RPC response.
async fn execute_chat_send_file(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    gateway: Option<&GatewayHandle>,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    // v0.8.7 (FIX-1) — resolve the live session's reply target FIRST, under
    // the gateway lock, then DROP the guard before any fs read / send (lock
    // discipline §7-1, mirroring run_session_collect). `None` here means no
    // live (project, role) session is tracked → run_chat_send_file falls back
    // to the on-disk registry. We resolve here (async) and inject the result
    // into the sync builder so build_send_file_event stays unit-testable.
    let live_target = resolve_live_reply_target(&args, gateway).await;
    let (text, is_error) = match run_chat_send_file(&args, sink, live_target) {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        },
    })
}

/// v0.8.7 (FIX-1) — resolve the live `(channel, chat_id)` for the firing
/// session the `chat_send_file` args name, by looking up the gateway's
/// in-memory session map. The gateway guard is taken and dropped INSIDE this
/// fn (the lookup is sync + holds no `.await`) so callers never hold it across
/// an fs read / send. `None` when no gateway handle, no firing sid, or no
/// tracked session matches.
///
/// v0.8.8 F1 — keyed by the firing session's ccteam sid (`_caller_sid`,
/// injected by the in-pane `forward_chat_send_file` forwarder from
/// `CCTEAM_CHAT_SID`): post-dedup `(slug, role)` is no longer unique, so the
/// sid is the only safe way to reach the SPECIFIC session's reply target.
async fn resolve_live_reply_target(
    args: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
) -> Option<(String, String)> {
    let gw = gateway?;
    let sid = args
        .get("_caller_sid")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if sid.is_empty() {
        return None;
    }
    let guard = gw.lock().await;
    guard.reply_target_for(sid)
}

fn run_chat_send_file(
    args: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    live_target: Option<(String, String)>,
) -> std::result::Result<String, String> {
    let sink = sink.ok_or_else(|| "chat_send_file: IM gateway not running".to_string())?;
    let seq = CHAT_SEND_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let event = build_send_file_event(args, seq, live_target)?;
    let dest = format!("{}/{}", event.channel, event.chat_id);
    sink.send(event)
        .map_err(|_| "chat_send_file: gateway sink closed".to_string())?;
    Ok(format!("delivered: queued to {dest}"))
}

/// Telegram bot-send ceilings: `sendPhoto` ≤ 10 MB, `sendDocument` ≤ 50 MB.
const OUTBOUND_PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;
const OUTBOUND_DOCUMENT_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Pure core of `run_chat_send_file`: parse args, validate the file
/// (exists + within the send ceiling), and build the `GatewayEvent`
/// addressed to the firing session's `live_target` — the SINGLE source of
/// truth for session→chat addressing. Only I/O is the file stat, so it is
/// unit-testable.
fn build_send_file_event(
    args: &serde_json::Value,
    seq: u64,
    live_target: Option<(String, String)>,
) -> std::result::Result<crate::gateway::GatewayEvent, String> {
    use crate::transport::OutboundFileKind;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat_send_file: missing `path`".to_string())?;
    let caption = args
        .get("caption")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let kind = parse_outbound_kind(args.get("kind").and_then(|v| v.as_str()), path);

    let meta =
        std::fs::metadata(path).map_err(|_| format!("chat_send_file: file not found: {path}"))?;
    let max = match kind {
        OutboundFileKind::Photo => OUTBOUND_PHOTO_MAX_BYTES,
        OutboundFileKind::Document => OUTBOUND_DOCUMENT_MAX_BYTES,
    };
    if meta.len() > max {
        return Err(format!(
            "chat_send_file: file too large ({} MB) for {:?} (limit {} MB)",
            meta.len() / (1024 * 1024),
            kind,
            max / (1024 * 1024),
        ));
    }
    // v0.8.8 — single source of truth: the firing session's live reply target
    // (its `owner` ChatKey, set at spawn, keyed by sid via `reply_target_for`).
    // NO registry fallback — the two-store addressing is gone; a missing
    // binding is a spawn/bind-flow defect, surfaced precisely, not papered over.
    let sid = args
        .get("_caller_sid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let (channel, chat_id) = live_target.ok_or_else(|| {
        format!(
            "chat_send_file: no IM chat bound to firing session sid={sid:?} ({slug}/{role}); owner unset at spawn/bind"
        )
    })?;
    Ok(crate::gateway::GatewayEvent {
        id: format!("chat-send-file-{slug}-{role}-{seq}"),
        channel,
        chat_id,
        thread_ts: None,
        content: String::new(),
        kind: crate::gateway::GatewayEventKind::Answer,
        attachments: vec![crate::transport::OutboundFile {
            path: path.to_string(),
            caption,
            kind,
        }],
        options: Vec::new(),
        // No gateway session backs the `chat_send_file` MCP path.
        sid: None,
        slug: if slug.is_empty() {
            None
        } else {
            Some(slug.to_string())
        },
    })
}

/// `kind` arg → [`OutboundFileKind`], inferring photo from common image
/// extensions when omitted.
fn parse_outbound_kind(kind: Option<&str>, path: &str) -> crate::transport::OutboundFileKind {
    use crate::transport::OutboundFileKind;
    match kind {
        Some("photo") => OutboundFileKind::Photo,
        Some("document") => OutboundFileKind::Document,
        _ => {
            let lower = path.to_lowercase();
            let is_image = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
                .iter()
                .any(|ext| lower.ends_with(ext));
            if is_image {
                OutboundFileKind::Photo
            } else {
                OutboundFileKind::Document
            }
        }
    }
}

/// v0.8.5 D6 — true when this line is the AskUserQuestion hook's
/// `interaction/ask` RPC (raw JSON-RPC `method`, not a `tools/call`).
fn is_interaction_ask_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("interaction/ask")
}

/// v0.8.5 D6 — handle one `interaction/ask` request from the AskUserQuestion
/// chat hook. Mint a token, build a [`ChoicePrompt`], resolve the bot's home
/// chat, register an External-origin pending in the SHARED registry, emit a
/// `GatewayEvent` so IM renders buttons, then block (with a TTL, holding NO
/// lock) on the user's selection.
///
/// Request:  `{"jsonrpc":"2.0","id":N,"method":"interaction/ask",
///             "params":{"slug","role","question","options":[..],"multi"}}`
/// Response: `{"result":{"answers":{<question>:<label>}}}` on a pick,
///           `{"result":{"timeout":true}}` on TTL lapse, or a JSON-RPC
///           `error` when addressing / wiring is unavailable (the hook then
///           degrades to deny-with-reason).
async fn execute_interaction_ask(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    pending: Option<&PendingRegistry>,
    gateway: Option<&GatewayHandle>,
) -> serde_json::Value {
    use crate::gateway::{GatewayEvent, GatewayEventKind};
    use crate::pending::InteractionOrigin;
    use crate::transport::MessageOption;
    use ccteam_harness::{ChoiceOption, ChoicePrompt, ChoiceSelection};

    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let err_resp = |msg: String| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": { "code": -32000, "message": msg },
        })
    };

    let (Some(sink), Some(pending)) = (sink, pending) else {
        return err_resp("interaction/ask: IM gateway not running".to_string());
    };
    let params = req
        .pointer("/params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let question = params
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let multi = params
        .get("multi")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let options: Vec<String> = params
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if question.is_empty() || options.is_empty() {
        return err_resp("interaction/ask: empty question or options".to_string());
    }

    // Resolve addressing first — no point registering a pending we can't show.
    // v0.8.8 — single source of truth: the live firing session's reply target
    // (its `owner` ChatKey, set at spawn, keyed by sid via `reply_target_for`;
    // resolve under the gateway lock, drop the guard before the long await —
    // the lookup is sync). NO registry fallback — the two-store addressing is
    // gone; a missing binding (empty/unpropagated sid, or no live session for
    // it) is a spawn/bind-flow defect, surfaced precisely.
    let session_sid = params
        .get("session_sid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let live_target = match gateway {
        Some(gw) if !session_sid.is_empty() => {
            let guard = gw.lock().await;
            guard.reply_target_for(session_sid)
        }
        _ => None,
    };
    let Some((channel, chat_id)) = live_target else {
        return err_resp(format!(
            "interaction/ask: no IM chat bound to firing session sid={session_sid:?} ({slug}/{role}); owner unset at spawn/bind — not falling back to the registry"
        ));
    };

    // Mint a short token (≤16B ASCII, no `:` — the ChoicePrompt contract).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("h{:x}", (nanos as u64) & 0xff_ffff_ffff);

    let prompt = ChoicePrompt {
        token: token.clone(),
        title: question.clone(),
        options: options
            .iter()
            .map(|o| ChoiceOption {
                id: o.clone(),
                label: o.clone(),
            })
            .collect(),
        multi,
    };
    let message_options: Vec<MessageOption> = prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{token}:{i}"),
            label: opt.label.clone(),
            // v0.8.7 review-fix (R-H1) — carry the stable option id (e.g.
            // "allow"/"deny") so the web SSE consumer can resolve by
            // {token, selection=id} through the same pending machinery.
            id: opt.id.clone(),
        })
        .collect();

    // Register the External-origin pending under the SHARED registry, keyed by
    // the token itself (the gateway resolves token-globally via take_by_token).
    // Release the guard BEFORE the long await (lock discipline §7-1).
    let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
    let ttl = std::time::Duration::from_secs(INTERACTION_ASK_TIMEOUT_SECS);
    {
        let mut guard = pending.lock().await;
        guard.register(
            token.clone(),
            prompt.clone(),
            InteractionOrigin::External { reply: tx },
            std::time::Instant::now() + ttl,
        );
    }

    // Render the buttons in IM.
    if sink
        .send(GatewayEvent {
            id: format!("interaction-{token}"),
            channel,
            chat_id,
            thread_ts: None,
            content: question.clone(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: message_options,
            // The D6 `interaction/ask` hook prompt has no gateway session.
            sid: None,
            slug: if slug.is_empty() {
                None
            } else {
                Some(slug.to_string())
            },
        })
        .is_err()
    {
        // Sink closed: forget the pending so it can't leak.
        pending.lock().await.take_by_token(&token);
        return err_resp("interaction/ask: gateway sink closed".to_string());
    }

    // Block on the selection, holding NO lock. The daemon enforces the TTL.
    match tokio::time::timeout(ttl, rx).await {
        Ok(Ok(selection)) => {
            // Map the resolved real id(s) back to label(s) for the hook echo.
            let label = prompt
                .options
                .iter()
                .find(|o| selection.ids.first() == Some(&o.id))
                .map(|o| o.label.clone())
                .or_else(|| selection.ids.first().cloned())
                .or_else(|| selection.free_text.clone())
                .unwrap_or_default();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "answers": { question: label } },
            })
        }
        _ => {
            // Timeout or sender dropped — forget the pending (best-effort).
            pending.lock().await.take_by_token(&token);
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "timeout": true },
            })
        }
    }
}

/// v0.8.7 W2 (DB.3) — true when this line is the HITL `PermissionRequest`
/// hook's `permission/ask` RPC (raw JSON-RPC `method`, not a `tools/call`).
fn is_permission_ask_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("permission/ask")
}

/// In-place IM hot-reload — `ccteam config` sends `{"method":"ccteam/reload"}`
/// over the daemon's mcp.sock after persisting `credentials.json`. The handler
/// signals the gateway's daemon reload task to rebuild the credential-driven
/// IM channel listeners without restarting any agent session or the daemon.
fn is_reload_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("ccteam/reload")
}

/// v0.8.7 W2 (DB.3/DB.4) — handle one `permission/ask` request from a HITL
/// session's `PermissionRequest` hook. Builds a 2-option (Approve / Deny)
/// [`ChoicePrompt`], renders it to the bound IM chat as clickable buttons,
/// and BLOCKS (with a TTL, holding NO lock) on the user's click — the exact
/// blocking-External-pending mechanism as [`execute_interaction_ask`].
///
/// Request:  `{"jsonrpc":"2.0","id":N,"method":"permission/ask",
///             "params":{"slug","role","tool_name","tool_input",
///                       "session_id","cwd"}}`
/// Response: `{"result":{"behavior":"allow"|"deny"}}` on a click,
///           `{"result":{"timeout":true}}` on TTL lapse (hook → deny), or a
///           JSON-RPC `error` when addressing / wiring is unavailable (the
///           hook then fail-safe denies).
async fn execute_permission_ask(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    pending: Option<&PendingRegistry>,
    gateway: Option<&GatewayHandle>,
) -> serde_json::Value {
    use crate::gateway::{GatewayEvent, GatewayEventKind};
    use crate::pending::InteractionOrigin;
    use crate::transport::MessageOption;
    use ccteam_harness::{ChoiceOption, ChoicePrompt, ChoiceSelection};

    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let err_resp = |msg: String| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": { "code": -32000, "message": msg },
        })
    };

    let (Some(sink), Some(pending)) = (sink, pending) else {
        return err_resp("permission/ask: IM gateway not running".to_string());
    };
    let params = req
        .pointer("/params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let tool_name = params
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if tool_name.is_empty() {
        return err_resp("permission/ask: missing tool_name".to_string());
    }
    let tool_input = params
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // v0.8.8 F1 — the firing session's own ccteam sid (`s<N>`), reported by the
    // hook via `session_sid` (sourced from `CCTEAM_CHAT_SID` / `X-Ccteam-Sid`).
    // 红线:ccteam 的 `s<N>` sid,不是 Anthropic 的 `session_id` UUID。
    let session_sid = params
        .get("session_sid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    // Single source of truth for the approval-prompt destination: the firing
    // session's live reply target (its `owner` ChatKey, set at spawn, keyed by
    // sid via `reply_target_for`). Resolve it AND the canonical sid label in one
    // read-only gateway lock dropped before the long await (lock discipline
    // §7-1). NO registry fallback — the two-store addressing is gone; a missing
    // binding is a spawn/bind-flow defect, surfaced precisely.
    let (dest, sid_label) = match (gateway, session_sid.is_empty()) {
        (Some(gw), false) => {
            let guard = gw.lock().await;
            (
                guard.reply_target_for(session_sid),
                guard.session_sid_for(session_sid),
            )
        }
        _ => (None, None),
    };
    let Some((channel, chat_id)) = dest else {
        return err_resp(format!(
            "permission/ask: no IM chat bound to firing session sid={session_sid:?} ({slug}/{role}); owner unset at spawn/bind — not falling back to the registry"
        ));
    };
    let session_desc = match (&sid_label, role.is_empty()) {
        (Some(sid), false) => format!("session {sid} ({role})"),
        (Some(sid), true) => format!("session {sid}"),
        (None, false) => format!("session ({role})"),
        (None, true) => "session".to_string(),
    };

    let summary = summarize_tool_input(&tool_name, &tool_input);
    let risk = crate::hitl::classify_tool_risk(&tool_name, &tool_input);
    let title = format!(
        "{badge} {session_desc} wants to run: {summary}",
        badge = crate::hitl::risk_badge(risk),
    );

    // Mint a short token (≤16B ASCII, no `:` — the ChoicePrompt contract).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("p{:x}", (nanos as u64) & 0xff_ffff_ffff);

    // The option `id`s are the decision wire values the hook maps to a
    // PermissionRequest behavior; the labels are the human-clickable text.
    let prompt = ChoicePrompt {
        token: token.clone(),
        title: title.clone(),
        options: vec![
            ChoiceOption {
                id: "allow".to_string(),
                label: "✅ Approve".to_string(),
            },
            ChoiceOption {
                id: "deny".to_string(),
                label: "⛔ Deny".to_string(),
            },
        ],
        multi: false,
    };
    let message_options: Vec<MessageOption> = prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{token}:{i}"),
            label: opt.label.clone(),
            // v0.8.7 review-fix (R-H1) — carry the stable option id (e.g.
            // "allow"/"deny") so the web SSE consumer can resolve by
            // {token, selection=id} through the same pending machinery.
            id: opt.id.clone(),
        })
        .collect();

    // Register the External-origin pending (token-keyed); release the guard
    // BEFORE the long await (lock discipline §7-1).
    // v0.8.7 review-fix (R-L1) — a permission prompt blocks the whole turn, so
    // it uses a SHORTER TTL than the 600s interaction/ask; fail-safe stays deny.
    let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
    let ttl_secs = permission_prompt_timeout_secs();
    let ttl = std::time::Duration::from_secs(ttl_secs);
    {
        let mut guard = pending.lock().await;
        guard.register(
            token.clone(),
            prompt.clone(),
            InteractionOrigin::External { reply: tx },
            std::time::Instant::now() + ttl,
        );
        // v0.8.22 P1 (review §3.1-3) — tag this pending with its sid so a web
        // SSE reconnect (or a brand-new tab) can re-seed it, same as the
        // stream-json protocol's `ask_permission` does.
        if let Some(sid) = sid_label.as_deref() {
            guard.tag_sid(&token, sid.to_string());
        }
    }

    // Render the approve/deny buttons in IM.
    if sink
        .send(GatewayEvent {
            id: format!("permission-{token}"),
            channel,
            chat_id,
            thread_ts: None,
            content: title,
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: message_options,
            // sid set so a per-session web UI stream can show the approval
            // (None would route to IM fine but be filtered out of SSE).
            sid: sid_label.clone(),
            slug: if slug.is_empty() {
                None
            } else {
                Some(slug.to_string())
            },
        })
        .is_err()
    {
        pending.lock().await.take_by_token(&token);
        return err_resp("permission/ask: gateway sink closed".to_string());
    }

    // v0.8.7 review-fix (R-L1) — emit a progress.jsonl line so an operator
    // (`ccteam status` / dashboard / `progress`) sees the session is PARKED
    // awaiting approval, not silently stuck. Best-effort: a write failure must
    // never block the approval flow. progress.jsonl stays the state SoT (we
    // only append; nothing here mutates session state).
    emit_permission_prompt_outstanding(slug, role, &tool_name, &summary, ttl_secs);

    // Block on the click, holding NO lock. The daemon enforces the TTL; on
    // lapse the hook degrades to deny.
    match tokio::time::timeout(ttl, rx).await {
        Ok(Ok(selection)) => {
            // The resolved id IS the decision ("allow" / "deny").
            let behavior = match selection.ids.first().map(String::as_str) {
                Some("allow") => "allow",
                _ => "deny",
            };
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "behavior": behavior },
            })
        }
        _ => {
            pending.lock().await.take_by_token(&token);
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "timeout": true },
            })
        }
    }
}

/// v0.8.7 review-fix (R-L1) — append a `chat_permission_prompt_outstanding`
/// line to the project's `progress.jsonl` so an operator sees the session is
/// parked awaiting approval. Best-effort: resolve the path from env (the
/// daemon process has `CCTEAM_HOME`); any failure (no env, write error) is
/// logged and swallowed — the approval flow must never depend on this signal.
/// progress.jsonl stays the state SoT (append-only; nothing here mutates
/// session state). A blank `slug` (the hook didn't pass one) is skipped.
fn emit_permission_prompt_outstanding(
    slug: &str,
    role: &str,
    tool_name: &str,
    summary: &str,
    ttl_secs: u64,
) {
    if slug.is_empty() {
        return;
    }
    let Ok(paths) = ccteam_core::CcteamPaths::from_env() else {
        return;
    };
    let event = ccteam_core::progress::build_chat_permission_prompt_outstanding_event(
        role, tool_name, summary, ttl_secs,
    );
    if let Err(e) = ccteam_core::progress::append_event(&paths.progress_jsonl(slug), &event) {
        tracing::warn!(%slug, %role, %e, "failed to append permission-prompt-outstanding progress line");
    }
}

/// v0.8.7 W2 (DB.4) — render a short, human-readable one-liner of a tool
/// call for the approval prompt. Picks the most useful field per common tool
/// (`Bash` → command, file tools → path) and truncates so the IM message
/// stays compact. Falls back to the tool name when no obvious field exists.
///
/// v0.8.22 P0-2 — delegates to [`crate::hitl::summarize_tool_input`] (the
/// SAME renderer the stream-json protocol's in-process HITL resolver uses),
/// so an approval prompt reads identically regardless of which protocol
/// produced it.
fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String {
    crate::hitl::summarize_tool_input(tool_name, tool_input)
}

// =====================================================================
// v0.9.0 W1 (F1) — session scheduling: daemon-side `session_*` tool handlers.
//
// The stdio MCP server (or HTTP `/mcp` session bearer) forwards
// `session_*` calls here (it doesn't own the gateway). This is where
// we (a) authenticate the caller by its `(sid, secret)` PRINCIPAL — any live
// session that holds the secret, role-agnostic; the retired cto-only gate is
// gone — and (b) drive the gateway session map (spawn / dispatch / list /
// stop) or tail a child's transcript (collect). The project scope is the
// SERVER's view of the caller's session (`CallerCtx.slug`), never the
// caller-supplied `_caller_slug`.
//
// Lock discipline (CLAUDE.md §6): spawn/dispatch/list/stop call the
// gateway's own async methods, so we hold the gateway lock across their
// `.await` (the gateway IS the lock target — the same pattern ccteam-web's
// AppState uses over HTTP). `collect` only needs a synchronous
// `session_resolve`, so we copy out the role + project_dir and DROP the
// guard BEFORE the (blocking) `read_all_turns` fs read.
// =====================================================================

/// True for a `tools/call` whose tool name is in the `session_` group.
fn is_session_tool_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req
            .pointer("/params/name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n.starts_with("session_"))
}

/// Build a tools/call-shaped JSON-RPC response carrying one text block.
fn session_tool_response(id: serde_json::Value, text: String, is_error: bool) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        },
    })
}

/// v0.9.0 W1 (F1) — handle one forwarded `session_*` call. Authenticates
/// the caller by its `(sid, secret)` PRINCIPAL (Ambient path), then dispatches
/// to the gateway. Returns a JSON-RPC response (the caller side propagates
/// `isError` to the agent).
async fn execute_session_tool(
    req: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
    caller: McpCaller,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let name = req
        .pointer("/params/name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let mut args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // The gateway must be running (web + IM both up). Mirror chat_send_file's
    // "gateway not running" structured error rather than panicking. It is also
    // REQUIRED to authenticate the caller (the secret map lives there), so a
    // missing gateway is a HARD STOP for every session_* call — fail-closed,
    // never a fall-through that would skip the principal check.
    let Some(gateway) = gateway else {
        return session_tool_response(
            id,
            format!("{name}: gateway not running (start ccteam with web + IM enabled)"),
            true,
        );
    };

    // Authenticate the Ambient caller by its `(sid, secret)` PRINCIPAL and
    // resolve its CallerCtx (server-side sid + slug + role). This is the sole
    // security-relevant check (best-effort defense-in-depth; single-uid honest
    // scope in `verify_session_principal`). Role plays NO part — the retired
    // cto-only pre-filter is gone. A missing/wrong secret or unknown sid fails
    // closed. We then OVERWRITE the identity args from CallerCtx so nothing
    // downstream trusts a caller-supplied `_caller_slug`/`_caller_sid`/role.
    //
    // `McpCaller::Admin` (HTTP `/mcp`, admin bearer already verified at the
    // transport layer) skips the principal gate: it names its target with an
    // explicit `project` arg (fleet-wide, same as the web admin Identity).
    if caller == McpCaller::Ambient {
        let caller_sid = args
            .get("_caller_sid")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let caller_secret = args
            .get("_caller_secret")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ctx = {
            let gw = gateway.lock().await;
            gw.verify_session_principal(caller_sid, caller_secret)
        };
        let Some(ctx) = ctx else {
            return session_tool_response(
                id,
                format!(
                    "{name}: permission denied — caller could not be authenticated (no live session holds the presented (sid, secret) principal)"
                ),
                true,
            );
        };
        if let Some(obj) = args.as_object_mut() {
            obj.insert("_caller_slug".to_string(), serde_json::json!(ctx.slug));
            obj.insert("_caller_sid".to_string(), serde_json::json!(ctx.sid));
            obj.insert("_caller_role".to_string(), serde_json::json!(ctx.role));
            // v0.9.0 W2 (F2/F5) — the caller's delegation depth (server-resolved
            // from CallerCtx, never caller-supplied): a child's depth = this + 1,
            // the input to the `delegation.max_depth` guardrail.
            obj.insert("_caller_depth".to_string(), serde_json::json!(ctx.depth));
        }
    }

    match run_session_tool(&name, &args, gateway, caller).await {
        Ok(text) => session_tool_response(id, text, false),
        Err(text) => session_tool_response(id, text, true),
    }
}

/// Dispatch a privileged `session_*` call to the gateway. Returns `Ok(body)`
/// (a pretty JSON string) on success, `Err(msg)` on a tool-level error.
async fn run_session_tool(
    name: &str,
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    match name {
        "session_spawn" => run_session_spawn(args, gateway, caller).await,
        "session_dispatch" => run_session_dispatch(args, gateway, caller).await,
        "session_collect" => run_session_collect(args, gateway, caller).await,
        "session_list" => run_session_list(gateway).await,
        "session_stop" => run_session_stop(args, gateway, caller).await,
        other => Err(format!("unknown session tool: {other}")),
    }
}

/// `session_spawn` — create a session in the caller's own project and return
/// its `s{n}` id + vendor resume key + host. v0.9.0 W1 (F1/G1): the caller is
/// authenticated by its `(sid, secret)` PRINCIPAL (see [`execute_session_tool`]),
/// so `_caller_slug` here is the SERVER's view of the caller's project — an
/// Ambient caller can only spawn into that project. Admin (HTTP front door)
/// names the target with an explicit `project` (fleet-wide).
///
/// Facets mirror the REST `CreateSessionForm`: `{role?, vendor?, model?,
/// effort?, protocol?, host?, permission_mode?, title?}`. `role` empty/absent =
/// roleless (bare vendor reads the project CLAUDE.md/AGENTS.md). `title` is
/// metadata/ledger only — NEVER concatenated into any prompt.
async fn run_session_spawn(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    // Roleless is a first-class form; absent or "" both mean roleless.
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // Project scope: Ambient = the caller's server-resolved slug (overwritten
    // in `execute_session_tool` from CallerCtx — never caller-supplied); Admin
    // = explicit `project` (fleet-wide).
    let project = match caller {
        McpCaller::Ambient => args
            .get("_caller_slug")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .ok_or_else(|| "session_spawn: no project (caller slug unset)".to_string())?,
        McpCaller::Admin => args
            .get("project")
            .or_else(|| args.get("_caller_slug"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .ok_or_else(|| {
                "session_spawn: missing `project` — pass the target project slug explicitly, \
                 or run from inside a registered project directory (cwd is resolved for local \
                 main-session callers)"
                    .to_string()
            })?,
    };
    let vendor = parse_session_vendor(args)?;
    // Optional `permission_mode` (`skip` default / `hitl`).
    let permission_mode = ccteam_harness::PermissionMode::parse_opt(
        args.get("permission_mode").and_then(|v| v.as_str()),
    )
    .map_err(|e| format!("session_spawn: {e}"))?;
    // Protocol: agents get `stream-json | acp` only — `terminal` is frozen and
    // never exposed to agents. Grok/opencode are always ACP (honest meta).
    let protocol = parse_session_protocol(args)?;
    let protocol = if matches!(
        vendor,
        ccteam_harness::AgentVendor::Grok | ccteam_harness::AgentVendor::Opencode
    ) {
        ccteam_harness::SessionProtocol::Acp
    } else {
        protocol
    };
    let host = args
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("local")
        .to_string();
    // Optional model/effort (composer facets). Grok effort is dropped (its
    // value set is undocumented — an invalid value would fail the spawn),
    // mirroring the REST `spawn_tuning_from_form` contract.
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let effort = args
        .get("effort")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let tuning = crate::gateway::SpawnTuning {
        model,
        effort: if vendor == ccteam_harness::AgentVendor::Grok {
            None
        } else {
            effort
        },
    };
    // Optional `title` — metadata/ledger only, NEVER concatenated into any
    // prompt. Validate ≤80 chars; W1 accepts + echoes it (meta persistence
    // lands with the W2 delegation ledger).
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if let Some(t) = &title {
        let n = t.chars().count();
        if n > 80 {
            return Err(format!(
                "session_spawn: `title` too long ({n} chars; max 80)"
            ));
        }
    }
    // v0.9.1 delegation-ergonomics — optional FIRST task: spawn+dispatch in one
    // call (the dominant flow; saves the second round-trip and closes the
    // crash window between a spawn and its first dispatch). Identical
    // semantics to session_dispatch{sid, task}: async by default with a
    // completion notification; `wait_seconds` blocks inline; `notify:false`
    // opts out. Cycle checks are moot for a fresh child; the spawn guardrails
    // (depth/children/delegated/budget) below already gate the delegation.
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let wait_seconds = args
        .get("wait_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(600);
    let notify = args.get("notify").and_then(|v| v.as_bool()).unwrap_or(true);
    // Owner = the shared ops pool (`web-api`): MCP-spawned children stay visible
    // to the owner's console + IM. Owner is NOT inherited from the caller — the
    // parent link is a `meta.parent_sid` property (v0.9.0 W2), not an owner change.
    let owner_id = "web-api".to_string();
    // v0.9.0 W2 (F7) — optional idempotency key: a client retry with the same
    // key replays the original spawn (same sid) with zero side effects.
    let idem_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    // v0.9.0 W2 (F2/F5) — the delegation parent. Ambient = the caller's
    // server-resolved principal (sid/depth/role, injected in `execute_session_tool`
    // from CallerCtx — never caller-supplied). Admin (HTTP front door) = a
    // human/root spawn (no parent, unrestricted). Guardrails apply only when a
    // real parent is present.
    let parent = match caller {
        McpCaller::Ambient => {
            let caller_sid = args
                .get("_caller_sid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if caller_sid.is_empty() {
                None
            } else {
                Some(crate::gateway::DelegationParent {
                    sid: caller_sid,
                    depth: args
                        .get("_caller_depth")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    role: args
                        .get("_caller_role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
            }
        }
        McpCaller::Admin => None,
    };
    // The dispatcher identity for an optional first `task` (captured before
    // `parent` moves into the create call).
    let parent_sid_for_task = parent.as_ref().map(|p| p.sid.clone());

    // Check idempotency + create under ONE lock so a concurrent same-key retry
    // can never race past the replay into a second spawn.
    let (sid, model_warning, resolved, replay) = {
        let mut gw = gateway.lock().await;
        if let Some(key) = idem_key.as_deref() {
            if let Some(body) = gw.spawn_idem_replay(&project, key) {
                (String::new(), None, None, Some(body))
            } else {
                let created = gw
                    .create_delegated_session(
                        project.clone(),
                        role.clone(),
                        vendor,
                        permission_mode,
                        protocol,
                        owner_id,
                        host.clone(),
                        tuning,
                        parent,
                        title.clone(),
                    )
                    .await
                    .map_err(|e| format!("session_spawn: {e}"))?;
                let sid = created.sid.clone();
                let resolved = gw.session_resolve(&sid);
                (sid, created.model_warning, resolved, None)
            }
        } else {
            let created = gw
                .create_delegated_session(
                    project.clone(),
                    role.clone(),
                    vendor,
                    permission_mode,
                    protocol,
                    owner_id,
                    host.clone(),
                    tuning,
                    parent,
                    title.clone(),
                )
                .await
                .map_err(|e| format!("session_spawn: {e}"))?;
            let sid = created.sid.clone();
            let resolved = gw.session_resolve(&sid);
            (sid, created.model_warning, resolved, None)
        }
    };
    // Idempotent replay: return the ORIGINAL body verbatim (+ a replay flag).
    if let Some(body) = replay {
        return Ok(mark_idempotent_replay(&body));
    }
    // Read the child meta once for the vendor resume key + the delegation
    // lineage (parent_sid/depth) the ledger just persisted.
    // vendor_session_id = the vendor's native resume key (`meta.vendor_uuid`).
    // May be empty for some vendors at spawn time — return "" honestly (the
    // codex-plugin-cc lesson: always surface the resume key when we have it).
    let child_meta = resolved.and_then(|r| {
        ccteam_harness::execution::session_meta::read_session_meta(&r.project_dir, &sid).ok()
    });
    let vendor_session_id = child_meta
        .as_ref()
        .map(|m| m.vendor_uuid.clone())
        .unwrap_or_default();
    let parent_sid = child_meta.as_ref().and_then(|m| m.parent_sid.clone());
    let delegation_depth = child_meta.as_ref().map(|m| m.delegation_depth).unwrap_or(0);

    let mut body = serde_json::json!({
        "ok": true,
        "sid": sid,
        "project": project,
        "role": role,
        "vendor": session_vendor_wire(vendor),
        "protocol": protocol.as_str(),
        "host": host,
        "vendor_session_id": vendor_session_id,
        "permission_mode": permission_mode.as_str(),
        "parent_sid": parent_sid,
        "delegation_depth": delegation_depth,
        "hint": "dispatch a task with session_dispatch{sid, task}, then read the result with session_collect{sid}.",
    });
    if let Some(t) = &title {
        body["title"] = serde_json::json!(t);
    }
    if let Some(model_warning) = model_warning {
        body["model_warning"] = serde_json::json!(model_warning);
    }
    // v0.9.1 — dispatch the optional first task through the SAME submit path
    // session_dispatch uses; its outcome (turn_id / status / inline result /
    // hint) merges into the spawn body so one call returns everything. The
    // caller's parent link doubles as the dispatcher identity (empty = admin,
    // ledger-only submit without a watch).
    if let Some(task) = task {
        let dispatcher_sid = parent_sid_for_task.as_deref().unwrap_or("");
        let frag = dispatch_task(
            gateway,
            "session_spawn",
            dispatcher_sid,
            &sid,
            task,
            wait_seconds,
            notify,
            title.clone(),
        )
        .await?;
        if let Some(obj) = body.as_object_mut() {
            obj.remove("hint");
            obj.extend(frag);
        }
    }
    let out = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string());
    // v0.9.0 W2 (F7) — record for idempotent replay (the exact body a retry
    // returns, with a replay flag added). Keyed per-project by the client key.
    if let Some(key) = idem_key.as_deref() {
        gateway.lock().await.spawn_idem_record(&project, key, &out);
    }
    Ok(out)
}

/// v0.9.0 W2 (F7) — mark a recorded idempotency body as a replay: parse it,
/// insert `"idempotent_replay": true`, re-serialize. On a parse miss (should
/// never happen — we only store our own bodies) return the stored body as-is.
fn mark_idempotent_replay(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("idempotent_replay".to_string(), serde_json::json!(true));
            }
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string())
        }
        Err(_) => body.to_string(),
    }
}

/// Parse the optional `protocol` arg for `session_spawn`. Agents may select
/// `stream-json` (default) or `acp` only — `terminal` (tmux/PTY) is frozen and
/// NEVER exposed to agents (an explicit reject keeps the red line legible even
/// if a caller bypasses the schema enum).
fn parse_session_protocol(
    args: &serde_json::Value,
) -> std::result::Result<ccteam_harness::SessionProtocol, String> {
    let raw = args.get("protocol").and_then(|v| v.as_str());
    if let Some(r) = raw {
        let low = r.trim().to_ascii_lowercase();
        if low == "terminal" || low == "tmux" {
            return Err(
                "session_spawn: protocol `terminal` is not available to agents (use `stream-json` or `acp`)"
                    .to_string(),
            );
        }
    }
    ccteam_harness::SessionProtocol::parse_opt(raw).map_err(|e| format!("session_spawn: {e}"))
}

/// Lowercase wire string for a spawned session's vendor (response field).
fn session_vendor_wire(v: ccteam_harness::AgentVendor) -> &'static str {
    match v {
        ccteam_harness::AgentVendor::Claude => "claude",
        ccteam_harness::AgentVendor::Codex => "codex",
        ccteam_harness::AgentVendor::Grok => "grok",
        ccteam_harness::AgentVendor::Opencode => "opencode",
    }
}

/// `session_dispatch` — forward a task as a user turn to a session by sid.
/// v0.9.0 W2 (F2/F5/F7): an Ambient (agent) dispatch (a) rejects a cycle
/// (target == caller or an ancestor), (b) arms a durable completion watch on
/// the child (parent = the dispatcher) so its next turn notifies the parent,
/// (c) emits `delegation_dispatched`, and (d) optionally blocks up to
/// `wait_seconds` for the child's answer inline. `idempotency_key` makes a
/// client retry replay the original turn (never double-dispatch). `title` is
/// ledger/notification only — NEVER concatenated into the task.
async fn run_session_dispatch(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "session_dispatch: missing `task`".to_string())?
        .to_string();
    // R-M3 — only operate sessions in the caller's own project.
    assert_caller_owns_session("session_dispatch", args, gateway, &sid, caller).await?;

    let wait_seconds = args
        .get("wait_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(600);
    let notify = args.get("notify").and_then(|v| v.as_bool()).unwrap_or(true);
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if let Some(t) = &title {
        let n = t.chars().count();
        if n > 80 {
            return Err(format!(
                "session_dispatch: `title` too long ({n} chars; max 80)"
            ));
        }
    }
    let idem_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    // The dispatcher's server-resolved principal (Ambient only; injected in
    // `execute_session_tool`). A delegation is armed only for an agent caller.
    let caller_sid = match caller {
        McpCaller::Ambient => args
            .get("_caller_sid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        McpCaller::Admin => String::new(),
    };
    let caller_slug = args
        .get("_caller_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let is_delegation = !caller_sid.is_empty();

    // ---- Scope 1: idempotent replay + cycle guard (fast, no submit) ----
    {
        let mut gw = gateway.lock().await;
        if let Some(key) = idem_key.as_deref() {
            if let Some(body) = gw.dispatch_idem_replay(&sid, key) {
                return Ok(mark_idempotent_replay(&body));
            }
        }
        if is_delegation {
            let emit_cycle = |gw: &crate::gateway::Gateway| {
                if let Some((vendor, host, _)) = gw.session_vendor_host_slug(&sid) {
                    gw.emit_delegation_progress(
                        &caller_slug,
                        ccteam_harness::execution::progress_bridge::DELEGATION_DENIED,
                        &caller_sid,
                        &sid,
                        vendor,
                        &host,
                        None,
                        title.as_deref(),
                        Some("cycle"),
                    );
                }
            };
            if sid == caller_sid {
                emit_cycle(&gw);
                return Err(
                    "session_dispatch: delegation denied: cannot dispatch a session to itself (cycle)"
                        .to_string(),
                );
            }
            if gw.ancestor_chain(&caller_sid).contains(&sid) {
                emit_cycle(&gw);
                return Err(format!(
                    "session_dispatch: delegation denied: target {sid} is an ancestor of the caller {caller_sid} (cycle)"
                ));
            }
            // Budget gate: the CHILD's vendor accrues the cost of the task.
            if let Some((vendor, host, slug)) = gw.session_vendor_host_slug(&sid) {
                if gw.delegation_budget_exceeded(&slug, vendor) {
                    gw.emit_delegation_progress(
                        &slug,
                        ccteam_harness::execution::progress_bridge::DELEGATION_DENIED,
                        &caller_sid,
                        &sid,
                        vendor,
                        &host,
                        None,
                        title.as_deref(),
                        Some("budget"),
                    );
                    return Err(format!(
                        "session_dispatch: delegation denied: vendor `{}` has reached its 24h budget for project `{slug}` (adjust budgets or wait for the window to slide)",
                        crate::delegation::vendor_key(vendor)
                    ));
                }
            }
        }
    }

    // ---- Scope 2 + wait: the shared submit half (also used by spawn{task}) ----
    let frag = dispatch_task(
        gateway,
        "session_dispatch",
        &caller_sid,
        &sid,
        task,
        wait_seconds,
        notify,
        title,
    )
    .await?;
    let mut body = serde_json::json!({ "ok": true, "sid": sid });
    if let Some(obj) = body.as_object_mut() {
        obj.extend(frag);
    }
    let out = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string());
    // v0.9.0 W2 (F7) — record for idempotent replay.
    if let Some(key) = idem_key.as_deref() {
        gateway.lock().await.dispatch_idem_record(&sid, key, &out);
    }
    Ok(out)
}

/// v0.9.1 delegation-ergonomics — the shared submit half of a dispatch, used
/// by BOTH `session_dispatch` and `session_spawn{task}` (one-call
/// spawn+dispatch, the dominant delegation flow). Subscribe (if waiting) →
/// submit the task as a verbatim user turn → arm the delegation watch (agent
/// callers only; `caller_sid` empty = admin, no watch) → emit
/// `delegation_dispatched` → optionally block inline for the child's answer.
/// Returns the response FRAGMENT (`turn_id`/`status`/result fields/`hint`)
/// the caller merges into its own body; `tool` prefixes error strings.
#[allow(clippy::too_many_arguments)]
async fn dispatch_task(
    gateway: &GatewayHandle,
    tool: &str,
    caller_sid: &str,
    sid: &str,
    task: String,
    wait_seconds: u64,
    notify: bool,
    title: Option<String>,
) -> std::result::Result<serde_json::Map<String, serde_json::Value>, String> {
    let is_delegation = !caller_sid.is_empty();
    let (turn_id, rx) = {
        let mut gw = gateway.lock().await;
        // Subscribe BEFORE submitting so a fast child can't answer before we
        // start listening (the wait races the child's own turn).
        let rx = if wait_seconds > 0 {
            Some(gw.subscribe_events())
        } else {
            None
        };
        let turn_id = gw
            .submit_to_sid(sid, task)
            .await
            .map_err(|e| format!("{tool} failed: {e}"))?;
        if is_delegation {
            gw.arm_delegation_watch(
                sid,
                caller_sid,
                notify,
                title.clone(),
                Some(turn_id.clone()),
            );
            if let Some((vendor, host, slug)) = gw.session_vendor_host_slug(sid) {
                gw.emit_delegation_progress(
                    &slug,
                    ccteam_harness::execution::progress_bridge::DELEGATION_DISPATCHED,
                    caller_sid,
                    sid,
                    vendor,
                    &host,
                    Some(&turn_id),
                    title.as_deref(),
                    None,
                );
            }
        }
        (turn_id, rx)
    };

    // ---- wait branch (OFF the gateway lock) ----
    if let Some(rx) = rx {
        Ok(
            dispatch_wait_for_completion(gateway, sid, &turn_id, wait_seconds, rx, is_delegation)
                .await,
        )
    } else {
        let mut m = serde_json::Map::new();
        m.insert("turn_id".to_string(), serde_json::json!(turn_id));
        m.insert("status".to_string(), serde_json::json!("dispatched"));
        m.insert(
            "hint".to_string(),
            serde_json::json!(
                "the child runs asynchronously; you will be notified on completion (or poll session_collect{sid})."
            ),
        );
        Ok(m)
    }
}

/// v0.9.0 W2 (F2) — the OFF-lock half of a `wait_seconds>0` dispatch. Awaits an
/// `Answer` for `child_sid` on the gateway broadcast until the deadline. NEVER
/// holds the gateway lock across the await (lock discipline). On completion it
/// reads the child's freshly-appended turn (clean text) + cost from meta and,
/// for a delegation, disarms the watch (the caller already has the result
/// inline — suppress the redundant notification). On timeout it returns
/// `pending` and leaves the watch armed (the child is not cancelled).
async fn dispatch_wait_for_completion(
    gateway: &GatewayHandle,
    child_sid: &str,
    turn_id: &str,
    wait_seconds: u64,
    mut rx: tokio::sync::broadcast::Receiver<crate::gateway::GatewayEvent>,
    is_delegation: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait_seconds);
    let completed = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break false;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                let hit = ev.sid.as_deref() == Some(child_sid)
                    && matches!(ev.kind, crate::gateway::GatewayEventKind::Answer);
                if hit {
                    break true;
                }
            }
            // Broadcast lag → keep waiting (we may have missed unrelated frames).
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            // Sender gone (daemon shutdown) or deadline → pending.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break false,
            Err(_) => break false,
        }
    };

    if !completed {
        let mut m = serde_json::Map::new();
        m.insert("turn_id".to_string(), serde_json::json!(turn_id));
        m.insert("status".to_string(), serde_json::json!("pending"));
        m.insert(
            "hint".to_string(),
            serde_json::json!(
                "still running; you will be notified on completion, or poll session_collect{sid}."
            ),
        );
        return m;
    }

    // Resolve the child (sync) under a brief lock, then read its transcript
    // tail OFF the lock for a clean, unprefixed result.
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve(child_sid)
    };
    let (result_text, result_turn, cost_usd) = resolved
        .as_ref()
        .map(|r| {
            let last =
                ccteam_harness::execution::turns_mirror::read_all_turns(&r.project_dir, &r.sid)
                    .ok()
                    .and_then(|all| all.into_iter().rev().find(|t| !t.assistant.is_empty()));
            let cost =
                ccteam_harness::execution::session_meta::read_session_meta(&r.project_dir, &r.sid)
                    .ok()
                    .and_then(|m| m.cost_usd);
            match last {
                Some(t) => (t.assistant, Some(t.turn_id), cost),
                None => (String::new(), None, cost),
            }
        })
        .unwrap_or((String::new(), None, None));

    // Inline completion: the caller already holds the result → disarm the watch
    // so a delegation doesn't ALSO wake the parent with a redundant turn.
    if is_delegation {
        gateway.lock().await.disarm_delegation_watch(child_sid);
    }

    let mut m = serde_json::Map::new();
    m.insert("turn_id".to_string(), serde_json::json!(turn_id));
    m.insert("status".to_string(), serde_json::json!("completed"));
    m.insert("result_text".to_string(), serde_json::json!(result_text));
    m.insert("result_turn".to_string(), serde_json::json!(result_turn));
    if let Some(c) = cost_usd {
        m.insert("cost_usd".to_string(), serde_json::json!(c));
    }
    m
}

/// v0.8.7 review-fix (R-L3) — pure paging core of [`run_session_collect`],
/// extracted so the cursor/truncation contract is unit-testable without a
/// gateway or filesystem. Given ALL mirrored turns, an optional `since`
/// turn-id cursor, and a page size `n`, returns `(rows, next_cursor,
/// truncated)`:
///
/// - keeps only assistant-side turns AFTER `since` (or all when `since` is
///   `None` / not found — never silently lose turns on a stale cursor),
/// - returns the OLDEST `n` of those (so repeated polls page forward in order),
/// - `truncated` is true when more than `n` were available → the caller polls
///   again with `next_cursor` to fetch the remainder (the old code kept the
///   NEWEST `n` and dropped the middle of a > `n` burst).
fn page_collected_turns(
    all: &[ccteam_harness::execution::turns_mirror::TurnRecord],
    since: Option<&str>,
    n: usize,
    tail: bool,
) -> (Vec<serde_json::Value>, Option<String>, bool) {
    let after: Vec<&ccteam_harness::execution::turns_mirror::TurnRecord> = match since {
        Some(cursor) => match all.iter().position(|t| t.turn_id == cursor) {
            Some(idx) => all.iter().skip(idx + 1).collect(),
            // Cursor not found (rotated / typo) → return everything so the
            // caller never silently loses turns.
            None => all.iter().collect(),
        },
        None => all.iter().collect(),
    };
    let mut rows: Vec<serde_json::Value> = after
        .iter()
        .filter(|t| !t.assistant.is_empty())
        .map(|t| {
            serde_json::json!({
                "turn_id": t.turn_id,
                "ts": t.ts.to_rfc3339(),
                "content": t.assistant,
            })
        })
        .collect();
    let truncated = rows.len() > n;
    if tail {
        // v0.9.1 — the "final answer" shape: keep the NEWEST n (chronological
        // order preserved inside the page).
        let drop = rows.len().saturating_sub(n);
        rows.drain(..drop);
    } else {
        rows.truncate(n);
    }
    let last_turn_id = rows
        .last()
        .and_then(|r| r.get("turn_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    (rows, last_turn_id, truncated)
}

/// v0.9.1 — honest per-sid activity for the MCP surfaces: classify the
/// session's progress stream the SAME way the web session list does
/// (`ccteam_core::stall`, → `working|idle|stale|stuck`), so a polling parent
/// can tell "child still thinking" from "turn done" without scraping
/// anything. Best-effort: any read miss degrades to `None` (field omitted).
fn classify_session_activity(slug: &str, sid: &str) -> Option<String> {
    let paths = ccteam_core::CcteamPaths::from_env().ok()?;
    let silent_seconds = ccteam_core::collect_projects(&paths)
        .ok()?
        .into_iter()
        .find(|p| p.state.slug == slug)
        .map(|p| p.stall_silent_seconds)?;
    let events =
        ccteam_core::progress::read_all_events(&paths.progress_jsonl(slug)).unwrap_or_default();
    let activity = ccteam_core::stall::classify_progress_activity_for_sid(
        &events,
        sid,
        silent_seconds,
        chrono::Utc::now(),
    );
    Some(activity.status.activity.to_string())
}

/// `session_collect` — tail the child's `turns.jsonl` (assistant turns).
/// Polled MVP: resolve sid → role + project_dir under the lock, drop the
/// guard, then read the ccteam-owned mirror. `since` is a turn_id cursor.
async fn run_session_collect(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    let since = args.get("since").and_then(|v| v.as_str()).map(String::from);
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|x| x as usize)
        .unwrap_or(SESSION_COLLECT_DEFAULT_N);
    let tail = args.get("tail").and_then(|v| v.as_bool()).unwrap_or(false);
    // R-M3 — only collect from sessions in the caller's own project.
    assert_caller_owns_session("session_collect", args, gateway, &sid, caller).await?;

    // Resolve under the lock (sync), then DROP the guard before the fs read.
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve(&sid)
    };
    let resolved = resolved.ok_or_else(|| format!("session_collect: unknown session: {sid}"))?;
    // A collectable session is one the gateway still tracks → "live" (the same
    // cheap liveness hint `session_list` reports; a fully-stopped session is no
    // longer resolvable so it errors above).
    let status = "live";

    // Tail the ccteam-owned transcript mirror.
    // v0.8.8 F1 — the mirror is keyed by `sid` (`.ccteam/chat/<sid>/turns.jsonl`),
    // not role, so read by `resolved.sid` (role is a content label only).
    let all = ccteam_harness::execution::turns_mirror::read_all_turns(
        &resolved.project_dir,
        &resolved.sid,
    )
    .map_err(|e| format!("session_collect: read turns.jsonl for {sid}: {e}"))?;

    // v0.9.0 W2 (F2) — surface the vendor resume key + accrued cost from meta.
    let meta = ccteam_harness::execution::session_meta::read_session_meta(
        &resolved.project_dir,
        &resolved.sid,
    )
    .ok();
    let vendor_session_id = meta
        .as_ref()
        .map(|m| m.vendor_uuid.clone())
        .unwrap_or_default();
    let cost_usd = meta.as_ref().and_then(|m| m.cost_usd);

    // Apply the `since` cursor + page forward (R-L3 — oldest-first, no silent
    // drop of a > `n` burst; `tail:true` flips to newest-first). Pure logic in
    // `page_collected_turns`.
    let (rows, last_turn_id, truncated) = page_collected_turns(&all, since.as_deref(), n, tail);

    let mut body = serde_json::json!({
        "ok": true,
        "sid": sid,
        "role": resolved.role,
        "vendor_session_id": vendor_session_id,
        "status": status,
        "turns": rows,
        // Cursor to pass as `since` on the next poll (None when no turns yet).
        // On truncation this is the boundary turn → poll again to get the rest.
        "cursor": last_turn_id,
        // True when more turns than `n` were available after `since`; the caller
        // should poll again with `cursor` to page through the remainder.
        "truncated": truncated,
    });
    if let Some(c) = cost_usd {
        body["cost_usd"] = serde_json::json!(c);
    }
    // v0.9.1 — honest per-sid activity (same classifier the web session list
    // uses): `working` = the child is mid-turn (keep polling), `idle` = the
    // turn is done. Best-effort: a read miss just omits the field.
    if let Some(activity) = classify_session_activity(&resolved.project, &resolved.sid) {
        body["activity"] = serde_json::json!(activity);
    }
    // v0.9.0 W2 (F2) — a real collection by an agent is a ledger point.
    if caller == McpCaller::Ambient && !rows.is_empty() {
        if let (Some(m), Some(caller_sid)) = (
            meta.as_ref(),
            args.get("_caller_sid")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty()),
        ) {
            let gw = gateway.lock().await;
            gw.emit_delegation_progress(
                &resolved.project,
                ccteam_harness::execution::progress_bridge::DELEGATION_COLLECTED,
                caller_sid,
                &resolved.sid,
                m.vendor,
                &m.host,
                last_turn_id.as_deref(),
                None,
                None,
            );
        }
    }
    Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string()))
}

/// `session_list` — snapshot the gateway's live sessions.
async fn run_session_list(gateway: &GatewayHandle) -> std::result::Result<String, String> {
    let views = {
        let gw = gateway.lock().await;
        gw.session_views()
    };
    // v0.9.1 — honest activity per row (same classifier as the web session
    // list): one progress read per DISTINCT project, not per session.
    let mut activity_ctx: std::collections::HashMap<String, (Vec<serde_json::Value>, u64)> =
        std::collections::HashMap::new();
    if let Ok(paths) = ccteam_core::CcteamPaths::from_env() {
        if let Ok(projects) = ccteam_core::collect_projects(&paths) {
            for p in projects {
                if views.iter().any(|v| v.project == p.state.slug) {
                    let events = ccteam_core::progress::read_all_events(
                        &paths.progress_jsonl(&p.state.slug),
                    )
                    .unwrap_or_default();
                    activity_ctx.insert(p.state.slug.clone(), (events, p.stall_silent_seconds));
                }
            }
        }
    }
    let now = chrono::Utc::now();
    let rows: Vec<serde_json::Value> = views
        .iter()
        .map(|v| {
            let activity = activity_ctx.get(&v.project).map(|(events, silent)| {
                ccteam_core::stall::classify_progress_activity_for_sid(events, &v.sid, *silent, now)
                    .status
                    .activity
                    .to_string()
            });
            serde_json::json!({
                "sid": v.sid,
                "project": v.project,
                "role": v.role,
                "vendor": v.vendor,
                "current": v.current,
                "status": v.status,
                // v0.9.1 — the honest busy signal (`working|idle|stale|stuck`)
                // + the hitl blocked-on-human flag.
                "activity": activity,
                "waiting_approval": v.waiting_approval,
                // v0.9.0 W2 (F2) — delegation topology + attribution.
                "parent_sid": v.parent_sid,
                "delegation_depth": v.delegation_depth,
                "host": v.host,
                "cost_usd": v.cost_usd,
                "title": v.title,
            })
        })
        .collect();
    // v0.9.0 W2 (F2) — a `tree` view (roots → children by `parent_sid`) so a
    // caller sees the delegation topology without recomputing it. Roots =
    // sessions whose parent isn't in this list (a true root, or a parent in
    // another project the caller can't see).
    let sids: std::collections::HashSet<&str> = views.iter().map(|v| v.sid.as_str()).collect();
    let tree: Vec<serde_json::Value> = views
        .iter()
        .filter(|v| {
            v.parent_sid
                .as_deref()
                .map(|p| !sids.contains(p))
                .unwrap_or(true)
        })
        .map(|v| session_tree_node(v, &views))
        .collect();
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "sessions": rows,
        "tree": tree,
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

/// v0.9.0 W2 (F2) — build one node of the `session_list` delegation tree:
/// `{sid, role, vendor, children:[...]}` recursively (children = sessions whose
/// `parent_sid` is this sid). Depth is bounded by the live set, so the
/// recursion terminates.
fn session_tree_node(
    v: &crate::gateway::SessionView,
    all: &[crate::gateway::SessionView],
) -> serde_json::Value {
    let children: Vec<serde_json::Value> = all
        .iter()
        .filter(|c| c.parent_sid.as_deref() == Some(v.sid.as_str()) && c.sid != v.sid)
        .map(|c| session_tree_node(c, all))
        .collect();
    serde_json::json!({
        "sid": v.sid,
        "role": v.role,
        "vendor": v.vendor,
        "children": children,
    })
}

/// `session_stop` — deregister + close a session by sid (explicit command).
async fn run_session_stop(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    // R-M3 — only stop sessions in the caller's own project (explicit command,
    // never a proactive kill; the scope check just prevents cross-project stop).
    assert_caller_owns_session("session_stop", args, gateway, &sid, caller).await?;
    // v0.9.0 W2 (F2) — an Ambient (agent) caller may only stop its OWN
    // descendants (walk the target's parent chain; it must reach the caller).
    // Admin/human callers are unrestricted (fleet-wide).
    let caller_sid = match caller {
        McpCaller::Ambient => args
            .get("_caller_sid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        McpCaller::Admin => String::new(),
    };
    let mut gw = gateway.lock().await;
    if !caller_sid.is_empty() && !gw.ancestor_chain(&sid).contains(&caller_sid) {
        return Err(format!(
            "session_stop: permission denied — session {sid} is not a descendant of the caller {caller_sid} (an agent may only stop the sessions it delegated)"
        ));
    }
    // Capture the delegation event fields + drop the child's own watch BEFORE
    // the stop removes it from the live map.
    let stopped_meta = gw.session_vendor_host_slug(&sid);
    if !caller_sid.is_empty() {
        gw.disarm_delegation_watch(&sid);
    }
    gw.stop_session(&sid)
        .await
        .map_err(|e| format!("session_stop failed: {e}"))?;
    if !caller_sid.is_empty() {
        if let Some((vendor, host, slug)) = stopped_meta {
            gw.emit_delegation_progress(
                &slug,
                ccteam_harness::execution::progress_bridge::DELEGATION_STOPPED,
                &caller_sid,
                &sid,
                vendor,
                &host,
                None,
                None,
                None,
            );
        }
    }
    drop(gw);
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "sid": sid,
        "stopped": true,
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

/// Pull a required `sid` arg (the gateway `s{n}` id).
fn arg_session_sid(args: &serde_json::Value) -> std::result::Result<String, String> {
    args.get("sid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "missing required `sid`".to_string())
}

/// v0.8.7 review-fix (R-M3) — project-scope a sid-addressed `session_*` call:
/// the caller may only dispatch/collect/stop a session that runs in the
/// caller's OWN bound project (`_caller_slug`). Resolves the sid under the
/// gateway lock (sync `session_resolve`, no `.await` held), drops the guard,
/// then compares the session's project to the ambient slug. An unknown sid, an
/// unset ambient slug, or a project mismatch all reject — so a cto bound to
/// project A can never operate a project-B sid (even one another chat created).
/// Meaningful now that R-M1 gives the caller a verified identity.
async fn assert_caller_owns_session(
    name: &str,
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    sid: &str,
    caller: McpCaller,
) -> std::result::Result<(), String> {
    // v0.9 T4 review fix — the HTTP front door's verified admin operates
    // fleet-wide (same semantics as the web admin Identity): no ambient slug
    // to bind to. Unknown sids still fail inside the op itself.
    if caller == McpCaller::Admin {
        return Ok(());
    }
    let caller_slug = args
        .get("_caller_slug")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{name}: no project scope (ambient slug unset)"))?
        .to_string();
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve(sid)
    };
    let resolved = resolved.ok_or_else(|| format!("{name}: unknown session: {sid}"))?;
    if resolved.project != caller_slug {
        return Err(format!(
            "{name}: permission denied — session {sid} runs in project `{}`, but the caller is bound to project `{caller_slug}`",
            resolved.project
        ));
    }
    Ok(())
}

/// Parse the optional `vendor` arg (default `claude`), lowercasing first so a
/// stray `"Claude"` still lands in the right variant (Bug A defense).
fn parse_session_vendor(
    args: &serde_json::Value,
) -> std::result::Result<ccteam_harness::AgentVendor, String> {
    match args.get("vendor").and_then(|v| v.as_str()) {
        None => Ok(ccteam_harness::AgentVendor::Claude),
        Some(raw) => match raw.to_lowercase().as_str() {
            "" | "claude" => Ok(ccteam_harness::AgentVendor::Claude),
            "codex" => Ok(ccteam_harness::AgentVendor::Codex),
            "grok" => Ok(ccteam_harness::AgentVendor::Grok),
            "opencode" => Ok(ccteam_harness::AgentVendor::Opencode),
            other => Err(format!(
                "session_spawn: invalid vendor `{other}`: expected `claude`, `codex`, `grok`, or `opencode`"
            )),
        },
    }
}

#[cfg(test)]
mod chat_send_file_tests {
    use super::*;
    use crate::transport::OutboundFileKind;

    #[test]
    fn parse_outbound_kind_infers_photo_from_extension() {
        assert_eq!(
            parse_outbound_kind(None, "/x/shot.PNG"),
            OutboundFileKind::Photo
        );
        assert_eq!(
            parse_outbound_kind(None, "/x/a.jpeg"),
            OutboundFileKind::Photo
        );
        assert_eq!(
            parse_outbound_kind(None, "/x/report.pdf"),
            OutboundFileKind::Document
        );
        // Explicit kind overrides the extension.
        assert_eq!(
            parse_outbound_kind(Some("document"), "/x/shot.png"),
            OutboundFileKind::Document
        );
    }

    #[test]
    fn build_send_file_event_uses_live_target_and_attaches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "caption": "the chart",
            "slug": "dev-foo",
            "role": "lead",
        });
        let live = Some(("telegram".to_string(), "chat-42".to_string()));
        let evt = build_send_file_event(&args, 7, live).unwrap();
        assert_eq!(evt.channel, "telegram");
        assert_eq!(evt.chat_id, "chat-42");
        assert_eq!(evt.attachments.len(), 1);
        assert_eq!(evt.attachments[0].kind, OutboundFileKind::Photo);
        assert_eq!(evt.attachments[0].caption.as_deref(), Some("the chart"));
        assert!(evt.id.ends_with("-7"));
    }

    /// v0.8.8 — the firing session's live reply target is the single source of
    /// truth; no registry is consulted (the actively-chatting agent pushes a
    /// file back without any prior `chat_register_bot`).
    #[test]
    fn build_send_file_event_uses_live_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "slug": "dev-foo",
            "role": "cto",
        });
        let live = Some(("telegram".to_string(), "live-chat-7".to_string()));
        let evt = build_send_file_event(&args, 1, live).unwrap();
        assert_eq!(evt.channel, "telegram");
        assert_eq!(evt.chat_id, "live-chat-7");
    }

    /// v0.8.8 — a web live target routes to the web channel (the single source
    /// of truth carries whatever channel the firing session is bound to).
    #[test]
    fn build_send_file_event_live_target_web_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("x.txt");
        std::fs::write(&file, b"hi").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "lead",
        });
        let live = Some(("web".to_string(), "web-live".to_string()));
        let evt = build_send_file_event(&args, 2, live).unwrap();
        assert_eq!(evt.channel, "web");
        assert_eq!(evt.chat_id, "web-live");
    }

    #[test]
    fn build_send_file_event_errors_on_missing_file() {
        let args = serde_json::json!({
            "path": "/nope/does-not-exist.png", "slug": "dev-foo", "role": "lead",
        });
        let err = build_send_file_event(&args, 0, None).unwrap_err();
        assert!(err.contains("file not found"), "got: {err}");
    }

    /// v0.8.8 — no live target (None) → precise error pointing at the
    /// spawn/bind flow; the registry is NOT consulted (single source of truth).
    #[test]
    fn build_send_file_event_errors_when_no_live_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("x.txt");
        std::fs::write(&file, b"hi").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "ghost",
        });
        let err = build_send_file_event(&args, 0, None).unwrap_err();
        assert!(err.contains("no IM chat bound"), "got: {err}");
    }

    #[test]
    fn build_send_file_event_errors_on_oversized_photo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("huge.png");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(11 * 1024 * 1024).unwrap(); // 11 MB (sparse) > 10 MB photo limit
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "lead",
        });
        let err = build_send_file_event(&args, 0, None).unwrap_err();
        assert!(err.contains("too large"), "got: {err}");
    }
}

#[cfg(test)]
mod session_tool_tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        })
    }

    /// A dispatcher with only `paths` wired (enough for the local-admin
    /// promotion, which never touches gateway/sink/pending).
    fn dispatch_with_root(root: &std::path::Path) -> McpDispatch {
        McpDispatch {
            paths: CcteamPaths {
                root: root.to_path_buf(),
                projects_root: root.join("projects"),
            },
            sink: None,
            pending: None,
            gateway: None,
        }
    }

    fn write_web_token(root: &std::path::Path, token: &str) {
        let secrets = root.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(secrets.join("web-token"), format!("{token}\n")).unwrap();
    }

    // v0.9.1 — main-session fallback: the LOCAL socket promotes a caller
    // presenting the admin web token to Admin semantics, and strips the
    // token arg either way.
    #[test]
    fn promote_local_admin_upgrades_on_matching_token_and_strips_arg() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_web_token(tmp.path(), "tok-abc123");
        let d = dispatch_with_root(tmp.path());
        let req = call(
            "session_list",
            json!({ "_caller_admin_token": "tok-abc123" }),
        );
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Admin);
        assert!(
            req.pointer("/params/arguments/_caller_admin_token")
                .is_none(),
            "token arg must be stripped before dispatch"
        );
    }

    #[test]
    fn promote_local_admin_fails_closed_on_wrong_or_missing_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_web_token(tmp.path(), "tok-abc123");
        let d = dispatch_with_root(tmp.path());

        // Wrong token → Ambient (and still stripped).
        let req = call("session_list", json!({ "_caller_admin_token": "wrong" }));
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
        assert!(req
            .pointer("/params/arguments/_caller_admin_token")
            .is_none());

        // No token arg → Ambient, request untouched.
        let req = call("session_list", json!({ "_caller_sid": "s1" }));
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
        assert_eq!(
            req.pointer("/params/arguments/_caller_sid"),
            Some(&json!("s1"))
        );

        // Token file absent on the daemon → Ambient even with an arg.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let d2 = dispatch_with_root(tmp2.path());
        let req = call("session_list", json!({ "_caller_admin_token": "anything" }));
        let (_req, caller) = d2.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
    }

    // v0.8.7 review-fix (R-M1/R-M3) — a no-process stub adapter so a real
    // `Gateway` can mint per-session secrets + track project scope without
    // spawning a `claude` pane. `start_thread` records the `(sid, secret)` the
    // gateway minted so the test can present the real secret to the gate.
    #[derive(Clone, Default)]
    struct StubAdapter {
        spawns: std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
        /// v0.9.0 W2 — when true, `submit_turn` enqueues an echo AgentMessage
        /// the pump folds into an `Answer` (for the dispatch-wait tests).
        /// Default false = empty event stream (existing principal tests).
        answer: bool,
        /// Delay (ms) before `events()` yields — forces a `wait` timeout.
        event_delay_ms: u64,
        events: std::sync::Arc<
            tokio::sync::Mutex<std::collections::VecDeque<(String, ccteam_harness::ThreadEvent)>>,
        >,
        notify: std::sync::Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl ccteam_harness::HarnessAdapter for StubAdapter {
        fn name(&self) -> &'static str {
            "stub-gate-test"
        }
        fn vendor(&self) -> ccteam_harness::AgentVendor {
            ccteam_harness::AgentVendor::Claude
        }
        async fn start_thread(
            &self,
            spec: &ccteam_harness::AgentSpecBrief,
            ctx: &ccteam_harness::SpawnCtx,
        ) -> std::result::Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError>
        {
            self.spawns
                .lock()
                .await
                .push((ctx.sid.clone(), ctx.secret.clone()));
            Ok(ccteam_harness::ThreadHandle {
                vendor: ccteam_harness::AgentVendor::Claude,
                mode: ccteam_harness::ExecutionMode::Chat,
                identity: format!("{}-{}-{}", ctx.slug, spec.role, ctx.sid),
                started_at: chrono::Utc::now(),
                raw_extras: json!({}),
            })
        }
        async fn submit_turn(
            &self,
            h: &ccteam_harness::ThreadHandle,
            input: ccteam_harness::TurnInput,
        ) -> std::result::Result<ccteam_harness::TurnId, ccteam_harness::HarnessError> {
            if self.answer {
                let text = match input {
                    ccteam_harness::TurnInput::UserText(t) => t,
                    _ => String::new(),
                };
                self.events.lock().await.push_back((
                    h.identity.clone(),
                    ccteam_harness::ThreadEvent::ItemCompleted {
                        item: ccteam_harness::ThreadItem {
                            id: "msg-1".into(),
                            details: ccteam_harness::ThreadItemDetails::AgentMessage(format!(
                                "echo: {text}"
                            )),
                        },
                    },
                ));
                self.notify.notify_one();
            }
            Ok(ccteam_harness::TurnId::new(format!("turn-{}", h.identity)))
        }
        fn events(
            &self,
            h: &ccteam_harness::ThreadHandle,
        ) -> futures::stream::BoxStream<'static, ccteam_harness::ThreadEvent> {
            if !self.answer {
                return Box::pin(futures::stream::empty());
            }
            let events = std::sync::Arc::clone(&self.events);
            let notify = std::sync::Arc::clone(&self.notify);
            let wanted = h.identity.clone();
            let delay = self.event_delay_ms;
            Box::pin(futures::stream::unfold((), move |_| {
                let events = std::sync::Arc::clone(&events);
                let notify = std::sync::Arc::clone(&notify);
                let wanted = wanted.clone();
                async move {
                    loop {
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        let mut guard = events.lock().await;
                        if let Some(idx) = guard.iter().position(|(t, _)| t == &wanted) {
                            let (_, evt) = guard.remove(idx).unwrap();
                            return Some((evt, ()));
                        }
                        drop(guard);
                        notify.notified().await;
                    }
                }
            }))
        }
        async fn resume_thread(
            &self,
            _persistent_id: &str,
        ) -> std::result::Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError>
        {
            Err(ccteam_harness::HarnessError::NotImplemented {
                reason: "stub".to_string(),
            })
        }
        async fn close_thread(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> std::result::Result<(), ccteam_harness::HarnessError> {
            Ok(())
        }
        async fn handle_directive(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _d: ccteam_harness::Directive,
        ) -> std::result::Result<ccteam_harness::DirectiveOutcome, ccteam_harness::HarnessError>
        {
            Ok(ccteam_harness::DirectiveOutcome::Rejected {
                reason: "stub".to_string(),
            })
        }
        async fn thread_status(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> std::result::Result<ccteam_harness::ThreadStatus, ccteam_harness::HarnessError>
        {
            Ok(ccteam_harness::ThreadStatus::default())
        }
    }

    /// Build a real `Gateway` (stub adapter) with a cto session in `alpha` and
    /// a reviewer session in `beta`, returning the handle + the cto's minted
    /// secret so an end-to-end gate test can present a real `(role, secret)`.
    /// The secret is read back from the stub adapter's spawn recording (the
    /// gateway minted + injected it into the spawn ctx).
    async fn gateway_with_cto_and_cross_project() -> (GatewayHandle, String, String, String) {
        let stub = StubAdapter::default();
        let stub_for_factory = stub.clone();
        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(stub_for_factory.clone())
                as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gw =
            crate::gateway::Gateway::new_with_factory(factory, "alpha", "/tmp/cc-gate-alpha");
        gw.register_project("beta", "/tmp/cc-gate-beta");
        let cto_sid = gw
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let beta_sid = gw
            .create_session_api(
                "beta".into(),
                "reviewer".into(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let cto_secret = stub
            .spawns
            .lock()
            .await
            .iter()
            .find(|(sid, _)| sid == &cto_sid)
            .map(|(_, secret)| secret.clone())
            .expect("stub recorded the cto session's minted secret");
        assert_eq!(cto_secret.len(), 32, "the minted secret is 128-bit hex");
        (
            std::sync::Arc::new(tokio::sync::Mutex::new(gw)),
            cto_sid,
            beta_sid,
            cto_secret,
        )
    }

    /// v0.9.0 W1 (F1) — end-to-end: a caller presenting the WRONG secret for a
    /// real sid is rejected by `execute_session_tool` (the `(sid, secret)`
    /// principal is the authoritative check; a forged role arg is irrelevant).
    #[tokio::test]
    async fn execute_session_tool_rejects_wrong_secret() {
        let (gw, cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "session_list",
            json!({
                "_caller_sid": cto_sid,
                "_caller_secret": "ffffffffffffffffffffffffffffffff",
            }),
        );
        let resp = execute_session_tool(&req, Some(&gw), McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("could not be authenticated"),
            "wrong secret must fail auth, got: {text}"
        );
    }

    /// v0.9.0 W1 (F1) — end-to-end: the CORRECT `(sid, secret)` principal passes
    /// the gate and the call reaches the gateway (session_list returns rows).
    #[tokio::test]
    async fn execute_session_tool_allows_correct_principal() {
        let (gw, cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "session_list",
            json!({
                "_caller_sid": cto_sid,
                "_caller_secret": cto_secret,
            }),
        );
        let resp = execute_session_tool(&req, Some(&gw), McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], false, "correct principal passes");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"sessions\""), "got: {text}");
    }

    /// v0.9.0 W1 (F1) — end-to-end: a caller authenticated for project `alpha`
    /// is REJECTED when it tries to dispatch/collect/stop a `beta` sid
    /// (cross-project). The scope comes from the SERVER-resolved CallerCtx.slug.
    #[tokio::test]
    async fn execute_session_tool_rejects_cross_project_sid() {
        let (gw, cto_sid, beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        for tool in ["session_dispatch", "session_collect", "session_stop"] {
            let mut args = json!({
                "_caller_sid": cto_sid.clone(),
                "_caller_secret": cto_secret.clone(),
                "sid": beta_sid.clone(),
            });
            if tool == "session_dispatch" {
                args["task"] = json!("do something");
            }
            let resp = execute_session_tool(&call(tool, args), Some(&gw), McpCaller::Ambient).await;
            assert_eq!(resp["result"]["isError"], true, "{tool} must reject");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("permission denied") && text.contains("bound to project `alpha`"),
                "{tool}: cross-project must be denied with a clear reason, got: {text}"
            );
        }
    }

    /// v0.9.0 W1 (F1) — server-side slug overwrite: even if the caller SPOOFS
    /// `_caller_slug: "beta"`, the gate overwrites it from CallerCtx (the real
    /// project of the presented sid = `alpha`), so a `beta` sid is still denied.
    #[tokio::test]
    async fn execute_session_tool_overwrites_spoofed_caller_slug() {
        let (gw, cto_sid, beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let resp = execute_session_tool(
            &call(
                "session_collect",
                json!({
                    "_caller_sid": cto_sid,
                    "_caller_secret": cto_secret,
                    "_caller_slug": "beta", // spoof attempt — must be ignored
                    "sid": beta_sid,
                }),
            ),
            Some(&gw),
            McpCaller::Ambient,
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], true,
            "spoofed slug must not grant cross-project access"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("bound to project `alpha`"),
            "server must use CallerCtx.slug (alpha), not the spoofed `beta`, got: {text}"
        );
    }

    /// v0.9.0 W1 (F1) — positive control: the SAME caller operating its OWN
    /// `alpha` sid is allowed (the scope check isn't blanket-deny).
    #[tokio::test]
    async fn execute_session_tool_allows_same_project_sid() {
        let (gw, cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let target_sid = cto_sid.clone();
        let resp = execute_session_tool(
            &call(
                "session_collect",
                json!({
                    "_caller_sid": cto_sid,
                    "_caller_secret": cto_secret,
                    "sid": target_sid,
                }),
            ),
            Some(&gw),
            McpCaller::Ambient,
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], false,
            "same-project collect must be allowed: {resp}"
        );
    }

    #[test]
    fn is_session_tool_call_matches_only_session_tools_calls() {
        assert!(is_session_tool_call(&call("session_spawn", json!({}))));
        assert!(is_session_tool_call(&call(
            "session_collect",
            json!({ "sid": "s1" })
        )));
        // Foreign tool name.
        assert!(!is_session_tool_call(&call(
            "ccteam__chat_register_bot",
            json!({})
        )));
        // Right name, wrong method.
        assert!(!is_session_tool_call(&json!({
            "method": "tools/list",
            "params": { "name": "session_spawn" }
        })));
    }

    /// v0.9.0 W1 (F1) — an Ambient caller whose `(sid, secret)` principal
    /// resolves to no live session is REJECTED (needs a gateway to check).
    #[tokio::test]
    async fn execute_session_tool_ambient_denies_unknown_principal() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "session_list",
            json!({ "_caller_sid": "s999", "_caller_secret": "deadbeefdeadbeefdeadbeefdeadbeef" }),
        );
        let resp = execute_session_tool(&req, Some(&gw), McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("could not be authenticated"),
            "unknown principal must be denied, got: {text}"
        );
    }

    /// v0.9.0 W1 (F1) — fail-closed: with no gateway wired, EVERY Ambient
    /// session_* call is refused ("gateway not running"), never a fall-through
    /// that would skip the principal check.
    #[tokio::test]
    async fn execute_session_tool_ambient_gateway_down_fails_closed() {
        let req = call(
            "session_list",
            json!({ "_caller_sid": "s1", "_caller_secret": "abc" }),
        );
        let resp = execute_session_tool(&req, None, McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("gateway not running"),
            "gateway-down must fail closed, got: {text}"
        );
    }

    /// v0.9 T4 — the HTTP front door's verified admin skips the principal gate
    /// entirely: NO `_caller_*` args, straight to the op (which then reports
    /// gateway-down here — proving the gate was bypassed, not that it denied).
    #[tokio::test]
    async fn execute_session_tool_admin_bypasses_gate_reports_gateway_down() {
        let req = call("session_list", json!({}));
        let resp = execute_session_tool(&req, None, McpCaller::Admin).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("permission denied") && !text.contains("could not be authenticated"),
            "admin must skip the principal gate, got: {text}"
        );
        assert!(
            text.contains("gateway not running"),
            "expected gateway-down after the bypassed gate, got: {text}"
        );
    }

    /// v0.9 T4 — admin `session_list` works with NO ambient args and reaches the
    /// live gateway (fleet-wide semantics, same as the web admin Identity).
    #[tokio::test]
    async fn execute_session_tool_admin_lists_sessions_fleet_wide() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let resp = execute_session_tool(
            &call("session_list", json!({})),
            Some(&gw),
            McpCaller::Admin,
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], false,
            "admin bypasses the principal gate: {resp}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"sessions\""), "got: {text}");
    }

    /// v0.9 T4 review fix — the internal-bus methods are refused on the admin
    /// (HTTP) transport with JSON-RPC `-32601`; they remain mcp.sock-only
    /// (HITL stays on vendor-native / in-band channels — tech-design §1.1).
    #[tokio::test]
    async fn dispatch_as_admin_refuses_internal_bus_methods() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatch = McpDispatch {
            paths: ccteam_core::CcteamPaths {
                root: tmp.path().join("home"),
                projects_root: tmp.path().join("projects"),
            },
            sink: None,
            pending: None,
            gateway: None,
        };
        for method in ["permission/ask", "interaction/ask"] {
            let req = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": {}});
            let resp = dispatch
                .dispatch_as(req, McpCaller::Admin)
                .await
                .expect("refusal is an error response, not a notification");
            assert_eq!(resp["error"]["code"], -32601, "{method}: {resp}");
            assert_eq!(resp["id"], 7, "{method}: id must round-trip");
        }
    }

    #[test]
    fn parse_session_vendor_defaults_to_claude_and_lowercases() {
        assert_eq!(
            parse_session_vendor(&json!({})).unwrap(),
            ccteam_harness::AgentVendor::Claude
        );
        assert_eq!(
            parse_session_vendor(&json!({ "vendor": "Claude" })).unwrap(),
            ccteam_harness::AgentVendor::Claude
        );
        assert_eq!(
            parse_session_vendor(&json!({ "vendor": "codex" })).unwrap(),
            ccteam_harness::AgentVendor::Codex
        );
        assert!(parse_session_vendor(&json!({ "vendor": "gpt" })).is_err());
    }

    #[test]
    fn arg_session_sid_requires_non_empty() {
        assert_eq!(arg_session_sid(&json!({ "sid": "s3" })).unwrap(), "s3");
        assert!(arg_session_sid(&json!({})).is_err());
        assert!(arg_session_sid(&json!({ "sid": "" })).is_err());
    }

    #[test]
    fn session_tool_response_shapes_content_and_is_error() {
        let ok = session_tool_response(json!(1), "done".into(), false);
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(ok["result"]["content"][0]["text"], "done");
        let err = session_tool_response(json!(2), "boom".into(), true);
        assert_eq!(err["result"]["isError"], true);
    }

    // ── v0.8.7 W2 (DB.3/DB.4) — HITL permission/ask wiring ────────────────

    #[test]
    fn is_permission_ask_call_matches_only_the_raw_method() {
        assert!(is_permission_ask_call(
            &json!({ "method": "permission/ask" })
        ));
        // Not a tools/call, and not interaction/ask.
        assert!(!is_permission_ask_call(
            &json!({ "method": "interaction/ask" })
        ));
        assert!(!is_permission_ask_call(&call("session_spawn", json!({}))));
    }

    #[test]
    fn summarize_tool_input_picks_the_useful_field() {
        // Bash → command.
        assert_eq!(
            summarize_tool_input("Bash", &json!({ "command": "rm -rf /tmp/x" })),
            "Bash rm -rf /tmp/x"
        );
        // Write with no content → just the path (no dangling preview).
        assert_eq!(
            summarize_tool_input("Write", &json!({ "file_path": "/a/b.rs" })),
            "Write /a/b.rs"
        );
        // No dedicated renderer + empty params → just the tool name.
        assert_eq!(summarize_tool_input("Glob", &json!({})), "Glob");
    }

    /// v0.8.22 P1 (review §3.1-2) — delegates to `crate::hitl`'s
    /// tool-aware summarizer: a `Write`/`Edit` approval now shows the
    /// content/diff, not just the path (see `hitl.rs`'s own unit tests for
    /// full per-tool coverage; this just locks in the CLI's delegation).
    #[test]
    fn summarize_tool_input_write_shows_content_preview() {
        assert_eq!(
            summarize_tool_input(
                "Write",
                &json!({ "file_path": "/a/b.rs", "content": "fn f(){}" })
            ),
            "Write /a/b.rs\n  + fn f(){}"
        );
    }

    #[test]
    fn summarize_tool_input_truncates_long_detail() {
        let long = "x".repeat(500);
        let out = summarize_tool_input("Bash", &json!({ "command": long }));
        // Truncated with an ellipsis; comfortably bounded (~200-char cap).
        assert!(out.ends_with('…'));
        assert!(
            out.chars().count() <= 207,
            "got {} chars",
            out.chars().count()
        );
    }

    /// permission/ask with no gateway/sink/pending wired returns a JSON-RPC
    /// error (the hook then fail-safe denies) — never panics. Deterministic:
    /// passes `None` for sink/pending/gateway, so no socket / IM is touched.
    #[tokio::test]
    async fn permission_ask_without_gateway_returns_error() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "permission/ask",
            "params": { "slug": "s", "role": "r", "tool_name": "Bash" }
        });
        let resp = execute_permission_ask(&req, None, None, None).await;
        assert!(
            resp.get("error").is_some(),
            "no gateway ⇒ JSON-RPC error so the hook denies: {resp}"
        );
        assert_eq!(resp["id"], json!(7));
    }

    /// v0.8.7 review-fix (R-L1) — the resolved HITL permission-prompt TTL is
    /// SHORTER than the 600s interaction/ask TTL (a tool-approval parks the
    /// whole turn, so a fast fail-safe deny beats a long park). Exercises the
    /// runtime resolver (default path, env unset) so the relation is a real
    /// value comparison, not a const fold. The env override is exercised by an
    /// integration test (separate process — env mutation discipline).
    #[test]
    fn permission_prompt_ttl_is_shorter_than_interaction_ttl() {
        let ttl = permission_prompt_timeout_secs();
        let interaction = INTERACTION_ASK_TIMEOUT_SECS;
        assert!(
            ttl < interaction,
            "permission TTL ({ttl}s) must be shorter than interaction TTL ({interaction}s)"
        );
        assert!(ttl >= 1, "TTL must be clamped to >= 1s, got {ttl}");
    }

    /// v0.8.7 review-fix (R-L1) — `emit_permission_prompt_outstanding` with a
    /// blank slug is a no-op (nothing to address) and never panics. The
    /// env-resolved write path is covered by an integration test (separate
    /// process); here we pin the cheap guard.
    #[test]
    fn emit_permission_prompt_outstanding_blank_slug_is_noop() {
        // Must not panic / must not touch the filesystem for a blank slug.
        emit_permission_prompt_outstanding("", "cto", "Bash", "rm -rf /", 120);
    }

    // ---------- v0.8.7 review-fix (R-L3) session_collect paging ----------

    fn turn(id: &str) -> ccteam_harness::execution::turns_mirror::TurnRecord {
        ccteam_harness::execution::turns_mirror::TurnRecord {
            turn_id: id.to_string(),
            ts: chrono::Utc::now(),
            vendor: "claude".to_string(),
            role: "cto".to_string(),
            user: "q".to_string(),
            assistant: format!("a-{id}"),
            usage: serde_json::Value::Null,
            tool_calls: Vec::new(),
        }
    }

    /// A burst of MORE than `n` turns after the cursor must NOT silently drop
    /// the middle: `page_collected_turns` returns the OLDEST `n`, sets the
    /// cursor to that page's boundary, and flags `truncated` so a follow-up
    /// poll fetches the rest. Walking the cursor returns EVERY turn in order.
    #[test]
    fn page_collected_turns_pages_a_burst_without_loss() {
        let all: Vec<_> = (0..25).map(|i| turn(&format!("t{i}"))).collect();
        // First poll, no cursor, page size 10.
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 10, false);
        assert_eq!(rows.len(), 10);
        assert!(truncated, "25 > 10 ⇒ truncated");
        assert_eq!(rows[0]["turn_id"], "t0", "oldest-first (not the newest 10)");
        assert_eq!(rows[9]["turn_id"], "t9");
        assert_eq!(cursor.as_deref(), Some("t9"), "cursor = boundary turn");

        // Second poll from the boundary.
        let (rows2, cursor2, truncated2) = page_collected_turns(&all, Some("t9"), 10, false);
        assert_eq!(rows2.len(), 10);
        assert!(truncated2);
        assert_eq!(
            rows2[0]["turn_id"], "t10",
            "no gap — resumes right after t9"
        );
        assert_eq!(cursor2.as_deref(), Some("t19"));

        // Third poll drains the remainder.
        let (rows3, _c3, truncated3) = page_collected_turns(&all, Some("t19"), 10, false);
        assert_eq!(rows3.len(), 5);
        assert!(!truncated3, "final page is not truncated");
        assert_eq!(rows3[0]["turn_id"], "t20");
        assert_eq!(rows3[4]["turn_id"], "t24");

        // The three pages reconstruct the full ordered set — zero loss.
        let mut seen: Vec<String> = Vec::new();
        for page in [&rows, &rows2, &rows3] {
            for r in page {
                seen.push(r["turn_id"].as_str().unwrap().to_string());
            }
        }
        let expected: Vec<String> = (0..25).map(|i| format!("t{i}")).collect();
        assert_eq!(seen, expected, "every turn returned exactly once, in order");
    }

    /// A short backlog (≤ `n`) returns everything, `truncated:false`, cursor =
    /// last turn. An unknown cursor returns everything (never silently lose).
    #[test]
    fn page_collected_turns_short_and_unknown_cursor() {
        let all: Vec<_> = (0..3).map(|i| turn(&format!("t{i}"))).collect();
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 20, false);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
        assert_eq!(cursor.as_deref(), Some("t2"));
        // Unknown cursor → all turns (defensive, no loss).
        let (rows_u, _c, trunc_u) = page_collected_turns(&all, Some("ghost"), 20, false);
        assert_eq!(rows_u.len(), 3);
        assert!(!trunc_u);
    }

    /// v0.9.1 — `tail:true` returns the NEWEST `n` (chronological inside the
    /// page), the "just give me the final answer" shape; cursor = newest turn.
    #[test]
    fn page_collected_turns_tail_returns_newest() {
        let all: Vec<_> = (0..25).map(|i| turn(&format!("t{i}"))).collect();
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 3, true);
        assert_eq!(rows.len(), 3);
        assert!(truncated, "25 > 3 ⇒ truncated");
        assert_eq!(rows[0]["turn_id"], "t22", "newest 3, oldest of them first");
        assert_eq!(rows[2]["turn_id"], "t24", "ends at the newest turn");
        assert_eq!(cursor.as_deref(), Some("t24"));
        // `since` still applies before the tail cut.
        let (rows2, _c, trunc2) = page_collected_turns(&all, Some("t22"), 5, true);
        assert_eq!(rows2.len(), 2, "only t23/t24 exist after t22");
        assert!(!trunc2);
        assert_eq!(rows2[0]["turn_id"], "t23");
    }

    // ========================================================================
    // v0.9.0 W2 (F2/F7) — dispatch-handler: idempotency, cycle, stop, wait.
    // The handlers are called directly with the `_caller_*` context that
    // `execute_session_tool` injects (so no secret dance).
    // ========================================================================

    /// Inject the server-resolved caller identity `execute_session_tool` sets.
    fn ambient(caller_sid: &str, slug: &str, mut args: serde_json::Value) -> serde_json::Value {
        let o = args.as_object_mut().unwrap();
        o.insert("_caller_sid".into(), json!(caller_sid));
        o.insert("_caller_slug".into(), json!(slug));
        o.insert("_caller_role".into(), json!(""));
        o.insert("_caller_depth".into(), json!(0));
        args
    }

    /// A delegation-wired gateway (fresh stub per spawn — own event stream;
    /// `answer`/`delay_ms` control the wait tests). Returns (handle, principal).
    async fn dispatch_gateway(
        answer: bool,
        delay_ms: u64,
        project_dir: &std::path::Path,
    ) -> (GatewayHandle, String) {
        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(StubAdapter {
                answer,
                event_delay_ms: delay_ms,
                ..Default::default()
            }) as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gw = Gateway::new_with_factory(factory, "alpha", project_dir);
        let (dtx, drx) = tokio::sync::mpsc::unbounded_channel();
        gw.set_delegation_notifier_tx(dtx);
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });
        let principal = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let handle = std::sync::Arc::new(tokio::sync::Mutex::new(gw));
        tokio::spawn(Gateway::run_delegation_notifier(
            std::sync::Arc::clone(&handle),
            drx,
        ));
        (handle, principal)
    }

    fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_idempotency_replay_returns_same_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let a = ambient(
            &principal,
            "alpha",
            json!({ "idempotency_key": "k1", "vendor": "claude" }),
        );
        let r1 = parse(
            &run_session_spawn(&a, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        let r2 = parse(
            &run_session_spawn(&a, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        assert_eq!(r1["sid"], r2["sid"], "replay returns the original sid");
        assert_eq!(r2["idempotent_replay"], json!(true));
        assert!(
            r1.get("idempotent_replay").is_none(),
            "first is not a replay"
        );
        // Exactly ONE child was created (principal + 1 child = 2 sessions).
        let list = parse(&run_session_list(&gw).await.unwrap());
        let children = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["parent_sid"] == json!(principal))
            .count();
        assert_eq!(children, 1, "no double-spawn: {list}");
    }

    /// v0.9.1 — `session_spawn{task}`: one call spawns AND dispatches (the
    /// dominant flow). The response merges the dispatch outcome (`turn_id` +
    /// `status:dispatched`) into the spawn body, and the delegation lineage
    /// is intact (`parent_sid` = the caller).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_with_task_dispatches_in_one_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let r = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "vendor": "claude", "task": "do the thing" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["status"], json!("dispatched"), "dispatch merged: {r}");
        assert!(
            r["turn_id"].as_str().is_some_and(|t| !t.is_empty()),
            "turn_id present: {r}"
        );
        assert_eq!(r["parent_sid"], json!(principal));
    }

    /// v0.9.1 — `session_spawn{task, wait_seconds}` with an answering child
    /// returns the answer inline (`status:completed`, `result_text`), exactly
    /// like session_dispatch's wait path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn spawn_with_task_and_wait_returns_inline_result() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let r = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "task": "answer me", "wait_seconds": 6 }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["status"], json!("completed"), "inline: {r}");
        assert!(
            r["result_text"]
                .as_str()
                .unwrap()
                .contains("echo: answer me"),
            "inline result: {r}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_idempotency_replay_returns_same_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let d = ambient(
            &principal,
            "alpha",
            json!({ "sid": child, "task": "go", "idempotency_key": "d1" }),
        );
        let t1 = parse(
            &run_session_dispatch(&d, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        let t2 = parse(
            &run_session_dispatch(&d, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        assert_eq!(t1["turn_id"], t2["turn_id"], "replay returns the same turn");
        assert_eq!(t2["idempotent_replay"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_cycle_self_and_ancestor_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        // self-dispatch.
        let e = run_session_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": principal, "task": "x" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e.contains("itself"), "self cycle: {e}");
        // child dispatching to its ancestor (principal).
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let e2 = run_session_dispatch(
            &ambient(&child, "alpha", json!({ "sid": principal, "task": "x" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e2.contains("ancestor"), "ancestor cycle: {e2}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_descendant_ok_nondescendant_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // A sibling root (not a descendant of principal).
        let sibling = {
            let mut g = gw.lock().await;
            g.create_session_api(
                "alpha".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid
        };
        let e = run_session_stop(
            &ambient(&principal, "alpha", json!({ "sid": sibling })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e.contains("not a descendant"), "non-descendant stop: {e}");
        // The real descendant stops fine.
        let ok = run_session_stop(
            &ambient(&principal, "alpha", json!({ "sid": child })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        assert_eq!(parse(&ok)["stopped"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_inline_completed_and_timeout_pending() {
        // inline completed (child answers immediately).
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let r = parse(
            &run_session_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": child, "task": "go", "wait_seconds": 6 }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["status"], json!("completed"), "inline: {r}");
        assert!(
            r["result_text"].as_str().unwrap().contains("echo: go"),
            "inline result: {r}"
        );

        // timeout pending (child's answer is delayed past the wait).
        let tmp2 = tempfile::TempDir::new().unwrap();
        let (gw2, p2) = dispatch_gateway(true, 10_000, tmp2.path()).await;
        let child2 = parse(
            &run_session_spawn(
                &ambient(&p2, "alpha", json!({ "vendor": "claude" })),
                &gw2,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let r2 = parse(
            &run_session_dispatch(
                &ambient(
                    &p2,
                    "alpha",
                    json!({ "sid": child2, "task": "go", "wait_seconds": 1 }),
                ),
                &gw2,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r2["status"], json!("pending"), "timeout: {r2}");
    }

    /// LOCK DISCIPLINE: the gateway lock is acquirable while a dispatch `wait`
    /// is parked (the wait awaits OFF the lock).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_does_not_hold_gateway_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 3_000, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // Park a dispatch wait in a task.
        let gw_w = std::sync::Arc::clone(&gw);
        let d = ambient(
            &principal,
            "alpha",
            json!({ "sid": child, "task": "go", "wait_seconds": 5 }),
        );
        let waiter =
            tokio::spawn(async move { run_session_dispatch(&d, &gw_w, McpCaller::Ambient).await });
        // Give the wait time to submit + park.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        // The gateway lock must be acquirable NOW (the wait is off-lock).
        let locked = tokio::time::timeout(std::time::Duration::from_millis(500), gw.lock()).await;
        assert!(
            locked.is_ok(),
            "gateway lock must be free while a dispatch wait is parked"
        );
        drop(locked);
        let _ = waiter.await;
    }
}
