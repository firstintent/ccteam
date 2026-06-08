//! Read-only xterm.js pane snapshot endpoint.
//!
//! `GET /api/<slug>/pane-snapshot.ansi` captures the active pane (project
//! session name) with ANSI escapes preserved and returns the bytes for
//! browser-side rendering by the vendored `@xterm/xterm` widget. Both mux
//! backends are byte-faithful here: tmux via `capture-pane -e`, rmux via a
//! raw-byte backlog drain (`output_stream`). This deliberately stays
//! snapshot-only: no WebSocket, no input forwarding, and no PTY resize
//! path. The existing PNG `/screenshot/<slug>.png` route remains as a
//! fallback.
//!
//! `GET /api/<slug>/<sid>/pane-snapshot.ansi` is the **per-session** variant.
//! v0.8.8 B5: F1 removed the project-level pane, so the sid is resolved via the
//! live gateway to the per-session pane name (claude = `ccteam-chat-{slug}-{sid}`,
//! codex = `ccteam-{slug}-{sid}`) using the shared [`super::session_pane`] helper
//! — the same resolution `pty_ws` uses. No live gateway → 503; a sid the gateway
//! does not track → 404. `capture(with_ansi)` returns raw ANSI bytes under both
//! backends (rmux drains its retained raw-byte backlog).

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use ccteam_harness::MuxSessionId;

use super::session_pane::{resolve_session_pane, PaneResolveError};
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
    // v0.8.8 B5 — F1 后没有项目级 pane:经 live gateway 把 sid 解析成
    // per-session pane 名(claude=ccteam-chat-{slug}-{sid} /
    // codex=ccteam-{slug}-{sid}),与 pty_ws 共用同一 helper(无 gateway → 503;
    // sid 未知 → 404)。替换了 W5 忽略 _sid 退回项目级 tmux session 的旧逻辑。
    let session_name = match resolve_session_pane(&app, &sid).await {
        Ok((name, _resolved)) => name,
        Err(PaneResolveError::NoGateway) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "no live gateway: per-session pane snapshot unavailable on standalone web\n",
            )
                .into_response()
        }
        Err(PaneResolveError::Unknown) => {
            return (StatusCode::NOT_FOUND, format!("unknown session: {sid}\n")).into_response()
        }
    };
    serve_pane_snapshot(slug, Some(sid), session_name).await
}

async fn serve_pane_snapshot(slug: String, sid: Option<String>, session_name: String) -> Response {
    // V0.8 W1 / G5 — route through the ProcessBackend trait, honoring the
    // configured backend via `ccteam_harness::from_env()`
    // (`CCTEAM_MUX_BACKEND=tmux|rmux`). Under the tmux backend (the
    // opt-out) `TmuxBackend` bridges to the same blocking
    // `tmux capture-pane / display-message` calls under
    // `spawn_blocking`, so the latency profile is unchanged.
    //
    // Both backends are byte-faithful: `capture(.., with_ansi=true)`
    // returns raw ANSI bytes — tmux via `capture-pane -e`, rmux via a
    // raw-byte backlog drain (`output_stream`) — so the xterm.js widget
    // renders with full color fidelity under either.
    let backend = match ccteam_harness::from_env() {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(slug = %slug, sid = ?sid, ?err, "pane snapshot mux backend selection failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("pane snapshot backend unavailable: {err}\n"),
            )
                .into_response();
        }
    };
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

fn digit_header(n: u16) -> HeaderValue {
    HeaderValue::from_str(&n.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}
