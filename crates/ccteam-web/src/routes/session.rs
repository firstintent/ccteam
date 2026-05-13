//! `GET /session/<slug>/<sid>` — 301 redirect to `/app/p/<slug>/s/<sid>`.
//!
//! V0.3.2 F59 retired the askama-rendered htmx session page. The SPA
//! `SessionDetail` page (mounted at `/app/p/:slug/s/:sid`) is now the
//! only live UI surface for flex session detail; this handler keeps
//! legacy bookmarks + cross-page links working.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/session/{slug}/{sid}", get(redirect_to_spa))
}

async fn redirect_to_spa(Path((slug, sid)): Path<(String, String)>) -> impl IntoResponse {
    let target = format!(
        "/app/p/{}/s/{}",
        urlencode_path(&slug),
        urlencode_path(&sid),
    );
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)])
}

fn urlencode_path(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}
