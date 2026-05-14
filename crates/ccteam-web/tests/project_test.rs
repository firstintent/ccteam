//! V0.3.2 F59 — legacy project/session route retirement.
//!
//! `GET /project/<slug>` and `GET /session/<slug>/<sid>` no longer
//! render askama HTML. They are permanent redirects into the React SPA.
//! JSON detail parity is covered by `api_v1_test.rs`; this file keeps
//! the legacy bookmark contract and form write-action contract pinned.

use std::fs;
use std::net::SocketAddr;

use ccteam_core::{CcteamPaths, HarnessKind, ProjectState, SessionRecord, TeamKind};
use ccteam_web::{router_with_state, AppState};
use chrono::Utc;
use reqwest::redirect::Policy;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_flex_project(paths: &CcteamPaths, slug: &str) {
    let mut state = ProjectState::initial(slug.to_string());
    state.team_kind = TeamKind::Flex;
    state.sessions.insert(
        "claude-1".into(),
        SessionRecord {
            harness: HarnessKind::Claude,
            tmux_session: format!("ccteam-{slug}-claude-1"),
            started_at: Utc::now(),
            pid: None,
            job_id: None,
        },
    );
    state.save(&paths.project_state(slug)).unwrap();
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

fn nofollow() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

fn location(resp: &reqwest::Response) -> &str {
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

#[tokio::test]
async fn project_detail_redirects_registered_slug_to_spa() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-foo";
    ProjectState::initial(slug.to_string())
        .save(&paths.project_state(slug))
        .unwrap();

    let addr = spawn_server(AppState::new(paths)).await;
    let resp = nofollow()
        .get(format!("http://{addr}/project/{slug}"))
        .send()
        .await
        .expect("GET /project/<slug>");
    assert_eq!(resp.status(), 301);
    assert_eq!(location(&resp), "/app/p/dev-foo");
}

#[tokio::test]
async fn project_detail_redirects_unknown_slug_without_lookup() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let addr = spawn_server(AppState::new(paths)).await;
    let resp = nofollow()
        .get(format!("http://{addr}/project/does-not-exist"))
        .send()
        .await
        .expect("GET /project/<missing>");
    assert_eq!(resp.status(), 301);
    assert_eq!(location(&resp), "/app/p/does-not-exist");
}

#[tokio::test]
async fn project_detail_redirect_location_url_encodes_slug_segment() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let addr = spawn_server(AppState::new(paths)).await;
    let resp = nofollow()
        .get(format!("http://{addr}/project/dev%20space"))
        .send()
        .await
        .expect("GET /project/<encoded>");
    assert_eq!(resp.status(), 301);
    assert_eq!(location(&resp), "/app/p/dev%20space");
}

#[tokio::test]
async fn session_detail_redirects_registered_flex_session_to_spa() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let resp = nofollow()
        .get(format!("http://{addr}/session/{slug}/claude-1"))
        .send()
        .await
        .expect("GET /session/<slug>/<sid>");
    assert_eq!(resp.status(), 301);
    assert_eq!(location(&resp), "/app/p/dev-flex/s/claude-1");
}

#[tokio::test]
async fn session_detail_redirects_unknown_sid_without_lookup() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let resp = nofollow()
        .get(format!("http://{addr}/session/{slug}/claude-99"))
        .send()
        .await
        .expect("GET /session/<slug>/<sid>");
    assert_eq!(resp.status(), 301);
    assert_eq!(location(&resp), "/app/p/dev-flex/s/claude-99");
}

#[tokio::test]
async fn session_btw_posts_to_session_inbox() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let paths_for_assert = paths.clone();
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/api/{slug}/claude-1/btw");
    let resp = nofollow()
        .post(url)
        .form(&[("text", "hello session")])
        .send()
        .await
        .expect("POST session btw");
    assert_eq!(resp.status(), 303);
    assert_eq!(location(&resp), "/session/dev-flex/claude-1");

    let inbox_dir = paths_for_assert
        .project_session_dir(slug, "claude-1")
        .join("inbox");
    let mut entries = fs::read_dir(&inbox_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries.len(), 1);
    let body = fs::read_to_string(&entries[0]).unwrap();
    assert!(body.contains("source: ccteam-web"), "body=\n{body}");
    assert!(body.contains("hello session"), "body=\n{body}");
}
