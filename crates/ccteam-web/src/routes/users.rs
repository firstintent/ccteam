//! v0.8.18 档1 — per-user web tenant management (web-first user CRUD).
//!
//! - `POST   /api/v1/users`        (admin) → mint a tenant + return its personal link
//! - `GET    /api/v1/users`        (admin) → list tenants (never the tokens)
//! - `DELETE /api/v1/users/{id}`   (admin) → remove a tenant
//!
//! **Admin-gated**: each handler requires the request's [`Identity`] (injected
//! by [`crate::auth::auth_layer`]) to be the admin/owner (the bootstrap web
//! token). The runtime user-management WRITE surface lives here on web/REST —
//! there is deliberately NO `ccteam user` CLI (owner decision: the CLI stays
//! bootstrap-only; web/IM/REST own runtime writes).
//!
//! The per-user **token** is returned ONLY once, at create time, inside the
//! personal link — it is never re-listed (the admin copies it then). Merged
//! into the `/api/v1` [`OpenApiRouter`] so the web-token gate applies.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_core::tenants::{Tenant, TenantLark, TenantRegistry, TenantTelegram};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::auth::{deny_non_admin, Identity, TOKEN_PREFIX};
use crate::state::AppState;

/// One tenant as the API exposes it — **never** carries the web token (that is
/// returned once, at create time, in the personal link).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TenantView {
    pub id: String,
    pub handle: String,
    /// Linked IM chat (`"channel:chat_id"`), if any — the same person's IM.
    pub linked_chat: Option<String>,
    pub created_at: String,
}

impl From<&Tenant> for TenantView {
    fn from(t: &Tenant) -> Self {
        Self {
            id: t.id.clone(),
            handle: t.handle.clone(),
            linked_chat: t.linked_chat.clone(),
            created_at: t.created_at.to_rfc3339(),
        }
    }
}

/// `POST /api/v1/users` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateUserForm {
    pub handle: String,
}

/// `POST /api/v1/users` response — the new tenant + its **one-time** personal
/// link (`?token=ccteam:<hex>`). The token is not stored anywhere the admin
/// can re-read, so the UI must surface this link immediately.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateUserResponse {
    pub tenant: TenantView,
    /// Personal entry link — a relative path (`/?token=ccteam:<hex>`) the user
    /// opens on the web console host to sign in as this tenant.
    pub personal_link: String,
}

/// `POST /api/v1/users` — mint a tenant + return its personal link (admin only).
#[utoipa::path(
    post,
    path = "/api/v1/users",
    tag = "users",
    request_body(content = CreateUserForm, description = "New user `{handle}`"),
    responses(
        (status = 201, description = "Created; `{tenant, personal_link}` (token shown once)", body = CreateUserResponse),
        (status = 400, description = "Empty handle"),
        (status = 403, description = "Not the admin/owner"),
        (status = 500, description = "Registry write failed"),
    ),
)]
pub(crate) async fn handle_create_user(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(form): Json<CreateUserForm>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    if form.handle.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "handle must not be empty"})),
        )
            .into_response();
    }
    let path = app.paths.users_dir();
    let mut reg = TenantRegistry::load(&path);
    let tenant = reg.add(&form.handle);
    // Only the new tenant's file — a full sweep here would delete anyone else
    // created since this registry was loaded.
    if let Err(err) = reg.save_one(&path, &tenant.id) {
        tracing::error!(%err, "POST /api/v1/users: registry save failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    let personal_link = format!("/?token={TOKEN_PREFIX}{}", tenant.web_token);
    (
        StatusCode::CREATED,
        Json(CreateUserResponse {
            tenant: TenantView::from(&tenant),
            personal_link,
        }),
    )
        .into_response()
}

/// `GET /api/v1/users` — list tenants (admin only; never returns tokens).
#[utoipa::path(
    get,
    path = "/api/v1/users",
    tag = "users",
    responses(
        (status = 200, description = "Tenants `[{id, handle, linked_chat, created_at}]`", body = Vec<TenantView>),
        (status = 403, description = "Not the admin/owner"),
    ),
)]
pub(crate) async fn handle_list_users(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let reg = TenantRegistry::load(&app.paths.users_dir());
    let views: Vec<TenantView> = reg.list().iter().map(TenantView::from).collect();
    Json(views).into_response()
}

/// `GET /api/v1/users/{id}/link` response — a tenant's personal entry link,
/// re-derivable by the admin at any time.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserLinkResponse {
    pub id: String,
    pub handle: String,
    /// Personal entry link (`/?token=ccteam:<hex>`) the tenant opens to sign in.
    pub personal_link: String,
}

/// `GET /api/v1/users/{id}/link` — re-reveal a tenant's personal login link
/// (admin only). v0.8.20 F3 relaxes 档1's "list never returns the token" FOR
/// THE ADMIN: the owner can re-copy any tenant's link (e.g. to re-send it),
/// not only at create time. Tenants still never see others' tokens — this is a
/// SEPARATE admin-gated route, so the list (`GET /api/v1/users`) keeps stripping
/// the token. 404 if the tenant is unknown.
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/link",
    tag = "users",
    params(("id" = String, Path, description = "Tenant id")),
    responses(
        (status = 200, description = "The tenant's personal login link", body = UserLinkResponse),
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown tenant"),
    ),
)]
pub(crate) async fn handle_user_link(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let reg = TenantRegistry::load(&app.paths.users_dir());
    match reg.by_id(&id) {
        Some(t) => Json(UserLinkResponse {
            id: t.id.clone(),
            handle: t.handle.clone(),
            personal_link: format!("/?token={TOKEN_PREFIX}{}", t.web_token),
        })
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown tenant: {id}")})),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/users/{id}` — remove a tenant (admin only). 404 if unknown.
#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}",
    tag = "users",
    params(("id" = String, Path, description = "Tenant id")),
    responses(
        (status = 200, description = "Removed `{removed:true}`", body = serde_json::Value),
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown tenant"),
        (status = 500, description = "Registry write failed"),
    ),
)]
pub(crate) async fn handle_delete_user(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let path = app.paths.users_dir();
    let mut reg = TenantRegistry::load(&path);
    if !reg.remove(&id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown tenant: {id}")})),
        )
            .into_response();
    }
    if let Err(err) = reg.save(&path) {
        tracing::error!(%err, "DELETE /api/v1/users: registry save failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    Json(json!({"removed": true})).into_response()
}

/// `PUT /api/v1/me/im` (+ admin `/users/{id}/im`) body — v0.8.20 F2.
///
/// Per-platform PATCH semantics: **absent = leave that platform alone**,
/// explicit `null` = clear it. It used to be whole-body REPLACE, which lost
/// data every time: the form never pre-fills a secret (they are write-only), so
/// a tenant re-saving a new Telegram token submitted no `lark` key and silently
/// destroyed their Lark app — with no way to restore it short of re-entering an
/// app secret the UI can never show them. Clearing stays possible, it just has
/// to be said out loud.
///
/// The Telegram token is `getMe`-validated (against the SAME base as the global
/// config route) before anything is written.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PutTenantImForm {
    /// The tenant's own Telegram bot token. Absent → unchanged; `null` or
    /// empty → no Telegram bot.
    #[serde(default, deserialize_with = "explicit_option")]
    #[schema(value_type = Option<String>)]
    pub telegram_bot_token: Option<Option<String>>,
    /// The tenant's own Lark/Feishu app. Absent → unchanged; `null` → no Lark
    /// bot.
    #[serde(default, deserialize_with = "explicit_option")]
    #[schema(value_type = Option<LarkImForm>)]
    pub lark: Option<Option<LarkImForm>>,
}

/// Deserialize a field so an ABSENT key and an explicit `null` stay
/// distinguishable: `None` = the caller said nothing, `Some(None)` = the caller
/// said "clear it". `#[serde(default)]` supplies the absent case.
fn explicit_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// Lark app credentials in a [`PutTenantImForm`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct LarkImForm {
    pub app_id: String,
    pub app_secret: String,
    /// `open_id`s (`ou_...`) allowed to drive this tenant's Lark/Feishu bot.
    /// Empty keeps the bot fail-closed, but the web setup helper can still
    /// discover candidates from rejected messages.
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
    #[serde(default = "default_true_im")]
    pub use_feishu: bool,
}

fn default_true_im() -> bool {
    true
}

fn normalize_lark_user_ids(ids: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = ids
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Validate + REPLACE a tenant's per-user IM creds, then persist. Shared by the
/// self-serve (`/me/im`) and admin (`/users/{id}/im`) handlers.
async fn apply_tenant_im(app: &AppState, tenant_id: &str, form: PutTenantImForm) -> Response {
    let path = app.paths.users_dir();
    let mut reg = TenantRegistry::load(&path);
    let Some(current) = reg.by_id(tenant_id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown tenant: {tenant_id}")})),
        )
            .into_response();
    };

    // Validate Telegram against `getMe` BEFORE touching disk (reuse the same
    // onboarding validator + base the global config route uses).
    let telegram = match form.telegram_bot_token {
        // Absent → this save is not about Telegram.
        None => current.telegram.clone(),
        Some(raw) => match raw.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(tok) => match ccteam_im::onboarding::telegram_validate_token_with_base(
                tok,
                &super::im_config::telegram_api_base(),
            )
            .await
            {
                Ok(_username) => Some(TenantTelegram {
                    bot_token: tok.to_string(),
                    // Keep the chat binding across a token change: a Telegram
                    // `chat_id` identifies the PERSON, not the bot, so making
                    // them re-bind after every save is pure friction. Cleared
                    // only by clearing the bot itself.
                    allowed_chat_ids: current
                        .telegram
                        .as_ref()
                        .map(|t| t.allowed_chat_ids.clone())
                        .unwrap_or_default(),
                }),
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("Telegram token rejected: {err}")})),
                    )
                        .into_response()
                }
            },
            None => None,
        },
    };
    let lark = match form.lark {
        None => current.lark.clone(),
        Some(l) => l.map(|l| TenantLark {
            app_id: l.app_id,
            app_secret: l.app_secret,
            allowed_user_ids: normalize_lark_user_ids(l.allowed_user_ids),
            use_feishu: l.use_feishu,
        }),
    };
    let has_telegram = telegram.is_some();
    let has_lark = lark.is_some();
    // An unbound Telegram bot answers nobody (fail-closed, like Lark) — say so
    // in the response instead of letting the tenant discover it as silence.
    let telegram_unbound = telegram
        .as_ref()
        .is_some_and(|t| t.allowed_chat_ids.is_empty());

    reg.set_telegram(tenant_id, telegram);
    reg.set_lark(tenant_id, lark);
    if let Err(err) = reg.save_one(&path, tenant_id) {
        tracing::error!(%err, "PUT tenant im: registry save failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    // v0.8.20 F2 — hot-reload: nudge the daemon to (re)start this tenant's bot
    // listener now (the reload reads tenants.json + diffs the per-tenant bots, so
    // it picks up exactly this change). Best-effort: `false` when the web runs
    // standalone (no daemon gateway handle) — then the bot starts on the next
    // daemon reload/restart.
    let reloaded = match app.gateway.as_ref() {
        Some(gw) => gw.lock().await.request_im_reload(),
        None => false,
    };
    Json(json!({
        "ok": true,
        "telegram": has_telegram,
        "lark": has_lark,
        "telegram_unbound": telegram_unbound,
        "reloaded": reloaded,
        "note": if reloaded {
            "saved; your bot listener is (re)starting now"
        } else {
            "saved; takes effect on the next daemon reload/restart"
        },
    }))
    .into_response()
}

/// `PUT /api/v1/me/im` — the caller sets its OWN per-user IM bot (self-serve).
/// Tenants only: the admin/owner's bot is the global one (`/api/v1/config/im`),
/// so an admin caller is 400'd here.
#[utoipa::path(
    put,
    path = "/api/v1/me/im",
    tag = "users",
    request_body(content = PutTenantImForm, description = "The caller's full per-user IM config (replace)"),
    responses(
        (status = 200, description = "Validated + persisted; `{ok, telegram, lark, restart_required, note}`", body = serde_json::Value),
        (status = 400, description = "Admin caller (uses /config/im) / Telegram token rejected"),
        (status = 404, description = "Caller is not a registered tenant"),
        (status = 500, description = "Registry write failed"),
    ),
)]
pub(crate) async fn handle_put_me_im(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(form): Json<PutTenantImForm>,
) -> Response {
    if identity.is_admin {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "the owner's bot is the global bot — set it via /api/v1/config/im"})),
        )
            .into_response();
    }
    apply_tenant_im(&app, &identity.id, form).await
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LarkAllowedUsersForm {
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CandidateQuery {
    #[serde(default)]
    pub since: Option<u64>,
}

/// `PUT /api/v1/me/im/telegram/allowed-chats` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramAllowedChatsForm {
    #[serde(default)]
    pub allowed_chat_ids: Vec<String>,
}

/// One id this tenant's bot saw and rejected — the thing to put in the
/// allowlist. Shared by the Lark and Telegram discovery endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct SenderCandidate {
    /// The id to allow: a Lark `open_id` (`ou_...`) or a Telegram `chat_id`.
    pub sender_id: String,
    /// Legacy alias of [`Self::sender_id`] kept so the Lark response shape is
    /// unchanged for its existing client.
    pub open_id: String,
    pub seen_at: u64,
    pub message_id: String,
    pub chat_id_last4: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SenderCandidatesResponse {
    pub candidates: Vec<SenderCandidate>,
}

fn last4(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

fn probe_path(app: &AppState) -> std::path::PathBuf {
    app.paths.im_state_dir().join("rejected-senders.jsonl")
}

/// Read the newest rejected senders one channel saw, newest first, deduped by
/// sender and capped. One reader for every platform: the daemon writes a single
/// `RejectedSenderProbe` shape, so Lark `open_id` discovery and Telegram
/// `chat_id` discovery differ only in the `channel` key passed in.
fn read_sender_candidates(
    path: &std::path::Path,
    channel: &str,
    since: Option<u64>,
) -> Vec<SenderCandidate> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut by_sender: std::collections::HashMap<String, SenderCandidate> =
        std::collections::HashMap::new();
    for line in raw.lines() {
        let Ok(probe) = serde_json::from_str::<ccteam_im::transport::RejectedSenderProbe>(line)
        else {
            continue;
        };
        if probe.channel != channel {
            continue;
        }
        if since.is_some_and(|min| probe.timestamp < min) {
            continue;
        }
        let candidate = SenderCandidate {
            sender_id: probe.sender_id.clone(),
            open_id: probe.sender_id.clone(),
            seen_at: probe.timestamp,
            message_id: probe.message_id,
            chat_id_last4: last4(&probe.chat_id),
        };
        match by_sender.get(&probe.sender_id) {
            Some(existing) if existing.seen_at >= candidate.seen_at => {}
            _ => {
                by_sender.insert(probe.sender_id, candidate);
            }
        }
    }
    let mut out: Vec<SenderCandidate> = by_sender.into_values().collect();
    out.sort_by(|a, b| {
        b.seen_at
            .cmp(&a.seen_at)
            .then(a.sender_id.cmp(&b.sender_id))
    });
    out.truncate(10);
    out
}

/// `GET /api/v1/me/im/lark/open-id-candidates` — tenants poll this while
/// setting up Lark/Feishu. It returns recent `open_id`s that this tenant's bot
/// saw but rejected because they were not in `allowed_user_ids`; those messages
/// were not routed to any agent.
#[utoipa::path(
    get,
    path = "/api/v1/me/im/lark/open-id-candidates",
    tag = "users",
    params(("since" = Option<u64>, Query, description = "Only candidates at/after this Unix timestamp")),
    responses(
        (status = 200, description = "Recent rejected Lark sender open_ids for this tenant", body = SenderCandidatesResponse),
        (status = 400, description = "Admin caller (uses global config)"),
    ),
)]
pub(crate) async fn handle_get_me_lark_open_id_candidates(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<CandidateQuery>,
) -> Response {
    if identity.is_admin {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "the owner's bot is the global bot — tenant open_id capture is under /api/v1/me"})),
        )
            .into_response();
    }
    let channel = format!("lark@{}", identity.id);
    Json(SenderCandidatesResponse {
        candidates: read_sender_candidates(&probe_path(&app), &channel, query.since),
    })
    .into_response()
}

/// `PUT /api/v1/me/im/lark/allowed-users` — update only the allowlist of an
/// already-configured tenant Lark app. This lets the web setup flow save the
/// captured `ou_...` without asking the user to re-enter the app secret.
#[utoipa::path(
    put,
    path = "/api/v1/me/im/lark/allowed-users",
    tag = "users",
    request_body(content = LarkAllowedUsersForm, description = "Full desired allowed_user_ids list"),
    responses(
        (status = 200, description = "Allowlist updated; `{ok, lark, reloaded, note}`", body = serde_json::Value),
        (status = 400, description = "Admin caller / no Lark app configured"),
        (status = 404, description = "Caller is not a registered tenant"),
        (status = 500, description = "Registry write failed"),
    ),
)]
pub(crate) async fn handle_put_me_lark_allowed_users(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(form): Json<LarkAllowedUsersForm>,
) -> Response {
    if identity.is_admin {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "the owner's bot is the global bot — set it via /api/v1/config/im"})),
        )
            .into_response();
    }
    let path = app.paths.users_dir();
    let mut reg = TenantRegistry::load(&path);
    let Some(tenant) = reg.by_id(&identity.id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown tenant: {}", identity.id)})),
        )
            .into_response();
    };
    let Some(mut lark) = tenant.lark else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error": "no Lark/Feishu app configured; save App ID and App Secret first"}),
            ),
        )
            .into_response();
    };
    lark.allowed_user_ids = normalize_lark_user_ids(form.allowed_user_ids);
    let allow_count = lark.allowed_user_ids.len();
    reg.set_lark(&identity.id, Some(lark));
    if let Err(err) = reg.save_one(&path, &identity.id) {
        tracing::error!(%err, "PUT /api/v1/me/im/lark/allowed-users: registry save failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    let reloaded = match app.gateway.as_ref() {
        Some(gw) => gw.lock().await.request_im_reload(),
        None => false,
    };
    Json(json!({
        "ok": true,
        "lark": true,
        "allowed_user_id_count": allow_count,
        "reloaded": reloaded,
        "note": if reloaded {
            "saved; your Lark bot listener is (re)starting now"
        } else {
            "saved; takes effect on the next daemon reload/restart"
        },
    }))
    .into_response()
}

/// `GET /api/v1/me/im/telegram/chat-id-candidates` — tenants poll this while
/// binding their own Telegram bot. A per-tenant bot is fail-closed, so until a
/// chat is allowlisted every inbound message is dropped and recorded here; the
/// tenant DMs their bot, sees their own `chat_id` appear, and allows it.
///
/// This mirrors the Lark `open_id` flow rather than the admin's `getUpdates`
/// capture on purpose: the live channel is already the one seeing the message,
/// so there is no second poller racing it for the same updates.
#[utoipa::path(
    get,
    path = "/api/v1/me/im/telegram/chat-id-candidates",
    tag = "users",
    params(("since" = Option<u64>, Query, description = "Only candidates at/after this Unix timestamp")),
    responses(
        (status = 200, description = "Recent rejected Telegram chat ids for this tenant", body = SenderCandidatesResponse),
        (status = 400, description = "Admin caller (uses global config)"),
    ),
)]
pub(crate) async fn handle_get_me_telegram_chat_id_candidates(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<CandidateQuery>,
) -> Response {
    if identity.is_admin {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "the owner's bot is the global bot — use /api/v1/config/im/telegram/chat-id"})),
        )
            .into_response();
    }
    let channel = format!("telegram@{}", identity.id);
    Json(SenderCandidatesResponse {
        candidates: read_sender_candidates(&probe_path(&app), &channel, query.since),
    })
    .into_response()
}

/// `PUT /api/v1/me/im/telegram/allowed-chats` — set the caller's own Telegram
/// chat allowlist without re-entering the bot token (the token is never echoed,
/// so asking for it again to bind a chat would be a dead end).
#[utoipa::path(
    put,
    path = "/api/v1/me/im/telegram/allowed-chats",
    tag = "users",
    request_body(content = TelegramAllowedChatsForm, description = "Full desired allowed_chat_ids list"),
    responses(
        (status = 200, description = "Allowlist updated; `{ok, telegram, reloaded, note}`", body = serde_json::Value),
        (status = 400, description = "Admin caller / no Telegram bot configured"),
        (status = 404, description = "Caller is not a registered tenant"),
        (status = 500, description = "Registry write failed"),
    ),
)]
pub(crate) async fn handle_put_me_telegram_allowed_chats(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(form): Json<TelegramAllowedChatsForm>,
) -> Response {
    if identity.is_admin {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "the owner's bot is the global bot — set it via /api/v1/config/im"})),
        )
            .into_response();
    }
    let path = app.paths.users_dir();
    let mut reg = TenantRegistry::load(&path);
    let Some(tenant) = reg.by_id(&identity.id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown tenant: {}", identity.id)})),
        )
            .into_response();
    };
    let Some(mut telegram) = tenant.telegram else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "no Telegram bot configured; save the bot token first"})),
        )
            .into_response();
    };
    telegram.allowed_chat_ids = normalize_lark_user_ids(form.allowed_chat_ids);
    let allow_count = telegram.allowed_chat_ids.len();
    reg.set_telegram(&identity.id, Some(telegram));
    if let Err(err) = reg.save_one(&path, &identity.id) {
        tracing::error!(%err, "PUT /api/v1/me/im/telegram/allowed-chats: registry save failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    let reloaded = match app.gateway.as_ref() {
        Some(gw) => gw.lock().await.request_im_reload(),
        None => false,
    };
    Json(json!({
        "ok": true,
        "telegram": true,
        "allowed_chat_id_count": allow_count,
        "reloaded": reloaded,
        "note": if reloaded {
            "saved; your Telegram bot listener is (re)starting now"
        } else {
            "saved; takes effect on the next daemon reload/restart"
        },
    }))
    .into_response()
}

/// `PUT /api/v1/users/{id}/im` — the admin sets a tenant's per-user IM bot.
#[utoipa::path(
    put,
    path = "/api/v1/users/{id}/im",
    tag = "users",
    params(("id" = String, Path, description = "Tenant id")),
    request_body(content = PutTenantImForm, description = "The tenant's full per-user IM config (replace)"),
    responses(
        (status = 200, description = "Validated + persisted", body = serde_json::Value),
        (status = 400, description = "Telegram token rejected"),
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown tenant"),
        (status = 500, description = "Registry write failed"),
    ),
)]
pub(crate) async fn handle_put_user_im(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    Json(form): Json<PutTenantImForm>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    apply_tenant_im(&app, &id, form).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::tenants::TenantRegistry;

    #[test]
    fn deny_non_admin_blocks_tenants_allows_admin() {
        assert!(deny_non_admin(&Identity::admin()).is_none());
        assert!(deny_non_admin(&Identity::tenant("u1".into())).is_some());
    }

    #[test]
    fn tenant_view_never_serializes_the_token() {
        let mut reg = TenantRegistry::default();
        let t = reg.add("alice");
        let v = TenantView::from(&t);
        assert_eq!(v.handle, "alice");
        assert_eq!(v.id, t.id);
        // The view carries NO token (the wire shape must not leak it).
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            !json.contains("token") && !json.contains(&t.web_token),
            "TenantView must not leak the web token: {json}"
        );
    }
}
