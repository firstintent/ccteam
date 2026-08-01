//! Static asset handlers — SPA bundle only after V0.4.0.
//!
//! V0.3.2 F59 retired the htmx HTML routes (the three `.html` templates
//! were deleted; `/`, `/project/<slug>`, `/session/<slug>/<sid>` are now
//! 301 redirects into `/app/...`). V0.4.0 F69 finishes the cleanup by
//! deleting the five legacy `include_bytes!` blocks (htmx, htmx-ext-sse,
//! xterm.js, xterm.css, hand-written style.css) along with the
//! `/assets/{file}` route and the underlying byte files in
//! `crates/ccteam-web/assets/`. The SPA bundle (`/app/*` + `/assets/spa/*`)
//! is the sole surface this module serves.
//!
//! Cache policy:
//!
//! - SPA hashed bundle under `/assets/spa/...` is `public,
//!   max-age=31536000, immutable` because vite hashes the filenames.
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

/// Root-served PWA files. A web app manifest and its icons are fetched by the
/// BROWSER, not by app code: the URL comes from `index.html` / the manifest
/// itself, and the fetch is anonymous (no cookie, no Bearer). They were only
/// mounted under `/app/`, so on a phone every one of them 404'd and the PWA
/// install lost its name, icon, `start_url` and standalone display — a failure
/// desktop never sees, because nothing on desktop asks for them.
pub const ROOT_PWA_FILES: &[&str] = &[
    "manifest.json",
    "sw.js",
    "icon-192.png",
    "icon-512.png",
    "logo.svg",
];

/// Whether `path` is one of the anonymous root PWA files (leading slash form).
pub fn is_root_pwa_file(path: &str) -> bool {
    path.strip_prefix('/')
        .is_some_and(|name| ROOT_PWA_FILES.contains(&name))
}

pub fn router() -> Router<AppState> {
    let mut router = Router::new();
    for name in ROOT_PWA_FILES {
        router = router.route(&format!("/{name}"), get(handle_root_pwa_file));
    }
    router
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

/// Serve a root PWA file from the embedded bundle. `no-cache` (not immutable):
/// these names are unhashed, so a stale manifest or icon would otherwise
/// outlive an upgrade.
async fn handle_root_pwa_file(uri: axum::http::Uri) -> impl IntoResponse {
    let name = uri.path().trim_start_matches('/').to_string();
    match SpaAssets::get(&name) {
        Some(file) => serve_embedded(&name, file, CACHE_NO_CACHE),
        None => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
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
