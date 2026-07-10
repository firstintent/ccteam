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

impl McpDispatch {
    /// Dispatch one JSON-RPC request. Order matches the historical
    /// `handle_mcp_socket_connection` intercept chain exactly:
    /// `interaction/ask` → `permission/ask` → `chat_send_file` →
    /// `session_*` → `ccteam/reload` → protocol core.
    pub async fn dispatch(&self, req: Value) -> Option<Value> {
        if is_interaction_ask_call(&req) {
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
            Some(execute_session_tool(&req, self.gateway.as_ref()).await)
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

/// v0.8.7 W1 — roles allowed to call the `session_*` scheduling tools. The
/// cto is the chat-first manager; work-roles are blocked (defense-in-depth
/// behind the per-agent tool allow-list). A hard daemon gate so a work-role
/// with a hand-edited allow-list still can't drive the session map.
const SESSION_TOOL_PRIVILEGED_ROLES: &[&str] = &["cto"];

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

/// Monotonic id source so each `chat_send_file` gets a distinct durable
/// ledger row (avoids `{id}-0` collisions in `outbound.jsonl`).
static CHAT_SEND_FILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn is_chat_send_file_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req.pointer("/params/name").and_then(|n| n.as_str()) == Some("ccteam__chat_send_file")
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
// v0.8.7 W1 — cto scheduling: daemon-side `session_*` tool handlers.
//
// The stdio MCP server forwards `ccteam__session_*` calls here (it doesn't
// own the gateway). This is where we (a) enforce the cto-only privilege
// gate on the ambient `_caller_role`, and (b) drive the gateway session map
// (spawn / dispatch / list / stop) or tail a child's transcript (collect).
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
            .is_some_and(|n| n.starts_with("ccteam__session_"))
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

/// v0.8.7 W1 — handle one forwarded `ccteam__session_*` call. Enforces the
/// privilege gate, then dispatches to the gateway. Returns a JSON-RPC
/// response (the stdio side propagates `isError` to the agent).
async fn execute_session_tool(
    req: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let name = req
        .pointer("/params/name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // The stdio forwarder injects `_caller_role` / `_caller_secret` from the
    // spawn-time pane env (`CCTEAM_CHAT_ROLE` / `CCTEAM_CHAT_SECRET`), not from
    // caller-supplied tool args. We treat both as UNTRUSTED until the secret is
    // checked against the gateway's `sid -> {role, secret}` map below.
    let caller_role = args
        .get("_caller_role")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let caller_secret = args
        .get("_caller_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Layer 1 (CHEAP PRE-FILTER, *not* the security boundary): reject an
    // obviously non-privileged role before touching any state, so a non-cto
    // caller is denied even when the gateway is unavailable. This is NOT a
    // trust boundary on its own — a plaintext role arg is forgeable; the real
    // authentication is the layer-2 secret check below.
    if !session_caller_authorized(caller_role) {
        let who = if caller_role.is_empty() {
            "<unknown>"
        } else {
            caller_role
        };
        return session_tool_response(
            id,
            format!(
                "{name}: permission denied — session scheduling tools are restricted to the {SESSION_TOOL_PRIVILEGED_ROLES:?} role(s); caller role is `{who}`"
            ),
            true,
        );
    }

    // The gateway must be running (web + IM both up). Mirror chat_send_file's
    // "IM gateway not running" structured error rather than panicking. It is
    // also REQUIRED to authenticate the caller (the secret map lives there), so
    // a missing gateway is a hard stop — never a fall-through that would skip
    // the secret check.
    let Some(gateway) = gateway else {
        return session_tool_response(
            id,
            format!("{name}: gateway not running (start ccteam with web + IM enabled)"),
            true,
        );
    };

    // Layer 2 (THE SECURITY-RELEVANT CHECK, best-effort defense-in-depth):
    // authenticate the caller by matching the `(role, secret)` PAIR it presents
    // against a tracked session, instead of trusting the plaintext role. A
    // missing / wrong secret fails closed. HONEST SCOPE: under the single-uid
    // full-trust model this only RAISES THE BAR (a same-uid process can read
    // another pane's env and recover the secret); it is NOT a hard boundary.
    // Real isolation = per-agent OS user / sandbox (v0.8.8-deferred).
    {
        let gw = gateway.lock().await;
        if !gw.verify_session_caller(caller_role, caller_secret) {
            return session_tool_response(
                id,
                format!(
                    "{name}: permission denied — caller could not be authenticated (no live `{caller_role}` session holds the presented secret)"
                ),
                true,
            );
        }
    }

    match run_session_tool(&name, &args, gateway).await {
        Ok(text) => session_tool_response(id, text, false),
        Err(text) => session_tool_response(id, text, true),
    }
}

/// v0.8.7 W1 — layer-1 privilege PRE-FILTER (not the security boundary): is
/// `caller_role` in the privileged set? Cheap + unit-testable without a
/// gateway. The authoritative check is [`Gateway::verify_session_caller`],
/// which matches the secret; this only short-circuits the obvious non-cto case.
fn session_caller_authorized(caller_role: &str) -> bool {
    SESSION_TOOL_PRIVILEGED_ROLES.contains(&caller_role)
}

/// Dispatch a privileged `session_*` call to the gateway. Returns `Ok(body)`
/// (a pretty JSON string) on success, `Err(msg)` on a tool-level error.
async fn run_session_tool(
    name: &str,
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    match name {
        "ccteam__session_spawn" => run_session_spawn(args, gateway).await,
        "ccteam__session_dispatch" => run_session_dispatch(args, gateway).await,
        "ccteam__session_collect" => run_session_collect(args, gateway).await,
        "ccteam__session_list" => run_session_list(gateway).await,
        "ccteam__session_stop" => run_session_stop(args, gateway).await,
        other => Err(format!("unknown session tool: {other}")),
    }
}

/// `session_spawn` — create (or reuse) a work-role session in the caller's
/// bound project. The project is ALWAYS the ambient `_caller_slug` (the cto's
/// project); `vendor` is honored where it makes sense.
///
/// v0.8.7 review-fix (R-M3) — the spawnable project is pinned to the caller's
/// own slug, so a cto bound to project A can never spawn into project B. The
/// previously-informational `project` arg is gone (it was a cross-project
/// foot-gun); the gateway resolves the slug → cwd from its own project map.
async fn run_session_spawn(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "session_spawn: missing `role`".to_string())?
        .to_string();
    // The session is always created in the cto's bound project (ambient slug),
    // never a caller-named project — project-scoping the gate (R-M3).
    let project = args
        .get("_caller_slug")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "session_spawn: no project (ambient slug unset)".to_string())?;
    let vendor = parse_session_vendor(args)?;
    // v0.8.7 W2 (DB.1) — optional `permission_mode` arg (`skip` default /
    // `hitl`). Lets the cto spawn a supervised work-role whose non-allowlist
    // tools require IM approval.
    let permission_mode = ccteam_harness::PermissionMode::parse_opt(
        args.get("permission_mode").and_then(|v| v.as_str()),
    )
    .map_err(|e| format!("session_spawn: {e}"))?;

    let mut gw = gateway.lock().await;
    let created = gw
        .create_session_api(project.clone(), role.clone(), vendor, permission_mode)
        .await
        .map_err(|e| format!("session_spawn failed: {e}"))?;
    drop(gw);
    let mut body = serde_json::json!({
        "ok": true,
        "sid": created.sid,
        "project": project,
        "role": role,
        "permission_mode": permission_mode.as_str(),
        "hint": "dispatch a task with session_dispatch{sid, task}, then poll session_collect{sid}.",
    });
    if let Some(model_warning) = created.model_warning {
        body["model_warning"] = serde_json::json!(model_warning);
    }
    Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string()))
}

/// `session_dispatch` — forward a task as a user turn to a session by sid.
async fn run_session_dispatch(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "session_dispatch: missing `task`".to_string())?
        .to_string();
    // R-M3 — only operate sessions in the caller's own project.
    assert_caller_owns_session("session_dispatch", args, gateway, &sid).await?;

    let mut gw = gateway.lock().await;
    let turn_id = gw
        .submit_to_sid(&sid, task)
        .await
        .map_err(|e| format!("session_dispatch failed: {e}"))?;
    drop(gw);
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "sid": sid,
        "turn_id": turn_id,
        "hint": "the child runs asynchronously; poll session_collect{sid, since: turn_id} for its answer.",
    }))
    .unwrap_or_else(|_| "{}".to_string()))
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
    rows.truncate(n);
    let last_turn_id = rows
        .last()
        .and_then(|r| r.get("turn_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    (rows, last_turn_id, truncated)
}

/// `session_collect` — tail the child's `turns.jsonl` (assistant turns).
/// Polled MVP: resolve sid → role + project_dir under the lock, drop the
/// guard, then read the ccteam-owned mirror. `since` is a turn_id cursor.
async fn run_session_collect(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    let since = args.get("since").and_then(|v| v.as_str()).map(String::from);
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|x| x as usize)
        .unwrap_or(SESSION_COLLECT_DEFAULT_N);
    // R-M3 — only collect from sessions in the caller's own project.
    assert_caller_owns_session("session_collect", args, gateway, &sid).await?;

    // Resolve under the lock (sync), then DROP the guard before the fs read.
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve(&sid)
    };
    let resolved = resolved.ok_or_else(|| format!("session_collect: unknown session: {sid}"))?;

    // Tail the ccteam-owned transcript mirror.
    // v0.8.8 F1 — the mirror is keyed by `sid` (`.ccteam/chat/<sid>/turns.jsonl`),
    // not role, so read by `resolved.sid` (role is a content label only).
    let all = ccteam_harness::execution::turns_mirror::read_all_turns(
        &resolved.project_dir,
        &resolved.sid,
    )
    .map_err(|e| format!("session_collect: read turns.jsonl for {sid}: {e}"))?;

    // Apply the `since` cursor + page forward (R-L3 — oldest-first, no silent
    // drop of a > `n` burst). Pure logic in `page_collected_turns`.
    let (rows, last_turn_id, truncated) = page_collected_turns(&all, since.as_deref(), n);

    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "sid": sid,
        "role": resolved.role,
        "turns": rows,
        // Cursor to pass as `since` on the next poll (None when no turns yet).
        // On truncation this is the boundary turn → poll again to get the rest.
        "cursor": last_turn_id,
        // True when more turns than `n` were available after `since`; the caller
        // should poll again with `cursor` to page through the remainder.
        "truncated": truncated,
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

/// `session_list` — snapshot the gateway's live sessions.
async fn run_session_list(gateway: &GatewayHandle) -> std::result::Result<String, String> {
    let views = {
        let gw = gateway.lock().await;
        gw.session_views()
    };
    let rows: Vec<serde_json::Value> = views
        .iter()
        .map(|v| {
            serde_json::json!({
                "sid": v.sid,
                "project": v.project,
                "role": v.role,
                "vendor": v.vendor,
                "current": v.current,
                "status": v.status,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "sessions": rows,
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

/// `session_stop` — deregister + close a session by sid (explicit command).
async fn run_session_stop(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    // R-M3 — only stop sessions in the caller's own project (explicit command,
    // never a proactive kill; the scope check just prevents cross-project stop).
    assert_caller_owns_session("session_stop", args, gateway, &sid).await?;
    let mut gw = gateway.lock().await;
    gw.stop_session(&sid)
        .await
        .map_err(|e| format!("session_stop failed: {e}"))?;
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
) -> std::result::Result<(), String> {
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
            other => Err(format!(
                "session_spawn: invalid vendor `{other}`: expected `claude`, `codex`, or `grok`"
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

    // v0.8.7 review-fix (R-M1/R-M3) — a no-process stub adapter so a real
    // `Gateway` can mint per-session secrets + track project scope without
    // spawning a `claude` pane. `start_thread` records the `(sid, secret)` the
    // gateway minted so the test can present the real secret to the gate.
    #[derive(Clone, Default)]
    struct StubAdapter {
        spawns: std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
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
            _input: ccteam_harness::TurnInput,
        ) -> std::result::Result<ccteam_harness::TurnId, ccteam_harness::HarnessError> {
            Ok(ccteam_harness::TurnId::new(format!("turn-{}", h.identity)))
        }
        fn events(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> futures::stream::BoxStream<'static, ccteam_harness::ThreadEvent> {
            Box::pin(futures::stream::empty())
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

    /// v0.8.7 review-fix (R-M1) — end-to-end: a cto presenting the WRONG secret
    /// is rejected by `execute_session_tool` even though its plaintext role is
    /// `cto` (the secret is the authoritative check, not the role arg).
    #[tokio::test]
    async fn execute_session_tool_rejects_cto_with_wrong_secret() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "ccteam__session_list",
            json!({
                "_caller_role": "cto",
                "_caller_slug": "alpha",
                "_caller_secret": "ffffffffffffffffffffffffffffffff",
            }),
        );
        let resp = execute_session_tool(&req, Some(&gw)).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("could not be authenticated"),
            "wrong secret must fail auth, got: {text}"
        );
    }

    /// v0.8.7 review-fix (R-M1) — end-to-end: the CORRECT cto `(role, secret)`
    /// pair passes the gate and the call reaches the gateway (session_list
    /// returns the live sessions).
    #[tokio::test]
    async fn execute_session_tool_allows_cto_with_correct_secret() {
        let (gw, _cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "ccteam__session_list",
            json!({
                "_caller_role": "cto",
                "_caller_slug": "alpha",
                "_caller_secret": cto_secret,
            }),
        );
        let resp = execute_session_tool(&req, Some(&gw)).await;
        assert_eq!(resp["result"]["isError"], false, "correct secret passes");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"sessions\""), "got: {text}");
    }

    /// v0.8.7 review-fix (R-M3) — end-to-end: a cto authenticated for project
    /// `alpha` is REJECTED when it tries to dispatch/collect/stop a `beta` sid
    /// (cross-project operation), with the correct secret.
    #[tokio::test]
    async fn execute_session_tool_rejects_cross_project_sid() {
        let (gw, _cto_sid, beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        for tool in [
            "ccteam__session_dispatch",
            "ccteam__session_collect",
            "ccteam__session_stop",
        ] {
            let mut args = json!({
                "_caller_role": "cto",
                "_caller_slug": "alpha",
                "_caller_secret": cto_secret.clone(),
                "sid": beta_sid.clone(),
            });
            if tool == "ccteam__session_dispatch" {
                args["task"] = json!("do something");
            }
            let resp = execute_session_tool(&call(tool, args), Some(&gw)).await;
            assert_eq!(resp["result"]["isError"], true, "{tool} must reject");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("permission denied") && text.contains("bound to project `alpha`"),
                "{tool}: cross-project must be denied with a clear reason, got: {text}"
            );
        }
    }

    /// v0.8.7 review-fix (R-M3) — the positive control: the SAME cto operating
    /// its OWN `alpha` sid is allowed (so the scope check isn't blanket-deny).
    #[tokio::test]
    async fn execute_session_tool_allows_same_project_sid() {
        let (gw, cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let resp = execute_session_tool(
            &call(
                "ccteam__session_collect",
                json!({
                    "_caller_role": "cto",
                    "_caller_slug": "alpha",
                    "_caller_secret": cto_secret,
                    "sid": cto_sid,
                }),
            ),
            Some(&gw),
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], false,
            "same-project collect must be allowed: {resp}"
        );
    }

    #[test]
    fn is_session_tool_call_matches_only_session_tools_calls() {
        assert!(is_session_tool_call(&call(
            "ccteam__session_spawn",
            json!({})
        )));
        assert!(is_session_tool_call(&call(
            "ccteam__session_collect",
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
            "params": { "name": "ccteam__session_spawn" }
        })));
    }

    #[test]
    fn gate_allows_cto_and_rejects_everyone_else() {
        assert!(session_caller_authorized("cto"));
        assert!(!session_caller_authorized("reviewer"));
        assert!(!session_caller_authorized("helper"));
        assert!(
            !session_caller_authorized(""),
            "empty role is not privileged"
        );
    }

    /// DA.3 layer 2 — a non-cto caller is REJECTED with isError, and the gate
    /// fires BEFORE any gateway use (so denial holds even with gateway None).
    #[tokio::test]
    async fn execute_session_tool_rejects_non_cto_caller() {
        let req = call(
            "ccteam__session_spawn",
            json!({ "role": "reviewer", "_caller_role": "reviewer", "_caller_slug": "demo" }),
        );
        let resp = execute_session_tool(&req, None).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("permission denied"),
            "non-cto must be denied, got: {text}"
        );
    }

    /// A missing ambient identity (no `_caller_role`) is treated as
    /// unprivileged — denied, never defaulting to allow.
    #[tokio::test]
    async fn execute_session_tool_denies_when_caller_role_absent() {
        let req = call("ccteam__session_list", json!({}));
        let resp = execute_session_tool(&req, None).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("permission denied"), "got: {text}");
    }

    /// DA.3 — a cto caller PASSES the gate. With no gateway wired the next
    /// failure is the structured "gateway not running" (not a permission
    /// denial), proving the gate let the cto through.
    #[tokio::test]
    async fn execute_session_tool_allows_cto_then_reports_gateway_down() {
        let req = call(
            "ccteam__session_list",
            json!({ "_caller_role": "cto", "_caller_slug": "demo" }),
        );
        let resp = execute_session_tool(&req, None).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("permission denied"),
            "cto must pass the gate, got: {text}"
        );
        assert!(
            text.contains("gateway not running"),
            "expected gateway-down error after the gate, got: {text}"
        );
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
        assert!(!is_permission_ask_call(&call(
            "ccteam__session_spawn",
            json!({})
        )));
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
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 10);
        assert_eq!(rows.len(), 10);
        assert!(truncated, "25 > 10 ⇒ truncated");
        assert_eq!(rows[0]["turn_id"], "t0", "oldest-first (not the newest 10)");
        assert_eq!(rows[9]["turn_id"], "t9");
        assert_eq!(cursor.as_deref(), Some("t9"), "cursor = boundary turn");

        // Second poll from the boundary.
        let (rows2, cursor2, truncated2) = page_collected_turns(&all, Some("t9"), 10);
        assert_eq!(rows2.len(), 10);
        assert!(truncated2);
        assert_eq!(
            rows2[0]["turn_id"], "t10",
            "no gap — resumes right after t9"
        );
        assert_eq!(cursor2.as_deref(), Some("t19"));

        // Third poll drains the remainder.
        let (rows3, _c3, truncated3) = page_collected_turns(&all, Some("t19"), 10);
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
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 20);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
        assert_eq!(cursor.as_deref(), Some("t2"));
        // Unknown cursor → all turns (defensive, no loss).
        let (rows_u, _c, trunc_u) = page_collected_turns(&all, Some("ghost"), 20);
        assert_eq!(rows_u.len(), 3);
        assert!(!trunc_u);
    }
}
