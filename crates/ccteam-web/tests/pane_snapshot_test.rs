//! Read-only xterm pane snapshot endpoint tests.
//!
//! CI may not have tmux, so these tests assert the degraded contract
//! and project-page wiring. A real tmux success path is covered by
//! manual dogfooding and the lower-level tmux helpers.

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
async fn pane_snapshot_returns_504_when_tmux_session_missing() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/api/this-slug-doesnt-exist-xyz-123/pane-snapshot.ansi");
    let resp = reqwest::get(&url).await.expect("GET pane snapshot");
    assert_eq!(resp.status(), 504);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        !ctype.starts_with("application/octet-stream"),
        "degraded response must not pretend to be raw ANSI bytes"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("pane snapshot"),
        "504 body should mention pane snapshot degraded reason; got: {body}",
    );
}
