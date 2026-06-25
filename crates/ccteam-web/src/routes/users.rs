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
    extract::{Path, State},
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
    let path = app.paths.tenants_json();
    let mut reg = TenantRegistry::load(&path);
    let tenant = reg.add(&form.handle);
    if let Err(err) = reg.save(&path) {
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
    let reg = TenantRegistry::load(&app.paths.tenants_json());
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
    let reg = TenantRegistry::load(&app.paths.tenants_json());
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
    let path = app.paths.tenants_json();
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

/// `PUT /api/v1/me/im` (+ admin `/users/{id}/im`) body — v0.8.20 F2. REPLACE
/// semantics: the body is the tenant's FULL desired per-user IM config; a
/// platform left absent/null is cleared. The Telegram token is `getMe`-validated
/// (against the SAME base as the global config route) before anything is written.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PutTenantImForm {
    /// The tenant's own Telegram bot token. Absent / empty → no Telegram bot.
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
    /// The tenant's own Lark/Feishu app. Absent / null → no Lark bot.
    #[serde(default)]
    pub lark: Option<LarkImForm>,
}

/// Lark app credentials in a [`PutTenantImForm`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct LarkImForm {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_true_im")]
    pub use_feishu: bool,
}

fn default_true_im() -> bool {
    true
}

/// Validate + REPLACE a tenant's per-user IM creds, then persist. Shared by the
/// self-serve (`/me/im`) and admin (`/users/{id}/im`) handlers.
async fn apply_tenant_im(app: &AppState, tenant_id: &str, form: PutTenantImForm) -> Response {
    // Validate Telegram against `getMe` BEFORE touching disk (reuse the same
    // onboarding validator + base the global config route uses).
    let telegram = match form
        .telegram_bot_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(tok) => match ccteam_im::onboarding::telegram_validate_token_with_base(
            tok,
            &super::im_config::telegram_api_base(),
        )
        .await
        {
            Ok(_username) => Some(TenantTelegram {
                bot_token: tok.to_string(),
                allowed_chat_ids: Vec::new(),
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
    };
    let lark = form.lark.map(|l| TenantLark {
        app_id: l.app_id,
        app_secret: l.app_secret,
        allowed_user_ids: Vec::new(),
        use_feishu: l.use_feishu,
    });
    let has_telegram = telegram.is_some();
    let has_lark = lark.is_some();

    let path = app.paths.tenants_json();
    let mut reg = TenantRegistry::load(&path);
    if reg.by_id(tenant_id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown tenant: {tenant_id}")})),
        )
            .into_response();
    }
    reg.set_telegram(tenant_id, telegram);
    reg.set_lark(tenant_id, lark);
    if let Err(err) = reg.save(&path) {
        tracing::error!(%err, "PUT tenant im: registry save failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    Json(json!({
        "ok": true,
        "telegram": has_telegram,
        "lark": has_lark,
        "restart_required": true,
        "note": "saved; takes effect once the daemon (re)starts this tenant's bot listener",
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
