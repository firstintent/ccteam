//! V0.3 M5.3 — write-action route integration tests.
//!
//! Each case fixtures a project under a tempdir-backed
//! `CcteamPaths`, spins a real axum listener, fires the relevant POST,
//! and asserts:
//!
//! - 303 See Other → `/project/<slug>`
//! - the side effect on disk (inbox file / decision file / state.json)
//! - 4xx for input the route boundary rejects (path traversal, length).
//!
//! Auth is left disabled (loopback default) so these tests focus on
//! the route logic; auth is covered by `auth_test.rs`.

use std::net::SocketAddr;

use ccteam_core::{
    bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths, ProjectState,
};
use ccteam_web::{router_with_state, AppState};
use reqwest::redirect::Policy;
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
async fn post_btw_writes_inbox_file_and_redirects() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let inbox_dir = paths.project_ccteam_dir("demo").join("inbox");

    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .post(format!("http://{addr}/api/demo/btw"))
        .form(&[("text", "hello from web")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/project/demo",
    );
    let entries: Vec<_> = std::fs::read_dir(&inbox_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one inbox file written");
    let body = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(
        body.contains("hello from web"),
        "inbox body must include text, got: {body}"
    );
}

#[tokio::test]
async fn post_btw_rejects_empty_text() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/btw"))
        .form(&[("text", "")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn post_btw_rejects_overlong_text() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let big = "x".repeat(5000);
    let resp = client()
        .post(format!("http://{addr}/api/demo/btw"))
        .form(&[("text", big.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn post_inject_decision_writes_file_under_ccteam_dir() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let target = paths.project_ccteam_dir("demo").join("decision-via-web.md");

    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .post(format!("http://{addr}/api/demo/inject_decision"))
        .form(&[
            ("path", target.display().to_string().as_str()),
            ("body", "**META-AGENT DECISION**: ship\n"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    assert!(target.exists(), "decision file must be written");
    let body = std::fs::read_to_string(&target).unwrap();
    assert!(body.contains("**META-AGENT DECISION**: ship"));
}

#[tokio::test]
async fn post_inject_decision_rejects_path_outside_ccteam_dir() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/inject_decision"))
        .form(&[("path", "/etc/passwd"), ("body", "evil")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn post_inject_decision_rejects_dotdot_traversal() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let raw = format!(
        "{}/../../../etc/passwd",
        paths.project_ccteam_dir("demo").display()
    );
    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let resp = client()
        .post(format!("http://{addr}/api/demo/inject_decision"))
        .form(&[("path", raw.as_str()), ("body", "evil")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn post_pause_sets_user_pause_pending() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let state_path = paths.project_state("demo");

    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .post(format!("http://{addr}/api/demo/pause"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    let after = ProjectState::load(&state_path).unwrap();
    assert!(after.user_pause_pending, "pause must flip the flag");
}

#[tokio::test]
async fn post_resume_clears_user_pause_pending() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let state_path = paths.project_state("demo");

    // Pre-pause so resume has something to undo.
    {
        let mut s = ProjectState::load(&state_path).unwrap();
        s.user_pause_pending = true;
        s.save(&state_path).unwrap();
    }

    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .post(format!("http://{addr}/api/demo/resume"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    let after = ProjectState::load(&state_path).unwrap();
    assert!(!after.user_pause_pending, "resume must clear the flag");
}

#[tokio::test]
async fn post_btw_for_unknown_slug_returns_4xx() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::new(paths);
    let addr = spawn(state).await;
    let resp = client()
        .post(format!("http://{addr}/api/missing/btw"))
        .form(&[("text", "hi")])
        .send()
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status.is_client_error() || status.is_server_error(),
        "missing slug should fail; got {status}"
    );
}
