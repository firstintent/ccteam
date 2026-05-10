//! V0.3 M5.1 — `GET /assets/{file}` integration tests.
//!
//! Asserts the vendored htmx + style.css are served with correct
//! Content-Type, that bodies are non-empty, and that an unknown asset
//! file returns 404.

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
    // Drop the TempDir guard at end of scope; assets routes don't
    // touch the filesystem so it doesn't matter that the dir vanishes.
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
async fn htmx_asset_is_served_with_js_content_type() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/assets/htmx.min.js");
    let resp = reqwest::get(&url).await.expect("GET /assets/htmx.min.js");
    assert_eq!(resp.status(), 200);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ctype.contains("application/javascript"),
        "content-type must be JavaScript, got: {ctype}",
    );
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        cache.contains("max-age=31536000"),
        "cache-control must be 1y"
    );
    let body = resp.bytes().await.unwrap();
    assert!(body.len() > 1000, "htmx body must be non-trivial");
    // htmx 2.x bundle starts with `var htmx=function`.
    let head = std::str::from_utf8(&body[..50]).unwrap_or("");
    assert!(
        head.contains("htmx"),
        "body should look like htmx; got: {head}"
    );
}

#[tokio::test]
async fn style_asset_is_served_with_css_content_type() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/assets/style.css");
    let resp = reqwest::get(&url).await.expect("GET /assets/style.css");
    assert_eq!(resp.status(), 200);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ctype.contains("text/css"),
        "content-type must be CSS, got: {ctype}",
    );
    let body = resp.bytes().await.unwrap();
    assert!(body.len() > 100, "style.css body must be non-trivial");
}

#[tokio::test]
async fn xterm_assets_are_served() {
    let addr = spawn_server().await;

    let js_url = format!("http://{addr}/assets/xterm.js");
    let js_resp = reqwest::get(&js_url).await.expect("GET /assets/xterm.js");
    assert_eq!(js_resp.status(), 200);
    let js_ctype = js_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        js_ctype.contains("application/javascript"),
        "xterm.js content-type must be JavaScript, got: {js_ctype}",
    );
    let js_body = js_resp.bytes().await.unwrap();
    assert!(
        js_body.len() > 100_000,
        "xterm.js body must be the vendored browser bundle"
    );
    let js_text = std::str::from_utf8(&js_body).unwrap_or("");
    assert!(
        !js_text.contains("sourceMappingURL="),
        "xterm.js should not reference a non-vendored source map"
    );

    let css_url = format!("http://{addr}/assets/xterm.css");
    let css_resp = reqwest::get(&css_url).await.expect("GET /assets/xterm.css");
    assert_eq!(css_resp.status(), 200);
    let css_ctype = css_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        css_ctype.contains("text/css"),
        "xterm.css content-type must be CSS, got: {css_ctype}",
    );
    let css_body = css_resp.text().await.unwrap();
    assert!(
        css_body.contains(".xterm"),
        "xterm.css should contain xterm class rules"
    );
}

#[tokio::test]
async fn unknown_asset_returns_404() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/assets/missing.js");
    let resp = reqwest::get(&url).await.expect("GET /assets/missing.js");
    assert_eq!(resp.status(), 404);
}
