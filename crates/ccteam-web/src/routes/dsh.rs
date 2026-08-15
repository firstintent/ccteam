//! DSH web companion lifecycle REST endpoints.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Extension, Json,
};

use crate::auth::Identity;
use crate::dsh_web::DshStatusResponse;
use crate::state::AppState;

/// `GET /api/v1/dsh/status` — status for this authenticated identity's DSH
/// web instance. When the daemon was started with `--dsh-web-bind off`, the
/// response still returns 200 with `state: "disabled"` and no companion port.
#[utoipa::path(
    get,
    path = "/api/v1/dsh/status",
    tag = "dsh",
    responses((status = 200, description = "DSH web instance status for the caller", body = DshStatusResponse)),
)]
pub(crate) async fn handle_dsh_status(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> impl IntoResponse {
    Json(app.dsh_web.status_for(&identity).await)
}

/// `POST /api/v1/dsh/start` — idempotently start or attach this identity's DSH
/// web instance.
#[utoipa::path(
    post,
    path = "/api/v1/dsh/start",
    tag = "dsh",
    responses((status = 200, description = "DSH web instance status after start", body = DshStatusResponse)),
)]
pub(crate) async fn handle_dsh_start(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    Json(app.dsh_web.start_for(&app, &identity).await).into_response()
}

/// `POST /api/v1/dsh/stop` — idempotently stop the owned managed instance.
/// Attached operator instances are detached from ccteam but not killed.
#[utoipa::path(
    post,
    path = "/api/v1/dsh/stop",
    tag = "dsh",
    responses((status = 200, description = "DSH web instance status after stop", body = DshStatusResponse)),
)]
pub(crate) async fn handle_dsh_stop(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    Json(app.dsh_web.stop_for(&identity).await).into_response()
}
