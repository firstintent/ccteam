//! `GET /` — 301 redirect to the SPA root `/app/`.
//!
//! V0.3.2 F59 retired the askama-rendered htmx dashboard. The SPA
//! (`/app/`) is now the only live UI surface; this handler exists only
//! to redirect legacy bookmarks (and the test harness) into it.
//!
//! `templates/base.html` stays in-repo as an askama SSR fallback per
//! the V0.3.2 PRD §F59 wording. Static htmx / xterm / `style.css`
//! assets are still served by `routes/assets.rs`.

use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(redirect_to_spa))
}

async fn redirect_to_spa() -> impl IntoResponse {
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "/app/")])
}
