//! Read-only xterm.js pane snapshot endpoint.
//!
//! `GET /api/<slug>/pane-snapshot.ansi` captures the active tmux pane
//! with ANSI escapes preserved and returns the raw bytes for browser-side
//! rendering by the vendored `@xterm/xterm` widget. This deliberately
//! stays snapshot-only: no WebSocket, no input forwarding, and no PTY
//! resize path. The existing PNG `/screenshot/<slug>.png` route remains
//! as a fallback.

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use ccteam_core::{ProjectState, TeamKind};
use ccteam_mux::{MuxBackend, MuxSessionId, TmuxBackend};

use crate::state::AppState;

const SNAPSHOT_LINES: usize = 50;
const FALLBACK_DIMS: (u16, u16) = (24, 80);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/{slug}/pane-snapshot.ansi", get(handle_pane_snapshot))
        .route(
            "/api/{slug}/{sid}/pane-snapshot.ansi",
            get(handle_session_pane_snapshot),
        )
}

async fn handle_pane_snapshot(State(app): State<AppState>, Path(slug): Path<String>) -> Response {
    let session_name = ccteam_core::session_name_for_project(app.paths.as_ref(), &slug);
    serve_pane_snapshot(slug, None, session_name).await
}

async fn handle_session_pane_snapshot(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
) -> Response {
    let session_name = match session_name_for_project_session(&app, &slug, &sid) {
        Ok(name) => name,
        Err((status, message)) => return (status, message).into_response(),
    };
    serve_pane_snapshot(slug, Some(sid), session_name).await
}

async fn serve_pane_snapshot(slug: String, sid: Option<String>, session_name: String) -> Response {
    // V0.8 W1 — route through the MuxBackend trait. `TmuxBackend`
    // bridges to the same blocking `tmux capture-pane / display-message`
    // calls under `spawn_blocking` internally, so the latency profile
    // is unchanged.
    let backend = TmuxBackend::new();
    let id = MuxSessionId::new(session_name.clone());

    let capture = backend.capture(&id, SNAPSHOT_LINES, true).await;
    let Some((bytes, rows, cols)) = (match capture {
        Ok(bytes) if !bytes.is_empty() => {
            let (rows, cols) = match backend.pane_dims(&id).await {
                Ok(Some((r, c))) if r > 0 && c > 0 => (r, c),
                _ => FALLBACK_DIMS,
            };
            let max_rows = SNAPSHOT_LINES.min(u16::MAX as usize) as u16;
            Some((bytes, rows.min(max_rows).max(1), cols))
        }
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(slug = %slug, sid = ?sid, ?err, "pane snapshot capture returned Err");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("pane snapshot capture failed: {err}"),
            )
                .into_response();
        }
    }) else {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            format!("pane snapshot unavailable: tmux session not found: {session_name}\n"),
        )
            .into_response();
    };

    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, must-revalidate"),
            ),
            (
                header::HeaderName::from_static("x-ccteam-pane-rows"),
                digit_header(rows),
            ),
            (
                header::HeaderName::from_static("x-ccteam-pane-cols"),
                digit_header(cols),
            ),
        ],
        bytes,
    )
        .into_response()
}

fn session_name_for_project_session(
    app: &AppState,
    slug: &str,
    sid: &str,
) -> Result<String, (StatusCode, String)> {
    let state = ProjectState::load(&app.paths.project_state(slug)).map_err(|err| {
        (
            StatusCode::NOT_FOUND,
            format!("project not found or unreadable: {slug}: {err}"),
        )
    })?;
    if state.team_kind != TeamKind::Flex {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("project {slug} is not a flex project"),
        ));
    }
    let record = state.sessions.get(sid).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("session not found: {slug}/{sid}"),
        )
    })?;
    Ok(record.tmux_session.clone())
}

fn digit_header(n: u16) -> HeaderValue {
    HeaderValue::from_str(&n.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}
