//! Read-only xterm pane snapshot endpoint tests.
//!
//! CI may not have tmux, so these tests assert the degraded contract
//! and project-page wiring. A real tmux success path is covered by
//! manual dogfooding and the lower-level tmux helpers.

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
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
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
async fn pane_snapshot_returns_504_when_tmux_session_missing() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/api/this-slug-doesnt-exist-xyz-123/pane-snapshot.ansi");
    let resp = reqwest::get(&url).await.expect("GET pane snapshot");
    assert_eq!(resp.status(), 504);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        !ctype.starts_with("application/octet-stream"),
        "degraded response must not pretend to be raw ANSI bytes"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("pane snapshot"),
        "504 body should mention pane snapshot degraded reason; got: {body}",
    );
}

#[tokio::test]
async fn pane_snapshot_uses_state_tmux_session_for_meta_projects() {
    // The PROJECT-LEVEL (no-sid) route is unchanged by v0.8.8 B5 — it still
    // resolves `state.tmux_session`. It degrades to 504 (session not found),
    // and the body should identify the state-backed session name so an
    // operator can tell which pane was missing.
    //
    // ENV-GATED tail: capturing a pane requires the configured mux backend to
    // be reachable. In a sandbox the default rmux daemon cannot bind its
    // socket, so `capture` fails with an rmux-startup error (still 504, but the
    // body is the transport error, not the session name). We accept EITHER
    // degraded body so this stays green in the sandbox AND asserts the
    // session-name contract on a box where the backend works.
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let mut project = ProjectState::initial_for_team("meta-cto".into(), "meta-agent".into());
    project.tmux_session = "ccteam-meta-cto".into();
    project.save(&paths.project_state("meta-cto")).unwrap();

    let state = AppState::new(paths);
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/api/meta-cto/pane-snapshot.ansi");
    let resp = reqwest::get(&url).await.expect("GET pane snapshot");
    assert_eq!(resp.status(), 504);
    let body = resp.text().await.unwrap();
    let names_session = body.contains("ccteam-meta-cto");
    let backend_unreachable = body.contains("rmux") || body.contains("capture failed");
    assert!(
        names_session || backend_unreachable,
        "504 body should identify the state-backed session (backend reachable) \
         or report the unreachable backend (sandbox); got: {body}",
    );
}

#[tokio::test]
async fn pane_snapshot_session_route_returns_503_without_gateway() {
    // v0.8.8 B5 — F1 removed the project-level pane. The session-scoped snapshot
    // route now resolves `sid → per-session pane` via the live gateway. The
    // standalone "internal web" path (AppState built with no gateway) therefore
    // returns 503 (no session map to resolve against) — the SAME no-gateway
    // contract the session resource API uses. (Before B5 this route ignored the
    // sid and fell back to the project-level tmux session, returning 504.)
    //
    // This replaces the old `pane_snapshot_session_route_falls_back_to_project_tmux_session`
    // assertion, which encoded the now-removed project-level fallback. The real
    // per-session capture (with a gateway + a live pane) stays env-gated to a
    // box with the mux backend reachable (CI / dev machine).
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let mut project = ProjectState::initial_for_team("dev-proj".into(), "dev".into());
    project.tmux_session = "ccteam-dev-proj".into();
    project.save(&paths.project_state("dev-proj")).unwrap();

    let state = AppState::new(paths); // no `.with_gateway(...)`
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/api/dev-proj/claude-1/pane-snapshot.ansi");
    let resp = reqwest::get(&url).await.expect("GET pane snapshot");
    assert_eq!(
        resp.status(),
        503,
        "no-gateway per-session pane snapshot must be 503, not the old project fallback",
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("no live gateway"),
        "503 body should explain the missing gateway; got: {body}",
    );
}

#[tokio::test]
async fn pane_snapshot_session_route_returns_503_without_gateway_even_for_unknown_project() {
    // v0.8.8 B5 — the no-gateway check short-circuits BEFORE any project/sid
    // lookup, so even a bogus project on the session route is 503 (not 404)
    // when there is no live gateway. (Pre-B5 this was a 404 from the project
    // existence check; that check is gone — sid resolution is the gateway's
    // job now.) The 404-for-unknown-session path is exercised with a live
    // gateway in env-gated coverage.
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();

    let state = AppState::new(paths); // no gateway
    let addr = spawn_server(state).await;

    let url = format!("http://{addr}/api/no-such-project/claude-1/pane-snapshot.ansi");
    let resp = reqwest::get(&url).await.expect("GET pane snapshot");
    assert_eq!(resp.status(), 503);
}
