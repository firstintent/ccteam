//! v0.8.24 F1.12 — project third-party MCP server registration, end to end.
//!
//! Proves:
//! 1. admin POST url-type → 201, idempotently merged into `<project>/.mcp.json`
//!    (never clobbers other entries); GET lists it
//! 2. stdio entries mask env VALUES on read (names only)
//! 3. validation: url XOR command, http(s)-only url, reserved `ccteam` name
//! 4. fail-closed ACL: tenant is 404 on a project it can't see (project ACL)
//!    and 403 on the write even for its OWN project (admin-only config write)

use std::net::SocketAddr;

use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn seed_project(paths: &CcteamPaths, slug: &str, owner: Option<&str>) {
    let state_path = paths.project_state(slug);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut st = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
    st.owner = owner.map(str::to_string);
    st.save(&state_path).unwrap();
}

#[tokio::test]
async fn register_and_list_third_party_mcp_servers() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha", Some("user:web-api"));
    let project_dir = paths.project_dir("alpha");

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");

    // Empty project → empty list, ccteam not registered.
    let list: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/projects/alpha/mcp-servers"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["servers"].as_array().unwrap().len(), 0);
    assert_eq!(list["ccteam_registered"], false);

    // Register a url-type server (context7 template shape).
    let r = c
        .post(format!("http://{addr}/api/v1/projects/alpha/mcp-servers"))
        .header("Authorization", &auth)
        .json(&serde_json::json!({
            "name": "context7",
            "url": "https://mcp.context7.com/mcp"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);

    // And a stdio server with an env token (playwright template shape).
    let r = c
        .post(format!("http://{addr}/api/v1/projects/alpha/mcp-servers"))
        .header("Authorization", &auth)
        .json(&serde_json::json!({
            "name": "playwright",
            "command": "npx",
            "args": ["@playwright/mcp@latest"],
            "env": {"PW_TOKEN": "sekrit"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);

    // On-disk config: both entries present (merge, not clobber), valid JSON.
    let raw = std::fs::read_to_string(project_dir.join(".mcp.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["mcpServers"]["context7"]["type"], "http");
    assert_eq!(v["mcpServers"]["playwright"]["command"], "npx");
    assert_eq!(v["mcpServers"]["playwright"]["env"]["PW_TOKEN"], "sekrit");

    // Idempotent re-register (same body) → 201, byte-stable config.
    let r = c
        .post(format!("http://{addr}/api/v1/projects/alpha/mcp-servers"))
        .header("Authorization", &auth)
        .json(&serde_json::json!({
            "name": "context7",
            "url": "https://mcp.context7.com/mcp"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let raw2 = std::fs::read_to_string(project_dir.join(".mcp.json")).unwrap();
    assert_eq!(raw, raw2, "same entry must be byte-idempotent");

    // GET lists both; env VALUES masked (names only).
    let list: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/projects/alpha/mcp-servers"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let servers = list["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 2);
    let body = list.to_string();
    assert!(
        !body.contains("sekrit"),
        "env value must never echo: {body}"
    );
    let pw = servers.iter().find(|s| s["name"] == "playwright").unwrap();
    assert_eq!(pw["env_keys"][0], "PW_TOKEN");

    // Validation: reserved name / bad url / url+command both / neither.
    for bad in [
        serde_json::json!({"name": "ccteam", "url": "https://x"}),
        serde_json::json!({"name": "a", "url": "ftp://x"}),
        serde_json::json!({"name": "a", "url": "https://x", "command": "npx"}),
        serde_json::json!({"name": "a"}),
        serde_json::json!({"name": "bad name!", "url": "https://x"}),
    ] {
        let r = c
            .post(format!("http://{addr}/api/v1/projects/alpha/mcp-servers"))
            .header("Authorization", &auth)
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "must reject {bad}");
    }

    // Unknown project → 404.
    let r = c
        .get(format!("http://{addr}/api/v1/projects/ghost/mcp-servers"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn mcp_servers_acl_fails_closed_for_tenants() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tauth = format!("Bearer ccteam:{}", tenant.web_token);
    let tenant_owner = format!("user:{}", tenant.id);

    // adminproj (not alice's) + aliceproj (hers).
    seed_project(&paths, "adminproj", Some("user:web-api"));
    seed_project(&paths, "aliceproj", Some(tenant_owner.as_str()));

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // Project ACL: a project the tenant can't see 404s (read AND write).
    let r = c
        .get(format!(
            "http://{addr}/api/v1/projects/adminproj/mcp-servers"
        ))
        .header("Authorization", &tauth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "tenant must not see adminproj");
    let r = c
        .post(format!(
            "http://{addr}/api/v1/projects/adminproj/mcp-servers"
        ))
        .header("Authorization", &tauth)
        .json(&serde_json::json!({"name": "x", "url": "https://x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Own project: read OK, but the config WRITE is admin-only → 403.
    let r = c
        .get(format!(
            "http://{addr}/api/v1/projects/aliceproj/mcp-servers"
        ))
        .header("Authorization", &tauth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "tenant reads own project's servers");
    let r = c
        .post(format!(
            "http://{addr}/api/v1/projects/aliceproj/mcp-servers"
        ))
        .header("Authorization", &tauth)
        .json(&serde_json::json!({"name": "x", "url": "https://x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "config write is admin-only");
}
