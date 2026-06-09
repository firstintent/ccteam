//! V0.3.2 F59 — legacy dashboard route retirement.
//!
//! The askama-rendered `GET /` dashboard was retired in favor of the
//! React SPA. The server keeps the old root path as a permanent
//! redirect so bookmarks and the CLI's printed URL keep working.

use std::net::SocketAddr;

use ccteam_core::{CcteamPaths, ProjectState, TeamKind};
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
        .no_proxy()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn assert_root_redirects_to_spa(paths: CcteamPaths) {
    let addr = spawn_server(AppState::new(paths)).await;
    let resp = nofollow()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("GET /");
    assert_eq!(resp.status(), 301);
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "/app/",
    );
}

#[tokio::test]
async fn dashboard_root_redirects_to_spa_with_one_project() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-foo";
    ProjectState::initial(slug.to_string())
        .save(&paths.project_state(slug))
        .unwrap();

    assert_root_redirects_to_spa(paths).await;
}

#[tokio::test]
async fn dashboard_root_redirects_to_spa_with_empty_projects_root() {
    let tmp = TempDir::new().unwrap();
    assert_root_redirects_to_spa(fake_paths(tmp.path())).await;
}

#[tokio::test]
async fn dashboard_root_redirect_is_independent_of_team_kind() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let mut workflow = ProjectState::initial("dev-workflow".to_string());
    workflow.current_phase = "implement".into();
    workflow.save(&paths.project_state("dev-workflow")).unwrap();

    let mut multi = ProjectState::initial("dev-multi".to_string());
    multi.team_kind = TeamKind::MultiWorkflow;
    multi.current_phase = "must-not-render".into();
    multi.save(&paths.project_state("dev-multi")).unwrap();

    assert_root_redirects_to_spa(paths).await;
}

#[tokio::test]
async fn dashboard_followed_redirect_reaches_spa_index() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let resp = client()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("GET / with default redirect policy");
    assert_eq!(resp.status(), 200);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ctype.contains("text/html"), "content-type={ctype}");
}
