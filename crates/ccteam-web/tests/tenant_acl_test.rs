//! v0.8.18 档1 — per-user web isolation, end to end. A tenant token is gated
//! off every admin-only / global surface (IM credentials, user management,
//! hosts, status) and sees none of the admin's projects; the admin reaches all
//! of them. Proves the owner-reported leaks are closed.

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

#[tokio::test]
async fn tenant_token_is_gated_off_admin_surfaces() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    // A registered tenant — so its token resolves to a tenant identity.
    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.tenants_json()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    // An admin-owned project on disk (owner = the shared web-api pool). Written
    // directly (no scaffold → never touches the real ~/.claude.json).
    let admin_state = paths.project_state("adminproj");
    std::fs::create_dir_all(admin_state.parent().unwrap()).unwrap();
    let mut st = ccteam_core::ProjectState::initial_for_team("adminproj".into(), "dev".into());
    st.owner = Some("web:web-api".into());
    st.save(&admin_state).unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // Admin-only / global surfaces → 403 for the tenant.
    for path in ["/api/v1/config/im", "/api/v1/hosts", "/api/v1/status"] {
        let r = c
            .get(format!("http://{addr}{path}"))
            .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "tenant must be 403 on {path}");
    }

    // The admin reaches the global surface (200).
    let r = c
        .get(format!("http://{addr}/api/v1/config/im"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "admin reads IM config");

    // User management is admin-only.
    let r = c
        .post(format!("http://{addr}/api/v1/users"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .json(&serde_json::json!({"handle": "bob"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant can't create users");

    // `/api/v1/me` reflects the caller's identity (the SPA branches on it).
    let me_admin: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me_admin["is_admin"], serde_json::json!(true));

    let me_tenant: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me_tenant["is_admin"], serde_json::json!(false));
    assert_eq!(me_tenant["handle"], serde_json::json!("alice"));

    // `/auth/token` returns the CALLER's own wire token — NEVER the admin's (a
    // tenant getting the bootstrap token would be a privilege escalation).
    let tok: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/auth/token"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tok["wire_token"],
        serde_json::json!(format!("ccteam:{tenant_tok}")),
        "tenant gets its OWN token",
    );
    assert_ne!(
        tok["wire_token"],
        serde_json::json!(format!("ccteam:{ADMIN_HEX}")),
        "must NOT leak the admin/bootstrap token",
    );

    // Projects: the tenant owns none → empty list (admin's projects don't leak).
    let projects: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/projects"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        projects.as_array().map(|a| a.len()),
        Some(0),
        "a tenant sees none of the admin's projects",
    );

    // The admin-owned project: the tenant 404s on EVERY project-scoped route —
    // the `project_acl_layer` middleware covers them all (detail, marketplace,
    // roles, …), not just the handlers with a per-handler gate.
    for path in [
        "/api/v1/projects/adminproj",
        "/api/v1/projects/adminproj/marketplace",
        "/api/v1/projects/adminproj/roles",
    ] {
        let r = c
            .get(format!("http://{addr}{path}"))
            .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "tenant must 404 on the admin's {path}");
    }
    // …including the destructive DELETE (a tenant can't drop the admin's project).
    let r = c
        .delete(format!("http://{addr}/api/v1/projects/adminproj"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "tenant can't DELETE the admin's project");

    // The admin reaches its own project's detail.
    let r = c
        .get(format!("http://{addr}/api/v1/projects/adminproj"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "admin sees its own project detail");
}
