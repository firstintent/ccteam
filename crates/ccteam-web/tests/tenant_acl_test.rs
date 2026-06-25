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
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    // An admin-owned project on disk (owner = the shared web-api pool). Written
    // directly (no scaffold → never touches the real ~/.claude.json).
    let admin_state = paths.project_state("adminproj");
    std::fs::create_dir_all(admin_state.parent().unwrap()).unwrap();
    let mut st = ccteam_core::ProjectState::initial_for_team("adminproj".into(), "dev".into());
    st.owner = Some("user:web-api".into());
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

    // v0.8.20 F3: the admin can RE-REVEAL a tenant's personal login link (a
    // separate admin-gated route, so the list still strips the token); a tenant
    // cannot reach it.
    let link: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/users/{}/link", tenant.id))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        link["personal_link"],
        serde_json::json!(format!("/?token=ccteam:{tenant_tok}")),
        "admin re-reveals the tenant's personal link",
    );
    let r = c
        .get(format!("http://{addr}/api/v1/users/{}/link", tenant.id))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        403,
        "a tenant can't reveal links via the admin route",
    );

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

/// v0.8.20 ownership-leak fix: the session cookie (the CURRENT login) must win
/// over a STALE `Authorization: Bearer` the SPA fetch shim still injects from a
/// prior admin login. Before the fix, `auth_layer` checked the header FIRST, so
/// a cached admin Bearer outranked the fresh tenant cookie → the tenant's new
/// projects were stamped `user:web-api` (admin pool) instead of `user:<tenant>`
/// and vanished from the tenant's own list. Now: shim → cookie → header.
#[tokio::test]
async fn session_cookie_beats_stale_bearer_header() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // The leak scenario: a fresh tenant cookie + a STALE admin Bearer. The
    // cookie (the current login) must win → identity resolves to the tenant.
    let me: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Cookie", format!("ccteam_token={tenant_tok}"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        me["is_admin"],
        serde_json::json!(false),
        "the fresh tenant cookie must beat a stale admin Bearer",
    );
    assert_eq!(me["handle"], serde_json::json!("alice"));

    // `/auth/token` (used by the SPA to re-scope its Bearer) returns the COOKIE
    // identity's token, not the stale header's → the SPA heals to the tenant.
    let tok: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/auth/token"))
        .header("Cookie", format!("ccteam_token={tenant_tok}"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tok["wire_token"],
        serde_json::json!(format!("ccteam:{tenant_tok}")),
        "auth/token reflects the cookie identity, not the stale Bearer",
    );

    // Fallback intact: a Bearer with NO cookie still resolves (API / iOS-PWA).
    let me_bearer: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        me_bearer["is_admin"],
        serde_json::json!(true),
        "Bearer remains the fallback when there is no cookie",
    );
}

/// v0.8.20 F2: per-user IM bot config — PUT /api/v1/me/im (self-serve) + admin
/// /api/v1/users/{id}/im. Covers storage + ACL + replace semantics. The
/// Telegram `getMe` path reuses the global onboarding validator (covered by
/// im_config_test), so these cases use Lark / empty bodies — no network call.
#[tokio::test]
async fn per_tenant_im_config_self_serve_and_admin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let mut reg = TenantRegistry::default();
    let alice = reg.add("alice");
    let bob = reg.add("bob");
    reg.save(&paths.users_dir()).unwrap();
    let alice_tok = alice.web_token.clone();
    let tenants_path = paths.users_dir();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // 1. Self-serve: alice sets her OWN Lark app via /me/im → lands on alice.
    let r = c
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", format!("Bearer ccteam:{alice_tok}"))
        .json(&serde_json::json!({"lark": {"app_id": "cli_a", "app_secret": "sek"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "alice configures her own IM bot");
    let a = TenantRegistry::load(&tenants_path);
    let a = a.by_id(&alice.id).unwrap().clone();
    assert_eq!(a.lark.as_ref().unwrap().app_id, "cli_a");
    assert!(a.telegram.is_none(), "no telegram in body → none");

    // 2. A tenant can NOT set another tenant's bot via the admin route.
    let r = c
        .put(format!("http://{addr}/api/v1/users/{}/im", bob.id))
        .header("Authorization", format!("Bearer ccteam:{alice_tok}"))
        .json(&serde_json::json!({"lark": {"app_id": "x", "app_secret": "y"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant can't set another tenant's IM");

    // 3. The admin CAN set a tenant's bot (and use_feishu=false is honored).
    let r = c
        .put(format!("http://{addr}/api/v1/users/{}/im", bob.id))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .json(&serde_json::json!({"lark": {"app_id": "cli_bob", "app_secret": "s", "use_feishu": false}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "admin sets bob's IM bot");
    let b = TenantRegistry::load(&tenants_path);
    let b = b.by_id(&bob.id).unwrap().clone();
    assert_eq!(b.lark.as_ref().unwrap().app_id, "cli_bob");
    assert!(
        !b.lark.as_ref().unwrap().use_feishu,
        "use_feishu=false honored"
    );

    // 4. The owner has no per-user bot — /me/im is 400 (uses /config/im).
    let r = c
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "owner uses global /config/im, not /me/im");

    // 5. Replace semantics: alice re-PUTs an empty body → her Lark is cleared.
    let r = c
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", format!("Bearer ccteam:{alice_tok}"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cleared = TenantRegistry::load(&tenants_path);
    assert!(
        cleared.by_id(&alice.id).unwrap().lark.is_none(),
        "an empty PUT clears (replace semantics)",
    );
}
