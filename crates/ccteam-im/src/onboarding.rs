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

use crate::credentials::{LarkCreds, SlackCreds, TelegramCreds};

/// HTTP client for one IM platform's API base. A loopback base (a test mock)
/// bypasses any configured HTTP proxy so `cargo test` never leaves the box.
pub(crate) fn client_for_api_base(
    api_base: &str,
    timeout: std::time::Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder().timeout(timeout);
    let builder = if api_base_is_loopback(api_base) {
        builder.no_proxy()
    } else {
        builder
    };
    builder.build()
}

fn api_base_is_loopback(api_base: &str) -> bool {
    let Some(host) = reqwest::Url::parse(api_base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

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
    /// The platform API answered but refused the call (Telegram/Slack
    /// `ok: false`, a non-zero Feishu `code`); the `String` names the
    /// platform + method and carries the upstream reason (e.g. an invalid
    /// bot token on `getMe` / `auth.test`).
    #[error("IM platform API rejected the call: {0}")]
    ApiNotOk(String),
    /// The long-poll window elapsed without the owner sending a message,
    /// so no `chat_id` could be captured.
    #[error("polled {seconds}s with no incoming message — please DM the bot and retry")]
    NoIncomingMessage {
        /// The poll budget (seconds) that was exhausted.
        seconds: u64,
    },
    /// A platform response decoded but was missing a field the flow needs
    /// (e.g. `getMe.result`, `auth.test.user_id`); the `String` describes
    /// what was absent.
    #[error("malformed IM platform response: {0}")]
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
            require_mention: false,
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
    let client = client_for_api_base(api_base, std::time::Duration::from_secs(30))?;
    let me: GetMeResponse = client
        .get(format!("{api_base}/bot{token}/getMe"))
        .send()
        .await?
        .json()
        .await?;
    if !me.ok {
        return Err(OnboardingError::ApiNotOk("Telegram getMe".into()));
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
    let client = client_for_api_base(api_base, std::time::Duration::from_secs(poll_seconds + 10))?;
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
            return Err(OnboardingError::ApiNotOk("Telegram getUpdates".into()));
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
    let client = client_for_api_base(api_base, std::time::Duration::from_secs(30))?;

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
            "Lark tenant_access_token (code={}): {msg}",
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
            require_mention: false,
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

// --- Slack onboarding ------------------------------------------------
//
// Like Lark there is nothing to long-poll: the provider keys its allowlist on
// operator-supplied Slack user ids (fail-closed) and opens an *outbound*
// Socket Mode connection. Setup proves both tokens with the exact calls the
// live channel makes (`transport::providers::slack::SlackChannel`):
// `auth.test` with the `xoxb-` bot token (Web API) and
// `apps.connections.open` with the `xapp-` app-level token (Socket Mode). The
// WSS URL the latter returns is discarded unused — no socket is kept.

/// Default Slack Web API root (also the live channel's default base).
pub const SLACK_API_BASE: &str = "https://slack.com/api";

/// The app name a setup surface proposes when the operator gives none.
pub const DEFAULT_SLACK_APP_NAME: &str = "ccteam";

/// Bot token scopes the Slack provider uses — the manifest and the setup
/// checklist are both built from this list.
pub const SLACK_BOT_SCOPES: &[&str] = &[
    "app_mentions:read",
    "chat:write",
    "channels:history",
    "groups:history",
    "im:history",
    "mpim:history",
    "reactions:write",
    "files:read",
    "files:write",
    "commands",
];

/// Bot events the Slack provider consumes over Socket Mode.
pub const SLACK_BOT_EVENTS: &[&str] = &[
    "message.channels",
    "message.groups",
    "message.im",
    "message.mpim",
];

/// Slack's limits on an app's display name, its bot user's handle and a
/// slash command (the leading `/` included).
const SLACK_APP_NAME_MAX: usize = 35;
const SLACK_BOT_HANDLE_MAX: usize = 80;
const SLACK_SLASH_COMMAND_MAX: usize = 32;

/// The app's display name: trimmed, capped, [`DEFAULT_SLACK_APP_NAME`] when
/// blank.
fn slack_app_name(app_name: &str) -> String {
    let name: String = app_name.trim().chars().take(SLACK_APP_NAME_MAX).collect();
    if name.is_empty() {
        DEFAULT_SLACK_APP_NAME.to_string()
    } else {
        name
    }
}

/// The bot user's handle for `app_name`: Slack only allows lowercase letters,
/// digits, `.`, `_` and `-` there.
fn slack_bot_handle(app_name: &str) -> String {
    let handle: String = slack_app_name(app_name)
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(SLACK_BOT_HANDLE_MAX)
        .collect();
    let handle = handle.trim_matches('-');
    if handle.is_empty() {
        DEFAULT_SLACK_APP_NAME.to_string()
    } else {
        handle.to_string()
    }
}

/// The slash command an app called `app_name` declares: `/` + its bot handle
/// (`cct2` → `/cct2`). Every app gets its own, so several ccteam daemons can
/// each have an app in one workspace without fighting over one command. The
/// provider needs no copy of it: Socket Mode only ever delivers the app's own
/// commands.
pub fn slack_slash_command(app_name: &str) -> String {
    let handle: String = slack_bot_handle(app_name)
        .chars()
        .take(SLACK_SLASH_COMMAND_MAX - 1)
        .collect();
    format!("/{}", handle.trim_end_matches(['-', '.']))
}

/// The Slack app manifest ccteam needs, for an app called `app_name`: Socket
/// Mode on (no public URL), a slash command named after the app
/// ([`slack_slash_command`]), interactivity for
/// option buttons, the App Home messages tab for DMs, and exactly the scopes
/// and events the provider uses. The one home of what "a ccteam Slack app"
/// is — the web setup card and [`slack_app_checklist`] both come from here.
pub fn slack_app_manifest(app_name: &str) -> serde_json::Value {
    let name = slack_app_name(app_name);
    let handle = slack_bot_handle(app_name);
    serde_json::json!({
        "display_information": {
            "name": name,
            "description": "ccteam — every agent session in its own thread",
        },
        "features": {
            "bot_user": { "display_name": handle, "always_online": true },
            "app_home": {
                "messages_tab_enabled": true,
                "messages_tab_read_only_enabled": false,
            },
            "slash_commands": [{
                "command": slack_slash_command(app_name),
                "description": "ccteam command: projects, cd, sessions, new, status, help",
                "usage_hint": "projects | cd <project> | sessions | new codex | status",
                "should_escape": false,
            }],
        },
        "oauth_config": { "scopes": { "bot": SLACK_BOT_SCOPES } },
        "settings": {
            "event_subscriptions": { "bot_events": SLACK_BOT_EVENTS },
            "interactivity": { "is_enabled": true },
            "socket_mode_enabled": true,
            "org_deploy_enabled": false,
            "token_rotation_enabled": false,
        },
    })
}

/// A link that opens Slack's "create app" flow with [`slack_app_manifest`]
/// already filled in (`new_app=1&manifest_json=…`): one click, pick the
/// workspace, Create — no YAML to copy.
pub fn slack_create_app_url(app_name: &str) -> String {
    let manifest = slack_app_manifest(app_name).to_string();
    reqwest::Url::parse_with_params(
        "https://api.slack.com/apps",
        &[("new_app", "1"), ("manifest_json", manifest.as_str())],
    )
    .map(|url| url.to_string())
    .unwrap_or_else(|_| "https://api.slack.com/apps".to_string())
}

/// What the Slack app itself must have for the provider to work — printed by
/// the setup surfaces after a successful save. Built from the same constants
/// as [`slack_app_manifest`].
pub fn slack_app_checklist() -> String {
    format!(
        "Slack app checklist (api.slack.com/apps → your app):
  - Socket Mode: ON (the xapp- app-level token needs connections:write)
  - Slash command: /<app name> ({} for an app named {})
  - Interactivity & Shortcuts: ON (option buttons)
  - App Home → Messages tab: ON, allow users to message the app (DMs)
  - Bot token scopes: {}
  - Event subscriptions → bot events: {}
  - Reinstall the app after scope changes, then invite the bot to a channel
    (/invite @<bot>) or DM it
  Easiest: create the app from {}
",
        slack_slash_command(DEFAULT_SLACK_APP_NAME),
        DEFAULT_SLACK_APP_NAME,
        SLACK_BOT_SCOPES.join(" "),
        SLACK_BOT_EVENTS.join(" "),
        slack_create_app_url(DEFAULT_SLACK_APP_NAME),
    )
}

/// Result of a successful Slack credential check: the on-disk
/// [`SlackCreds`] record plus the workspace/bot names for the setup UX.
#[derive(Debug, Clone, PartialEq)]
pub struct SlackSetupResult {
    /// Validated tokens + the provider allowlist.
    pub creds: SlackCreds,
    /// Workspace name from `auth.test` (`team`).
    pub team: String,
    /// The bot user's handle from `auth.test` (`user`).
    pub bot_user: String,
    /// The bot user's id (`U…`) from `auth.test` (`user_id`).
    pub bot_user_id: String,
}

/// Validate Slack credentials against the real Web API and return the
/// on-disk record. `allowed_user_ids` is the provider allowlist of Slack user
/// ids (`U…`) — **fail-closed**: empty means the bot answers no one.
pub async fn slack_setup(
    bot_token: &str,
    app_token: &str,
    allowed_user_ids: Vec<String>,
) -> Result<SlackSetupResult, OnboardingError> {
    slack_setup_with_base(bot_token, app_token, allowed_user_ids, SLACK_API_BASE).await
}

/// Test-friendly variant of [`slack_setup`] with an overridable API base
/// (point a deterministic local mock at it — `cargo test` never calls Slack).
pub async fn slack_setup_with_base(
    bot_token: &str,
    app_token: &str,
    allowed_user_ids: Vec<String>,
    api_base: &str,
) -> Result<SlackSetupResult, OnboardingError> {
    let client = client_for_api_base(api_base, std::time::Duration::from_secs(30))?;

    let auth: SlackOkResponse = client
        .post(format!("{api_base}/auth.test"))
        .bearer_auth(bot_token)
        .send()
        .await?
        .json()
        .await?;
    if !auth.ok {
        return Err(OnboardingError::ApiNotOk(format!(
            "Slack auth.test (bot token xoxb-…): {}",
            auth.error.unwrap_or_else(|| "unknown_error".into())
        )));
    }
    let bot_user_id = auth
        .user_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| OnboardingError::BadResponse("Slack auth.test: user_id missing".into()))?;

    let socket: SlackOkResponse = client
        .post(format!("{api_base}/apps.connections.open"))
        .bearer_auth(app_token)
        .send()
        .await?
        .json()
        .await?;
    if !socket.ok {
        return Err(OnboardingError::ApiNotOk(format!(
            "Slack apps.connections.open (app-level token xapp-…, Socket Mode): {}",
            socket.error.unwrap_or_else(|| "unknown_error".into())
        )));
    }
    if socket.url.unwrap_or_default().is_empty() {
        return Err(OnboardingError::BadResponse(
            "Slack apps.connections.open: url missing".into(),
        ));
    }

    Ok(SlackSetupResult {
        creds: SlackCreds {
            bot_token: bot_token.into(),
            app_token: app_token.into(),
            allowed_user_ids,
            require_mention: false,
        },
        team: auth.team.unwrap_or_default(),
        bot_user: auth.user.unwrap_or_default(),
        bot_user_id,
    })
}

/// The fields of `auth.test` / `apps.connections.open` setup reads.
#[derive(Debug, Deserialize)]
struct SlackOkResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    url: Option<String>,
}
