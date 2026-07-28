//! v0.9.11 TEAM-2 — division-of-labor charter endpoints
//! (`GET`/`PUT /api/v1/projects/{slug}/routing`).
//!
//! Same harness as `resource_api_test` (tempdir-backed `CcteamPaths`
//! injection — nothing touches the real `~/.ccteam` / `~/.claude.json`;
//! `disable_tool_surface_bootstrap_for_tests` guards the bootstrap path) +
//! `tenant_acl_test` (token auth on, tenant vs admin identities) for the
//! ACL cases.
//!
//! NOTE: `bootstrap_project` runs `ensure_ccteam_home`, which seeds the
//! global `~/.ccteam/routing.md` starter — precedence cases delete it first
//! and rebuild the ladder explicitly (none → global → project).

use std::net::SocketAddr;

use ccteam_core::tenants::TenantRegistry;
use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState, AuthState};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// Lower-hex sha256 — the digest convention the endpoints must report
/// (mirrors `hub::sha256_hex` / the MCP vendor panel).
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn get_routing_walks_none_then_global_then_project() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_file = paths.project_routing_notes("demo");
    let global_file = paths.global_routing_notes();
    // bootstrap seeded the global starter — clear it to start the ladder.
    std::fs::remove_file(&global_file).unwrap();

    let addr = spawn(AppState::new(paths)).await;
    let c = client();
    let url = format!("http://{addr}/api/v1/projects/demo/routing");

    // 1. none: no file anywhere → honest empty doc; `path` = the PUT target.
    let doc: serde_json::Value = c.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(doc["source"], "none");
    assert_eq!(doc["exists"], false);
    assert_eq!(doc["content"], "");
    assert_eq!(doc["sha256"], serde_json::Value::Null);
    assert_eq!(doc["updated_at"], serde_json::Value::Null);
    assert_eq!(doc["fallback_path"], serde_json::Value::Null);
    assert_eq!(doc["path"], project_file.display().to_string());

    // 2. global fallback: only ~/.ccteam/routing.md exists → its content is
    // served, but `path` stays the PROJECT save target (the file a PUT
    // creates); the served file is `fallback_path`.
    std::fs::write(&global_file, "GLOBAL charter\n").unwrap();
    let doc: serde_json::Value = c.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(doc["source"], "global");
    assert_eq!(doc["exists"], true);
    assert_eq!(doc["content"], "GLOBAL charter\n");
    assert_eq!(doc["sha256"], sha256_hex(b"GLOBAL charter\n"));
    assert_eq!(doc["path"], project_file.display().to_string());
    assert_eq!(doc["fallback_path"], global_file.display().to_string());
    assert!(
        doc["updated_at"]
            .as_str()
            .is_some_and(|ts| { chrono::DateTime::parse_from_rfc3339(ts).is_ok() }),
        "global updated_at must be RFC3339, got {:?}",
        doc["updated_at"]
    );

    // 3. project file wins over the (still present) global one.
    std::fs::create_dir_all(project_file.parent().unwrap()).unwrap();
    std::fs::write(&project_file, "PROJECT charter\n").unwrap();
    let doc: serde_json::Value = c.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(doc["source"], "project");
    assert_eq!(doc["exists"], true);
    assert_eq!(doc["content"], "PROJECT charter\n");
    assert_eq!(doc["sha256"], sha256_hex(b"PROJECT charter\n"));
    assert_eq!(doc["path"], project_file.display().to_string());
    assert_eq!(doc["fallback_path"], serde_json::Value::Null);

    // Unknown project stays 404 (same convention as every project resource).
    let r = c
        .get(format!("http://{addr}/api/v1/projects/ghost/routing"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn put_routing_round_trips_and_never_touches_the_global_file() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_file = paths.project_routing_notes("demo");
    let global_file = paths.global_routing_notes();
    std::fs::remove_file(&global_file).unwrap();

    let addr = spawn(AppState::new(paths)).await;
    let c = client();
    let url = format!("http://{addr}/api/v1/projects/demo/routing");

    // Create: PUT writes the project file (creating `.ccteam/`), returns the
    // digest + mtime a follow-up GET reports.
    let body = "# 分工\n- codex: build\n- grok: research\n";
    let r = c
        .put(&url)
        .json(&serde_json::json!({"content": body}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let saved: serde_json::Value = r.json().await.unwrap();
    assert_eq!(saved["sha256"], sha256_hex(body.as_bytes()));
    assert!(saved["updated_at"]
        .as_str()
        .is_some_and(|ts| chrono::DateTime::parse_from_rfc3339(ts).is_ok()));
    assert_eq!(std::fs::read_to_string(&project_file).unwrap(), body);

    // Reread through GET: the saved doc is the effective one.
    let doc: serde_json::Value = c.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(doc["source"], "project");
    assert_eq!(doc["content"], body);
    assert_eq!(doc["sha256"], saved["sha256"]);

    // Replace: a second PUT overwrites atomically (no stray tmp file).
    let r = c
        .put(&url)
        .json(&serde_json::json!({"content": "v2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(std::fs::read_to_string(&project_file).unwrap(), "v2");
    assert!(!project_file.with_file_name("routing.md.tmp").exists());

    // The global file was never created — web writes are project-only.
    assert!(
        !global_file.exists(),
        "PUT must never touch the global file"
    );
}

#[tokio::test]
async fn put_routing_rejects_oversize_body_with_413() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_file = paths.project_routing_notes("demo");

    let addr = spawn(AppState::new(paths)).await;
    let c = client();

    let oversize = "x".repeat(256 * 1024 + 1);
    let r = c
        .put(format!("http://{addr}/api/v1/projects/demo/routing"))
        .json(&serde_json::json!({"content": oversize}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    let body: serde_json::Value = r.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("256 KiB")),
        "413 must carry a readable cap message, got {body}"
    );
    assert!(!project_file.exists(), "an oversize PUT must not write");
}

/// ACL: the `/api/v1/projects/{slug}/…` shape rides `project_acl_layer`, so
/// a tenant is 404'd off another owner's charter (GET and PUT), the admin
/// stays out of tenant projects, an anonymous caller is 401'd, and each
/// identity reaches its OWN project.
#[tokio::test]
async fn routing_acl_isolates_owners_and_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    let mut reg = TenantRegistry::default();
    let alice = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let alice_tok = alice.web_token.clone();

    // Projects written directly (no scaffold): one admin-pool, one alice's.
    for (slug, owner) in [
        ("adminproj", "user:web-api".to_string()),
        ("aliceproj", format!("user:{}", alice.id)),
    ] {
        let state_path = paths.project_state(slug);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let mut st = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
        st.owner = Some(owner);
        st.save(&state_path).unwrap();
    }
    let alice_file = paths.project_routing_notes("aliceproj");

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let admin_auth = format!("Bearer ccteam:{ADMIN_HEX}");
    let alice_auth = format!("Bearer ccteam:{alice_tok}");
    let url = |slug: &str| format!("http://{addr}/api/v1/projects/{slug}/routing");

    // Anonymous: fail-closed 401 before any project resolution.
    let r = c.get(url("adminproj")).send().await.unwrap();
    assert_eq!(r.status(), 401, "anonymous caller must be 401'd");

    // Tenant → admin's project: 404 on GET and PUT (the choke point, not the
    // handler, gates both verbs).
    let r = c
        .get(url("adminproj"))
        .header("Authorization", &alice_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "tenant must not read the admin's charter");
    let r = c
        .put(url("adminproj"))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({"content": "hijack"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "tenant must not write the admin's charter");

    // Admin → tenant's project: also 404 (`can_see_owner` keeps the admin
    // out of tenant projects).
    let r = c
        .get(url("aliceproj"))
        .header("Authorization", &admin_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "admin must not peek into a tenant project");

    // Each identity reaches its OWN project, end to end.
    let r = c
        .get(url("adminproj"))
        .header("Authorization", &admin_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "admin reads its own charter");
    let r = c
        .put(url("aliceproj"))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({"content": "alice's charter"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "tenant writes her own charter");
    assert_eq!(
        std::fs::read_to_string(&alice_file).unwrap(),
        "alice's charter"
    );
    let doc: serde_json::Value = c
        .get(url("aliceproj"))
        .header("Authorization", &alice_auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc["source"], "project");
    assert_eq!(doc["content"], "alice's charter");
}
