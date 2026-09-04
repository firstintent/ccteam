//! `GET /api/v1/usage` — the script-side read of per-harness ACCOUNT quota.
//!
//! The contract these lock:
//! - it lives inside the ordinary web-token gate (401 without one), not the
//!   admin gate: it is machine state, readable by any logged-in identity;
//! - a harness with an unexpired observation appears, one with none does NOT
//!   (no `unknown` row a reader could mistake for headroom), and a window whose
//!   own reset has passed is gone rather than presented as current;
//! - `?vendor=` narrows it, and an unknown token narrows it to nothing;
//! - the shape is the same one the MCP `status{detail:"usage"}` body publishes
//!   ([`ccteam_im::usage_view`]), because both render through it.
//!
//! No gateway is attached, which is deliberate: it proves the CACHED half
//! answers on its own — the numbers are the daemon's recorded observations,
//! not something a live session has to be around to supply.

use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_harness::{AccountUsage, ModelWindow};
use ccteam_web::{router_with_state, AppState, AuthState};
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

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

/// Seed the daemon's recorded observations: claude with every window kind
/// (including a per-model one), codex with one window whose reset has already
/// passed, grok to prove the map is harness-generic.
fn seed_observations(root: &std::path::Path) {
    let now = chrono::Utc::now();
    let iso = |hours: i64| (now + chrono::Duration::hours(hours)).to_rfc3339();
    ccteam_harness::usage_catalog::record_vendor_usage_in(
        root,
        "claude",
        "status card",
        &AccountUsage {
            subscription: Some("max".into()),
            five_hour_pct: Some(8),
            five_hour_resets_at: Some(iso(2)),
            weekly_pct: Some(23),
            weekly_resets_at: Some(iso(48)),
            weekly_severity: Some("warning".into()),
            credits_pct: Some(3),
            model_windows: vec![ModelWindow {
                model: "Fable".into(),
                pct: Some(16),
                resets_at: Some(iso(48)),
            }],
        },
    )
    .unwrap();
    ccteam_harness::usage_catalog::record_vendor_usage_in(
        root,
        "codex",
        "session release",
        &AccountUsage {
            five_hour_pct: Some(90),
            five_hour_resets_at: Some(iso(-1)),
            weekly_pct: Some(12),
            weekly_resets_at: Some(iso(72)),
            ..Default::default()
        },
    )
    .unwrap();
    ccteam_harness::usage_catalog::record_vendor_usage_in(
        root,
        "grok",
        "status card",
        &AccountUsage {
            subscription: Some("SuperGrok Heavy".into()),
            weekly_pct: Some(42),
            weekly_resets_at: Some(iso(24)),
            ..Default::default()
        },
    )
    .unwrap();
}

#[tokio::test]
async fn usage_is_inside_the_web_token_gate() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;

    let denied = client()
        .get(format!("http://{addr}/api/v1/usage"))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401, "no token ⇒ 401");

    // Any authenticated identity reads it — no admin gate (contrast
    // `/api/v1/vendors/quota`, which probes vendor APIs with credentials).
    let allowed = client()
        .get(format!("http://{addr}/api/v1/usage"))
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);
    assert!(allowed.json::<Value>().await.unwrap()["usage"].is_object());
}

#[tokio::test]
async fn usage_reports_every_observed_harness_and_drops_expired_windows() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_observations(&paths.root);
    let addr = spawn(AppState::new(paths)).await;

    let body: Value = client()
        .get(format!("http://{addr}/api/v1/usage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let usage = body["usage"].as_object().expect("a usage map");
    assert_eq!(
        usage.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["claude", "codex", "grok"],
        "observed harnesses only, and all of them: {body}"
    );

    assert_eq!(usage["claude"]["subscription"], "max");
    assert_eq!(usage["claude"]["source"], "status card");
    assert!(usage["claude"]["observed"].is_string());
    let windows = usage["claude"]["windows"].as_array().unwrap();
    assert_eq!(windows[0]["w"], "5h");
    assert_eq!(windows[0]["pct"], 8);
    assert_eq!(windows[1]["w"], "7d");
    assert_eq!(windows[1]["severity"], "warning");
    // A per-model window is a weekly row PLUS the harness's own model name.
    assert_eq!(windows[2]["w"], "7d");
    assert_eq!(windows[2]["model"], "Fable");
    assert_eq!(windows[2]["pct"], 16);
    assert_eq!(windows[3]["w"], "credits");

    // The codex 5-hour window's own reset has passed: absent, never stale.
    let codex = usage["codex"]["windows"].as_array().unwrap();
    assert_eq!(codex.len(), 1, "{body}");
    assert_eq!(codex[0]["w"], "7d");

    // Nothing observed for kimi ⇒ no row at all (not a zeroed one).
    assert!(usage.get("kimi").is_none(), "{body}");
}

#[tokio::test]
async fn the_vendor_filter_narrows_to_one_harness_and_a_typo_to_none() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_observations(&paths.root);
    let addr = spawn(AppState::new(paths)).await;

    let one: Value = client()
        .get(format!("http://{addr}/api/v1/usage?vendor=claude"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        one["usage"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["claude"]
    );

    // A harness with no observation filters to an empty map, same as a typo:
    // both mean "nothing to report", and neither widens the answer.
    for filter in ["kimi", "not-a-harness"] {
        let empty: Value = client()
            .get(format!("http://{addr}/api/v1/usage?vendor={filter}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            empty["usage"].as_object().unwrap().len(),
            0,
            "`{filter}` must not widen the answer: {empty}"
        );
    }
}

/// An install nobody has run yet answers with an empty map, not an error —
/// a caller can branch on "no harness observed" without special-casing a 500.
#[tokio::test]
async fn an_empty_install_is_an_empty_map() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn(AppState::new(fake_paths(tmp.path()))).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/usage"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["usage"].as_object().unwrap().len(), 0);
}
