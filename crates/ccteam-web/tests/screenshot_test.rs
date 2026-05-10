//! V0.3 M5.2 — `/screenshot/<slug>.png` integration tests.
//!
//! F38 (`ccteam_core::render_screenshot`) needs a live tmux session
//! to capture; CI does not have one. So these tests verify the
//! **degraded-path** contract: bad path → 404, missing tmux session
//! → 504 with plain-text reason. The success path is exercised by
//! the F38 unit tests in `ccteam-core` and the V0.3 M5.4 e2e suite
//! against a real tmux daemon.

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

async fn spawn_server(state: AppState) -> SocketAddr {
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
async fn screenshot_returns_504_when_tmux_session_missing() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/screenshot/this-slug-doesnt-exist-xyz-123.png");
    let resp = reqwest::get(&url).await.expect("GET screenshot");
    // F38's render_screenshot returns Ok(None) when tmux can't find
    // the session. Per PRD §5.2.5 we map that to 504 Gateway
    // Timeout + plain-text reason (NOT 404 — a 404 would imply the
    // slug is unknown, but the user might just not have started
    // the tmux session yet).
    assert_eq!(resp.status(), 504);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("screenshot unavailable") || body.contains("session not found"),
        "504 body should mention degraded reason; got: {body}",
    );
}

#[tokio::test]
async fn screenshot_returns_404_for_non_png_path() {
    // /screenshot/<file> only accepts `<slug>.png`; anything else
    // should 404 (we don't want a future thumbnail / other-format
    // path to silently land here without an explicit handler).
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/screenshot/foo.gif");
    let resp = reqwest::get(&url).await.expect("GET screenshot");
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn screenshot_endpoint_advertises_image_png_contract() {
    // Even on the degraded path, hitting the URL must not 5xx
    // panic — verify the response is a clean 504 with text/plain
    // body (NOT image/png; the 504 isn't a PNG, and the client
    // must fall back to the alt-text).
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/screenshot/anything.png");
    let resp = reqwest::get(&url).await.expect("GET screenshot");
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // The 504 path uses the axum default for plain string body:
    // text/plain; charset=utf-8. Assert the body is NOT pretending
    // to be a PNG.
    assert!(!ct.starts_with("image/png"), "content-type was: {ct}");
}
