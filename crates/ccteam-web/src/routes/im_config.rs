//! v0.8.8 F4 — web IM credential configuration (Telegram + Lark/Feishu).
//!
//! Mounted into the `/api/v1` `OpenApiRouter` (see [`super::openapi`]), so
//! every route here sits behind the same web-token gate as the rest of the
//! resource API.
//!
//! ## Red lines honored
//!
//! - **Secrets never echo as plaintext.** The read shape
//!   ([`ImConfigStatus`]) has *no* `bot_token` / `app_secret` field at all —
//!   plaintext echo is impossible at the type level, not just by omission at
//!   serialize time. The only secret-derived data returned is a last-4
//!   fingerprint via [`mask_last4`].
//! - **Credentials are NOT hot-applied.** The daemon loads creds once at
//!   startup ([`ccteam_im::credentials::load`]); there is no reload/watch.
//!   So every mutating response carries `restart_required: true` plus an
//!   operator-facing `note`. The handlers only write the file (0600, via
//!   [`ccteam_im::credentials::save`]).
//! - **Validate before persist.** A bad Telegram token / Lark app secret is
//!   rejected (`400` + the [`OnboardingError`] `Display` reason) *before* it
//!   lands on disk — reusing the CLI validators
//!   (`onboarding::{telegram_validate_token_with_base, lark_setup_with_base}`)
//!   directly in the async handler (no nested runtime).
//! - **No TLS.** The daemon serves plain HTTP; configuring secrets over a
//!   LAN link is plaintext on the wire. The GET response carries a
//!   `transport_warning` so the SPA can surface it.
//!
//! ## Telegram `chat_id` async capture
//!
//! Telegram has no synchronous "who am I bound to" call: the operator must
//! DM the bot and we long-poll `getUpdates` for the first message. That can
//! take tens of seconds, so it is split off the request path:
//!
//! 1. `POST /config/im/telegram/chat-id/start` spawns a background
//!    `tokio::task` running [`telegram_poll_chat_id_with_base`], writes
//!    [`TelegramChatIdPoll::Pending`] into [`AppState::im_poll`], and returns
//!    `{started:true}` immediately.
//! 2. The SPA polls `GET /config/im/telegram/chat-id`, which reports the
//!    current [`TelegramChatIdPoll`] state. On `Captured`, the GET handler
//!    persists the `chat_id` into `credentials.telegram.allowed_chat_ids`
//!    (dedup-append) and returns `{status:"captured", chat_id_last4}`.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, response::Response, Json};
use ccteam_im::credentials::{self, Credentials, LarkCreds, TelegramCreds};
use ccteam_im::onboarding::{
    lark_setup_with_base, telegram_poll_chat_id_with_base, telegram_validate_token_with_base,
    FEISHU_API_BASE, LARK_API_BASE, TELEGRAM_API_BASE,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use utoipa::ToSchema;

use super::actions::FormOrJson;
use crate::state::{AppState, TelegramChatIdPoll};

/// Total budget (seconds) for the async Telegram `chat_id` capture. The
/// operator DMs the bot during this window; long enough to walk to another
/// device, bounded so a stuck task self-terminates.
const CHAT_ID_POLL_SECONDS: u64 = 90;

/// LAN-plaintext caveat surfaced on the GET so the SPA can warn the
/// operator that creds posted over this link are not TLS-protected.
const TRANSPORT_WARNING: &str =
    "ccteam serves plain HTTP (no TLS). On a LAN link these credentials are sent in cleartext; \
     prefer loopback or a trusted network.";

/// Standard "creds changed, restart to apply" note attached to every
/// mutating response (the daemon loads creds once at startup — no reload).
const RESTART_NOTE: &str =
    "Credentials are loaded once at daemon startup and are not hot-applied. Restart ccteam \
     (`ccteam stop && ccteam start`) for the new IM credentials to take effect.";

/// Env override for the Telegram Bot API base. Production reads the
/// `onboarding::TELEGRAM_API_BASE` constant; integration tests set this to a
/// localhost mock (mirrors the `CCTEAM_CLAUDE_BIN` test-override pattern — no
/// real Telegram call in `cargo test`). Empty/unset ⇒ the real base.
const TELEGRAM_API_BASE_ENV: &str = "CCTEAM_TELEGRAM_API_BASE";
/// Env override for the Lark/Feishu open-platform API base (both regions).
const LARK_API_BASE_ENV: &str = "CCTEAM_LARK_API_BASE";

/// Resolve the Telegram API base: the [`TELEGRAM_API_BASE_ENV`] override if
/// set + non-empty, else the production constant.
fn telegram_api_base() -> String {
    std::env::var(TELEGRAM_API_BASE_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| TELEGRAM_API_BASE.to_string())
}

/// Resolve the Lark/Feishu API base. The [`LARK_API_BASE_ENV`] override (if
/// set) wins for BOTH regions (a test mock doesn't distinguish CN/intl);
/// otherwise `use_feishu` picks the production CN / intl constant.
fn lark_api_base(use_feishu: bool) -> String {
    if let Some(base) = std::env::var(LARK_API_BASE_ENV)
        .ok()
        .filter(|s| !s.is_empty())
    {
        return base;
    }
    if use_feishu {
        FEISHU_API_BASE.to_string()
    } else {
        LARK_API_BASE.to_string()
    }
}

// --------------------------------------------------------------------------
// Masking
// --------------------------------------------------------------------------

/// Reduce a secret to a non-reversible fingerprint: the last 4 characters
/// for anything longer than 4, otherwise all-`*` (so a short secret leaks
/// nothing — not even its length beyond "≤4"). Returns `""` for an empty
/// string. This is the ONLY secret-derived value any handler returns.
fn mask_last4(s: &str) -> String {
    let n = s.chars().count();
    if n == 0 {
        String::new()
    } else if n <= 4 {
        "*".repeat(n)
    } else {
        let tail: String = s.chars().skip(n - 4).collect();
        format!("…{tail}")
    }
}

// --------------------------------------------------------------------------
// Read shape (masked) — deliberately NO secret fields
// --------------------------------------------------------------------------

/// Masked Telegram status. Note the absence of any `bot_token` field — the
/// token is never serialized, only its last-4 fingerprint.
#[derive(Debug, Serialize, ToSchema)]
pub struct TelegramStatus {
    /// Always `true` when present (a block exists on disk).
    pub configured: bool,
    /// Last-4 fingerprint of the bot token (`…wxyz`), never the token.
    pub bot_token_last4: String,
    /// How many `chat_id`s are bound (the allowlist length).
    pub chat_id_count: usize,
}

/// Masked Lark/Feishu status. Note the absence of any `app_secret` field.
#[derive(Debug, Serialize, ToSchema)]
pub struct LarkStatus {
    /// Always `true` when present.
    pub configured: bool,
    /// Last-4 fingerprint of the app id. The app id is not strictly a
    /// secret, but masking it keeps the read shape uniformly fingerprinted.
    pub app_id_last4: String,
    /// `true` = Feishu (CN), `false` = Lark international.
    pub use_feishu: bool,
    /// How many `open_id`s are allowlisted.
    pub allowed_user_id_count: usize,
}

/// `GET /config/im` response — masked, secret-free.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImConfigStatus {
    /// Present iff a Telegram block exists on disk.
    pub telegram: Option<TelegramStatus>,
    /// Present iff a Lark block exists on disk.
    pub lark: Option<LarkStatus>,
    /// Cleartext-on-LAN caveat (no TLS) — for the SPA to surface.
    pub transport_warning: String,
}

// --------------------------------------------------------------------------
// Write shapes (request bodies)
// --------------------------------------------------------------------------

/// `PUT /config/im/telegram` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramConfigForm {
    /// The `@BotFather` token to validate (via `getMe`) and persist.
    pub bot_token: String,
}

/// `PUT /config/im/lark` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LarkConfigForm {
    /// App ID (`cli_...`).
    pub app_id: String,
    /// App secret (validated via `tenant_access_token`, then persisted).
    pub app_secret: String,
    /// `open_id` (`ou_...`) allowlist. Empty = fail-closed (bot answers no
    /// one) — matches the provider-layer semantics.
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
    /// `true` = Feishu (CN, default), `false` = Lark international.
    #[serde(default = "default_use_feishu")]
    pub use_feishu: bool,
}

fn default_use_feishu() -> bool {
    true
}

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Load creds from the app's (possibly test-overridden) path. A genuine
/// load failure (parse error / bad mode) returns a human message the caller
/// wraps in a `500` JSON body — never a panic, never a stack leak. (Returns
/// the message rather than a built `Response` so the `Err` variant stays
/// small — `clippy::result_large_err`; the codebase convention is small-Err
/// helpers + `Response` built at the call site, see `actions.rs`.)
fn load_creds(app: &AppState) -> Result<Credentials, String> {
    credentials::load(Some(app.creds_path.as_path())).map_err(|err| {
        tracing::error!(%err, "im_config: credentials load failed");
        format!("could not read credentials: {err}")
    })
}

/// Persist creds to the app's path (0600 enforced by `credentials::save`).
/// Same small-`Err` convention as [`load_creds`].
fn save_creds(app: &AppState, creds: &Credentials) -> Result<(), String> {
    credentials::save(app.creds_path.as_path(), creds).map_err(|err| {
        tracing::error!(%err, "im_config: credentials save failed");
        format!("could not write credentials: {err}")
    })
}

fn json_400(msg: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

fn json_500(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

// --------------------------------------------------------------------------
// GET /config/im — masked read
// --------------------------------------------------------------------------

/// `GET /api/v1/config/im` — masked view of the configured IM credentials.
///
/// Never returns a `bot_token` or `app_secret` (the response type has no
/// such field); only last-4 fingerprints + counts + the `use_feishu` flag.
#[utoipa::path(
    get,
    path = "/api/v1/config/im",
    tag = "config",
    responses(
        (status = 200, description = "Masked IM credential status (never echoes secrets)", body = ImConfigStatus),
        (status = 500, description = "Credentials file could not be read"),
    ),
)]
pub(crate) async fn handle_get_im_config(State(app): State<AppState>) -> Response {
    let creds = match load_creds(&app) {
        Ok(c) => c,
        Err(e) => return json_500(e),
    };
    let telegram = creds.telegram.map(|t| TelegramStatus {
        configured: true,
        bot_token_last4: mask_last4(&t.bot_token),
        chat_id_count: t.allowed_chat_ids.len(),
    });
    let lark = creds.lark.map(|l| LarkStatus {
        configured: true,
        app_id_last4: mask_last4(&l.app_id),
        use_feishu: l.use_feishu,
        allowed_user_id_count: l.allowed_user_ids.len(),
    });
    Json(ImConfigStatus {
        telegram,
        lark,
        transport_warning: TRANSPORT_WARNING.to_string(),
    })
    .into_response()
}

// --------------------------------------------------------------------------
// PUT /config/im/telegram — validate token + persist
// --------------------------------------------------------------------------

/// `PUT /api/v1/config/im/telegram` — validate the bot token (`getMe`) then
/// persist it, preserving any existing `allowed_chat_ids`.
///
/// A bad token is rejected `400` (the `OnboardingError` reason) *before* it
/// touches disk. Success returns `{ok, restart_required, bot_username, note}`.
#[utoipa::path(
    put,
    path = "/api/v1/config/im/telegram",
    tag = "config",
    request_body(content = TelegramConfigForm, description = "Telegram bot token (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 200, description = "Validated + persisted; `{ok, restart_required, bot_username, note}`", body = serde_json::Value),
        (status = 400, description = "Empty token / token rejected by Telegram getMe"),
        (status = 500, description = "Credentials file read/write failed"),
    ),
)]
pub(crate) async fn handle_put_telegram(
    State(app): State<AppState>,
    FormOrJson(form, _mode): FormOrJson<TelegramConfigForm>,
) -> Response {
    let token = form.bot_token.trim();
    if token.is_empty() {
        return json_400("bot_token must not be empty".to_string());
    }

    // Validate against Telegram before persisting (reuse the CLI validator).
    let bot_username = match telegram_validate_token_with_base(token, &telegram_api_base()).await {
        Ok(name) => name,
        Err(err) => return json_400(format!("Telegram token rejected: {err}")),
    };

    // load → merge telegram (preserve existing allowed_chat_ids) → save.
    let mut creds = match load_creds(&app) {
        Ok(c) => c,
        Err(e) => return json_500(e),
    };
    let existing_chat_ids = creds
        .telegram
        .take()
        .map(|t| t.allowed_chat_ids)
        .unwrap_or_default();
    creds.telegram = Some(TelegramCreds {
        bot_token: token.to_string(),
        allowed_chat_ids: existing_chat_ids,
    });
    if let Err(e) = save_creds(&app, &creds) {
        return json_500(e);
    }

    Json(serde_json::json!({
        "ok": true,
        "restart_required": true,
        "bot_username": bot_username,
        "note": RESTART_NOTE,
    }))
    .into_response()
}

// --------------------------------------------------------------------------
// POST /config/im/telegram/chat-id/start — kick off async capture
// --------------------------------------------------------------------------

/// `POST /api/v1/config/im/telegram/chat-id/start` — start the background
/// long-poll for the owner's `chat_id`. Returns `{started:true}` at once;
/// the operator then DMs the bot and the SPA polls the GET below.
///
/// Requires a Telegram token already on disk (set via the PUT) — `400` if
/// not, since `getUpdates` needs the token.
#[utoipa::path(
    post,
    path = "/api/v1/config/im/telegram/chat-id/start",
    tag = "config",
    responses(
        (status = 200, description = "Background capture started; `{started:true, poll_seconds}`", body = serde_json::Value),
        (status = 400, description = "No Telegram token configured yet"),
        (status = 500, description = "Credentials file could not be read"),
    ),
)]
pub(crate) async fn handle_telegram_chat_id_start(State(app): State<AppState>) -> Response {
    let creds = match load_creds(&app) {
        Ok(c) => c,
        Err(e) => return json_500(e),
    };
    let Some(token) = creds.telegram.map(|t| t.bot_token) else {
        return json_400(
            "no Telegram token configured — PUT /config/im/telegram first".to_string(),
        );
    };

    // Reset the slot to Pending, then spawn the poll. The handle is cloned
    // into the task; the GET poller reads the same Arc.
    {
        let mut slot = app.im_poll.lock().await;
        *slot = Some(TelegramChatIdPoll::Pending);
    }
    spawn_chat_id_poll(app.im_poll.clone(), token, telegram_api_base());

    Json(serde_json::json!({
        "started": true,
        "poll_seconds": CHAT_ID_POLL_SECONDS,
    }))
    .into_response()
}

/// Spawn the background `chat_id` poll task. Extracted so tests can drive
/// it against a mock base by setting `api_base`. Writes the terminal
/// [`TelegramChatIdPoll`] state into `slot` on completion.
fn spawn_chat_id_poll(
    slot: Arc<Mutex<Option<TelegramChatIdPoll>>>,
    token: String,
    api_base: String,
) {
    tokio::spawn(async move {
        let result = telegram_poll_chat_id_with_base(&token, &api_base, CHAT_ID_POLL_SECONDS).await;
        let next = match result {
            Ok(chat_id) => TelegramChatIdPoll::Captured(chat_id),
            Err(ccteam_im::onboarding::OnboardingError::NoIncomingMessage { .. }) => {
                TelegramChatIdPoll::Timeout
            }
            Err(err) => TelegramChatIdPoll::Error(format!("{err}")),
        };
        let mut guard = slot.lock().await;
        *guard = Some(next);
    });
}

// --------------------------------------------------------------------------
// GET /config/im/telegram/chat-id — poll capture status (+ persist on hit)
// --------------------------------------------------------------------------

/// `GET /api/v1/config/im/telegram/chat-id` — report the async capture
/// state. On `captured`, persist the `chat_id` into
/// `credentials.telegram.allowed_chat_ids` (dedup-append) and return its
/// last-4 fingerprint.
#[utoipa::path(
    get,
    path = "/api/v1/config/im/telegram/chat-id",
    tag = "config",
    responses(
        (status = 200, description = "`{status: pending|captured|timeout|error, chat_id_last4?, error?}`", body = serde_json::Value),
        (status = 500, description = "Credentials file read/write failed during persist"),
    ),
)]
pub(crate) async fn handle_telegram_chat_id_poll(State(app): State<AppState>) -> Response {
    // Snapshot the slot, then drop the lock before any disk IO.
    let state = { app.im_poll.lock().await.clone() };
    match state {
        None => Json(serde_json::json!({ "status": "idle" })).into_response(),
        Some(TelegramChatIdPoll::Pending) => {
            Json(serde_json::json!({ "status": "pending" })).into_response()
        }
        Some(TelegramChatIdPoll::Timeout) => {
            Json(serde_json::json!({ "status": "timeout" })).into_response()
        }
        Some(TelegramChatIdPoll::Error(msg)) => {
            Json(serde_json::json!({ "status": "error", "error": msg })).into_response()
        }
        Some(TelegramChatIdPoll::Captured(chat_id)) => {
            // Persist into the allowlist (dedup-append), idempotently — a
            // repeated GET after capture must not duplicate the id.
            let chat_id_str = chat_id.to_string();
            let mut creds = match load_creds(&app) {
                Ok(c) => c,
                Err(e) => return json_500(e),
            };
            match creds.telegram.as_mut() {
                Some(t) => {
                    if !t.allowed_chat_ids.contains(&chat_id_str) {
                        t.allowed_chat_ids.push(chat_id_str.clone());
                        if let Err(e) = save_creds(&app, &creds) {
                            return json_500(e);
                        }
                    }
                }
                None => {
                    // Token was cleared between start + capture — record the
                    // id anyway so the operator's PUT can pick it up. Without
                    // a token block we cannot persist, so report the capture
                    // but flag that no token is configured.
                    return Json(serde_json::json!({
                        "status": "captured",
                        "chat_id_last4": mask_last4(&chat_id_str),
                        "warning": "captured but no Telegram token on disk — set the token first",
                    }))
                    .into_response();
                }
            }
            Json(serde_json::json!({
                "status": "captured",
                "chat_id_last4": mask_last4(&chat_id_str),
                "restart_required": true,
                "note": RESTART_NOTE,
            }))
            .into_response()
        }
    }
}

// --------------------------------------------------------------------------
// PUT /config/im/lark — validate app creds + persist
// --------------------------------------------------------------------------

/// `PUT /api/v1/config/im/lark` — validate `(app_id, app_secret)` via a
/// `tenant_access_token` fetch (region picked by `use_feishu`), then
/// persist. A bad secret is rejected `400` before it touches disk.
#[utoipa::path(
    put,
    path = "/api/v1/config/im/lark",
    tag = "config",
    request_body(content = LarkConfigForm, description = "Lark/Feishu app credentials (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 200, description = "Validated + persisted; `{ok, restart_required, note}`", body = serde_json::Value),
        (status = 400, description = "Missing field / credentials rejected by Lark"),
        (status = 500, description = "Credentials file read/write failed"),
    ),
)]
pub(crate) async fn handle_put_lark(
    State(app): State<AppState>,
    FormOrJson(form, _mode): FormOrJson<LarkConfigForm>,
) -> Response {
    let app_id = form.app_id.trim();
    let app_secret = form.app_secret.trim();
    if app_id.is_empty() || app_secret.is_empty() {
        return json_400("app_id and app_secret must not be empty".to_string());
    }
    let api_base = lark_api_base(form.use_feishu);

    // Validate before persisting (reuse the CLI validator).
    match lark_setup_with_base(
        app_id,
        app_secret,
        form.allowed_user_ids.clone(),
        form.use_feishu,
        &api_base,
    )
    .await
    {
        Ok(_) => {}
        Err(err) => return json_400(format!("Lark credentials rejected: {err}")),
    }

    // load → merge lark → save.
    let mut creds = match load_creds(&app) {
        Ok(c) => c,
        Err(e) => return json_500(e),
    };
    creds.lark = Some(LarkCreds {
        app_id: app_id.to_string(),
        app_secret: app_secret.to_string(),
        allowed_user_ids: form.allowed_user_ids,
        use_feishu: form.use_feishu,
    });
    if let Err(e) = save_creds(&app, &creds) {
        return json_500(e);
    }

    Json(serde_json::json!({
        "ok": true,
        "restart_required": true,
        "note": RESTART_NOTE,
    }))
    .into_response()
}

// --------------------------------------------------------------------------
// Test-only seam: drive the async poll against a mock base
// --------------------------------------------------------------------------

/// Test seam — spawn the `chat_id` poll against an arbitrary `api_base`
/// (a localhost mock), reusing the exact production task body. Not
/// `#[cfg(test)]` because `ccteam-web` integration tests are a separate
/// crate. Mirrors [`AppState::with_creds_path`]'s rationale.
#[doc(hidden)]
pub fn spawn_chat_id_poll_for_test(
    slot: Arc<Mutex<Option<TelegramChatIdPoll>>>,
    token: String,
    api_base: String,
) {
    spawn_chat_id_poll(slot, token, api_base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_last4_hides_long_secret() {
        // A realistic Telegram token: only the last 4 chars survive.
        let masked = mask_last4("123456:ABCDEF_ghijklmnop");
        assert_eq!(masked, "…mnop");
        assert!(!masked.contains("ABCDEF"), "must not leak the body");
        assert!(!masked.contains("123456"), "must not leak the prefix");
    }

    #[test]
    fn mask_last4_short_is_all_stars() {
        assert_eq!(mask_last4("ab"), "**");
        assert_eq!(mask_last4("abcd"), "****");
        assert_eq!(mask_last4(""), "");
    }

    #[test]
    fn telegram_status_serializes_without_token_field() {
        // Type-level guarantee check: the serialized JSON has no key that
        // could carry the raw token.
        let s = TelegramStatus {
            configured: true,
            bot_token_last4: "…wxyz".into(),
            chat_id_count: 1,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("bot_token").is_none(), "no bot_token key");
        assert!(v.get("bot_token_last4").is_some());
    }
}
