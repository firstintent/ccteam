//! VENDOR-QUOTA-1 — `GET /api/v1/vendors/quota` endpoint tests.
//!
//! Proves:
//! 1. a non-admin tenant is 403 (the probe reads the daemon user's vendor
//!    credential files — owner-scoped)
//! 2. with all credential homes pointed at an EMPTY tempdir, every probe
//!    resolves locally without any network call: claude/codex/kimi report
//!    `not_subscription` (no credential file), grok `unavailable` (stubbed
//!    by construction), and opencode/pi/dsh are absent from the list
//! 3. a repeat GET rides the per-vendor cache (same rows, still 200)
//!
//! The env mutation (HOME + the three vendor home overrides) is why this
//! lives in an integration test binary (own process) and serializes on
//! ENV_LOCK — the daemon process on a real machine would otherwise read the
//! host's true `~/.claude/.credentials.json` and make a LIVE network call,
//! which tests must never do.

use std::net::SocketAddr;

use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

/// Serializes the env-mutating tests (a tokio Mutex: its guard is meant to
/// be held across `.await`).
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
async fn tenant_is_403_on_vendor_quota() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(paths.users_dir()).unwrap();
    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tauth = format!("Bearer ccteam:{}", tenant.web_token);

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let r = client()
        .get(format!("http://{addr}/api/v1/vendors/quota"))
        .header("Authorization", &tauth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant must not read vendor quotas");
}

#[tokio::test]
async fn empty_credential_homes_yield_local_only_rows_and_cache_repeats() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::TempDir::new().unwrap();
    // Point every credential home at an empty sandbox: no credential files →
    // no HTTP request is ever attempted (NotSubscription), and grok is
    // unavailable by construction. This is what keeps the test offline.
    let sandbox = tmp.path().join("homes");
    std::fs::create_dir_all(&sandbox).unwrap();
    std::env::set_var("HOME", &sandbox);
    std::env::set_var("CLAUDE_CONFIG_HOME", sandbox.join(".claude"));
    std::env::set_var("CODEX_HOME", sandbox.join(".codex"));
    std::env::set_var("KIMI_CODE_HOME", sandbox.join(".kimi-code"));

    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");

    let mut rows: Option<Vec<serde_json::Value>> = None;
    for attempt in 0..2 {
        let r = c
            .get(format!("http://{addr}/api/v1/vendors/quota"))
            .header("Authorization", &auth)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: serde_json::Value = r.json().await.unwrap();
        let quotas = body["quotas"].as_array().unwrap().clone();
        if attempt == 0 {
            rows = Some(quotas);
        } else {
            // Second GET: the per-vendor cache serves byte-identical rows.
            assert_eq!(rows.as_ref().unwrap(), &quotas, "cached repeat must match");
        }
    }
    let quotas = rows.unwrap();

    let by_vendor: std::collections::BTreeMap<String, String> = quotas
        .iter()
        .map(|q| {
            (
                q["vendor"].as_str().unwrap().to_string(),
                q["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        by_vendor.get("claude").map(String::as_str),
        Some("not_subscription")
    );
    assert_eq!(
        by_vendor.get("codex").map(String::as_str),
        Some("not_subscription")
    );
    assert_eq!(
        by_vendor.get("kimi").map(String::as_str),
        Some("not_subscription")
    );
    assert_eq!(
        by_vendor.get("grok").map(String::as_str),
        Some("unavailable")
    );
    // No probe surface: absent from the list entirely (UI renders nothing).
    for vendor in ["opencode", "pi", "dsh"] {
        assert!(!by_vendor.contains_key(vendor), "{vendor} must be absent");
    }
    // Exactly the four probe-bearing vendors, registry-ordered.
    let order: Vec<&str> = quotas
        .iter()
        .map(|q| q["vendor"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec!["claude", "codex", "grok", "kimi"]);
}
