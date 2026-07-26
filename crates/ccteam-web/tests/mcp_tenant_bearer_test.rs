//! Phase 1 external-agent MCP authentication: a tenant's existing web bearer
//! reaches the same `/mcp` ingress without being promoted to admin.

use std::ffi::OsString;
use std::net::SocketAddr;

use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use serial_test::serial;
use tempfile::TempDir;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

struct EnvGuard {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let old = std::env::var_os(key);
        // SAFETY: this integration-test binary serializes its only env-mutating
        // test, and the guard restores the prior value before process exit.
        unsafe { std::env::set_var(key, value) };
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `EnvGuard::set`; the test is serialized.
        unsafe {
            match &self.old {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router_with_state(state))
            .await
            .unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

async fn post_mcp(addr: SocketAddr, bearer: &str, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn tools_list(addr: SocketAddr, bearer: &str) -> reqwest::Response {
    post_mcp(
        addr,
        bearer,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await
}

#[tokio::test]
#[serial]
async fn tenant_bearer_is_accepted_while_bad_and_admin_tokens_keep_their_posture() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("HOME", tmp.path());
    let _ccteam_home = EnvGuard::set("CCTEAM_HOME", &tmp.path().join(".ccteam"));
    let paths = fake_paths(tmp.path());

    let mut tenants = TenantRegistry::default();
    let tenant = tenants.add("alice");
    tenants.save(&paths.users_dir()).unwrap();
    let admin_project = paths.project_state("admin-project");
    std::fs::create_dir_all(admin_project.parent().unwrap()).unwrap();
    let mut project = ccteam_core::ProjectState::initial("admin-project".into());
    project.owner = Some("user:web-api".into());
    project.save(&admin_project).unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;

    let tenant_response = tools_list(addr, &format!("ccteam:{}", tenant.web_token)).await;
    assert_eq!(
        tenant_response.status(),
        reqwest::StatusCode::OK,
        "a registered tenant web bearer must authenticate at /mcp"
    );
    let body: serde_json::Value = tenant_response.json().await.unwrap();
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 7);

    // This distinguishes User from Admin at the real route boundary: an admin
    // caller naming `admin-project` would get the status vendor panel, while
    // the tenant is denied by project ownership first.
    let scoped = post_mcp(
        addr,
        &format!("ccteam:{}", tenant.web_token),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "status", "arguments": {"project": "admin-project"}}
        }),
    )
    .await;
    assert_eq!(scoped.status(), reqwest::StatusCode::OK);
    let scoped_body: serde_json::Value = scoped.json().await.unwrap();
    assert_eq!(scoped_body["result"]["isError"], true, "{scoped_body}");
    assert_eq!(
        scoped_body["result"]["content"][0]["text"],
        "status: project not found"
    );

    assert_eq!(
        tools_list(addr, "ccteam:deadbeef").await.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "an unknown web-family token remains rejected"
    );
    assert_eq!(
        tools_list(addr, &format!("ccteam:{ADMIN_HEX}"))
            .await
            .status(),
        reqwest::StatusCode::OK,
        "the admin bearer remains accepted"
    );
}
