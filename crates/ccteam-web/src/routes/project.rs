//! `GET /project/<slug>` — 301 redirect to `/app/p/<slug>`.
//!
//! V0.3.2 F59 retired the askama-rendered htmx project page. The SPA
//! `ProjectDetail` page (mounted at `/app/p/:slug`) is now the only
//! live UI surface for project detail; this handler keeps legacy
//! bookmarks + cross-page links working.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/project/{slug}", get(redirect_to_spa))
}

async fn redirect_to_spa(Path(slug): Path<String>) -> impl IntoResponse {
    // `slug` is operator-controlled but already validated as a path
    // segment by the router. URL-encode it defensively so a stray "/"
    // can't break out of the SPA path.
    let target = format!("/app/p/{}", urlencode_path(&slug));
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)])
}

/// Minimal path-segment encoder. We can't pull `urlencoding` here
/// without a new dep; the slug grammar in ccteam-core's project
/// creator restricts to `[a-z0-9-]+` so this is overkill but cheap.
fn urlencode_path(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}
