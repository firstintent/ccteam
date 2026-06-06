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
//! the `sid` field — never `chat_id`. The web event source here is the
//! file-watcher [`EventBus`](crate::watcher::EventBus): each
//! [`ProgressUpdate`](crate::watcher::ProgressUpdate) carries an optional
//! `sid` parsed from `~/.ccteam/progress/<slug>/<sid>.jsonl`, so this
//! mirrors [`super::sse::handle_sse_project_session`] but matches on the
//! `sid` alone (the API addresses a session by id, not by slug).
//!
//! **History**: the gateway keeps no in-memory transcript, so the history
//! endpoint tails the per-session source on disk — the project's
//! `progress.jsonl`, filtered to lines carrying this `session_id`. It is
//! best-effort: an empty `events` array is a valid 200 when nothing has
//! been written yet. A `sid` unknown to the gateway is a 404.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use ccteam_harness::AgentVendor;
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

/// Keep-alive cadence for the per-session SSE stream. Mirrors
/// [`super::sse`]'s 15s contract (its constant is private; we restate it
/// to keep the same reverse-proxy idle-timeout defeat).
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{slug}/sessions",
            get(handle_list_sessions).post(handle_create_session),
        )
        .route("/api/v1/sessions/{sid}", get(handle_session_history))
        .route("/api/v1/sessions/{sid}/turn", post(handle_session_turn))
        .route("/api/v1/sessions/{sid}/events", get(handle_session_events))
        .route("/api/v1/sessions/{sid}/stop", post(handle_session_stop))
}

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
async fn handle_list_sessions(State(app): State<AppState>, Path(slug): Path<String>) -> Response {
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
/// defaults `claude`). Accepts form or JSON via [`FormOrJson`].
#[derive(Debug, Deserialize)]
pub struct CreateSessionForm {
    pub role: String,
    #[serde(default)]
    pub vendor: Option<String>,
}

/// `POST /api/v1/projects/{slug}/sessions`
///
/// Creates (or idempotently reuses) a `(project, role)` session via the
/// spine. 201 `{sid}` on success. 400 on a bad vendor token or empty
/// role. 503 with no gateway. 500 if the gateway create fails (e.g.
/// project not registered / adapter spawn error).
async fn handle_create_session(
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

    let sid = {
        let mut guard = gw.lock().await;
        guard
            .create_session_api(slug.clone(), role.clone(), vendor)
            .await
    };
    match sid {
        Ok(sid) => (StatusCode::CREATED, Json(json!({"sid": sid}))).into_response(),
        Err(err) => {
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
/// History for one session. The gateway keeps no in-memory transcript, so
/// we resolve the session's project via [`SessionView`] (404 if the sid is
/// unknown to the gateway), then tail that project's `progress.jsonl`,
/// keeping lines whose `session_id == sid`. Best-effort: returns
/// `{sid, events: []}` (200) when nothing matches. 503 with no gateway.
async fn handle_session_history(State(app): State<AppState>, Path(sid): Path<String>) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // Resolve the owning project from the live view (also our 404 gate).
    let project = {
        let guard = gw.lock().await;
        guard
            .session_views()
            .into_iter()
            .find(|v| v.sid == sid)
            .map(|v| v.project)
    };
    let Some(project) = project else {
        return unknown_session(&sid);
    };
    let events = collect_session_progress(&app, &project, &sid);
    Json(json!({ "sid": sid, "events": events })).into_response()
}

/// Reconstruct a session's history from the project `progress.jsonl`,
/// keeping only lines whose `session_id` matches `sid`. Mirrors the
/// progress reconstruction `super::api_v1::build_workflow_session_detail`
/// uses (read → split lines → parse JSON objects → filter by
/// `session_id`), but returns the raw event objects rather than a
/// presentation DTO. Any read/parse miss folds to an empty list — this is
/// a best-effort history view.
fn collect_session_progress(app: &AppState, slug: &str, sid: &str) -> Vec<serde_json::Value> {
    let progress_path = app.paths.progress_jsonl(slug);
    let Ok(body) = std::fs::read_to_string(&progress_path) else {
        return Vec::new();
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e.get("session_id").and_then(|s| s.as_str()) == Some(sid))
        .collect()
}

/// POST body for a turn submission — `text` (required). Form or JSON.
#[derive(Debug, Deserialize)]
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
async fn handle_session_turn(
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

/// `GET /api/v1/sessions/{sid}/events`
///
/// SSE stream for one session. Subscribes to the file-watcher
/// [`EventBus`](crate::watcher::EventBus) and keeps only
/// [`ProgressUpdate`](crate::watcher::ProgressUpdate)s whose `sid` matches
/// this session id — the cross-stage filter key. Mirrors
/// [`super::sse::handle_sse_project_session`] (15s keep-alive; lagging
/// consumers get a synthetic `reconnect_hint` then the stream closes for
/// the SPA's `EventSource` to auto-reconnect).
///
/// No-gateway: a 503 here would close the `EventSource` and the SPA would
/// retry-loop, so we instead emit a single `gateway_unavailable` SSE frame
/// and keep an (empty) keep-alive stream open — the SPA shows "no live
/// gateway" without hammering reconnects.
async fn handle_session_events(
    State(app): State<AppState>,
    Path(sid): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.bus.subscribe();
    let has_gateway = app.gateway.is_some();
    let target_sid = sid.clone();
    let stream = BroadcastStream::new(rx).filter_map(move |item| {
        let target_sid = target_sid.clone();
        async move {
            match item {
                Ok(update) if update.sid.as_deref() == Some(target_sid.as_str()) => {
                    Some(Ok(session_event(&update)))
                }
                Ok(_) => None,
                Err(err) => Some(Ok(reconnect_hint(&format!("{err}")))),
            }
        }
    });
    // Prepend a one-shot `gateway_unavailable` notice on the no-gateway
    // path so the client learns why no session events will arrive (the
    // file watcher still streams, but nothing is sid-tagged for it).
    let prefix = if has_gateway {
        futures::stream::iter(Vec::new())
    } else {
        futures::stream::iter(vec![Ok(gateway_unavailable_event())])
    };
    Sse::new(prefix.chain(stream)).keep_alive(KeepAlive::default().interval(KEEPALIVE_INTERVAL))
}

/// `POST /api/v1/sessions/{sid}/stop`
///
/// Stops (deregisters) the session via the spine. 200 `{stopped:true}`.
/// 404 for an unknown sid. 503 with no gateway. Never file-purges — the
/// spine's `stop_session` is deregister-only.
async fn handle_session_stop(State(app): State<AppState>, Path(sid): Path<String>) -> Response {
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

/// Build the `event: progress` SSE frame for a session update. Same wire
/// shape as [`super::sse`]'s `progress_event` (single-line JSON object
/// with `slug` + `sid` stitched in) so the SPA's existing parser handles
/// it unchanged.
fn session_event(update: &crate::watcher::ProgressUpdate) -> Event {
    let payload = match serde_json::from_str::<serde_json::Value>(&update.event_json) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert(
                "slug".into(),
                serde_json::Value::String(update.slug.clone()),
            );
            if let Some(sid) = &update.sid {
                map.insert("sid".into(), serde_json::Value::String(sid.clone()));
            }
            serde_json::Value::Object(map)
        }
        _ => json!({ "slug": update.slug, "sid": update.sid, "raw": update.event_json }),
    };
    Event::default().event("progress").data(payload.to_string())
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
    fn collect_session_progress_filters_by_session_id() {
        use ccteam_core::CcteamPaths;
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        let app = AppState::new(paths);
        let slug = "demo";
        let progress_path = app.paths.progress_jsonl(slug);
        std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&progress_path).unwrap();
        // Two for s1, one for s2, one with no session_id, one garbage line.
        writeln!(f, r#"{{"event":"a","session_id":"s1","ts":"t1"}}"#).unwrap();
        writeln!(f, r#"{{"event":"b","session_id":"s2","ts":"t2"}}"#).unwrap();
        writeln!(f, r#"{{"event":"c","session_id":"s1","ts":"t3"}}"#).unwrap();
        writeln!(f, r#"{{"event":"d","ts":"t4"}}"#).unwrap();
        writeln!(f, "not-json").unwrap();
        let s1 = collect_session_progress(&app, slug, "s1");
        assert_eq!(s1.len(), 2);
        assert_eq!(s1[0]["event"], "a");
        assert_eq!(s1[1]["event"], "c");
        let s2 = collect_session_progress(&app, slug, "s2");
        assert_eq!(s2.len(), 1);
        let none = collect_session_progress(&app, slug, "s99");
        assert!(none.is_empty());
    }

    #[test]
    fn collect_session_progress_missing_file_is_empty() {
        use ccteam_core::CcteamPaths;
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        let app = AppState::new(paths);
        assert!(collect_session_progress(&app, "ghost", "s1").is_empty());
    }
}
