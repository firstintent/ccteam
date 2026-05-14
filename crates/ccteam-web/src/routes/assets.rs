//! Static asset handlers.
//!
//! Two surfaces share this module:
//!
//! - **Legacy `/assets/{file}`** (M5.1) — htmx, the htmx SSE
//!   extension, @xterm/xterm 6.0.0, and the hand-written `style.css`,
//!   all baked via `include_bytes!`. V0.3.2 F59 retired the htmx
//!   *routes* (the three `.html` templates are gone; `/`,
//!   `/project/<slug>`, `/session/<slug>/<sid>` are now 301 redirects
//!   into `/app/...`), but the static byte assets stay served for one
//!   more release. **TODO(V0.3.3)**: delete the five `include_bytes!`
//!   blocks below + the `handle_legacy_asset` route + the underlying
//!   files in `crates/ccteam-web/assets/` once V0.3.2 has shipped and
//!   no external page is known to be deep-linking them.
//! - **V0.3.2 F53 SPA surface** — `GET /app/*` serves the vite-built
//!   React shell (with react-router fallback to `index.html`) and
//!   `GET /assets/spa/*` serves its hashed bundle assets. The bundle
//!   is embedded via `rust-embed::RustEmbed` from `web/dist/` (driven
//!   by `build.rs`).
//!
//! Cache policy:
//!
//! - Legacy `/assets/{file}` bytes are version-frozen at build time,
//!   so `public, max-age=31536000, immutable` is safe.
//! - SPA hashed bundle under `/assets/spa/...` carries the same long
//!   `immutable` cache because vite hashes the filenames.
//! - SPA `index.html` (returned for any `/app/...` route) is
//!   `no-cache` because every SPA upgrade replaces it.

use axum::{
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use rust_embed::RustEmbed;

use crate::state::AppState;

const HTMX_JS: &[u8] = include_bytes!("../../assets/htmx.min.js");
const HTMX_EXT_SSE_JS: &[u8] = include_bytes!("../../assets/htmx-ext-sse.js");
const XTERM_JS: &[u8] = include_bytes!("../../assets/xterm.js");
const XTERM_CSS: &[u8] = include_bytes!("../../assets/xterm.css");
const STYLE_CSS: &[u8] = include_bytes!("../../assets/style.css");

const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
const CACHE_NO_CACHE: &str = "no-cache";

/// Embedded view of the vite-built SPA bundle. `build.rs` guarantees
/// `web/dist/index.html` exists (either as the real vite output when
/// the `web-bundle` feature is on, or as a placeholder when it's off
/// or when `CCTEAM_SKIP_WEB_BUILD=1`), so the macro expansion always
/// resolves to a non-empty folder.
#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct SpaAssets;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/assets/{file}", get(handle_legacy_asset))
        // SPA hashed bundle. Vite emits paths like `assets/<n>-<hash>.js`
        // inside `dist/` when `base: "/app/"` is set, so the embedded
        // lookup key is the suffix after `/assets/spa/`.
        .route("/assets/spa/{*path}", get(handle_spa_bundle_asset))
        // `/app` exact (no trailing slash).
        .route("/app", get(handle_spa_root))
        // `/app/` is the canonical F59 user entrypoint. Axum's
        // catch-all route below does not match the empty suffix, so
        // keep this exact route alongside `/app`.
        .route("/app/", get(handle_spa_root))
        // `/app/` and any deeper path — direct lookup first, then
        // react-router fallback to index.html.
        .route("/app/{*path}", get(handle_spa_path))
}

async fn handle_legacy_asset(Path(file): Path<String>) -> impl IntoResponse {
    let (bytes, ctype): (&'static [u8], &'static str) = match file.as_str() {
        "htmx.min.js" => (HTMX_JS, "application/javascript; charset=utf-8"),
        "htmx-ext-sse.js" => (HTMX_EXT_SSE_JS, "application/javascript; charset=utf-8"),
        "xterm.js" => (XTERM_JS, "application/javascript; charset=utf-8"),
        "xterm.css" => (XTERM_CSS, "text/css; charset=utf-8"),
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
                HeaderValue::from_static(CACHE_IMMUTABLE),
            ),
        ],
        bytes,
    )
        .into_response()
}

async fn handle_spa_bundle_asset(Path(path): Path<String>) -> impl IntoResponse {
    match SpaAssets::get(&path) {
        Some(file) => serve_embedded(&path, file, CACHE_IMMUTABLE),
        None => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}

async fn handle_spa_root() -> impl IntoResponse {
    serve_spa_index()
}

async fn handle_spa_path(Path(path): Path<String>) -> impl IntoResponse {
    // Direct hit on a packaged SPA file (e.g. `manifest.json`) —
    // serve it. Otherwise fall through to `index.html` so react-router
    // can resolve the route client-side.
    if let Some(file) = SpaAssets::get(&path) {
        return serve_embedded(&path, file, CACHE_IMMUTABLE);
    }
    serve_spa_index()
}

fn serve_spa_index() -> axum::response::Response {
    match SpaAssets::get("index.html") {
        Some(file) => serve_embedded("index.html", file, CACHE_NO_CACHE),
        // build.rs guarantees index.html exists; this branch should be
        // unreachable but guards against a corrupted bundle.
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "SPA bundle missing index.html (build.rs invariant violation)",
        )
            .into_response(),
    }
}

fn serve_embedded(
    path: &str,
    file: rust_embed::EmbeddedFile,
    cache_control: &'static str,
) -> axum::response::Response {
    let ctype = mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    // Prefer the charset-tagged variant for text-y types so browsers
    // don't fall back to latin-1.
    let ctype = if ctype.starts_with("text/")
        || ctype == "application/javascript"
        || ctype == "application/json"
    {
        format!("{ctype}; charset=utf-8")
    } else {
        ctype
    };

    let ctype_header = HeaderValue::from_str(&ctype)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let cache_header = HeaderValue::from_static(cache_control);

    (
        [
            (header::CONTENT_TYPE, ctype_header),
            (header::CACHE_CONTROL, cache_header),
        ],
        file.data.into_owned(),
    )
        .into_response()
}
