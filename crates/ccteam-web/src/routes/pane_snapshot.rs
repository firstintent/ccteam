//! Read-only xterm.js pane snapshot endpoint.
//!
//! `GET /api/<slug>/pane-snapshot.ansi` captures the active tmux pane
//! with ANSI escapes preserved and returns the raw bytes for browser-side
//! rendering by the vendored `@xterm/xterm` widget. This deliberately
//! stays snapshot-only: no WebSocket, no input forwarding, and no PTY
//! resize path. The existing PNG `/screenshot/<slug>.png` route remains
//! as a fallback.

use axum::{
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::state::AppState;

const SNAPSHOT_LINES: usize = 50;
const FALLBACK_DIMS: (u16, u16) = (24, 80);

pub fn router() -> Router<AppState> {
    Router::new().route("/api/{slug}/pane-snapshot.ansi", get(handle_pane_snapshot))
}

async fn handle_pane_snapshot(Path(slug): Path<String>) -> Response {
    let capture_slug = slug.clone();
    let capture =
        tokio::task::spawn_blocking(move || capture_snapshot_bytes(&capture_slug, SNAPSHOT_LINES))
            .await;

    let Some((bytes, rows, cols)) = (match capture {
        Ok(Ok(Some(snapshot))) => Some(snapshot),
        Ok(Ok(None)) => None,
        Ok(Err(err)) => {
            tracing::warn!(slug, ?err, "pane snapshot capture returned Err");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("pane snapshot capture failed: {err}"),
            )
                .into_response();
        }
        Err(err) => {
            tracing::warn!(slug, ?err, "pane snapshot worker failed");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("pane snapshot worker failed: {err}"),
            )
                .into_response();
        }
    }) else {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            "pane snapshot unavailable: tmux session not found\n",
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

fn capture_snapshot_bytes(slug: &str, lines: usize) -> anyhow::Result<Option<(Vec<u8>, u16, u16)>> {
    let bytes = match ccteam_core::capture_pane_with_ansi(slug, lines)? {
        Some(b) => b,
        None => return Ok(None),
    };
    let (rows, cols) = match ccteam_core::query_pane_dims(slug) {
        Ok(Some((r, c))) if r > 0 && c > 0 => (r, c),
        Ok(Some(_)) | Ok(None) | Err(_) => FALLBACK_DIMS,
    };
    let max_rows = lines.min(u16::MAX as usize) as u16;
    Ok(Some((bytes, rows.min(max_rows).max(1), cols)))
}

fn digit_header(n: u16) -> HeaderValue {
    HeaderValue::from_str(&n.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}
