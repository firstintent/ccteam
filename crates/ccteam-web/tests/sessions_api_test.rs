//! v0.8.6 W5b ResSessions — session resource API integration tests.
//!
//! These exercise the **no-gateway** (standalone internal-web) path:
//! `AppState::new` leaves `gateway = None`, so every session endpoint must
//! return 503 (the locked W5b contract) — except the SSE endpoint, which
//! keeps the stream open and emits a one-shot `gateway_unavailable` frame
//! so a browser `EventSource` doesn't retry-loop on a 503.
//!
//! The gateway-attached happy path (create/list/turn/stop driving a real
//! `Gateway`) needs a live daemon + harness fakes and is covered by the
//! gateway spine's own unit tests in `ccteam-im`; here we lock the network
//! contract + that the router builds without a route-matcher conflict
//! (`/api/v1/sessions/active` from api_v1 vs `/api/v1/sessions/{sid}` here).

use std::net::SocketAddr;
use std::time::Duration;

use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn_server(state: AppState) -> SocketAddr {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // router_with_state builds the FULL stateful_router; if the new
    // session routes conflicted with api_v1's `/api/v1/sessions/active`
    // in the matchit router, this would panic here.
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

#[tokio::test]
async fn list_sessions_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_some());
}

#[tokio::test]
async fn create_session_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "reviewer"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn session_history_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/sessions/s1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn session_turn_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/sessions/s1/turn"))
        .json(&serde_json::json!({"text": "hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn session_stop_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/sessions/s1/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

/// The SSE endpoint must NOT 503 — it keeps the stream open and emits a
/// one-shot `gateway_unavailable` frame so a browser EventSource shows the
/// state without hammering reconnects. It is still a 200 text/event-stream.
#[tokio::test]
async fn session_events_no_gateway_streams_unavailable_notice() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let addr = spawn_server(AppState::new(paths)).await;

    let url = format!("http://{addr}/api/v1/sessions/s1/events");
    let resp = reqwest::get(&url).await.expect("sse get");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "text/event-stream",
    );

    let stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mapped = stream.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(mapped);
    let mut lines = tokio::io::BufReader::new(reader).lines();

    // Read until we see the `gateway_unavailable` event name (skip the
    // 15s keep-alive comment lines, which never arrive this fast anyway).
    let saw_notice = tokio::time::timeout(Duration::from_secs(5), async {
        let mut event_name: Option<String> = None;
        loop {
            let next = lines.next_line().await.ok().flatten()?;
            if let Some(rest) = next.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            }
            if next.is_empty() {
                if let Some(name) = event_name.take() {
                    return Some(name);
                }
            }
        }
    })
    .await
    .ok()
    .flatten();

    assert_eq!(saw_notice.as_deref(), Some("gateway_unavailable"));
}
