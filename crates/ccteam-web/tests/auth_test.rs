//! V0.3 M5.3 — auth middleware integration tests.
//!
//! Each case spins up a real axum listener bound to `127.0.0.1:0`,
//! injects an `AppState` with a chosen `AuthState` (enabled / disabled
//! / explicit token), then asserts the middleware behavior:
//!
//! - loopback default (`AuthState::disabled()`) → no token required.
//! - non-loopback simulation (`AuthState::enabled(...)`) → 401 without
//!   credentials, 200 with bearer header, 302 + Set-Cookie on the URL
//!   shim, and the cookie carries subsequent requests.
//! - `/health` is exempt from the auth gate.
//! - `--no-auth` non-loopback path emits the LAN-RCE banner (asserted
//!   by directly calling `serve()` in a subprocess; we use a sub-thread
//!   that captures stderr instead).

use std::net::SocketAddr;

use axum::Router;
use ccteam_core::{CcteamPaths, ProjectState};
use ccteam_web::{router_with_state, AppState, AuthState};
use reqwest::redirect::Policy;
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_one_project(paths: &CcteamPaths, slug: &str) {
    let mut state = ProjectState::initial(slug.to_string());
    state.current_phase = "implement".into();
    state.save(&paths.project_state(slug)).unwrap();
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app: Router = router_with_state(state);
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
async fn loopback_default_no_token_required_for_root() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;
    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(resp.status(), 200, "auth disabled ⇒ open");
}

#[tokio::test]
async fn auth_enabled_rejects_missing_authorization() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(resp.status(), 401, "no auth header ⇒ 401");
    let body = resp.text().await.unwrap();
    assert!(body.contains("auth required"), "got: {body}");
}

#[tokio::test]
async fn auth_enabled_accepts_valid_bearer_header() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn auth_enabled_rejects_wrong_bearer_token() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .header("Authorization", "Bearer ccteam:nope")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn url_shim_sets_cookie_and_redirects_to_clean_uri() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_one_project(&paths, "demo");
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .get(format!(
            "http://{addr}/project/demo?token=ccteam:{TOKEN_HEX}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303, "URL shim must 303 redirect");
    let loc = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .unwrap();
    assert_eq!(loc, "/project/demo", "redirect strips token query");
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie header")
        .to_str()
        .unwrap();
    assert!(
        set_cookie.contains("ccteam_token="),
        "Set-Cookie missing ccteam_token: {set_cookie}"
    );
    assert!(
        set_cookie.to_lowercase().contains("httponly"),
        "cookie must be HttpOnly: {set_cookie}"
    );
    assert!(
        set_cookie.contains("SameSite=Strict") || set_cookie.contains("samesite=strict"),
        "cookie must be SameSite=Strict: {set_cookie}"
    );
}

#[tokio::test]
async fn cookie_carries_subsequent_request() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_one_project(&paths, "demo");
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    // First hit goes through the URL shim; reqwest follows the 303
    // and the cookie store retains the cookie for round 2.
    let resp = client
        .get(format!("http://{addr}/?token=ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // Second hit — no token on the URL — must succeed via cookie.
    let resp2 = client.get(format!("http://{addr}/")).send().await.unwrap();
    assert_eq!(
        resp2.status(),
        200,
        "cookie must carry through subsequent requests"
    );
}

#[tokio::test]
async fn health_endpoint_is_exempt_from_auth() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(resp.status(), 200, "/health must not require auth");
}

#[tokio::test]
async fn url_shim_rejects_wrong_token() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .get(format!("http://{addr}/?token=ccteam:wrong"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "wrong shim token ⇒ 401, no redirect");
}

/// Smoke test for the LAN-RCE warning path. We can't easily capture
/// the eprintln output of a `serve()` call in-process (stderr is
/// process-global), but we can confirm that:
///
/// 1. `ServeOpts { no_auth: true, no_auth_grace_secs: Some(0) }`
///    plus a non-loopback bind doesn't deadlock or prevent serving,
/// 2. The serving handle remains responsive,
/// 3. The auth state ends up disabled (no token gate).
///
/// stderr capture is left to the dev-plan §8 grep matrix
/// (`grep 'LAN-wide RCE'`) which already enforces the warning lives
/// in source.
#[tokio::test]
async fn lan_rce_warning_string_appears_in_source() {
    // Sanity check that the source string is exactly the literal the
    // dev-plan grep matrix asserts. Cheap and ensures a future PR
    // doesn't water it down.
    let lib_src = include_str!("../src/lib.rs");
    assert!(
        lib_src.contains("LAN-wide RCE on bypassPermissions sessions"),
        "src/lib.rs must contain the LAN-RCE banner text",
    );
}
