//! v0.9.0 W3 (F3) — `ccteam-exec.v1` bearer gate over a REAL WS handshake:
//! `GET /ws/exec` must reject a missing/wrong `Authorization` bearer
//! BEFORE the upgrade (401), never reaching the exec handler. Everything
//! else about the satellite (vendor allowlist / slug registry / path
//! confinement / `{{DAEMON_URL}}` substitution) is unit-tested directly in
//! `ccteam-web/src/satellite.rs`; this is the one invariant that needs a
//! live socket to prove (header-gated pre-upgrade rejection).

use ccteam_core::CcteamPaths;
use ccteam_web::satellite::satellite_router;

async fn spawn_satellite(agent_token: &str) -> std::net::SocketAddr {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    std::mem::forget(tmp); // keep the tempdir alive for the server's lifetime
    let router = satellite_router(
        paths,
        agent_token.to_string(),
        "http://127.0.0.1:7331".into(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn ws_exec_rejects_missing_bearer_before_upgrade() {
    let addr = spawn_satellite("real-token").await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/ws/exec"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ws_exec_rejects_wrong_bearer_before_upgrade() {
    let addr = spawn_satellite("real-token").await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/ws/exec"))
        .header("Authorization", "Bearer wrong-token")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_is_reachable_without_auth() {
    let addr = spawn_satellite("real-token").await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}
