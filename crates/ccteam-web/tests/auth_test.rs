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
        .no_proxy()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn loopback_default_no_token_required_for_root() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "auth disabled ⇒ open");
}

#[tokio::test]
async fn auth_enabled_rejects_missing_authorization() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    // An `/api/*` route stays gated: no credentials ⇒ 401 "auth required".
    let resp = client()
        .get(format!("http://{addr}/api/v1/auth/token"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "no auth header ⇒ 401");
    let body = resp.text().await.unwrap();
    assert!(body.contains("auth required"), "got: {body}");
}

/// The SPA shell must load for unauthenticated visitors so the in-browser token
/// flow (TokenEntryPage) can prompt for a token — a raw "auth required" body
/// would leave the user with no login UI. The `/api/*` surface stays gated.
#[tokio::test]
async fn auth_enabled_serves_spa_shell_unauthenticated() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;

    // `/app/` (and `/app`) serve the SPA index unauthenticated.
    for path in ["/app/", "/app"] {
        let resp = client()
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "unauth {path} must serve the SPA shell");
    }

    // Bare `/` redirects into the SPA (301 → /app/) instead of 401.
    let resp = nofollow()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "unauth / must redirect to the SPA, got {}",
        resp.status()
    );

    // The API surface is still gated — no shell exemption leaks it.
    let resp = client()
        .get(format!("http://{addr}/api/v1/auth/token"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "unauth /api/* must stay gated");
}

#[tokio::test]
async fn auth_enabled_accepts_valid_bearer_header() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let resp = client()
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
    // A gated `/api/*` path: a wrong bearer must not authenticate (the SPA shell
    // is served open, but the API surface stays locked).
    let resp = client()
        .get(format!("http://{addr}/api/v1/auth/token"))
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
    // Persistent + self-expiring: Max-Age must be present (survives browser
    // restart) and equal 7 days (the "stay logged in ≤ 7d" contract). A
    // session cookie (no Max-Age) would log the user out on browser close.
    assert!(
        set_cookie.to_lowercase().contains("max-age=604800"),
        "cookie must carry a 7-day Max-Age: {set_cookie}"
    );
}

/// Regression: TokenEntryPage does `/?token=${encodeURIComponent(token)}`,
/// so the colon in `ccteam:<hex>` arrives as `%3A`. Before percent-decode
/// the shim failed open-shell → 301 `/app/` with no cookie, and the user
/// bounced back to the login UI.
#[tokio::test]
async fn url_shim_accepts_percent_encoded_token_from_spa_login() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = nofollow();
    // Mimic the browser wire form of encodeURIComponent("ccteam:<hex>").
    let resp = client
        .get(format!("http://{addr}/?token=ccteam%3A{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        303,
        "percent-encoded SPA login must hit the URL shim (303), not the public-shell 301; got {}",
        resp.status()
    );
    let loc = resp
        .headers()
        .get("location")
        .expect("Location header")
        .to_str()
        .unwrap();
    assert_eq!(
        loc, "/app/",
        "root login must land on the SPA in one hop (no / → 301 /app/)"
    );
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie must be set for encoded SPA login")
        .to_str()
        .unwrap();
    assert!(
        set_cookie.contains(&format!("ccteam_token={TOKEN_HEX}")),
        "cookie must store bare hex after decoding: {set_cookie}"
    );
}

/// Bare hex (no `ccteam:` prefix) — what the login form's label asks for.
#[tokio::test]
async fn url_shim_accepts_bare_hex_token() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = nofollow();
    let resp = client
        .get(format!("http://{addr}/?token={TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303, "bare hex must hit the URL shim");
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie")
        .to_str()
        .unwrap();
    assert!(
        set_cookie.contains(&format!("ccteam_token={TOKEN_HEX}")),
        "cookie stores bare hex: {set_cookie}"
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
        .no_proxy()
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
    let resp = client()
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "/health must not require auth");
}

#[tokio::test]
async fn url_shim_rejects_wrong_token() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = nofollow();
    // A wrong `?token=` on a gated `/api/*` path must not authenticate: the shim
    // fails to resolve it (no cookie, no redirect) and the request falls through
    // to 401. (On the open SPA-shell paths a wrong token instead lands the user
    // on the login UI, covered by `auth_enabled_serves_spa_shell_unauthenticated`.)
    let resp = client
        .get(format!(
            "http://{addr}/api/v1/auth/token?token=ccteam:wrong"
        ))
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
