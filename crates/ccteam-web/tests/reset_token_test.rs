//! v0.8.24 — self-serve web-token rotation (`POST /api/v1/me/reset-token`).
//!
//! Proves: admin + tenant both rotate only their own token; the OLD token is
//! dead immediately and the NEW one works without a restart; auth-disabled →
//! 400.

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
async fn admin_reset_rotates_live_and_persists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.secrets_dir()).unwrap();
    std::fs::write(paths.web_token_path(), ADMIN_HEX).unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let old_auth = format!("Bearer ccteam:{ADMIN_HEX}");

    // Rotate.
    let body: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/me/reset-token"))
        .header("Authorization", &old_auth)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let wire = body["wire_token"].as_str().unwrap().to_string();
    let new_hex = wire.strip_prefix("ccteam:").unwrap().to_string();
    assert_ne!(new_hex, ADMIN_HEX);
    assert_eq!(new_hex.len(), 64, "32-byte hex secret");

    // Persisted to ~/.ccteam/secrets/web-token.
    let on_disk = std::fs::read_to_string(paths.web_token_path()).unwrap();
    assert_eq!(on_disk.trim(), new_hex);

    // Old token is dead IMMEDIATELY; new token works — no restart.
    let r = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", &old_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "old token must die on rotation");
    let me: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{new_hex}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["is_admin"], true);
}

#[tokio::test]
async fn tenant_reset_rotates_only_the_caller_and_disabled_auth_is_400() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();

    let tenant_id = tenant.id.clone();
    let old_tenant_hex = tenant.web_token.clone();
    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let body: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/me/reset-token"))
        .header("Authorization", format!("Bearer ccteam:{old_tenant_hex}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let wire = body["wire_token"].as_str().unwrap();
    let new_tenant_hex = wire.strip_prefix("ccteam:").unwrap();
    assert_ne!(new_tenant_hex, old_tenant_hex);

    let r = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{old_tenant_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "old tenant token dies immediately");

    let me: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{new_tenant_hex}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["id"], tenant_id);
    assert_eq!(me["is_admin"], false);

    let admin: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/me"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admin["is_admin"], true, "admin token is unaffected");

    // Auth disabled → 400 (no token in use).
    let tmp2 = tempfile::TempDir::new().unwrap();
    let addr2 = spawn(AppState::new(fake_paths(tmp2.path()))).await;
    let r = c
        .post(format!("http://{addr2}/api/v1/me/reset-token"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}
