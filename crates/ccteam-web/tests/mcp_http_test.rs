//! v0.9 T4 — in-process router tests for `POST /mcp`.
//!
//! Acceptance: initialize echoes protocolVersion; tools/list matches the
//! managed Pi bridge readiness contract;
//! tools/call status succeeds; no/bad bearer → 401 (auth on AND off);
//! GET /mcp → 405; notification → 202 empty.
//!
//! **Credential.** The subject here is the TRANSPORT contract, so it uses the
//! lightest credential that can carry it: a user-scoped ENROLLMENT credential,
//! which needs no gateway and no live session. It used to use the admin web
//! token; that family is no longer accepted at `/mcp` at all (see
//! `mcp_tenant_bearer_test` for the refusal, and `routes::mcp`'s module doc for
//! why a credential a static config file can carry must grant nothing).
//!
//! Two tests left with that family: `mcp_session_list_admin_bearer_bypasses_cto_gate`
//! (an owner front door that no longer exists) and
//! `mcp_internal_bus_methods_not_exposed_over_http` (the internal-bus refusal
//! applies to front-door callers only — every HTTP caller is now an agent
//! identity, and the stronger statement, that a console token cannot reach
//! `permission/ask` at all, is asserted in `mcp_tenant_bearer_test`).

use std::net::SocketAddr;

use axum::Router;
use ccteam_core::enroll::{self, EnrollCredential, EnrollScope};
use ccteam_core::CcteamPaths;
use ccteam_harness::PI_REQUIRED_MCP_TOOL_NAMES;
use ccteam_web::{router_with_state, AppState, AuthState};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

/// A hand-started client's credential, minted under the tempdir root the router
/// verifies against (`_in(root)`-injected, so nothing reaches the real
/// `~/.ccteam`). User-scoped: it names no project, which is exactly the posture
/// of a vendor's global config entry.
fn mint_enroll(paths: &CcteamPaths) -> EnrollCredential {
    enroll::mint_in(&paths.root, EnrollScope::User, "user:web-api", None).unwrap()
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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn post_mcp(
    addr: SocketAddr,
    auth: Option<&str>,
    mcp_session_id: Option<&str>,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut req = client()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(h) = auth {
        req = req.header("Authorization", format!("Bearer {h}"));
    }
    if let Some(id) = mcp_session_id {
        req = req.header("Mcp-Session-Id", id);
    }
    req.send().await.unwrap()
}

fn initialize_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-03-26" }
    })
}

/// Run `initialize` and return the `Mcp-Session-Id` the server issued — the
/// per-process identity every later request must echo.
async fn initialize(addr: SocketAddr, bearer: &str) -> String {
    let resp = post_mcp(addr, Some(bearer), None, initialize_body()).await;
    assert_eq!(resp.status(), 200, "initialize must succeed");
    resp.headers()
        .get("mcp-session-id")
        .expect("initialize issues the per-process identity")
        .to_str()
        .unwrap()
        .to_string()
}

/// The common fixture: a router with no gateway plus one enrollment credential.
async fn serve(tmp: &TempDir, auth: AuthState) -> (SocketAddr, EnrollCredential) {
    let paths = fake_paths(tmp.path());
    let cred = mint_enroll(&paths);
    let addr = spawn(AppState::with_auth(paths, auth)).await;
    (addr, cred)
}

// ── ① initialize echoes client protocolVersion ──────────────────────

#[tokio::test]
async fn mcp_initialize_echoes_client_protocol_version() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;

    let resp = post_mcp(addr, Some(&cred.bearer()), None, initialize_body()).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    assert!(body["result"]["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn mcp_initialize_defaults_protocol_version_when_omitted() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;

    let resp = post_mcp(
        addr,
        Some(&cred.bearer()),
        None,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
}

// ── ② tools/list returns the full tool surface ─────────────────────

#[tokio::test]
async fn mcp_tools_list_returns_the_full_surface() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;
    let id = initialize(addr, &cred.bearer()).await;

    let resp = post_mcp(
        addr,
        Some(&cred.bearer()),
        Some(&id),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let mut actual = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    let mut expected = PI_REQUIRED_MCP_TOOL_NAMES.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "tools={tools:?}");
}

// ── ③ tools/call status succeeds ────────────────────────────────────

/// `status` answers every authenticated caller, including one with no project:
/// a client must be able to see what exists and where it stands. What it does
/// NOT get is a vendor panel for a workspace it never named — the panel is
/// scoped to the caller's own project, and this credential (user-scoped, no
/// ledger node) has none. The bound-caller panel is covered where a real
/// project exists: `mcp_enroll_test::a_bound_client_gets_the_vendor_panel_for_its_own_project`.
#[tokio::test]
async fn mcp_tools_call_status_succeeds() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;
    let id = initialize(addr, &cred.bearer()).await;

    let resp = post_mcp(
        addr,
        Some(&cred.bearer()),
        Some(&id),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "status", "arguments": {} }
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    // v0.10 T1 — the daemon-aware `status` returns the base health JSON, then
    // the vendor panel or an honest note appended as trailing plain text
    // (separated by a blank line). The JSON is the first `\n\n`-delimited chunk.
    let json_chunk = text.split("\n\n").next().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(json_chunk).unwrap();
    assert!(parsed.get("projects").is_some() || parsed.get("orchestrator").is_some());
    assert!(
        text.contains("the vendor panel is scoped to your own project"),
        "a projectless caller must be told why the panel is withheld, got: {text}"
    );
    assert!(
        !text.contains("vendors (project="),
        "ccteam must not render a panel for a project this caller never named, got: {text}"
    );
}

// ── ④ no/bad bearer → 401 (auth enabled AND disabled) ───────────────

#[tokio::test]
async fn mcp_auth_enabled_rejects_missing_and_bad_bearer() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::enabled(TOKEN_HEX.into())).await;
    let list = || serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});

    let missing = post_mcp(addr, None, None, list()).await;
    assert_eq!(missing.status(), 401, "no bearer under auth-enabled → 401");

    let bad = post_mcp(addr, Some("ccteam-enroll:deadbeef:nope"), None, list()).await;
    assert_eq!(bad.status(), 401, "bad bearer under auth-enabled → 401");

    // The web-console token is a different family entirely, valid or not.
    let web = post_mcp(addr, Some(&format!("ccteam:{TOKEN_HEX}")), None, list()).await;
    assert_eq!(
        web.status(),
        401,
        "a LIVE web token is still not an MCP credential"
    );

    // Sanity: a valid enrollment credential works.
    let id = initialize(addr, &cred.bearer()).await;
    let ok = post_mcp(addr, Some(&cred.bearer()), Some(&id), list()).await;
    assert_eq!(
        ok.status(),
        200,
        "valid enrollment bearer under auth-enabled → 200"
    );
}

#[tokio::test]
async fn mcp_auth_disabled_still_requires_bearer() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;
    let list = || serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});

    let missing = post_mcp(addr, None, None, list()).await;
    assert_eq!(
        missing.status(),
        401,
        "no bearer under auth-disabled still → 401"
    );

    let bad = post_mcp(addr, Some("ccteam-enroll:deadbeef:nope"), None, list()).await;
    assert_eq!(
        bad.status(),
        401,
        "bad bearer under auth-disabled still → 401"
    );

    // Loopback / --no-auth does not resurrect the web family: it was the ONE
    // configuration where the old gate loaded the admin token off disk itself.
    let web = post_mcp(addr, Some(&format!("ccteam:{TOKEN_HEX}")), None, list()).await;
    assert_eq!(
        web.status(),
        401,
        "no web token is accepted, gate off or on"
    );

    let id = initialize(addr, &cred.bearer()).await;
    let ok = post_mcp(addr, Some(&cred.bearer()), Some(&id), list()).await;
    assert_eq!(ok.status(), 200, "valid bearer under auth-disabled → 200");
}

// ── ⑤ GET /mcp → 405 ────────────────────────────────────────────────

#[tokio::test]
async fn mcp_get_returns_405() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;

    let resp = client()
        .get(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {}", cred.bearer()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
}

// ── ⑥ notification → 202 empty ──────────────────────────────────────

#[tokio::test]
async fn mcp_notification_returns_202_empty() {
    let tmp = TempDir::new().unwrap();
    let (addr, cred) = serve(&tmp, AuthState::disabled()).await;
    let id = initialize(addr, &cred.bearer()).await;

    let resp = post_mcp(
        addr,
        Some(&cred.bearer()),
        Some(&id),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    )
    .await;
    assert_eq!(resp.status(), 202);
    let body = resp.bytes().await.unwrap();
    assert!(
        body.is_empty(),
        "notification body must be empty, got {body:?}"
    );
}
