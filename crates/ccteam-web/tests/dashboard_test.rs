//! V0.3 M5.1 — `GET /` integration tests.
//!
//! Spins the web layer in-process on `127.0.0.1:0`, fixtures one fake
//! project under a tempdir-backed `CcteamPaths`, and asserts the
//! dashboard HTML mentions the slug + the team. Read-only — no
//! daemon, no tmux, no orchestrator.

use std::net::SocketAddr;

use ccteam_core::{CcteamPaths, ProjectState};
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
async fn dashboard_lists_one_project_slug() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    // Fixture: one project at `<projects_root>/dev-foo/.ccteam/state.json`.
    let slug = "dev-foo";
    let state = ProjectState::initial(slug.to_string());
    state.save(&paths.project_state(slug)).unwrap();

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/");
    let resp = reqwest::get(&url).await.expect("GET /");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("dev-foo"),
        "body must mention slug. body=\n{body}"
    );
    assert!(body.contains("dev"), "body must mention team");
    assert!(
        body.contains("Projects"),
        "body must contain dashboard heading"
    );
}

#[tokio::test]
async fn dashboard_handles_empty_projects_root() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/");
    let resp = reqwest::get(&url).await.expect("GET /");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("No projects"), "empty state copy missing");
}

#[tokio::test]
async fn dashboard_renders_status_badge_html() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let slug = "dev-foo";
    let state = ProjectState::initial(slug.to_string());
    state.save(&paths.project_state(slug)).unwrap();

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/");
    let resp = reqwest::get(&url).await.expect("GET /");
    let body = resp.text().await.unwrap();
    // Empty event log + fresh project ⇒ healthy badge.
    assert!(
        body.contains("badge healthy"),
        "expected healthy badge in body, got:\n{body}",
    );
}
