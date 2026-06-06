//! v0.8.6 W5b ResSessions — session resource endpoints over the gateway spine.
//!
//! These are the network face of the live IM gateway's session lifecycle
//! (the W5b spine: [`Gateway::session_views`] /
//! [`Gateway::create_session_api`] / [`Gateway::submit_to_sid`] /
//! [`Gateway::stop_session`]). The web server runs in the same daemon
//! process that owns the in-memory `s{n}` session map, so when a gateway
//! is attached ([`AppState::gateway`] = `Some`) these endpoints drive it
//! directly under its `Mutex`.
//!
//! Routes (all `/api/v1`, behind the shared `auth_layer`):
//!
//! - `GET    /api/v1/projects/{slug}/sessions`        → `[SessionView]` for the slug
//! - `POST   /api/v1/projects/{slug}/sessions`        → create → 201 `{sid}`
//! - `GET    /api/v1/sessions/{sid}`                  → `{sid, events}` history
//! - `POST   /api/v1/sessions/{sid}/turn`             → submit → 202 `{accepted:true}`
//! - `GET    /api/v1/sessions/{sid}/events`           → SSE (filtered by `sid`)
//! - `POST   /api/v1/sessions/{sid}/stop`             → stop → 200 `{stopped:true}`
//!
//! **No-gateway contract (locked W5b)**: the standalone "internal web"
//! path runs without a daemon gateway ([`AppState::gateway`] = `None`).
//! Every session endpoint then returns **503** — there is no live session
//! map to act on. The 503 short-circuit is the first thing each handler
//! checks.
//!
//! **SSE filter key (cross-stage from the spine)**: a per-session SSE
//! handler keeps only events whose `sid` matches its target. Every
//! web-API session shares `chat_id == "web-api"`, so filtering MUST be on
//! the `sid` field — never `chat_id`. The event source is the **gateway's
//! own event stream** ([`Gateway::subscribe_events`](ccteam_im::gateway::Gateway::subscribe_events)),
//! a broadcast tee of every [`GatewayEvent`](ccteam_im::gateway::GatewayEvent)
//! the gateway emits (pump answers + progress, turn-timeout, choice prompts).
//! Each event tied to a tracked session carries `sid == Some("s{n}")`, so
//! this handler keeps the ones whose `sid` matches its target and drops the
//! rest. (The earlier file-watcher `EventBus` source only ever saw flat
//! `<slug>.jsonl` progress with `sid == None`, so a real session got nothing
//! but keep-alives — fix #2.)
//!
//! **History**: the gateway keeps no in-memory transcript, so the history
//! endpoint tails the per-session source on disk. The gateway session id
//! (`s{n}`) never appears in the flat `<slug>.jsonl` progress, so the
//! handler resolves `sid → {role, project_dir}` via
//! [`Gateway::session_resolve`](ccteam_im::gateway::Gateway::session_resolve)
//! (under the gateway lock, which it drops before the blocking fs read)
//! then reads the ccteam-owned mirror
//! `<project_dir>/.ccteam/chat/<role>/turns.jsonl` via
//! [`read_all_turns`](ccteam_harness::execution::turns_mirror::read_all_turns).
//! It is best-effort: an empty `events` array is a valid 200 when nothing
//! has been written yet. A `sid` unknown to the gateway is a 404.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use ccteam_harness::execution::turns_mirror::{read_all_turns, TurnRecord};
use ccteam_harness::{AgentVendor, PermissionMode};
use ccteam_im::gateway::GatewayEvent;
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use utoipa::ToSchema;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

/// Keep-alive cadence for the per-session SSE stream. Mirrors
/// [`super::sse`]'s 15s contract (its constant is private; we restate it
/// to keep the same reverse-proxy idle-timeout defeat).
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// 503 body for the no-gateway (standalone internal-web) path. Returned
/// by every session endpoint when [`AppState::gateway`] is `None`.
fn no_gateway() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "no live gateway: session API unavailable on standalone web"})),
    )
        .into_response()
}

/// 404 body for a `sid` the gateway does not track.
fn unknown_session(sid: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": format!("unknown session: {sid}")})),
    )
        .into_response()
}

/// Map a wire vendor token (`"claude"` / `"codex"`, case-insensitive) to
/// the harness [`AgentVendor`]. The web layer owns this mapping so it
/// never depends on the gateway's private `parse_vendor` (the spine note:
/// "map its own request string to AgentVendor before calling
/// create_session_api"). Matches `AgentVendor`'s lowercase serde form.
fn parse_vendor(raw: &str) -> Result<AgentVendor, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" => Ok(AgentVendor::Claude),
        "codex" => Ok(AgentVendor::Codex),
        other => Err(format!("unknown vendor: {other} (expected claude|codex)")),
    }
}

/// `GET /api/v1/projects/{slug}/sessions`
///
/// 200 `[{sid, project, role, vendor, current, status}]` — the gateway's
/// [`SessionView`](ccteam_im::gateway::SessionView)s filtered to this
/// project. Empty array when the project has no live sessions. 503 with no
/// gateway.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/sessions",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Live sessions `[{sid, project, role, vendor, permission_mode, current, status}]`", body = serde_json::Value),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_list_sessions(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // session_views() is sync after the lock — no `.await` is held under it.
    let views = {
        let guard = gw.lock().await;
        guard
            .session_views()
            .into_iter()
            .filter(|v| v.project == slug)
            .collect::<Vec<_>>()
    };
    Json(views).into_response()
}

/// POST body for session creation — `role` (required), `vendor` (optional,
/// defaults `claude`), `permission_mode` (optional, `skip` default / `hitl`).
/// Accepts form or JSON via [`FormOrJson`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSessionForm {
    pub role: String,
    #[serde(default)]
    pub vendor: Option<String>,
    /// v0.8.7 W2 (DB.1) — `"skip"` (default) or `"hitl"`. Hitl drops the
    /// skip flag at spawn so non-allowlist tool calls prompt the IM user.
    #[serde(default)]
    pub permission_mode: Option<String>,
}

/// `POST /api/v1/projects/{slug}/sessions`
///
/// Creates (or idempotently reuses) a `(project, role)` session via the
/// spine. 201 `{sid}` on success. 400 on a bad vendor token or empty
/// role. 422 when the named role has no `.claude/agents/<role>.md` (a caller
/// mistake, R-M6). 503 with no gateway. 500 if the gateway create fails for a
/// genuine internal reason (project not registered / adapter spawn error).
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/sessions",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    request_body(content = CreateSessionForm, description = "Session to create (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 201, description = "Created; `{sid}`", body = serde_json::Value),
        (status = 400, description = "Empty role / bad vendor / bad permission_mode"),
        (status = 422, description = "Unknown role (no `.claude/agents/<role>.md`)"),
        (status = 503, description = "No live gateway (standalone web)"),
        (status = 500, description = "Gateway create failed (internal)"),
    ),
)]
pub(crate) async fn handle_create_session(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    FormOrJson(form, mode): FormOrJson<CreateSessionForm>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let role = form.role.trim().to_string();
    if role.is_empty() {
        return create_error(
            StatusCode::BAD_REQUEST,
            "role must not be empty".into(),
            mode,
        );
    }
    let vendor_raw = form.vendor.as_deref().unwrap_or("claude");
    let vendor = match parse_vendor(vendor_raw) {
        Ok(v) => v,
        Err(msg) => return create_error(StatusCode::BAD_REQUEST, msg, mode),
    };
    // v0.8.7 W2 (DB.1) — optional `permission_mode` body field; default skip.
    let permission_mode = match PermissionMode::parse_opt(form.permission_mode.as_deref()) {
        Ok(m) => m,
        Err(msg) => return create_error(StatusCode::BAD_REQUEST, msg, mode),
    };

    let sid = {
        let mut guard = gw.lock().await;
        guard
            .create_session_api(slug.clone(), role.clone(), vendor, permission_mode)
            .await
    };
    match sid {
        Ok(sid) => (StatusCode::CREATED, Json(json!({"sid": sid}))).into_response(),
        // v0.8.7 review-fix (R-M6) — distinguish a caller mistake (the named
        // role has no `.claude/agents/<role>.md`) from a real internal failure
        // (adapter spawn / fs error). A bad role is a client error → 422
        // Unprocessable Entity with the clear hint, NOT a 500.
        Err(err) => {
            if let Some(missing) = err.downcast_ref::<ccteam_im::gateway::RoleNotFound>() {
                tracing::info!(%slug, %role, "create_session_api: unknown role -> 422");
                return create_error(StatusCode::UNPROCESSABLE_ENTITY, missing.to_string(), mode);
            }
            tracing::warn!(%slug, %role, %err, "create_session_api failed");
            create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("create session failed: {err}"),
                mode,
            )
        }
    }
}

/// `GET /api/v1/sessions/{sid}`
///
/// History for one session. The gateway keeps no in-memory transcript, and
/// the gateway session id (`s{n}`) is *not* the `session_id` that ever
/// appears in the flat `<slug>.jsonl` progress — so we resolve the sid to
/// its `{role, project_dir}` via [`Gateway::session_resolve`] (404 if the
/// sid is unknown to the gateway) and read the ccteam-owned per-session
/// mirror `<project_dir>/.ccteam/chat/<role>/turns.jsonl`. Best-effort:
/// returns `{sid, events: []}` (200) when no turn has been mirrored yet (or
/// the file read fails). 503 with no gateway.
///
/// Lock discipline: `session_resolve` is sync (no `.await`) and only clones
/// scalar fields, so we run it under the gateway guard, then **drop the
/// guard** before the blocking `read_all_turns` fs read.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{sid}",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "History `{sid, events:[{turn_id, ts, role, user, assistant}]}`", body = serde_json::Value),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_history(
    State(app): State<AppState>,
    Path(sid): Path<String>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // Resolve sid → role + project_dir under the lock (also our 404 gate),
    // then drop the guard before touching the filesystem.
    let resolved = {
        let guard = gw.lock().await;
        guard.session_resolve(&sid)
    };
    let Some(resolved) = resolved else {
        return unknown_session(&sid);
    };
    let events = collect_session_turns(&resolved.project_dir, &resolved.role);
    Json(json!({ "sid": sid, "events": events })).into_response()
}

/// Reconstruct a session's history from its ccteam-owned transcript mirror
/// `<project_dir>/.ccteam/chat/<role>/turns.jsonl` (the same file the W1
/// `session_collect` path reads). Each [`TurnRecord`] becomes one event
/// object; any read error folds to an empty list — a best-effort history
/// view (an absent file is the legitimate first-turn case, which
/// [`read_all_turns`] already returns as `Ok(empty)`). Split out from the
/// handler so the disk → events mapping is unit-testable without a live
/// gateway.
fn collect_session_turns(project_dir: &std::path::Path, role: &str) -> Vec<serde_json::Value> {
    match read_all_turns(project_dir, role) {
        Ok(turns) => turns.iter().map(turn_to_event).collect(),
        Err(_) => Vec::new(),
    }
}

/// Map one mirrored [`TurnRecord`] to the history event shape the SPA
/// renders. Keeps the user prompt + assistant reply + turn id/ts so a
/// reopened per-session page can seed its transcript before the live SSE
/// takes over.
fn turn_to_event(turn: &TurnRecord) -> serde_json::Value {
    json!({
        "turn_id": turn.turn_id,
        "ts": turn.ts,
        "role": turn.role,
        "user": turn.user,
        "assistant": turn.assistant,
    })
}

/// POST body for a turn submission — `text` (required). Form or JSON.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TurnForm {
    pub text: String,
}

/// `POST /api/v1/sessions/{sid}/turn`
///
/// Submits a user-text turn to the session via the spine. The turn
/// executes asynchronously — the reply + progress arrive over
/// `GET /api/v1/sessions/{sid}/events` — so success is 202
/// `{accepted:true}`. 404 for an unknown sid (the gateway returns `Err`),
/// 503 with no gateway, 400 on empty text.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/turn",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    request_body(content = TurnForm, description = "Turn text (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 202, description = "Accepted; reply/progress arrive over `/events`. `{accepted:true}`", body = serde_json::Value),
        (status = 400, description = "Empty text"),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_turn(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    FormOrJson(form, mode): FormOrJson<TurnForm>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    if form.text.trim().is_empty() {
        return create_error(
            StatusCode::BAD_REQUEST,
            "text must not be empty".into(),
            mode,
        );
    }
    let result = {
        let mut guard = gw.lock().await;
        guard.submit_to_sid(&sid, form.text).await
    };
    match result {
        Ok(_turn_id) => (StatusCode::ACCEPTED, Json(json!({"accepted": true}))).into_response(),
        Err(err) => {
            // submit_to_sid returns Err for an unknown sid → 404. We can't
            // cleanly distinguish that from a genuine submit error without
            // probing first; an unknown-sid 404 is the meaningful client
            // signal, so prefer it (a stale sid is the common failure).
            tracing::warn!(%sid, %err, "submit_to_sid failed");
            unknown_session(&sid)
        }
    }
}

/// POST body for resolving a pending choice (the HITL approve/deny path) —
/// `token` (the pending-resolution token carried on the SSE choice frame) +
/// `selection` (the chosen option `id`, e.g. `"allow"` / `"deny"`). Form or
/// JSON. v0.8.7 review-fix (R-H1).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveForm {
    /// Pending-resolution token (from the SSE choice frame's `token`).
    pub token: String,
    /// Chosen option id (the SSE choice frame's `options[].id`).
    pub selection: String,
}

/// `POST /api/v1/sessions/{sid}/resolve`
///
/// v0.8.7 review-fix (R-H1) — resolve a token-keyed pending choice (the web
/// HITL approve/deny click) through the SAME gateway machinery an IM option
/// click uses ([`Gateway::resolve_web_selection`] → `take_by_token` →
/// `apply_pending`). This is **not** a turn: it delivers the decision to the
/// blocked `permission/ask` hook so `[Approve]` makes the tool actually run
/// and `[Deny]` denies immediately (no 600s timeout). 200 `{resolved:true}`
/// on success. 400 on empty token/selection. 404 (clean 4xx, never a turn)
/// when the token is unknown/expired or the selection is not a valid option
/// for that prompt. 503 with no gateway.
///
/// The `{sid}` is the addressing namespace for parity with the other session
/// endpoints; resolution itself is token-global (a pending is keyed by its
/// token, unique per outstanding prompt), so the token is the authority.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/resolve",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    request_body(content = ResolveForm, description = "Pending resolution (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 200, description = "Resolved; the decision was delivered to the waiting hook. `{resolved:true}`", body = serde_json::Value),
        (status = 400, description = "Empty token or selection"),
        (status = 404, description = "Unknown/expired token or invalid selection (NOT submitted as a turn)"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_resolve(
    State(app): State<AppState>,
    Path(sid): Path<String>,
    FormOrJson(form, mode): FormOrJson<ResolveForm>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let token = form.token.trim();
    let selection = form.selection.trim();
    if token.is_empty() || selection.is_empty() {
        return create_error(
            StatusCode::BAD_REQUEST,
            "token and selection must not be empty".into(),
            mode,
        );
    }
    let result = {
        // resolve_web_selection takes &self (the pending registry is behind its
        // own inner lock), so a shared guard suffices.
        let guard = gw.lock().await;
        guard.resolve_web_selection(token, selection).await
    };
    match result {
        Ok(()) => Json(json!({"resolved": true})).into_response(),
        Err(err) => {
            // Unknown/expired token or a bad option id — a clean 4xx, never a
            // turn (the whole point of R-H1). 404 mirrors the unknown-session
            // shape the other session endpoints return.
            tracing::warn!(%sid, %err, "resolve_web_selection failed");
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("resolve failed: {err}")})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/sessions/{sid}/events`
///
/// SSE stream for one session. Subscribes to the gateway's event broadcast
/// ([`Gateway::subscribe_events`](ccteam_im::gateway::Gateway::subscribe_events))
/// and keeps only [`GatewayEvent`]s whose `sid` matches this session id —
/// the cross-stage filter key. 15s keep-alive; a lagging consumer (broadcast
/// `Lagged`) gets a synthetic `reconnect_hint` then the stream closes for the
/// SPA's `EventSource` to auto-reconnect.
///
/// No-gateway: a 503 here would close the `EventSource` and the SPA would
/// retry-loop, so we instead emit a single `gateway_unavailable` SSE frame
/// and keep an (empty) keep-alive stream open — the SPA shows "no live
/// gateway" without hammering reconnects.
///
/// Unknown sid (gateway present but no session by that id) is *not* a 404
/// here: the stream simply never matches, so only keep-alives flow. A
/// session created concurrently then starts matching live — closing the
/// stream on a momentarily-unknown sid would race that.
///
/// OpenAPI note: this is a **Server-Sent Events** stream, which OpenAPI
/// cannot fully model as a JSON response body. The response is declared
/// as `text/event-stream`; each `event: progress` frame's `data` is a
/// JSON line `{id, sid, kind:"answer"|"progress", content, done?, options?}`.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{sid}/events",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "SSE stream (text/event-stream). Frames: `event: progress` with `data` = `{id, sid, kind, content, done?, options?}`. Never 503 — a no-gateway path emits one `gateway_unavailable` frame then keep-alives.", content_type = "text/event-stream"),
    ),
)]
pub(crate) async fn handle_session_events(
    State(app): State<AppState>,
    Path(sid): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Subscribe under a brief lock (subscribe_events only clones a Sender +
    // registers a Receiver; no `.await` is held under the guard). `None`
    // gateway keeps the standalone no-gateway contract.
    let rx = match app.gateway.as_ref() {
        Some(gw) => Some(gw.lock().await.subscribe_events()),
        None => None,
    };
    let target_sid = sid.clone();
    // Unify both arms into one stream type (`Either`) so the function has a
    // single `impl Stream` return. With a gateway: the filtered broadcast.
    // Without: a one-shot `gateway_unavailable` notice (then only keep-alives).
    let stream = match rx {
        Some(rx) => BroadcastStream::new(rx)
            .filter_map(move |item| {
                let target_sid = target_sid.clone();
                async move {
                    match item {
                        Ok(ev) if event_matches_sid(&ev, &target_sid) => {
                            Some(Ok(session_event(&ev)))
                        }
                        Ok(_) => None,
                        Err(BroadcastStreamRecvError::Lagged(n)) => {
                            Some(Ok(reconnect_hint(&format!("lagged {n} events"))))
                        }
                    }
                }
            })
            .left_stream(),
        None => futures::stream::iter(vec![Ok(gateway_unavailable_event())]).right_stream(),
    };
    Sse::new(stream).keep_alive(KeepAlive::default().interval(KEEPALIVE_INTERVAL))
}

/// `POST /api/v1/sessions/{sid}/stop`
///
/// Stops (deregisters) the session via the spine. 200 `{stopped:true}`.
/// 404 for an unknown sid. 503 with no gateway. Never file-purges — the
/// spine's `stop_session` is deregister-only.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/stop",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "Stopped (deregistered). `{stopped:true}`", body = serde_json::Value),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_stop(
    State(app): State<AppState>,
    Path(sid): Path<String>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let result = {
        let mut guard = gw.lock().await;
        guard.stop_session(&sid).await
    };
    match result {
        Ok(()) => Json(json!({"stopped": true})).into_response(),
        Err(err) => {
            tracing::warn!(%sid, %err, "stop_session failed");
            unknown_session(&sid)
        }
    }
}

/// The per-session SSE filter key (cross-stage from the spine): keep a
/// [`GatewayEvent`] iff its `sid` is exactly `Some(target)`. Events with a
/// different `sid` — or none at all (the `chat_send_file` MCP path, the D6
/// `interaction/ask` hook prompt) — are dropped.
fn event_matches_sid(ev: &GatewayEvent, target: &str) -> bool {
    ev.sid.as_deref() == Some(target)
}

/// Build the `event: progress` SSE frame for one [`GatewayEvent`]. The
/// payload is a single-line JSON object carrying the event `id`, its `sid`,
/// a `kind` label (`"answer"` / `"progress"`, with `done` for a finalizing
/// progress update), and the user-visible `content`. Choice prompts arrive
/// as `Answer` events whose `options` are non-empty; those are surfaced too
/// so the SPA can render them. The SSE event name stays `progress` so the
/// SPA's existing per-session parser handles it unchanged.
fn session_event(ev: &GatewayEvent) -> Event {
    Event::default()
        .event("progress")
        .data(session_event_payload(ev).to_string())
}

/// The JSON payload [`session_event`] serializes (split out for unit tests —
/// asserting on an `axum` `Event`'s rendered body is awkward).
///
/// v0.8.7 review-fix (R-H1): a choice prompt (e.g. the HITL approve/deny
/// bubble) now also carries the resolution `token` plus, per option, its
/// stable `id` — so the SPA can resolve the pending via
/// `POST /api/v1/sessions/{sid}/resolve {token, selection=id}` (the SAME
/// token-keyed pending the IM click resolves), instead of misfiring the
/// option index as a brand-new turn (which never resolved the pending and
/// let the blocked permission hook time out to deny). The `options` array
/// stays backward-friendly: each entry is `{label, id}`. The token is parsed
/// from the option callback `data` (`"{token}:{idx}"`), the single source of
/// the token on the wire.
fn session_event_payload(ev: &GatewayEvent) -> serde_json::Value {
    use ccteam_im::gateway::GatewayEventKind;
    let (kind, done) = match &ev.kind {
        GatewayEventKind::Answer => ("answer", false),
        GatewayEventKind::Progress { done, .. } => ("progress", *done),
    };
    let mut payload = json!({
        "id": ev.id,
        "sid": ev.sid,
        "kind": kind,
        "content": ev.content,
    });
    if done {
        payload["done"] = serde_json::Value::Bool(true);
    }
    if !ev.options.is_empty() {
        payload["options"] = serde_json::Value::Array(
            ev.options
                .iter()
                .map(|o| json!({ "label": o.label, "id": o.id }))
                .collect(),
        );
        if let Some(token) = approval_token(ev) {
            payload["token"] = serde_json::Value::String(token);
        }
    }
    payload
}

/// Extract the pending-resolution token from a choice-prompt event. Every
/// option's callback `data` is `"{token}:{idx}"` (the single on-wire carrier
/// of the token), so the token is the prefix before the first `:` of the
/// first option. `None` when there are no options or the shape is unexpected
/// (the SPA then omits the resolve affordance rather than guess).
fn approval_token(ev: &GatewayEvent) -> Option<String> {
    let data = &ev.options.first()?.data;
    data.split_once(':').map(|(token, _)| token.to_string())
}

/// Synthetic lag/close frame — mirrors [`super::sse`]'s `reconnect_hint`.
fn reconnect_hint(reason: &str) -> Event {
    Event::default()
        .event("reconnect_hint")
        .data(json!({ "type": "reconnect_hint", "reason": reason }).to_string())
}

/// One-shot notice emitted on the no-gateway SSE path.
fn gateway_unavailable_event() -> Event {
    Event::default()
        .event("gateway_unavailable")
        .data(json!({ "type": "gateway_unavailable", "reason": "no live gateway" }).to_string())
}

/// Shared POST error responder honoring the [`FormOrJson`] mode
/// convention: form ⇒ plain text, JSON ⇒ `{ "ok": false, "error": ... }`.
fn create_error(status: StatusCode, msg: String, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => (status, msg).into_response(),
        InputMode::Json => (status, Json(json!({"ok": false, "error": msg}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vendor_accepts_both_case_insensitive() {
        assert_eq!(parse_vendor("claude").unwrap(), AgentVendor::Claude);
        assert_eq!(parse_vendor("Codex").unwrap(), AgentVendor::Codex);
        assert_eq!(parse_vendor("  CLAUDE ").unwrap(), AgentVendor::Claude);
    }

    #[test]
    fn parse_vendor_rejects_unknown() {
        assert!(parse_vendor("gemini").is_err());
        assert!(parse_vendor("").is_err());
    }

    #[test]
    fn create_session_form_parses_optional_permission_mode() {
        // v0.8.7 W2 (DB.1) — JSON body with permission_mode → parsed; absent
        // field → None → default skip at the handler.
        let with: CreateSessionForm =
            serde_json::from_str(r#"{"role":"r","permission_mode":"hitl"}"#).unwrap();
        assert_eq!(with.permission_mode.as_deref(), Some("hitl"));
        assert_eq!(
            PermissionMode::parse_opt(with.permission_mode.as_deref()).unwrap(),
            PermissionMode::Hitl
        );

        let without: CreateSessionForm = serde_json::from_str(r#"{"role":"r"}"#).unwrap();
        assert!(without.permission_mode.is_none());
        assert_eq!(
            PermissionMode::parse_opt(without.permission_mode.as_deref()).unwrap(),
            PermissionMode::Skip,
            "absent permission_mode ⇒ skip"
        );

        // A bad token is rejected at the API edge (→ 400).
        let bad: CreateSessionForm =
            serde_json::from_str(r#"{"role":"r","permission_mode":"nope"}"#).unwrap();
        assert!(PermissionMode::parse_opt(bad.permission_mode.as_deref()).is_err());
    }

    #[test]
    fn collect_session_turns_reads_mirrored_turns() {
        // v0.8.7 W4 — the history handler reads the ccteam-owned mirror
        // `<project_dir>/.ccteam/chat/<role>/turns.jsonl` (resolved from the
        // gateway sid), NOT the flat progress.jsonl. Seed two turns and one
        // garbage line; expect two well-formed history events in order.
        use ccteam_harness::execution::turns_mirror::append_turn;
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        let role = "reviewer";
        let mk = |id: &str, user: &str, assistant: &str| TurnRecord {
            turn_id: id.into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: role.into(),
            user: user.into(),
            assistant: assistant.into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
        };
        append_turn(project_dir, role, &mk("t1", "review the diff", "LGTM")).unwrap();
        append_turn(project_dir, role, &mk("t2", "and the tests?", "all green")).unwrap();
        // A half-flushed / garbage line must be skipped (read_all_turns drops it).
        let path = ccteam_harness::execution::turns_mirror::turns_jsonl_path(project_dir, role);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "not-json").unwrap();
        }
        let events = collect_session_turns(project_dir, role);
        assert_eq!(events.len(), 2, "two parseable turns → two events");
        assert_eq!(events[0]["turn_id"], "t1");
        assert_eq!(events[0]["user"], "review the diff");
        assert_eq!(events[0]["assistant"], "LGTM");
        assert_eq!(events[1]["turn_id"], "t2");
        assert_eq!(events[1]["assistant"], "all green");
    }

    #[test]
    fn turn_to_event_carries_user_and_assistant() {
        let turn = TurnRecord {
            turn_id: "t9".into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: "cto".into(),
            user: "spawn a reviewer".into(),
            assistant: "done — s2".into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
        };
        let ev = turn_to_event(&turn);
        assert_eq!(ev["turn_id"], "t9");
        assert_eq!(ev["role"], "cto");
        assert_eq!(ev["user"], "spawn a reviewer");
        assert_eq!(ev["assistant"], "done — s2");
    }

    /// Build a minimal [`GatewayEvent`] with the given `sid` for filter tests.
    fn gw_event(sid: Option<&str>) -> GatewayEvent {
        use ccteam_im::gateway::GatewayEventKind;
        GatewayEvent {
            id: "e1".into(),
            channel: "web".into(),
            chat_id: "web-api".into(),
            thread_ts: None,
            content: "hi".into(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: sid.map(str::to_string),
        }
    }

    #[test]
    fn event_matches_sid_keeps_target_drops_others() {
        // Target sid passes; a different sid and a None sid are dropped — the
        // cross-stage SSE filter key.
        assert!(event_matches_sid(&gw_event(Some("s1")), "s1"));
        assert!(!event_matches_sid(&gw_event(Some("s2")), "s1"));
        assert!(!event_matches_sid(&gw_event(None), "s1"));
    }

    #[test]
    fn session_event_maps_answer_and_progress() {
        use ccteam_im::gateway::GatewayEventKind;
        // Answer → kind "answer", sid + content carried, no `done` key.
        let answer = session_event_payload(&gw_event(Some("s1")));
        assert_eq!(answer["kind"], "answer");
        assert_eq!(answer["sid"], "s1");
        assert_eq!(answer["content"], "hi");
        assert!(answer.get("done").is_none());
        // No options ⇒ no token / no options key.
        assert!(answer.get("options").is_none());
        assert!(answer.get("token").is_none());

        // Finalizing progress → kind "progress" + done:true.
        let mut prog = gw_event(Some("s1"));
        prog.kind = GatewayEventKind::Progress {
            status_key: "s1-0".into(),
            done: true,
        };
        let prog = session_event_payload(&prog);
        assert_eq!(prog["kind"], "progress");
        assert_eq!(prog["done"], true);
    }

    /// v0.8.7 review-fix (R-H1) — an approval ChoicePrompt event serializes its
    /// resolution `token` plus, per option, `{label, id}`, so the SPA can
    /// resolve via `POST /resolve {token, selection=id}` (NOT misfire the index
    /// as a turn). The token is parsed from the option callback `data`
    /// (`"{token}:{idx}"`), the single on-wire carrier.
    #[test]
    fn session_event_carries_token_and_option_ids_for_approval() {
        use ccteam_im::transport::MessageOption;
        let mut ev = gw_event(Some("s7"));
        ev.content = "session s7 (cto) wants to run: Bash rm -rf /".into();
        ev.options = vec![
            MessageOption {
                data: "pcafef00d:0".into(),
                label: "✅ Approve".into(),
                id: "allow".into(),
            },
            MessageOption {
                data: "pcafef00d:1".into(),
                label: "⛔ Deny".into(),
                id: "deny".into(),
            },
        ];
        let payload = session_event_payload(&ev);
        // token lifted from the option `data` prefix.
        assert_eq!(payload["token"], "pcafef00d");
        // each option carries its stable id (the decision value) + label.
        let opts = payload["options"].as_array().expect("options array");
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["label"], "✅ Approve");
        assert_eq!(opts[0]["id"], "allow");
        assert_eq!(opts[1]["id"], "deny");
    }

    #[test]
    fn approval_token_parses_prefix_and_handles_empty() {
        use ccteam_im::transport::MessageOption;
        let mut ev = gw_event(Some("s1"));
        ev.options = vec![MessageOption {
            data: "ptok:0".into(),
            label: "x".into(),
            id: "allow".into(),
        }];
        assert_eq!(approval_token(&ev).as_deref(), Some("ptok"));
        // No options ⇒ None (the payload then omits the resolve affordance).
        let bare = gw_event(Some("s1"));
        assert!(approval_token(&bare).is_none());
    }

    #[test]
    fn collect_session_turns_missing_file_is_empty() {
        // Absent turns.jsonl is the legitimate first-turn case → empty (200),
        // not an error. read_all_turns returns Ok(empty) for a missing file.
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(collect_session_turns(tmp.path(), "ghost").is_empty());
    }
}
