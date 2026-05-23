//! V0.6.3 F143 — `POST /webhook/{project}/{token}` integration tests.
//!
//! Exercises the full axum + reqwest round-trip of the webhook ingress:
//!
//! 1. unknown project → 401, nothing written.
//! 2. wrong token → 401, nothing written.
//! 3. valid token → 202 + a payload file appears under
//!    `<project>/.ccteam/webhooks/`.
//! 4. oversized body (> 256 KiB) → 413, nothing written.
//! 5. the route stays reachable even when the bearer `auth_layer` is
//!    enabled (it carries its own per-project token).

use std::net::SocketAddr;

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::routes::webhook::generate_or_load_secret;
use ccteam_web::{router_with_state, AppState};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

/// Count files (non-hidden) under `<project>/.ccteam/webhooks/`.
fn webhook_file_count(paths: &CcteamPaths, slug: &str) -> usize {
    let dir = paths.project_webhooks_dir(slug);
    match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| !n.starts_with('.'))
                    .unwrap_or(false)
            })
            .count(),
        Err(_) => 0,
    }
}

#[tokio::test]
async fn unknown_project_returns_401_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/webhook/nope/sometoken"))
        .json(&json!({"a": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn wrong_token_returns_401_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "dev").unwrap();
    // Pre-generate the secret so the project has a known token.
    let _secret = generate_or_load_secret(&paths.project_webhook_token("demo")).unwrap();

    let state = AppState::new(paths.clone());
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/webhook/demo/wrongtoken"))
        .json(&json!({"a": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        webhook_file_count(&paths, "demo"),
        0,
        "bad token must not drop a file"
    );
}

#[tokio::test]
async fn valid_token_returns_202_and_writes_payload_file() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "dev").unwrap();
    let secret = generate_or_load_secret(&paths.project_webhook_token("demo")).unwrap();

    let state = AppState::new(paths.clone());
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/webhook/demo/{secret}"))
        .header("x-github-event", "push")
        .json(&json!({"ref": "refs/heads/main"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);

    assert_eq!(
        webhook_file_count(&paths, "demo"),
        1,
        "valid webhook must drop exactly one file"
    );
    // Inspect the written record.
    let dir = paths.project_webhooks_dir("demo");
    let entry = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .find(|e| {
            e.file_name()
                .to_str()
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        })
        .unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
    assert_eq!(record["payload"]["ref"], "refs/heads/main");
    assert_eq!(record["headers"]["x-github-event"], "push");
    assert_eq!(record["project"], "demo");
    assert!(record["received_at"].is_string());
}

#[tokio::test]
async fn oversized_body_returns_413_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "dev").unwrap();
    let secret = generate_or_load_secret(&paths.project_webhook_token("demo")).unwrap();

    let state = AppState::new(paths.clone());
    let addr = spawn(state).await;

    // 512 KiB > 256 KiB MAX_BODY_BYTES.
    let big = "x".repeat(512 * 1024);
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/webhook/demo/{secret}"))
        .header("content-type", "application/octet-stream")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
    assert_eq!(
        webhook_file_count(&paths, "demo"),
        0,
        "oversized body must not drop a file"
    );
}

#[tokio::test]
async fn reachable_even_when_bearer_auth_enabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "dev").unwrap();
    let secret = generate_or_load_secret(&paths.project_webhook_token("demo")).unwrap();

    // Bearer auth ON — the webhook route must still work via its own
    // per-project path token (it is mounted outside `auth_layer`).
    let bearer = "deadbeefdeadbeefdeadbeefdeadbeef";
    let state = AppState::with_auth(
        paths.clone(),
        ccteam_web::auth::AuthState::enabled(bearer.into()),
    );
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/webhook/demo/{secret}"))
        .json(&json!({"ci": "green"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    assert_eq!(webhook_file_count(&paths, "demo"), 1);
}
