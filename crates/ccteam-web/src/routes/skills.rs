//! User-level global skill-library REST surface.
//!
//! The library is rooted at [`ccteam_core::CcteamPaths::skills_dir`] and is
//! independent of every project tree. This module only lists library entries;
//! attaching one to a turn is handled by `sessions_api` as an absolute path
//! reference and never copies it into the project.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};

use crate::auth::{deny_non_admin, Identity};
use crate::state::AppState;

/// `GET /api/v1/skills`
///
/// Lists the admin's user-level global skill library as
/// `{skills:[{id, description, path}]}`. Nested ids are preserved and sorted
/// by the core scanner. Tenant identities are denied because the library is a
/// daemon-user global surface rather than a tenant-owned resource.
#[utoipa::path(
    get,
    path = "/api/v1/skills",
    tag = "skills",
    responses(
        (status = 200, description = "Global skill library sorted by id", body = serde_json::Value),
        (status = 403, description = "Admin only"),
    ),
)]
pub(crate) async fn handle_list_library_skills(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let skills = ccteam_core::list_library_skills(&app.paths.skills_dir());
    (StatusCode::OK, Json(serde_json::json!({"skills": skills}))).into_response()
}
