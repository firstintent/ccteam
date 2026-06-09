//! v0.8.8 F4 — `/api/v1/config/im/*` integration tests.
//!
//! Cover the masked read (no plaintext secret in the body), the
//! validate-before-persist PUTs (token/secret checked against a mock base
//! before landing on disk), the async telegram `chat_id` capture, and the
//! web-token gate.
//!
//! ## Mock + isolation discipline
//!
//! - **Creds path:** every test points `AppState::with_creds_path` at a
//!   tempdir file so the real `~/.ccteam/im/credentials.json` is never read
//!   or written (CLAUDE.md test-isolation rule).
//! - **Telegram/Lark HTTP:** an in-test axum mock stands in for the Bot /
//!   Feishu API, injected via the `CCTEAM_TELEGRAM_API_BASE` /
//!   `CCTEAM_LARK_API_BASE` env overrides. Those are process-global, so the
//!   env-mutating PUT tests are `#[serial]`. The async `chat_id` capture
//!   test uses the `spawn_chat_id_poll_for_test` seam (explicit base, no
//!   env) so it needs no serialization.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{routing::get, routing::post, Json, Router};
use ccteam_core::CcteamPaths;
use ccteam_im::credentials::{self, Credentials, LarkCreds, TelegramCreds};
use ccteam_web::{router_with_state, AppState, AuthState};
use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

/// Spin the full ccteam-web router with the given state on a loopback port.
async fn spawn_app(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

/// Build an `AppState` whose creds live under `tmp` (never the real home).
fn state_with_creds(tmp: &TempDir, auth: AuthState) -> (AppState, std::path::PathBuf) {
    let paths = fake_paths(tmp.path());
    let creds_path = tmp.path().join("creds.json");
    let state = AppState::with_auth(paths, auth).with_creds_path(creds_path.clone());
    (state, creds_path)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

// --------------------------------------------------------------------------
// Telegram mock (getMe + getUpdates)
// --------------------------------------------------------------------------

/// Spawn an axum mock for the Telegram Bot API on a loopback port. `getMe`
/// returns the given `ok` + username; `getUpdates` returns a single message
/// from `chat_id`. Returns the base URL (`http://127.0.0.1:<port>`), which
/// the handler reaches via the `{base}/bot{token}/<method>` shape.
///
/// The real Telegram path is `/bot<token>/getMe` with NO slash between
/// `bot` and the token (the token itself carries a `:`), so a single
/// fallback handler dispatches on the path suffix rather than a typed route
/// param (which can't cleanly capture `bot111:TOK`).
async fn spawn_telegram_mock(get_me_ok: bool, username: &str, chat_id: i64) -> String {
    use axum::http::Uri;
    let username = username.to_string();
    let handler = move |uri: Uri| {
        let username = username.clone();
        async move {
            let path = uri.path();
            if path.ends_with("/getMe") {
                Json(serde_json::json!({
                    "ok": get_me_ok,
                    "result": if get_me_ok {
                        serde_json::json!({"username": username})
                    } else {
                        Value::Null
                    },
                }))
            } else if path.ends_with("/getUpdates") {
                Json(serde_json::json!({
                    "ok": true,
                    "result": [
                        {"update_id": 1, "message": {"chat": {"id": chat_id}}}
                    ],
                }))
            } else {
                Json(serde_json::json!({"ok": false}))
            }
        }
    };
    let app = Router::new().fallback(get(handler));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    format!("http://{addr}")
}

/// Spawn an axum mock for the Feishu/Lark `tenant_access_token` endpoint.
async fn spawn_lark_mock(code: i64) -> String {
    let app = Router::new().route(
        "/auth/v3/tenant_access_token/internal",
        post(move || async move {
            Json(serde_json::json!({
                "code": code,
                "msg": if code == 0 { "ok" } else { "invalid app_secret" },
                "tenant_access_token": if code == 0 { "t-tok" } else { "" },
                "expire": 7200,
            }))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    format!("http://{addr}")
}

// --------------------------------------------------------------------------
// GET /config/im — masked read
// --------------------------------------------------------------------------

#[tokio::test]
async fn get_im_config_masks_secrets() {
    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    // Seed creds with BOTH a telegram token and a lark secret on disk.
    let creds = Credentials {
        telegram: Some(TelegramCreds {
            bot_token: "111222:SUPERSECRETTOKENvalue".into(),
            allowed_chat_ids: vec!["98765".into()],
        }),
        lark: Some(LarkCreds {
            app_id: "cli_app_xyz".into(),
            app_secret: "larkAPPSECRETvalue".into(),
            allowed_user_ids: vec!["ou_a".into(), "ou_b".into()],
            use_feishu: true,
        }),
        ..Default::default()
    };
    credentials::save(&creds_path, &creds).unwrap();

    let addr = spawn_app(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/config/im"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();

    // HARD: the raw body must NOT contain either full secret.
    assert!(
        !raw.contains("SUPERSECRETTOKENvalue"),
        "bot_token leaked into GET body: {raw}"
    );
    assert!(
        !raw.contains("larkAPPSECRETvalue"),
        "app_secret leaked into GET body: {raw}"
    );
    // And no `bot_token` / `app_secret` key at all.
    let v: Value = serde_json::from_str(&raw).unwrap();
    let tg = v.get("telegram").unwrap();
    assert!(tg.get("bot_token").is_none(), "no bot_token key");
    assert_eq!(tg.get("configured").unwrap(), true);
    assert_eq!(tg.get("chat_id_count").unwrap(), 1);
    assert!(tg
        .get("bot_token_last4")
        .unwrap()
        .as_str()
        .unwrap()
        .ends_with("alue")); // last-4 of "...value"
    let lk = v.get("lark").unwrap();
    assert!(lk.get("app_secret").is_none(), "no app_secret key");
    assert_eq!(lk.get("use_feishu").unwrap(), true);
    assert_eq!(lk.get("allowed_user_id_count").unwrap(), 2);
    // transport (no-TLS) warning present.
    assert!(v.get("transport_warning").unwrap().as_str().unwrap().len() > 10);
}

#[tokio::test]
async fn get_im_config_empty_when_no_creds() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/config/im"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert!(v.get("telegram").unwrap().is_null());
    assert!(v.get("lark").unwrap().is_null());
}

// --------------------------------------------------------------------------
// PUT /config/im/telegram — validate + persist (env-injected mock base)
// --------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn put_telegram_valid_token_persists() {
    let base = spawn_telegram_mock(true, "myccteambot", 555).await;
    std::env::set_var("CCTEAM_TELEGRAM_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let client = client();
    let resp = client
        .put(format!("http://{addr}/api/v1/config/im/telegram"))
        .json(&serde_json::json!({"bot_token": "111:GOODTOKEN"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v.get("ok").unwrap(), true);
    assert_eq!(v.get("restart_required").unwrap(), true);
    assert_eq!(v.get("bot_username").unwrap(), "@myccteambot");
    assert!(v.get("note").is_some(), "restart note present");

    // Persisted on disk with the token.
    let saved = credentials::load(Some(&creds_path)).unwrap();
    let tg = saved.telegram.expect("telegram block persisted");
    assert_eq!(tg.bot_token, "111:GOODTOKEN");

    std::env::remove_var("CCTEAM_TELEGRAM_API_BASE");
}

#[tokio::test]
#[serial]
async fn put_telegram_preserves_existing_chat_ids() {
    let base = spawn_telegram_mock(true, "bot2", 1).await;
    std::env::set_var("CCTEAM_TELEGRAM_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    // Seed an existing token + chat id; a re-PUT of the token must keep the
    // chat id allowlist.
    credentials::save(
        &creds_path,
        &Credentials {
            telegram: Some(TelegramCreds {
                bot_token: "old".into(),
                allowed_chat_ids: vec!["42".into()],
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let addr = spawn_app(state).await;

    let client = client();
    let resp = client
        .put(format!("http://{addr}/api/v1/config/im/telegram"))
        .json(&serde_json::json!({"bot_token": "new:TOKEN"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let saved = credentials::load(Some(&creds_path)).unwrap();
    let tg = saved.telegram.unwrap();
    assert_eq!(tg.bot_token, "new:TOKEN");
    assert_eq!(tg.allowed_chat_ids, vec!["42".to_string()]);

    std::env::remove_var("CCTEAM_TELEGRAM_API_BASE");
}

#[tokio::test]
#[serial]
async fn put_telegram_bad_token_is_400_no_persist() {
    // getMe returns ok:false → handler must 400 and NOT write the file.
    let base = spawn_telegram_mock(false, "", 0).await;
    std::env::set_var("CCTEAM_TELEGRAM_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let client = client();
    let resp = client
        .put(format!("http://{addr}/api/v1/config/im/telegram"))
        .json(&serde_json::json!({"bot_token": "111:BADTOKEN"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v
        .get("error")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("rejected"));
    // No file written.
    assert!(
        !creds_path.exists(),
        "bad token must not persist credentials"
    );

    std::env::remove_var("CCTEAM_TELEGRAM_API_BASE");
}

#[tokio::test]
async fn put_telegram_empty_token_is_400() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let client = client();
    let resp = client
        .put(format!("http://{addr}/api/v1/config/im/telegram"))
        .json(&serde_json::json!({"bot_token": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --------------------------------------------------------------------------
// PUT /config/im/lark — validate + persist
// --------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn put_lark_valid_creds_persists() {
    let base = spawn_lark_mock(0).await;
    std::env::set_var("CCTEAM_LARK_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let client = client();
    let resp = client
        .put(format!("http://{addr}/api/v1/config/im/lark"))
        .json(&serde_json::json!({
            "app_id": "cli_good",
            "app_secret": "secretGOOD",
            "allowed_user_ids": ["ou_x", "ou_y"],
            "use_feishu": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v.get("ok").unwrap(), true);
    assert_eq!(v.get("restart_required").unwrap(), true);

    let saved = credentials::load(Some(&creds_path)).unwrap();
    let lk = saved.lark.expect("lark block persisted");
    assert_eq!(lk.app_id, "cli_good");
    assert_eq!(lk.app_secret, "secretGOOD");
    assert_eq!(lk.allowed_user_ids, vec!["ou_x", "ou_y"]);
    assert!(lk.use_feishu);

    std::env::remove_var("CCTEAM_LARK_API_BASE");
}

#[tokio::test]
#[serial]
async fn put_lark_bad_creds_is_400_no_persist() {
    let base = spawn_lark_mock(10003).await;
    std::env::set_var("CCTEAM_LARK_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let client = client();
    let resp = client
        .put(format!("http://{addr}/api/v1/config/im/lark"))
        .json(&serde_json::json!({
            "app_id": "cli_bad",
            "app_secret": "wrong",
            "use_feishu": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert!(!creds_path.exists(), "bad creds must not persist");

    std::env::remove_var("CCTEAM_LARK_API_BASE");
}

// --------------------------------------------------------------------------
// Async telegram chat_id capture (no env — explicit-base test seam)
// --------------------------------------------------------------------------

#[tokio::test]
async fn chat_id_capture_writes_into_allowlist() {
    // A token is already on disk (precondition for capture); the mock
    // getUpdates returns chat_id 777.
    let base = spawn_telegram_mock(true, "bot", 777).await;
    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    credentials::save(
        &creds_path,
        &Credentials {
            telegram: Some(TelegramCreds {
                bot_token: "111:TOK".into(),
                allowed_chat_ids: vec![],
            }),
            ..Default::default()
        },
    )
    .unwrap();

    // Drive the background poll directly via the test seam (explicit base).
    ccteam_web::routes::im_config::spawn_chat_id_poll_for_test(
        state.im_poll.clone(),
        "111:TOK".into(),
        base,
    );

    let addr = spawn_app(state).await;

    // Poll the GET endpoint until it reports `captured` (the background task
    // resolves quickly against the mock).
    let client = client();
    let mut captured = None;
    for _ in 0..50 {
        let v: Value = client
            .get(format!("http://{addr}/api/v1/config/im/telegram/chat-id"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if v.get("status").unwrap() == "captured" {
            captured = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let v = captured.expect("chat_id capture should reach `captured`");
    // last-4 of "777" is the short-mask all-stars form (≤4 chars).
    assert!(v.get("chat_id_last4").is_some());

    // Persisted into the allowlist.
    let saved = credentials::load(Some(&creds_path)).unwrap();
    let tg = saved.telegram.unwrap();
    assert_eq!(tg.allowed_chat_ids, vec!["777".to_string()]);
}

#[tokio::test]
async fn chat_id_poll_idle_when_not_started() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let v: Value = client()
        .get(format!("http://{addr}/api/v1/config/im/telegram/chat-id"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v.get("status").unwrap(), "idle");
}

#[tokio::test]
async fn chat_id_start_without_token_is_400() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let client = client();
    let resp = client
        .post(format!(
            "http://{addr}/api/v1/config/im/telegram/chat-id/start"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --------------------------------------------------------------------------
// Web-token gate
// --------------------------------------------------------------------------

#[tokio::test]
async fn config_im_requires_web_token() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn_app(state).await;

    // No Authorization header → 401 on the GET.
    let resp = client()
        .get(format!("http://{addr}/api/v1/config/im"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "config/im must sit behind the web-token gate"
    );

    // With the bearer token → 200.
    let client = client();
    let ok = client
        .get(format!("http://{addr}/api/v1/config/im"))
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
}
