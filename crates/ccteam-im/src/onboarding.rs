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

use crate::credentials::TelegramCreds;

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
pub async fn telegram_setup_with_base(
    token: &str,
    poll_seconds: u64,
    api_base: &str,
) -> Result<TelegramSetupResult, OnboardingError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(poll_seconds + 10))
        .build()?;

    // Step 1: getMe — token validation + bot username capture.
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
    let bot_username = format!("@{}", bot_user.username);

    // Step 2: getUpdates long-poll for first chat_id.
    let owner_chat_id = poll_first_chat_id(&client, token, api_base, poll_seconds).await?;

    Ok(TelegramSetupResult {
        creds: TelegramCreds {
            bot_token: token.into(),
            allowed_chat_ids: vec![owner_chat_id.to_string()],
        },
        bot_username,
    })
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
