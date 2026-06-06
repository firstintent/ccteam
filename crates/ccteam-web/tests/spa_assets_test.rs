//! V0.3.2 F53 — integration coverage for the SPA asset surface added in
//! `routes/assets.rs`:
//!
//! - `GET /app/` → 200 + `text/html` (serves the bundled `index.html`,
//!   or the placeholder one written by `build.rs` when the `web-bundle`
//!   feature is off / `CCTEAM_SKIP_WEB_BUILD=1`).
//! - `GET /app/<any unknown path>` → 200 + `text/html` (react-router
//!   fallback to `index.html`).
//! - `GET /assets/spa/missing.js` → 404 (no fallback for SPA bundle
//!   assets — only `/app/*` is the SPA route).
//!
//! These tests work both with the real vite build and with the
//! placeholder dist (because `build.rs` always writes a valid
//! `index.html`). Run as:
//!
//! ```ignore
//! # default (`web-bundle` on) — requires `npm` to be installed:
//! cargo test -p ccteam-web --test spa_assets_test
//!
//! # placeholder path (no `npm` needed):
//! CCTEAM_SKIP_WEB_BUILD=1 cargo test -p ccteam-web --test spa_assets_test
//! # or
//! cargo test -p ccteam-web --no-default-features --test spa_assets_test
//! ```

use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn_server() -> SocketAddr {
    let tmp = TempDir::new().unwrap();
    let state = AppState::new(fake_paths(tmp.path()));
    // SPA asset routes don't touch the filesystem; let the TempDir go.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

#[tokio::test]
async fn get_app_root_returns_index_html() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/app/");
    let resp = reqwest::get(&url).await.expect("GET /app/");
    assert_eq!(resp.status(), 200);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ctype.contains("text/html"),
        "content-type must be HTML, got: {ctype}",
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("<html") || body.to_lowercase().contains("<!doctype html"),
        "SPA index.html must contain html doctype; got: {body}",
    );
}

#[tokio::test]
async fn get_app_unknown_path_falls_back_to_index_html() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/app/p/some-slug/s/sid-42");
    let resp = reqwest::get(&url).await.expect("GET /app/p/.../s/...");
    assert_eq!(
        resp.status(),
        200,
        "react-router SPA fallback must yield 200, not 404",
    );
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ctype.contains("text/html"),
        "fallback must be HTML, got: {ctype}",
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("<html") || body.to_lowercase().contains("<!doctype html"),
        "fallback body must look like HTML; got: {body}",
    );
}

/// v0.8.7 W4 (DD.1) — the per-session chat deep link `/app/chat/s/{sid}`
/// (the new gateway `s{n}` route ChatConsole reads via `useParams`) must
/// also resolve to the SPA `index.html` (react-router renders the route
/// client-side). Extends the v032-spa fallback coverage to the new shape.
#[tokio::test]
async fn get_app_chat_session_deep_link_falls_back_to_index_html() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/app/chat/s/s2");
    let resp = reqwest::get(&url).await.expect("GET /app/chat/s/s2");
    assert_eq!(
        resp.status(),
        200,
        "per-session chat deep link must SPA-fallback to 200",
    );
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ctype.contains("text/html"),
        "fallback must be HTML, got: {ctype}"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("<html") || body.to_lowercase().contains("<!doctype html"),
        "fallback body must look like HTML; got: {body}",
    );
}

#[tokio::test]
async fn get_assets_spa_unknown_returns_404() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/assets/spa/missing-chunk.js");
    let resp = reqwest::get(&url)
        .await
        .expect("GET /assets/spa/missing-chunk.js");
    assert_eq!(
        resp.status(),
        404,
        "unknown SPA bundle assets must NOT fall back to index.html",
    );
}
