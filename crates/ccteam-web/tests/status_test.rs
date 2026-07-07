//! v0.8.9 Phase 4 — `GET /api/v1/status` integration tests.
//!
//! The daemon-wide status aggregate the unified-shell **cost pill** + **Status
//! view** read: `{daemon_healthy, sessions_live, sessions_idle, cost_24h_usd,
//! cost_24h_by_vendor, budget_cap_24h}`.
//!
//! No daemon runs in-process, so `daemon_healthy` is always `false` here —
//! which (per the live/idle split) forces every tracked session to `idle` and
//! `sessions_live` to 0. The handler is best-effort: it must still return a
//! well-formed 200 with zeroed cost for an empty install, and sum cost +
//! per-vendor + the per-project workflow budget cap when projects exist.

use std::fs;
use std::net::SocketAddr;

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState};
use serde_json::{json, Value};
use serial_test::serial;
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

fn write_workflow_yaml(paths: &CcteamPaths, slug: &str, body: &str) {
    let ccteam_dir = paths.project_ccteam_dir(slug);
    fs::create_dir_all(&ccteam_dir).unwrap();
    fs::write(ccteam_dir.join("workflow.yaml"), body).unwrap();
}

fn append_event(paths: &CcteamPaths, slug: &str, event: Value) {
    let path = paths.progress_jsonl(slug);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut line = serde_json::to_string(&event).unwrap();
    line.push('\n');
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

fn write_one_tracked_session(root: &std::path::Path) {
    // v0.8.21 Wave-2 — a "tracked" session is now per-session meta.json + a
    // routing.json live-set (projects enumerated from config.yaml), not the
    // retired gateway-state.json sessions vec. Seed a COMPLETE one in THIS home
    // so the test proves the status route reads its OWN `app.paths.root` and
    // does NOT leak sessions from an unrelated CCTEAM_HOME.
    let project_dir = root.join("outside-proj");
    fs::create_dir_all(&project_dir).unwrap();
    ccteam_core::config::upsert_project(
        root,
        ccteam_core::config::ProjectEntry {
            slug: "outside".to_string(),
            path: project_dir.clone(),
            team: "dev".to_string(),
            installed_at: chrono::Utc::now(),
        },
    )
    .unwrap();
    let now = "2026-06-08T00:00:00Z".to_string();
    ccteam_harness::write_session_meta(
        &project_dir,
        &ccteam_harness::SessionMeta {
            sid: "s1".to_string(),
            slug: "outside".to_string(),
            vendor: ccteam_harness::AgentVendor::Claude,
            protocol: ccteam_harness::SessionProtocol::StreamJson,
            role: "cto".to_string(),
            permission_mode: ccteam_harness::PermissionMode::Skip,
            owner: "user:outside-chat".to_string(),
            vendor_uuid: String::new(),
            host: "local".to_string(),
            created_at: now.clone(),
            last_active: now,
            origin: ccteam_harness::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
        },
    )
    .unwrap();
    fs::create_dir_all(root.join("state").join("gateway")).unwrap();
    fs::write(
        root.join("state").join("gateway").join("routing.json"),
        serde_json::to_vec_pretty(&json!({
            "default_project": "outside",
            "current_project": [],
            "current_session": [],
            "live_sids": ["s1"],
        }))
        .unwrap(),
    )
    .unwrap();
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

/// reqwest client that ignores `HTTP_PROXY` / `HTTPS_PROXY` (some shells
/// resolve `127.0.0.1:<random>` through a corporate proxy → 502).
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// Empty install, no daemon: a well-formed best-effort snapshot —
/// `daemon_healthy: false`, zero sessions/cost, `budget_cap_24h: null`.
#[tokio::test]
#[serial]
async fn t01_status_empty_install_is_zeroed_snapshot() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let global = tmp.path().join("global-home");
    write_one_tracked_session(&global);
    let old_home = std::env::var_os("CCTEAM_HOME");
    std::env::set_var("CCTEAM_HOME", &global);

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/status"))
        .send()
        .await
        .unwrap();
    match old_home {
        Some(path) => std::env::set_var("CCTEAM_HOME", path),
        None => std::env::remove_var("CCTEAM_HOME"),
    }
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    // The full shape parses (each field present + correct type).
    assert_eq!(
        body["daemon_healthy"],
        json!(false),
        "no daemon runs in-process → daemon_healthy must be false"
    );
    assert_eq!(body["sessions_live"], json!(0));
    assert_eq!(body["sessions_idle"], json!(0));
    assert_eq!(body["cost_24h_usd"].as_f64().unwrap(), 0.0);
    assert!(
        body["cost_24h_by_vendor"].as_object().unwrap().is_empty(),
        "no projects → empty per-vendor breakdown"
    );
    assert!(
        body["budget_cap_24h"].is_null(),
        "no project configures a budget → null cap"
    );
}

/// Two projects with `agent_done` cost events + one project carrying a
/// `workflow.yaml` budget: cost sums across projects + the per-vendor map
/// merges + the aggregate budget cap surfaces. Sessions still idle/zero (no
/// daemon).
#[tokio::test]
#[serial]
async fn t02_status_sums_cost_and_surfaces_budget_cap() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-a");
    fixture_project(&paths, "team-b");

    // team-a: a Claude $1.50 done + a Codex $0.50 done (no ts → folded into 24h).
    append_event(
        &paths,
        "team-a",
        json!({"event": "agent_done", "cost_usd": 1.5, "vendor": "claude"}),
    );
    append_event(
        &paths,
        "team-a",
        json!({"event": "agent_done", "cost_usd": 0.5, "vendor": "codex"}),
    );
    // team-b: another Claude $2.00 done.
    append_event(
        &paths,
        "team-b",
        json!({"event": "agent_done", "cost_usd": 2.0, "vendor": "claude"}),
    );

    // team-a declares a per-vendor budget (claude $5 + codex $2 → aggregate $7).
    // team-b has no budget block, so it contributes nothing to the cap.
    write_workflow_yaml(
        &paths,
        "team-a",
        "\
name: team-a
budgets_v060:
  claude:
    max_cost_usd_per_24h: 5.0
  codex:
    max_cost_usd_per_24h: 2.0
agents:
  worker:
    trigger: manual
",
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["daemon_healthy"], json!(false));
    // No live gateway attached → tracked-session count is the on-disk snapshot
    // (empty in this fixture), and daemon-down forces any count to idle.
    assert_eq!(body["sessions_live"], json!(0));

    // Cost: 1.5 + 0.5 + 2.0 = 4.0 across the two projects.
    let total = body["cost_24h_usd"].as_f64().unwrap();
    assert!(
        (total - 4.0).abs() < 1e-9,
        "expected cost_24h_usd 4.0, got {total}"
    );

    // Per-vendor merge across projects: claude 1.5 + 2.0 = 3.5; codex 0.5.
    let by_vendor = body["cost_24h_by_vendor"].as_object().unwrap();
    assert!((by_vendor["claude"].as_f64().unwrap() - 3.5).abs() < 1e-9);
    assert!((by_vendor["codex"].as_f64().unwrap() - 0.5).abs() < 1e-9);

    // Aggregate budget cap = team-a's claude 5 + codex 2 = 7 (team-b: none).
    let cap = body["budget_cap_24h"].as_f64().unwrap();
    assert!(
        (cap - 7.0).abs() < 1e-9,
        "expected aggregate budget cap 7.0, got {cap}"
    );
}
