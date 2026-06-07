//! V0.6.0 Wave 2 F117 — IM onboarding flows.
//!
//! Each platform exposes a single async entry point that:
//! 1. validates the bot token (`getMe`-equivalent),
//! 2. long-polls for the first incoming message to capture the
//!    `chat_id` of the user the credentials should be bound to,
//! 3. returns a typed credential record.
//!
//! Persisting the result is the caller's responsibility — see
//! [`crate::credentials::write_credentials`].
//!
//! ## HTTP transport
//!
//! Uses `reqwest` with rustls. The base URL is parameterized so
//! integration tests can point at a local mock server (no real
//! Telegram call required for `cargo test`).

use serde::Deserialize;
use thiserror::Error;

use crate::credentials::{LarkCreds, TelegramCreds};

/// Wrapper around [`TelegramCreds`] that carries the `bot_username`
/// returned by `getMe` for the skill UX ("在 TG 找 @xxx"). Kept off
/// the on-disk [`TelegramCreds`] struct so credentials.json stays
/// minimal (per imd's reply: "don't add bot_username to TelegramCreds
/// because that's the on-disk schema").
#[derive(Debug, Clone, PartialEq)]
pub struct TelegramSetupResult {
    /// The validated bot token plus the owner `chat_id` captured by the
    /// long-poll — exactly the on-disk [`TelegramCreds`] shape, ready to
    /// merge into the credentials document.
    pub creds: TelegramCreds,
    /// Bot handle from `getMe`, including leading `@`.
    pub bot_username: String,
}

/// Default Telegram Bot API root.
pub const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// Errors returned by the onboarding flows.
#[derive(Debug, Error)]
pub enum OnboardingError {
    /// The underlying `reqwest` HTTP call (getMe / getUpdates) failed —
    /// DNS, TLS, connect, or read timeout.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// Telegram returned a `200` with `ok: false`; the `String` names the
    /// API method that was rejected (e.g. an invalid bot token on `getMe`).
    #[error("Telegram API returned `ok: false`: {0}")]
    ApiNotOk(String),
    /// The long-poll window elapsed without the owner sending a message,
    /// so no `chat_id` could be captured.
    #[error("polled {seconds}s with no incoming message — please DM the bot and retry")]
    NoIncomingMessage {
        /// The poll budget (seconds) that was exhausted.
        seconds: u64,
    },
    /// A Telegram response decoded but was missing a field the flow needs
    /// (e.g. `getMe.result`); the `String` describes what was absent.
    #[error("malformed Telegram response: {0}")]
    BadResponse(String),
}

/// Public entry point used by `/ccteam-im-setup`.
///
/// Calls Telegram's `getMe` to verify the token + capture bot
/// username, then long-polls `getUpdates` until the user sends the
/// first message (typically `"hello"`) to capture their `chat_id`.
///
/// `poll_seconds` bounds the long-poll window (skill prompts the user
/// to DM the bot during this window).
pub async fn telegram_setup(
    token: &str,
    poll_seconds: u64,
) -> Result<TelegramSetupResult, OnboardingError> {
    telegram_setup_with_base(token, poll_seconds, TELEGRAM_API_BASE).await
}

/// Test-friendly variant that lets callers override the API base.
///
/// Composed from the two reusable steps below so the CLI keeps its
/// one-shot "validate + capture chat_id" flow while the web config
/// backend (F4) can call the steps independently (validate the token on
/// `PUT`, then poll for the `chat_id` in a separate background task).
pub async fn telegram_setup_with_base(
    token: &str,
    poll_seconds: u64,
    api_base: &str,
) -> Result<TelegramSetupResult, OnboardingError> {
    // Step 1: getMe — token validation + bot username capture.
    let bot_username = telegram_validate_token_with_base(token, api_base).await?;
    // Step 2: getUpdates long-poll for first chat_id.
    let owner_chat_id = telegram_poll_chat_id_with_base(token, api_base, poll_seconds).await?;

    Ok(TelegramSetupResult {
        creds: TelegramCreds {
            bot_token: token.into(),
            allowed_chat_ids: vec![owner_chat_id.to_string()],
        },
        bot_username,
    })
}

/// Step 1 of the Telegram flow, exposed for reuse: validate the bot token
/// via `getMe` and return the bot handle (incl. leading `@`). A `200` with
/// `ok: false` (e.g. an invalid token) surfaces as
/// [`OnboardingError::ApiNotOk`]; a `200` missing the `result` block is
/// [`OnboardingError::BadResponse`]. No long-poll — returns immediately.
///
/// The web config backend calls this on `PUT /config/im/telegram` to fail
/// a bad token before it ever lands on disk.
pub async fn telegram_validate_token_with_base(
    token: &str,
    api_base: &str,
) -> Result<String, OnboardingError> {
    // getMe is a single short request; a 30s budget is plenty and avoids
    // the long-poll timeout the combined flow uses.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let me: GetMeResponse = client
        .get(format!("{api_base}/bot{token}/getMe"))
        .send()
        .await?
        .json()
        .await?;
    if !me.ok {
        return Err(OnboardingError::ApiNotOk("getMe".into()));
    }
    let bot_user = me
        .result
        .ok_or_else(|| OnboardingError::BadResponse("getMe.result missing".into()))?;
    Ok(format!("@{}", bot_user.username))
}

/// Step 2 of the Telegram flow, exposed for reuse: long-poll `getUpdates`
/// until the owner DMs the bot, capturing their `chat_id`. Times out as
/// [`OnboardingError::NoIncomingMessage`] after `poll_seconds`.
///
/// The web config backend calls this from a background task (the
/// `POST .../chat-id/start` → `GET .../chat-id` async capture), so it
/// builds its own client (rather than borrowing the combined flow's).
pub async fn telegram_poll_chat_id_with_base(
    token: &str,
    api_base: &str,
    poll_seconds: u64,
) -> Result<i64, OnboardingError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(poll_seconds + 10))
        .build()?;
    poll_first_chat_id(&client, token, api_base, poll_seconds).await
}

async fn poll_first_chat_id(
    client: &reqwest::Client,
    token: &str,
    api_base: &str,
    poll_seconds: u64,
) -> Result<i64, OnboardingError> {
    // Telegram's long-poll cap is 50s per request; loop until we either
    // capture a message or exhaust the user-provided budget.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(poll_seconds);
    let mut last_update_id: Option<i64> = None;

    while std::time::Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_secs();
        let timeout = remaining.clamp(1, 50);
        let mut url = format!("{api_base}/bot{token}/getUpdates?timeout={timeout}");
        if let Some(off) = last_update_id {
            url.push_str(&format!("&offset={}", off + 1));
        }

        let resp: GetUpdatesResponse = client.get(&url).send().await?.json().await?;
        if !resp.ok {
            return Err(OnboardingError::ApiNotOk("getUpdates".into()));
        }
        for upd in resp.result.iter() {
            last_update_id = Some(upd.update_id);
            if let Some(msg) = &upd.message {
                return Ok(msg.chat.id);
            }
        }
    }
    Err(OnboardingError::NoIncomingMessage {
        seconds: poll_seconds,
    })
}

// --- Telegram wire types (minimal subset) ----------------------------

#[derive(Debug, Deserialize)]
struct GetMeResponse {
    ok: bool,
    result: Option<BotUser>,
}

#[derive(Debug, Deserialize)]
struct BotUser {
    username: String,
}

#[derive(Debug, Deserialize)]
struct GetUpdatesResponse {
    ok: bool,
    #[serde(default)]
    result: Vec<Update>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    chat: Chat,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
}

// --- Lark / Feishu onboarding ----------------------------------------
//
// Unlike Telegram there is no `chat_id` long-poll: the Lark provider keys
// its allowlist on the operator-supplied `open_id` list (fail-closed) and
// the daemon opens an *outbound* WS long-connection, so the only thing to
// confirm at setup time is that `(app_id, app_secret)` are valid app
// credentials. We do that by fetching a `tenant_access_token` — the same
// `auth/v3/tenant_access_token/internal` call the live channel makes
// (`transport::providers::lark::LarkChannel::get_tenant_access_token`).
// A bad app_id / secret surfaces as an honest network/API error rather
// than persisting dead credentials.

/// Default Lark/Feishu open-platform API roots. `use_feishu = true`
/// (CN) → `open.feishu.cn`; `false` (intl) → `open.larksuite.com`.
/// Mirrors the constants in `transport::providers::lark`.
pub const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";
/// Lark international open-platform API root.
pub const LARK_API_BASE: &str = "https://open.larksuite.com/open-apis";

/// Result of a successful Lark/Feishu credential check: the on-disk
/// [`LarkCreds`] record ready to merge into the credentials document.
#[derive(Debug, Clone, PartialEq)]
pub struct LarkSetupResult {
    /// Validated credentials (app id/secret + provider allowlist + region).
    pub creds: LarkCreds,
}

/// Validate Lark/Feishu app credentials and return the on-disk record.
///
/// `allowed_user_ids` is the provider-layer `open_id` (`ou_…`) allowlist —
/// **fail-closed**: an empty list means the bot answers no one (the
/// opposite of Telegram, where an empty allowlist is open). `use_feishu`
/// selects the region (`true` = Feishu/CN, `false` = Lark international).
pub async fn lark_setup(
    app_id: &str,
    app_secret: &str,
    allowed_user_ids: Vec<String>,
    use_feishu: bool,
) -> Result<LarkSetupResult, OnboardingError> {
    let api_base = if use_feishu {
        FEISHU_API_BASE
    } else {
        LARK_API_BASE
    };
    lark_setup_with_base(app_id, app_secret, allowed_user_ids, use_feishu, api_base).await
}

/// Test-friendly variant that lets callers override the API base (point a
/// deterministic mock server at it — no real Feishu/Lark call required for
/// `cargo test`). Mirrors [`telegram_setup_with_base`].
pub async fn lark_setup_with_base(
    app_id: &str,
    app_secret: &str,
    allowed_user_ids: Vec<String>,
    use_feishu: bool,
    api_base: &str,
) -> Result<LarkSetupResult, OnboardingError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // tenant_access_token/internal — same body the live channel posts.
    let url = format!("{api_base}/auth/v3/tenant_access_token/internal");
    let resp: TenantTokenResponse = client
        .post(&url)
        .json(&serde_json::json!({
            "app_id": app_id,
            "app_secret": app_secret,
        }))
        .send()
        .await?
        .json()
        .await?;

    // Feishu wraps errors in a `200` with a non-zero `code` (mirrors the
    // channel's `get_tenant_access_token` check) — treat that as "bad
    // credentials" so the operator gets an honest failure, not a saved
    // dead token.
    if resp.code != 0 {
        let msg = resp.msg.unwrap_or_else(|| "unknown error".into());
        return Err(OnboardingError::ApiNotOk(format!(
            "tenant_access_token (code={}): {msg}",
            resp.code
        )));
    }
    if resp.tenant_access_token.unwrap_or_default().is_empty() {
        return Err(OnboardingError::BadResponse(
            "tenant_access_token missing from response".into(),
        ));
    }

    Ok(LarkSetupResult {
        creds: LarkCreds {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            allowed_user_ids,
            use_feishu,
        },
    })
}

/// `auth/v3/tenant_access_token/internal` response (minimal subset).
#[derive(Debug, Deserialize)]
struct TenantTokenResponse {
    code: i64,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    tenant_access_token: Option<String>,
}
