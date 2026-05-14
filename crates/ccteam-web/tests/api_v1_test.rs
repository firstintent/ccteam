//! V0.3.2 F52 — JSON API parity integration tests.
//!
//! Covers:
//!
//! - `GET /api/v1/projects` returns a JSON array with the same row
//!   shape the dashboard renders, including `kind` and `cost_label`.
//! - `GET /api/v1/projects/{slug}` returns the composite
//!   `ProjectSummary` shape: state / events / outbox / sessions /
//!   decision_candidates — and **does not** include
//!   `wire_token` / `auth_wire_token` / `auth_enabled` (redline grep #4).
//! - `GET /api/v1/projects/{slug}/sessions/{sid}` (flex only) returns
//!   `SessionDetail` with `harness_snapshot` populated when the
//!   harness mirror file exists.
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
    bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths, HarnessKind,
    HarnessSnapshot, ProjectState, SessionRecord, TeamKind,
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

fn fixture_flex_project(paths: &CcteamPaths, slug: &str, sid: &str) {
    let mut state = ProjectState::initial_for_team(slug.to_string(), "flex".into());
    state.team_kind = TeamKind::Flex;
    state.sessions.insert(
        sid.to_string(),
        SessionRecord {
            harness: HarnessKind::Claude,
            tmux_session: format!("ccteam-{slug}-{sid}"),
            started_at: chrono::Utc::now(),
            pid: None,
            job_id: None,
        },
    );
    state.next_sid_seq.insert(HarnessKind::Claude, 2);
    fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
    state.save(&paths.project_state(slug)).unwrap();
}

fn fixture_snapshot(model: &str) -> HarnessSnapshot {
    HarnessSnapshot {
        harness: "claude-code".into(),
        model_display_name: model.into(),
        context_used_pct: 17,
        cost_usd_total: 1.23,
        rate_limit_pct: Some(4),
        cwd: None,
        raw: serde_json::json!({"source": "api_v1-test"}),
        captured_at: chrono::Utc::now(),
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

fn nofollow() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

#[tokio::test]
async fn get_api_v1_projects_returns_dashboard_row_array() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");

    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects"))
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
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/demo"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["slug"], "demo");
    assert!(body["team"].is_string());
    assert!(body["kind"].is_string());
    assert!(body["is_flex"].is_boolean());
    assert!(body["created_at"].is_string());
    assert!(
        body["state"].is_object(),
        "state is structured JSON, not pretty string"
    );
    assert!(body["events"].is_array());
    assert!(body["outbox"].is_array());
    assert!(body["sessions"].is_array());
    assert!(body["decision_candidates"].is_array());
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
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/missing"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap_or("").contains("missing"));
}

#[tokio::test]
async fn get_api_v1_session_detail_returns_harness_snapshot() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "flex-demo";
    let sid = "claude-1";
    fixture_flex_project(&paths, slug, sid);
    fs::create_dir_all(paths.harness_dir()).unwrap();
    let mirror = paths.harness_dir().join(format!("{slug}-{sid}.json"));
    fs::write(
        &mirror,
        serde_json::to_string(&fixture_snapshot("Claude Opus 4.7")).unwrap(),
    )
    .unwrap();

    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!(
        "http://{addr}/api/v1/projects/{slug}/sessions/{sid}"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["slug"], slug);
    assert_eq!(body["sid"], sid);
    assert_eq!(body["kind"], "flex");
    assert!(body["events"].is_array());
    assert!(body["outbox"].is_array());
    assert!(body["decision_candidates"].is_array());
    let snap = &body["harness_snapshot"];
    assert_eq!(snap["model"], "Claude Opus 4.7");
    assert!(snap["captured_at"].is_string());
    // Redline: tokens never leak via session JSON.
    assert!(body.get("wire_token").is_none());
    assert!(body.get("auth_wire_token").is_none());
}

#[tokio::test]
async fn get_api_v1_session_detail_404_for_non_flex_project() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!(
        "http://{addr}/api/v1/projects/demo/sessions/missing"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn get_api_v1_auth_token_returns_null_when_auth_disabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/auth/token"))
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
    let resp = reqwest::Client::new()
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
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects"))
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
    let resp = reqwest::Client::new()
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
    let resp = reqwest::Client::new()
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
    let resp = reqwest::Client::new()
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
    let resp = reqwest::Client::new()
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
    let resp = reqwest::Client::new()
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
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/demo/inject_decision"))
        .json(&serde_json::json!({"path": "/etc/passwd", "body": "evil"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
}
