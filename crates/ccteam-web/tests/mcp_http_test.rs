//! v0.9 T4 — in-process router tests for `POST /mcp`.
//!
//! Acceptance: initialize echoes protocolVersion; tools/list = 8;
//! tools/call status succeeds; no/bad bearer → 401 (auth on AND off);
//! GET /mcp → 405; notification → 202 empty.

use std::net::SocketAddr;

use axum::Router;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, token::generate_or_load_token, AppState, AuthState};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

/// Write the admin web-token file under `paths` so the auth-disabled
/// `/mcp` path can constant-time-validate against a known hex.
fn seed_web_token(paths: &CcteamPaths, hex: &str) {
    let path = paths.web_token_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, hex).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();
    }
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

fn bearer(hex: &str) -> String {
    format!("Bearer ccteam:{hex}")
}

async fn post_mcp(
    addr: SocketAddr,
    auth: Option<&str>,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut req = client()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(h) = auth {
        req = req.header("Authorization", h);
    }
    req.send().await.unwrap()
}

// ── ① initialize echoes client protocolVersion ──────────────────────

#[tokio::test]
async fn mcp_initialize_echoes_client_protocol_version() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-03-26" }
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    assert!(body["result"]["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn mcp_initialize_defaults_protocol_version_when_omitted() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
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

// ── ② tools/list returns exactly 7 tools ────────────────────────────

#[tokio::test]
async fn mcp_tools_list_returns_exactly_seven() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
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
    assert_eq!(tools.len(), 7, "tools={tools:?}");
}

// ── ③ tools/call status succeeds ────────────────────────────────────

#[tokio::test]
async fn mcp_tools_call_status_succeeds() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
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
    // the vendor panel + routing notes appended as trailing plain text
    // (separated by a blank line). The JSON is the first `\n\n`-delimited
    // chunk; the appended panel is the new observability surface.
    let json_chunk = text.split("\n\n").next().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(json_chunk).unwrap();
    assert!(parsed.get("projects").is_some() || parsed.get("orchestrator").is_some());
    // Admin caller with no `project` arg and no cwd project → the panel falls
    // back to the local host with a note, but the vendor section is present.
    assert!(
        text.contains("vendors (project="),
        "status must append the vendor panel, got: {text}"
    );
    assert!(
        text.contains("routing notes"),
        "status must append the routing-notes transport, got: {text}"
    );
}

// ── ④ no/bad bearer → 401 (auth enabled AND disabled) ───────────────

#[tokio::test]
async fn mcp_auth_enabled_rejects_missing_and_bad_bearer() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;

    let missing = post_mcp(
        addr,
        None,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(missing.status(), 401, "no bearer under auth-enabled → 401");

    let bad = post_mcp(
        addr,
        Some("Bearer ccteam:deadbeef"),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(bad.status(), 401, "bad bearer under auth-enabled → 401");

    // Sanity: valid admin bearer works.
    let ok = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(
        ok.status(),
        200,
        "valid admin bearer under auth-enabled → 200"
    );
}

#[tokio::test]
async fn mcp_auth_disabled_still_requires_bearer() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    // Confirm generate_or_load_token reads our seed (same path the handler uses).
    let loaded = generate_or_load_token(&paths.web_token_path()).unwrap();
    assert_eq!(loaded, TOKEN_HEX);

    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let missing = post_mcp(
        addr,
        None,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(
        missing.status(),
        401,
        "no bearer under auth-disabled still → 401"
    );

    let bad = post_mcp(
        addr,
        Some("Bearer ccteam:0000000000000000000000000000000000000000000000000000000000000000"),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(
        bad.status(),
        401,
        "bad bearer under auth-disabled still → 401"
    );

    let ok = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(ok.status(), 200, "valid bearer under auth-disabled → 200");
}

// ── ⑤ GET /mcp → 405 ────────────────────────────────────────────────

#[tokio::test]
async fn mcp_get_returns_405() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = client()
        .get(format!("http://{addr}/mcp"))
        .header("Authorization", bearer(TOKEN_HEX))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
}

// ── ⑥ notification → 202 empty ──────────────────────────────────────

#[tokio::test]
async fn mcp_notification_returns_202_empty() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
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

// ── ⑦ v0.9 review fix — admin front door: cto-gate bypass + internal-bus refusal ──

/// The verified admin bearer must reach `session_*` WITHOUT ambient
/// `_caller_role`/`_caller_secret` args: with no gateway attached the op
/// reports gateway-down — proving the cto gate was bypassed (pre-fix this
/// returned "permission denied").
#[tokio::test]
async fn mcp_session_list_admin_bearer_bypasses_cto_gate() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let resp = post_mcp(
        addr,
        Some(&bearer(TOKEN_HEX)),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "session_list", "arguments": {} }
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("permission denied"),
        "admin bearer must bypass the cto gate, got: {text}"
    );
    assert!(text.contains("gateway not running"), "got: {text}");
}

/// The internal-bus methods (`permission/ask`, `interaction/ask`) are NOT
/// exposed on the HTTP front door — JSON-RPC `-32601` (they stay on
/// mcp.sock; HITL rides vendor-native in-band channels).
#[tokio::test]
async fn mcp_internal_bus_methods_not_exposed_over_http() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    for method in ["permission/ask", "interaction/ask"] {
        let resp = post_mcp(
            addr,
            Some(&bearer(TOKEN_HEX)),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": method,
                "params": {}
            }),
        )
        .await;
        assert_eq!(resp.status(), 200, "{method}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32601, "{method}: {body}");
        assert_eq!(body["id"], 9, "{method}: id must round-trip");
    }
}
