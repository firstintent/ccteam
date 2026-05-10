//! `GET /assets/{file}` — vendored static assets.
//!
//! Bytes are baked into the binary via `include_bytes!` (mirrors F38's
//! TTF vendoring) so `ccteam web` is self-contained — no separate
//! static-files install step. Currently serves htmx 2.0.4 + the
//! single hand-written `style.css`.
//!
//! Cache-Control is set to `public, max-age=31536000` because the
//! assets are version-frozen at build time — a new ccteam release
//! ships new bytes, but an in-flight binary's bytes never change.

use axum::{
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};

use crate::state::AppState;

const HTMX_JS: &[u8] = include_bytes!("../../assets/htmx.min.js");
const HTMX_EXT_SSE_JS: &[u8] = include_bytes!("../../assets/htmx-ext-sse.js");
const STYLE_CSS: &[u8] = include_bytes!("../../assets/style.css");

const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

pub fn router() -> Router<AppState> {
    Router::new().route("/assets/{file}", get(handle_asset))
}

async fn handle_asset(Path(file): Path<String>) -> impl IntoResponse {
    let (bytes, ctype): (&'static [u8], &'static str) = match file.as_str() {
        "htmx.min.js" => (HTMX_JS, "application/javascript; charset=utf-8"),
        // V0.3 M5.2 — htmx 2.x SSE extension (separate file from the
        // core lib). Loaded by `<script>` after htmx.min.js so the
        // extension can register its handlers.
        "htmx-ext-sse.js" => (HTMX_EXT_SSE_JS, "application/javascript; charset=utf-8"),
        "style.css" => (STYLE_CSS, "text/css; charset=utf-8"),
        _ => {
            return (StatusCode::NOT_FOUND, "asset not found").into_response();
        }
    };

    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(ctype)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(CACHE_CONTROL),
            ),
        ],
        bytes,
    )
        .into_response()
}
