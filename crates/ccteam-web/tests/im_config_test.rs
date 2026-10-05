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
//! - **Telegram/Lark/Slack HTTP:** an in-test axum mock stands in for the
//!   Bot / Feishu / Slack Web API, injected via the
//!   `CCTEAM_TELEGRAM_API_BASE` / `CCTEAM_LARK_API_BASE` /
//!   `CCTEAM_SLACK_API_BASE` env overrides. Those are process-global, so the
//!   env-mutating PUT tests are `#[serial]`. The async `chat_id` capture
//!   test uses the `spawn_chat_id_poll_for_test` seam (explicit base, no
//!   env) so it needs no serialization.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{routing::get, routing::post, Json, Router};
use ccteam_core::CcteamPaths;
use ccteam_im::credentials::{self, Credentials, LarkCreds, SlackCreds, TelegramCreds};
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

/// Spawn an axum mock for the two Slack calls setup makes: `auth.test` (bot
/// token) and `apps.connections.open` (app-level token). `bot_ok` / `app_ok`
/// pick success vs `invalid_auth` for each.
async fn spawn_slack_mock(bot_ok: bool, app_ok: bool) -> String {
    let app = Router::new()
        .route(
            "/auth.test",
            post(move || async move {
                Json(if bot_ok {
                    serde_json::json!({"ok": true, "team": "Acme", "user": "ccteam", "user_id": "UBOT"})
                } else {
                    serde_json::json!({"ok": false, "error": "invalid_auth"})
                })
            }),
        )
        .route(
            "/apps.connections.open",
            post(move || async move {
                Json(if app_ok {
                    serde_json::json!({"ok": true, "url": "wss://wss-primary.slack.com/link/?t=1"})
                } else {
                    serde_json::json!({"ok": false, "error": "invalid_auth"})
                })
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
            require_mention: false,
        }),
        lark: Some(LarkCreds {
            app_id: "cli_app_xyz".into(),
            app_secret: "larkAPPSECRETvalue".into(),
            allowed_user_ids: vec!["ou_a".into(), "ou_b".into()],
            use_feishu: true,
            require_mention: false,
        }),
        slack: Some(SlackCreds {
            bot_token: "xoxb-SLACKBOTSECRETvalue".into(),
            app_token: "xapp-SLACKAPPSECRETtail".into(),
            allowed_user_ids: vec!["U0ALICE".into()],
            require_mention: false,
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
    assert!(
        !raw.contains("SLACKBOTSECRET") && !raw.contains("SLACKAPPSECRET"),
        "slack tokens leaked into GET body: {raw}"
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
    let sl = v.get("slack").unwrap();
    assert!(sl.get("bot_token").is_none() && sl.get("app_token").is_none());
    assert_eq!(sl.get("configured").unwrap(), true);
    assert_eq!(sl.get("bot_token_last4").unwrap(), "…alue");
    assert_eq!(sl.get("app_token_last4").unwrap(), "…tail");
    assert_eq!(
        sl.get("allowed_user_ids").unwrap(),
        &serde_json::json!(["U0ALICE"])
    );
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
    assert!(v.get("slack").unwrap().is_null());
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
                require_mention: false,
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
// PUT /config/im/slack — validate both tokens + persist
// --------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn put_slack_valid_tokens_persist_and_preserve_other_platforms() {
    let base = spawn_slack_mock(true, true).await;
    std::env::set_var("CCTEAM_SLACK_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    credentials::save(
        &creds_path,
        &Credentials {
            telegram: Some(TelegramCreds {
                bot_token: "tg".into(),
                allowed_chat_ids: vec!["42".into()],
                require_mention: false,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let addr = spawn_app(state).await;

    let resp = client()
        .put(format!("http://{addr}/api/v1/config/im/slack"))
        .json(&serde_json::json!({
            "bot_token": " xoxb-good ",
            "app_token": "xapp-good",
            "allowed_user_ids": ["U0ALICE", " ", "U0BOB "],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v.get("ok").unwrap(), true);
    assert_eq!(v.get("restart_required").unwrap(), true);
    assert_eq!(v.get("team").unwrap(), "Acme");
    assert_eq!(v.get("bot_user").unwrap(), "ccteam");

    let saved = credentials::load(Some(&creds_path)).unwrap();
    let sl = saved.slack.expect("slack block persisted");
    assert_eq!(sl.bot_token, "xoxb-good");
    assert_eq!(sl.app_token, "xapp-good");
    assert_eq!(sl.allowed_user_ids, vec!["U0ALICE", "U0BOB"]);
    assert_eq!(
        saved.telegram.unwrap().allowed_chat_ids,
        vec!["42".to_string()],
        "other platforms survive the merge"
    );

    std::env::remove_var("CCTEAM_SLACK_API_BASE");
}

#[tokio::test]
#[serial]
async fn put_slack_rejected_app_token_is_400_no_persist() {
    let base = spawn_slack_mock(true, false).await;
    std::env::set_var("CCTEAM_SLACK_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let resp = client()
        .put(format!("http://{addr}/api/v1/config/im/slack"))
        .json(&serde_json::json!({"bot_token": "xoxb-good", "app_token": "xapp-bad"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    let error = v.get("error").unwrap().as_str().unwrap();
    assert!(
        error.contains("Slack credentials rejected") && error.contains("apps.connections.open"),
        "{error}"
    );
    assert!(!creds_path.exists(), "rejected tokens must not persist");

    std::env::remove_var("CCTEAM_SLACK_API_BASE");
}

#[tokio::test]
async fn put_slack_missing_token_is_400() {
    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let resp = client()
        .put(format!("http://{addr}/api/v1/config/im/slack"))
        .json(&serde_json::json!({"bot_token": "xoxb-1", "app_token": "  "}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert!(!creds_path.exists());
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
                require_mention: false,
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
// Slack guided setup — create link, sender capture, allowlist-only update
// --------------------------------------------------------------------------

#[tokio::test]
async fn slack_manifest_comes_with_a_one_click_create_link() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let v: Value = client()
        .get(format!(
            "http://{addr}/api/v1/config/im/slack/app-manifest?name=My%20Team%20Bot"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let manifest = v.get("manifest").unwrap();
    assert_eq!(manifest["display_information"]["name"], "My Team Bot");
    assert_eq!(
        manifest["features"]["bot_user"]["display_name"],
        "my-team-bot"
    );
    assert_eq!(manifest["settings"]["socket_mode_enabled"], true);
    assert_eq!(
        manifest["features"]["slash_commands"][0]["command"], "/my-team-bot",
        "the slash command follows the app name"
    );

    let create = reqwest::Url::parse(v["create_url"].as_str().unwrap()).unwrap();
    assert_eq!(create.host_str(), Some("api.slack.com"));
    let query: std::collections::HashMap<String, String> =
        create.query_pairs().into_owned().collect();
    assert_eq!(query.get("new_app").map(String::as_str), Some("1"));
    let embedded: Value = serde_json::from_str(&query["manifest_json"]).unwrap();
    assert_eq!(
        &embedded, manifest,
        "the link carries exactly the manifest shown"
    );
}

#[tokio::test]
async fn slack_allowed_users_update_keeps_the_tokens() {
    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let url = format!("http://{addr}/api/v1/config/im/slack/allowed-users");

    // No Slack app yet → nothing to bind to.
    let r = client()
        .put(&url)
        .json(&serde_json::json!({"allowed_user_ids": ["U1"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    credentials::save(
        &creds_path,
        &Credentials {
            slack: Some(SlackCreds {
                bot_token: "xoxb-keep".into(),
                app_token: "xapp-keep".into(),
                allowed_user_ids: vec![],
                require_mention: false,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let r = client()
        .put(&url)
        .json(&serde_json::json!({"allowed_user_ids": [" U0ALICE ", "", "U0BOB"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let saved = credentials::load(Some(&creds_path)).unwrap().slack.unwrap();
    assert_eq!(saved.allowed_user_ids, vec!["U0ALICE", "U0BOB"]);
    assert_eq!(saved.bot_token, "xoxb-keep");
    assert_eq!(saved.app_token, "xapp-keep");
}

#[tokio::test]
async fn slack_user_id_candidates_are_the_global_bots_rejected_senders() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let probe_dir = paths.im_state_dir();
    std::fs::create_dir_all(&probe_dir).unwrap();
    let probe = |channel: &str, sender: &str, ts: u64| {
        serde_json::json!({
            "channel": channel,
            "sender_id": sender,
            "chat_id": "D0DM",
            "message_id": format!("{ts}.000100"),
            "timestamp": ts,
        })
        .to_string()
    };
    std::fs::write(
        probe_dir.join("rejected-senders.jsonl"),
        [
            probe("slack", "U0OLD", 100),
            probe("telegram", "339", 150),
            probe("slack@u1", "U0TENANT", 160),
            probe("slack", "U0NEW", 200),
        ]
        .join("\n"),
    )
    .unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;

    let v: Value = client()
        .get(format!(
            "http://{addr}/api/v1/config/im/slack/user-id-candidates?since=150"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sender_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["U0NEW"],
        "only the global Slack bot, since the cutoff"
    );
}

// --------------------------------------------------------------------------
// A regular user's OWN Slack app — symmetric with their Telegram / Lark bot
// --------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn a_tenant_sets_up_their_own_slack_app() {
    use ccteam_core::tenants::TenantRegistry;
    let base = spawn_slack_mock(true, true).await;
    std::env::set_var("CCTEAM_SLACK_API_BASE", &base);

    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let mut reg = TenantRegistry::default();
    let alice = reg.add("alice");
    let bob = reg.add("bob");
    reg.save(&paths.users_dir()).unwrap();
    let users = paths.users_dir();
    let probe = paths.im_state_dir().join("rejected-senders.jsonl");
    let (state, _) = state_with_creds(&tmp, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn_app(state).await;
    let alice_auth = format!("Bearer ccteam:{}", alice.web_token);

    // ② Tokens, validated against Slack; fail-closed until someone is allowed.
    let v: Value = client()
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({"slack": {"bot_token": "xoxb-a", "app_token": "xapp-a"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["slack"], true, "{v}");
    assert_eq!(v["slack_unbound"], true, "{v}");
    let saved = TenantRegistry::load(&users)
        .by_id(&alice.id)
        .unwrap()
        .clone();
    assert_eq!(saved.slack.as_ref().unwrap().bot_token, "xoxb-a");
    assert!(saved.lark.is_none() && saved.telegram.is_none());

    // ③ Capture is scoped to HER bot (`slack@<alice>`), then one click allows.
    std::fs::create_dir_all(probe.parent().unwrap()).unwrap();
    let row = |channel: String, sender: &str| {
        serde_json::json!({
            "channel": channel, "sender_id": sender, "chat_id": "D1",
            "message_id": "1.1", "timestamp": 2000_u64,
        })
        .to_string()
    };
    std::fs::write(
        &probe,
        [
            row(format!("slack@{}", alice.id), "U0ALICE"),
            row(format!("slack@{}", bob.id), "U0BOB"),
            row("slack".into(), "U0OWNER"),
        ]
        .join("\n"),
    )
    .unwrap();
    let candidates: Value = client()
        .get(format!(
            "http://{addr}/api/v1/me/im/slack/user-id-candidates"
        ))
        .header("Authorization", &alice_auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = candidates["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sender_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["U0ALICE"], "only her own bot's rejected senders");
    let r = client()
        .put(format!("http://{addr}/api/v1/me/im/slack/allowed-users"))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({"allowed_user_ids": ["U0ALICE"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // A later token change keeps the binding (a member id names the person).
    client()
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({"slack": {"bot_token": "xoxb-a2", "app_token": "xapp-a2"}}))
        .send()
        .await
        .unwrap();
    let saved = TenantRegistry::load(&users)
        .by_id(&alice.id)
        .unwrap()
        .clone();
    let slack = saved.slack.unwrap();
    assert_eq!(slack.bot_token, "xoxb-a2");
    assert_eq!(slack.allowed_user_ids, vec!["U0ALICE"]);
    assert!(
        TenantRegistry::load(&users)
            .by_id(&bob.id)
            .unwrap()
            .slack
            .is_none(),
        "bob is untouched"
    );

    std::env::remove_var("CCTEAM_SLACK_API_BASE");
}

// --------------------------------------------------------------------------
// `require_mention` — the group-reply switch, the same on every IM
// --------------------------------------------------------------------------

/// Creds with all three IMs configured and the switch off everywhere.
fn all_three_ims() -> Credentials {
    Credentials {
        telegram: Some(TelegramCreds {
            bot_token: "111:tg-secret".into(),
            allowed_chat_ids: vec!["42".into()],
            require_mention: false,
        }),
        lark: Some(LarkCreds {
            app_id: "cli_x".into(),
            app_secret: "lark-secret".into(),
            allowed_user_ids: vec!["ou_a".into()],
            use_feishu: true,
            require_mention: false,
        }),
        slack: Some(SlackCreds {
            bot_token: "xoxb-keep".into(),
            app_token: "xapp-keep".into(),
            allowed_user_ids: vec!["U0ALICE".into()],
            require_mention: false,
        }),
        ..Default::default()
    }
}

fn switch_flags(c: &Credentials) -> [bool; 3] {
    [
        c.telegram.as_ref().unwrap().require_mention,
        c.lark.as_ref().unwrap().require_mention,
        c.slack.as_ref().unwrap().require_mention,
    ]
}

/// The owner flips each IM's switch with one flag — no tokens re-entered, no
/// other IM touched — and the masked read reports it back, per IM.
#[tokio::test]
async fn require_mention_switch_is_symmetric_across_ims_and_keeps_the_tokens() {
    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    credentials::save(&creds_path, &all_three_ims()).unwrap();
    let addr = spawn_app(state).await;

    for (i, platform) in ["telegram", "lark", "slack"].into_iter().enumerate() {
        let url = format!("http://{addr}/api/v1/config/im/{platform}/require-mention");
        let r = client()
            .put(&url)
            .json(&serde_json::json!({"require_mention": true}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{platform}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["platform"], platform);
        assert_eq!(v["require_mention"], true);
        assert_eq!(
            v["restart_required"], true,
            "standalone web: no live reload"
        );

        let mut want = [false; 3];
        for flag in want.iter_mut().take(i + 1) {
            *flag = true;
        }
        let saved = credentials::load(Some(&creds_path)).unwrap();
        assert_eq!(switch_flags(&saved), want, "after {platform}");
        assert_eq!(saved.slack.as_ref().unwrap().bot_token, "xoxb-keep");
        assert_eq!(saved.lark.as_ref().unwrap().app_secret, "lark-secret");
        assert_eq!(saved.telegram.as_ref().unwrap().bot_token, "111:tg-secret");
    }

    // The masked read carries it for each IM.
    let status: Value = client()
        .get(format!("http://{addr}/api/v1/config/im"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for platform in ["telegram", "lark", "slack"] {
        assert_eq!(status[platform]["require_mention"], true, "{platform}");
    }

    // And back off.
    let r = client()
        .put(format!(
            "http://{addr}/api/v1/config/im/slack/require-mention"
        ))
        .json(&serde_json::json!({"require_mention": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let saved = credentials::load(Some(&creds_path)).unwrap();
    assert_eq!(switch_flags(&saved), [true, true, false]);
}

#[tokio::test]
async fn require_mention_switch_refuses_unknown_and_unconfigured_ims() {
    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let addr = spawn_app(state).await;
    let put = |platform: &str| {
        client()
            .put(format!(
                "http://{addr}/api/v1/config/im/{platform}/require-mention"
            ))
            .json(&serde_json::json!({"require_mention": true}))
            .send()
    };

    // Nothing configured yet: every IM says so, nothing is written.
    for platform in ["telegram", "lark", "slack"] {
        assert_eq!(put(platform).await.unwrap().status(), 400, "{platform}");
    }
    // A typo is a 400, never a silent no-op.
    let r = put("slakc").await.unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("telegram, lark, slack"));
    assert!(
        !creds_path.exists()
            || credentials::load(Some(&creds_path))
                .unwrap()
                .slack
                .is_none(),
        "a refused flip writes nothing"
    );
}

/// The switch is the owner's: a tenant is refused, and the endpoint sits
/// behind the web-token gate like the rest of `/config/im`.
#[tokio::test]
async fn require_mention_switch_is_admin_only_behind_the_gate() {
    use ccteam_core::tenants::TenantRegistry;
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let mut reg = TenantRegistry::default();
    let alice = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::enabled(TOKEN_HEX.into()));
    credentials::save(&creds_path, &all_three_ims()).unwrap();
    let addr = spawn_app(state).await;
    let url = format!("http://{addr}/api/v1/config/im/slack/require-mention");
    let body = serde_json::json!({"require_mention": true});

    let r = client().put(&url).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = client()
        .put(&url)
        .header(
            "Authorization",
            format!("Bearer ccteam:{}", alice.web_token),
        )
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "a tenant cannot flip the owner's bot");
    assert!(
        !credentials::load(Some(&creds_path))
            .unwrap()
            .slack
            .unwrap()
            .require_mention
    );
    let r = client()
        .put(&url)
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// Saving the tokens again (a rotated bot token, a re-run setup) is not a
/// reason to forget how the bot was told to behave in groups.
#[tokio::test]
#[serial]
async fn re_saving_an_ims_credentials_keeps_its_require_mention_switch() {
    let tg_base = spawn_telegram_mock(true, "ccteam_bot", 1).await;
    let lark_base = spawn_lark_mock(0).await;
    let slack_base = spawn_slack_mock(true, true).await;
    std::env::set_var("CCTEAM_TELEGRAM_API_BASE", &tg_base);
    std::env::set_var("CCTEAM_LARK_API_BASE", &lark_base);
    std::env::set_var("CCTEAM_SLACK_API_BASE", &slack_base);

    let tmp = TempDir::new().unwrap();
    let (state, creds_path) = state_with_creds(&tmp, AuthState::disabled());
    let mut creds = all_three_ims();
    creds.telegram.as_mut().unwrap().require_mention = true;
    creds.lark.as_mut().unwrap().require_mention = true;
    creds.slack.as_mut().unwrap().require_mention = true;
    credentials::save(&creds_path, &creds).unwrap();
    let addr = spawn_app(state).await;

    for (platform, body) in [
        ("telegram", serde_json::json!({"bot_token": "222:rotated"})),
        (
            "lark",
            serde_json::json!({"app_id": "cli_y", "app_secret": "s2", "use_feishu": true,
                               "allowed_user_ids": ["ou_a"]}),
        ),
        (
            "slack",
            serde_json::json!({"bot_token": "xoxb-new", "app_token": "xapp-new"}),
        ),
    ] {
        let r = client()
            .put(format!("http://{addr}/api/v1/config/im/{platform}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{platform}: {:?}", r.text().await);
    }
    let saved = credentials::load(Some(&creds_path)).unwrap();
    assert_eq!(saved.telegram.as_ref().unwrap().bot_token, "222:rotated");
    assert_eq!(saved.lark.as_ref().unwrap().app_id, "cli_y");
    assert_eq!(saved.slack.as_ref().unwrap().bot_token, "xoxb-new");
    assert_eq!(
        switch_flags(&saved),
        [true, true, true],
        "a token change must not reset the switch"
    );

    for var in [
        "CCTEAM_TELEGRAM_API_BASE",
        "CCTEAM_LARK_API_BASE",
        "CCTEAM_SLACK_API_BASE",
    ] {
        std::env::remove_var(var);
    }
}

/// A regular user reads and flips the switch of their OWN bots, on every IM,
/// through the same shapes the owner has; nobody else's bot moves, and a
/// token change keeps it.
#[tokio::test]
#[serial]
async fn a_tenant_reads_and_flips_the_require_mention_switch_of_their_own_bots() {
    use ccteam_core::tenants::{TenantLark, TenantRegistry, TenantSlack, TenantTelegram};
    let tg_base = spawn_telegram_mock(true, "alice_bot", 1).await;
    let slack_base = spawn_slack_mock(true, true).await;
    std::env::set_var("CCTEAM_TELEGRAM_API_BASE", &tg_base);
    std::env::set_var("CCTEAM_SLACK_API_BASE", &slack_base);

    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let users = paths.users_dir();
    let mut reg = TenantRegistry::default();
    let alice = reg.add("alice");
    let bob = reg.add("bob");
    reg.set_telegram(
        &alice.id,
        Some(TenantTelegram {
            bot_token: "111:alice-secret".into(),
            allowed_chat_ids: vec!["42".into()],
            require_mention: false,
        }),
    );
    reg.set_lark(
        &alice.id,
        Some(TenantLark {
            app_id: "cli_alice".into(),
            app_secret: "alice-lark-secret".into(),
            allowed_user_ids: vec!["ou_1".into(), "ou_2".into()],
            use_feishu: true,
            require_mention: false,
        }),
    );
    reg.set_slack(
        &alice.id,
        Some(TenantSlack {
            bot_token: "xoxb-alice".into(),
            app_token: "xapp-alice".into(),
            allowed_user_ids: vec!["U0ALICE".into()],
            require_mention: false,
        }),
    );
    reg.set_slack(
        &bob.id,
        Some(TenantSlack {
            bot_token: "xoxb-bob".into(),
            app_token: "xapp-bob".into(),
            allowed_user_ids: vec![],
            require_mention: false,
        }),
    );
    reg.save(&users).unwrap();
    let (state, _) = state_with_creds(&tmp, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn_app(state).await;
    let alice_auth = format!("Bearer ccteam:{}", alice.web_token);
    let admin_auth = format!("Bearer ccteam:{TOKEN_HEX}");

    // She can read her own bots — masked, same shape as the owner's read.
    let me: Value = client()
        .get(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", &alice_auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["telegram"]["chat_id_count"], 1, "{me}");
    assert_eq!(me["lark"]["allowed_user_id_count"], 2, "{me}");
    // The saved allowlists come back too: the allowlist PUTs replace the whole
    // list, so the card has to start from what is already bound.
    assert_eq!(
        me["telegram"]["allowed_chat_ids"],
        serde_json::json!(["42"])
    );
    assert_eq!(
        me["lark"]["allowed_user_ids"],
        serde_json::json!(["ou_1", "ou_2"])
    );
    assert_eq!(
        me["slack"]["allowed_user_ids"],
        serde_json::json!(["U0ALICE"])
    );
    for platform in ["telegram", "lark", "slack"] {
        assert_eq!(me[platform]["configured"], true);
        assert_eq!(me[platform]["require_mention"], false, "{platform}");
    }
    let body = me.to_string();
    for secret in [
        "alice-secret",
        "alice-lark-secret",
        "xoxb-alice",
        "xapp-alice",
    ] {
        assert!(!body.contains(secret), "no secret in the read: {secret}");
    }

    // Flip each of her bots.
    for platform in ["telegram", "lark", "slack"] {
        let r = client()
            .put(format!(
                "http://{addr}/api/v1/me/im/{platform}/require-mention"
            ))
            .header("Authorization", &alice_auth)
            .json(&serde_json::json!({"require_mention": true}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{platform}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["platform"], platform);
        assert_eq!(v["require_mention"], true);
    }
    let reg = TenantRegistry::load(&users);
    let a = reg.by_id(&alice.id).unwrap();
    assert!(a.telegram.as_ref().unwrap().require_mention);
    assert!(a.lark.as_ref().unwrap().require_mention);
    assert!(a.slack.as_ref().unwrap().require_mention);
    assert_eq!(a.slack.as_ref().unwrap().bot_token, "xoxb-alice");
    assert_eq!(
        a.lark.as_ref().unwrap().allowed_user_ids,
        vec!["ou_1", "ou_2"]
    );
    assert!(
        !reg.by_id(&bob.id)
            .unwrap()
            .slack
            .as_ref()
            .unwrap()
            .require_mention,
        "bob's bot did not move"
    );

    // A token change keeps it (Telegram + Slack validate against the mocks).
    let r = client()
        .put(format!("http://{addr}/api/v1/me/im"))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({
            "telegram_bot_token": "222:rotated",
            "lark": {"app_id": "cli_alice2", "app_secret": "s2"},
            "slack": {"bot_token": "xoxb-a2", "app_token": "xapp-a2"},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{:?}", r.text().await);
    let reg = TenantRegistry::load(&users);
    let a = reg.by_id(&alice.id).unwrap();
    assert_eq!(a.telegram.as_ref().unwrap().bot_token, "222:rotated");
    assert_eq!(a.lark.as_ref().unwrap().app_id, "cli_alice2");
    assert_eq!(a.slack.as_ref().unwrap().bot_token, "xoxb-a2");
    assert!(
        a.telegram.as_ref().unwrap().require_mention
            && a.lark.as_ref().unwrap().require_mention
            && a.slack.as_ref().unwrap().require_mention,
        "a token change must not reset the switch"
    );

    // Bob has no Telegram bot: refused, not silently created. A typo too.
    let bob_auth = format!("Bearer ccteam:{}", bob.web_token);
    let r = client()
        .put(format!(
            "http://{addr}/api/v1/me/im/telegram/require-mention"
        ))
        .header("Authorization", &bob_auth)
        .json(&serde_json::json!({"require_mention": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = client()
        .put(format!(
            "http://{addr}/api/v1/me/im/discord/require-mention"
        ))
        .header("Authorization", &alice_auth)
        .json(&serde_json::json!({"require_mention": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // The owner's bot is the global one: the tenant endpoints say so.
    for method_url in [
        ("GET", format!("http://{addr}/api/v1/me/im")),
        (
            "PUT",
            format!("http://{addr}/api/v1/me/im/slack/require-mention"),
        ),
    ] {
        let req = match method_url.0 {
            "GET" => client().get(&method_url.1),
            _ => client()
                .put(&method_url.1)
                .json(&serde_json::json!({"require_mention": true})),
        };
        let r = req
            .header("Authorization", &admin_auth)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{}", method_url.1);
    }

    std::env::remove_var("CCTEAM_TELEGRAM_API_BASE");
    std::env::remove_var("CCTEAM_SLACK_API_BASE");
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
