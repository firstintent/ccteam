//! v0.8.18 档1 — per-user web isolation, end to end. A tenant token is gated
//! off user management + global IM credentials, while shared operational
//! surfaces stay available and project resources remain ownership-scoped.

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
async fn tenant_token_keeps_only_user_management_and_global_im_admin_only() {
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

    // The admin's global IM credentials stay admin-only.
    let r = c
        .get(format!("http://{addr}/api/v1/config/im"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant must be 403 on global IM config");

    // Shared operational/library surfaces are available to every identity.
    for path in ["/api/v1/hosts", "/api/v1/status", "/api/v1/skills"] {
        let r = c
            .get(format!("http://{addr}{path}"))
            .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "tenant reaches shared surface {path}");
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

/// Regression: `can_see_project` must NOT wave the admin past the ACL for a
/// slug that was NEVER registered (a "ghost"). The orphan-deregister feature
/// loosened the admin branch to allow ANY state.json load failure, so
/// `/projects/<ghost>/sessions` reached the gateway (200-`[]` on GET, 500 on
/// POST) instead of 404. Now the admin is allowed only for a genuine ORPHAN
/// (registered in config.yaml, state.json gone) so it can still be deregistered.
#[tokio::test]
async fn admin_404s_on_ghost_slug_but_reaches_registered_orphan() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    // An ORPHAN: registered in config.yaml, but with NO .ccteam/state.json.
    ccteam_core::config::upsert_project(
        &paths.root,
        ccteam_core::config::ProjectEntry {
            slug: "orphanp".into(),
            path: tmp.path().join("orphanp"),
            host: ccteam_core::LOCAL_HOST.to_string(),
            remote_slug: None,
            remote_path: None,
            team: "dev".into(),
            installed_at: chrono::Utc::now(),
        },
    )
    .unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let admin = format!("Bearer ccteam:{ADMIN_HEX}");

    // Ghost (never registered) → 404 for the admin, NOT a gateway hit.
    let r = c
        .get(format!("http://{addr}/api/v1/projects/ghostxyz/sessions"))
        .header("Authorization", &admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        404,
        "admin must 404 on a never-registered ghost slug, not reach the gateway"
    );

    // Orphan (registered, state.json gone) → admin still gets past the ACL so a
    // broken registration remains cleanable (not 404).
    let r = c
        .get(format!("http://{addr}/api/v1/projects/orphanp/sessions"))
        .header("Authorization", &admin)
        .send()
        .await
        .unwrap();
    assert_ne!(
        r.status(),
        404,
        "admin must still reach a registered orphan to deregister it"
    );
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
    let probe_path = paths.im_state_dir().join("lark-open-id-probes.jsonl");

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // 1. Self-serve: alice sets her OWN Lark app via /me/im → lands on alice.
    let r = c
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", format!("Bearer ccteam:{alice_tok}"))
        .json(&serde_json::json!({"lark": {
            "app_id": "cli_a",
            "app_secret": "sek",
            "allowed_user_ids": ["ou_alice"]
        }}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "alice configures her own IM bot");
    let a = TenantRegistry::load(&tenants_path);
    let a = a.by_id(&alice.id).unwrap().clone();
    assert_eq!(a.lark.as_ref().unwrap().app_id, "cli_a");
    assert_eq!(
        a.lark.as_ref().unwrap().allowed_user_ids,
        vec!["ou_alice"],
        "self-serve Lark allowlist must persist",
    );
    assert!(a.telegram.is_none(), "no telegram in body → none");

    // 1b. Setup-helper capture is tenant-scoped: alice only sees probes from
    // her own `lark@<tenant>` bot, then can save that open_id without
    // re-submitting app_secret.
    std::fs::create_dir_all(probe_path.parent().unwrap()).unwrap();
    std::fs::write(
        &probe_path,
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "channel": format!("lark@{}", alice.id),
                "open_id": "ou_captured_alice",
                "chat_id": "oc_alice_room",
                "message_id": "om_alice",
                "timestamp": 2000_u64
            }),
            serde_json::json!({
                "channel": format!("lark@{}", bob.id),
                "open_id": "ou_bob_private",
                "chat_id": "oc_bob_room",
                "message_id": "om_bob",
                "timestamp": 2001_u64
            }),
        ),
    )
    .unwrap();
    let candidates: serde_json::Value = c
        .get(format!(
            "http://{addr}/api/v1/me/im/lark/open-id-candidates?since=1500"
        ))
        .header("Authorization", format!("Bearer ccteam:{alice_tok}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        candidates["candidates"][0]["open_id"],
        serde_json::json!("ou_captured_alice"),
    );
    assert_eq!(
        candidates["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["open_id"] == "ou_bob_private")
            .count(),
        0,
        "alice must not see bob's rejected open_id probes",
    );
    let r = c
        .put(format!("http://{addr}/api/v1/me/im/lark/allowed-users"))
        .header("Authorization", format!("Bearer ccteam:{alice_tok}"))
        .json(
            &serde_json::json!({"allowed_user_ids": [" ou_captured_alice ", "ou_captured_alice"]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "alice saves captured allowlist");
    let a = TenantRegistry::load(&tenants_path);
    assert_eq!(
        a.by_id(&alice.id)
            .unwrap()
            .lark
            .as_ref()
            .unwrap()
            .allowed_user_ids,
        vec!["ou_captured_alice"],
        "allowlist-only update trims + dedups without needing app_secret",
    );

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
        .json(&serde_json::json!({"lark": {
            "app_id": "cli_bob",
            "app_secret": "s",
            "allowed_user_ids": ["ou_bob"],
            "use_feishu": false
        }}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "admin sets bob's IM bot");
    let b = TenantRegistry::load(&tenants_path);
    let b = b.by_id(&bob.id).unwrap().clone();
    assert_eq!(b.lark.as_ref().unwrap().app_id, "cli_bob");
    assert_eq!(
        b.lark.as_ref().unwrap().allowed_user_ids,
        vec!["ou_bob"],
        "admin-set tenant Lark allowlist must persist",
    );
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

/// v0.9.11 CROSS-USER — the project-ownership choke point must cover EVERY
/// project-addressed route family, not just `/api/v1/projects/{slug}/…`.
///
/// The regression: the legacy action routes (`/api/{slug}/pause|resume|btw|
/// inject_decision`), the pane snapshots (`/api/{slug}[/{sid}]/pane-snapshot
/// .ansi`) and the live terminal sockets (`/ws/{slug}[/{sid}]/pty`) named a
/// project but sat OUTSIDE `project_acl_layer` and extracted no `Identity` of
/// their own — so any authenticated tenant could snapshot, attach a terminal
/// to, or pause another user's project. A new route in those families is now
/// covered automatically, which is the whole point of a choke point.
#[tokio::test]
async fn project_acl_covers_legacy_action_pane_and_pty_routes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    // An admin-owned project the tenant must not reach by ANY door.
    let state_path = paths.project_state("adminproj");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut st = ccteam_core::ProjectState::initial_for_team("adminproj".into(), "dev".into());
    st.owner = Some("user:web-api".into());
    st.save(&state_path).unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    for path in [
        "/api/adminproj/pane-snapshot.ansi",
        "/api/adminproj/s1/pane-snapshot.ansi",
        "/ws/adminproj/pty",
        "/ws/adminproj/s1/pty",
    ] {
        let r = c
            .get(format!("http://{addr}{path}"))
            .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "tenant must be gated off {path}");
    }
    for path in [
        "/api/adminproj/pause",
        "/api/adminproj/resume",
        "/api/adminproj/btw",
        "/api/adminproj/inject_decision",
        "/api/adminproj/s1/pause",
    ] {
        let r = c
            .post(format!("http://{addr}{path}"))
            .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
            .json(&serde_json::json!({"text": "x", "decision": "x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "tenant must be gated off {path}");
    }

    // The owner still reaches its own project's doors (the gate ran and
    // PASSED — these fail later, on the missing tmux/rmux pane, not at the ACL).
    let r = c
        .get(format!("http://{addr}/api/adminproj/pane-snapshot.ansi"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_ne!(
        r.status(),
        404,
        "the owner must still reach its own project's pane snapshot"
    );
}
