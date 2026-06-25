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
use ccteam_core::tenants::{Tenant, TenantRegistry};
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
