//! V0.3.2 F52 — JSON API parity integration tests.
//!
//! Covers:
//!
//! - `GET /api/v1/projects` returns a JSON array with the same row
//!   shape the dashboard renders, including `kind` and `cost_label`.
//! - `GET /api/v1/projects/{slug}` returns the composite
//!   `ProjectSummary` shape: state / events / outbox /
//!   workflow_summary — and **does not** include
//!   `wire_token` / `auth_wire_token` / `auth_enabled` (redline grep #4).
//!   V0.4.0 F67 dropped the legacy `decision_candidates` /
//!   `current_phase` fields — the workflow view consumes
//!   `workflow_summary` (a `WorkflowSummary` or `null`).
//! - `GET /api/v1/projects/{slug}/sessions/{sid}` returns the
//!   synthesized workflow `SessionDetail` (404 on unknown sid). See
//!   `api_v1_workflow_panels_test.rs` for the full session-detail
//!   coverage.
//! - `GET /api/v1/auth/token` returns `{"wire_token":null}` when
//!   auth is disabled and `"ccteam:<hex>"` when enabled.
//! - JSON POST `/api/{slug}/btw` returns `{"ok":true}` with status
//!   200 (NOT 303 — the SPA contract).
//! - JSON POST with empty / overlong / bad body returns 400 +
//!   `{"ok":false,"error":...}`.
//! - HTML form POST still returns 303 (regression guard — the htmx
//!   path is kept until F59 retires askama).
//! - Unauthenticated requests hit 401 when auth is enabled.

use std::fs;
use std::net::SocketAddr;

use ccteam_core::{
    bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths, ProjectState,
};
use ccteam_web::{router_with_state, AppState, AuthState};
use reqwest::redirect::Policy;
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_project(paths: &CcteamPaths, slug: &str) {
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(paths, slug, "demo request", "dev").unwrap();
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

fn nofollow() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn get_api_v1_projects_returns_dashboard_row_array() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .starts_with("application/json"));
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().expect("top-level array");
    assert_eq!(arr.len(), 1);
    let row = &arr[0];
    assert_eq!(row["slug"], "demo");
    assert!(row["kind"].is_string(), "kind must be a string label");
    assert!(row["cost_label"].is_string());
    assert!(row["last_event_label"].is_string());
    // Redline: token must not leak to the list endpoint.
    assert!(row.get("wire_token").is_none());
    assert!(row.get("auth_wire_token").is_none());
}

#[tokio::test]
async fn get_api_v1_project_detail_returns_summary_shape() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/demo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["slug"], "demo");
    assert!(body["team"].is_string());
    assert!(body["kind"].is_string());
    assert!(body["created_at"].is_string());
    assert!(
        body["state"].is_object(),
        "state is structured JSON, not pretty string"
    );
    assert!(body["events"].is_array());
    assert!(body["outbox"].is_array());
    // V0.4.0 F67: `decision_candidates` / `current_phase` retired
    // along with the phase machinery (F60). `workflow_summary` may
    // be null for legacy projects without a workflow.yaml.
    assert!(body.get("decision_candidates").is_none());
    assert!(body.get("current_phase").is_none());
    assert!(body.get("workflow_summary").is_some());
    // Redline: token / auth fields not present.
    assert!(body.get("wire_token").is_none());
    assert!(body.get("auth_wire_token").is_none());
    assert!(body.get("auth_enabled").is_none());
    // state should round-trip the project state fields.
    assert_eq!(body["state"]["slug"], "demo");
}

#[tokio::test]
async fn get_api_v1_project_detail_404_for_unknown_slug() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap_or("").contains("missing"));
}

#[tokio::test]
async fn get_api_v1_session_detail_404_for_unknown_workflow_sid() {
    // V0.5.1 F103c: workflow projects now resolve a SessionDetail
    // when an `agent_spawn` for the sid exists in progress.jsonl.
    // Unknown sids still 404 — there's nothing to anchor the detail
    // shape on. (Pre-F103c this test asserted "non-flex always
    // 404'd"; the workflow branch in `handle_session` flipped that
    // contract, so the test is reframed to keep covering the
    // 404-on-missing-sid path.)
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/demo/sessions/missing"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn get_api_v1_auth_token_returns_null_when_auth_disabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/auth/token"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["wire_token"].is_null());
}

#[tokio::test]
async fn get_api_v1_auth_token_returns_wire_token_when_enabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token_hex = "deadbeefcafebabe".to_string();
    let state = AppState::with_auth(paths, AuthState::enabled(token_hex.clone()));
    let addr = spawn(state).await;

    // Auth-on: present Bearer header to satisfy auth_layer.
    let resp = client()
        .get(format!("http://{addr}/api/v1/auth/token"))
        .header("Authorization", format!("Bearer ccteam:{token_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["wire_token"], format!("ccteam:{token_hex}"));
}

#[tokio::test]
async fn get_api_v1_projects_rejects_unauthenticated_when_auth_enabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled("abc123".into()));
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn post_btw_json_returns_ok_true_and_writes_inbox() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let inbox_dir = paths.project_ccteam_dir("demo").join("inbox");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/btw"))
        .json(&serde_json::json!({"text": "hello via JSON"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let entries: Vec<_> = fs::read_dir(&inbox_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1);
    let body_on_disk = fs::read_to_string(entries[0].path()).unwrap();
    assert!(body_on_disk.contains("hello via JSON"));
}

#[tokio::test]
async fn post_btw_json_empty_returns_ok_false() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/btw"))
        .json(&serde_json::json!({"text": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap_or("").contains("empty"));
}

#[tokio::test]
async fn post_btw_json_overlong_returns_ok_false() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");

    let addr = spawn(AppState::new(paths)).await;
    let big = "x".repeat(5000);
    let resp = client()
        .post(format!("http://{addr}/api/demo/btw"))
        .json(&serde_json::json!({"text": big}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
}

#[tokio::test]
async fn post_btw_form_path_still_returns_303_regression_guard() {
    // Regression guard: even with the F52 content-type dispatch, the
    // existing htmx flow (form-encoded) must keep returning 303 →
    // /project/<slug> until F59 retires the askama UI.
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");

    let addr = spawn(AppState::new(paths)).await;
    let resp = nofollow()
        .post(format!("http://{addr}/api/demo/btw"))
        .form(&[("text", "hello via form")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "/project/demo",
    );
}

#[tokio::test]
async fn post_pause_json_returns_ok_true() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let state_path = paths.project_state("demo");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/pause"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let after = ProjectState::load(&state_path).unwrap();
    assert!(after.user_pause_pending);
}

#[tokio::test]
async fn post_inject_decision_json_writes_file_and_returns_ok() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let target = paths.project_ccteam_dir("demo").join("decision-json.md");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/inject_decision"))
        .json(&serde_json::json!({
            "path": target.display().to_string(),
            "body": "**META-AGENT DECISION**: via JSON\n",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert!(target.exists());
    let on_disk = fs::read_to_string(&target).unwrap();
    assert!(on_disk.contains("via JSON"));
}

#[tokio::test]
async fn post_inject_decision_json_rejects_outside_ccteam_dir() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/inject_decision"))
        .json(&serde_json::json!({"path": "/etc/passwd", "body": "evil"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
}
