//! v0.8.7 W5 (Item E) — OpenAPI auto-docs for the `/api/v1` surface.
//!
//! **Single source of truth (anti-drift).** Every `/api/v1` handler is
//! registered here exactly once through [`utoipa_axum::router::OpenApiRouter`]
//! via the [`utoipa_axum::routes`] macro. That macro reads each handler's
//! `#[utoipa::path(...)]` annotation and registers BOTH the axum route
//! (method + path) AND the matching OpenAPI operation from the same call —
//! so a route cannot exist without a spec entry, and a spec entry cannot
//! exist without a route. [`api_v1_router`] then [`OpenApiRouter::split_for_parts`]s
//! into the live `Router<AppState>` (merged by [`super::stateful_router`])
//! and the [`utoipa::openapi::OpenApi`] document (served at
//! `/api/v1/openapi.json` + rendered by Scalar at `/api/docs`).
//!
//! Co-pathed handlers (different HTTP methods on one path — e.g. `GET` +
//! `POST /api/v1/projects`) are passed together to one [`routes!`] call so
//! they merge into a single route entry, mirroring how the old per-module
//! `Router::route(path, get(a).post(b))` registrations worked.
//!
//! **Auth.** The spec + Scalar UI are mounted on the stateful router, so
//! the existing `auth::auth_layer` web-token gate applies to them exactly
//! like every other `/api/v1` route (DE.3 — consistent, no public spec).

use axum::response::IntoResponse;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

use crate::state::AppState;

/// Route at which the generated spec is served (inside the auth gate).
pub const OPENAPI_JSON_PATH: &str = "/api/v1/openapi.json";
/// Route at which the Scalar interactive UI is served (inside the auth gate).
pub const DOCS_PATH: &str = "/api/docs";

/// Top-level OpenAPI metadata. Operations are contributed by the
/// per-handler `#[utoipa::path]` registrations in [`api_v1_router`]
/// (`nest`/`merge` collects them), NOT by a static `paths(...)` list — so
/// this derive only carries the info block + tags.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "ccteam resource API",
        description = "The `/api/v1` resource surface: capabilities, projects, roles, \
                       sessions (+ turn / events / stop), workflow panels, and teams. \
                       Auth: the same web-token gate as every other `/api/v1` route \
                       (`Authorization: Bearer ccteam:<hex>` or the `ccteam_token` cookie).",
        version = env!("CARGO_PKG_VERSION"),
    ),
    tags(
        (name = "capabilities", description = "Harness vendor probe"),
        (name = "projects", description = "Project lifecycle + detail"),
        (name = "roles", description = "Project-scoped agent roles (`.claude/agents/<role>.md`)"),
        (name = "sessions", description = "Live gateway sessions (spawn / turn / events / stop)"),
        (name = "workflow", description = "Workflow dashboard panels (artifacts / cost / jobs)"),
        (name = "teams", description = "Read-only Anthropic Agent Teams mirror"),
        (name = "auth", description = "Web-token introspection"),
    ),
)]
struct ApiDoc;

/// Build the `/api/v1` [`OpenApiRouter`] with every handler registered
/// once (single source of truth). Split into `(Router, OpenApi)` by
/// [`api_v1_router`].
fn build_api_v1() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        // capabilities
        .routes(routes!(super::capabilities::handle_capabilities))
        // projects — GET list + POST create share `/api/v1/projects`;
        // GET detail + DELETE share `/api/v1/projects/{slug}`.
        .routes(routes!(
            super::api_v1::handle_projects,
            super::projects::handle_create_project
        ))
        .routes(routes!(
            super::api_v1::handle_project,
            super::projects::handle_delete_project
        ))
        .routes(routes!(super::api_v1::handle_session))
        .routes(routes!(super::api_v1::handle_auth_token))
        // workflow panels
        .routes(routes!(super::api_v1::handle_artifact_queue))
        .routes(routes!(super::api_v1::handle_artifact_status))
        .routes(routes!(super::api_v1::handle_cost_history))
        .routes(routes!(super::api_v1::handle_active_sessions))
        .routes(routes!(super::api_v1::handle_job_log))
        .routes(routes!(super::api_v1::handle_active_sessions_aggregate))
        // roles
        .routes(routes!(super::roles::handle_list_roles))
        .routes(routes!(
            super::roles::handle_get_role,
            super::roles::handle_put_role
        ))
        // sessions (gateway spine) — GET list + POST create share the path.
        .routes(routes!(
            super::sessions_api::handle_list_sessions,
            super::sessions_api::handle_create_session
        ))
        .routes(routes!(super::sessions_api::handle_session_history))
        .routes(routes!(super::sessions_api::handle_session_turn))
        // v0.8.7 review-fix (R-H1) — token-resolve for the web HITL approve/deny
        // path (same pending machinery as an IM click, NOT a turn).
        .routes(routes!(super::sessions_api::handle_session_resolve))
        .routes(routes!(super::sessions_api::handle_session_events))
        .routes(routes!(super::sessions_api::handle_session_stop))
        // teams
        .routes(routes!(super::teams_api::handle_list))
        .routes(routes!(super::teams_api::handle_detail))
        .routes(routes!(super::teams_api::handle_tasks))
        .routes(routes!(super::teams_api::handle_inbox))
        .routes(routes!(super::teams_api::handle_definition))
        .routes(routes!(super::teams_sse::handle_team_events))
}

/// The complete `/api/v1` router PLUS its self-documenting endpoints.
///
/// Returns a `Router<AppState>` carrying every `/api/v1` handler (the
/// live surface), the spec at [`OPENAPI_JSON_PATH`], and the Scalar UI at
/// [`DOCS_PATH`]. [`super::stateful_router`] merges this in place of the
/// seven former per-module `.merge(...)` calls, so the auth layer wraps it
/// all uniformly.
pub fn api_v1_router() -> axum::Router<AppState> {
    let (router, api) = build_api_v1().split_for_parts();
    router
        .route(OPENAPI_JSON_PATH, axum::routing::get(serve_openapi_json))
        .merge(Scalar::with_url(DOCS_PATH, api.clone()))
        // Stash the generated spec in an extension-free closure capture so
        // the JSON handler can serve it without rebuilding. `Scalar` already
        // owns a clone for the UI; we keep our own for the raw-JSON route.
        .layer(axum::Extension(SpecHandle(std::sync::Arc::new(api))))
}

/// The generated spec, cloned once at router-build time and shared (cheap
/// `Arc`) with the `/api/v1/openapi.json` handler. Wrapped in a newtype so
/// the axum `Extension` extractor can find it unambiguously.
#[derive(Clone)]
struct SpecHandle(std::sync::Arc<utoipa::openapi::OpenApi>);

/// `GET /api/v1/openapi.json` — serve the generated OpenAPI 3.x document.
async fn serve_openapi_json(
    axum::Extension(spec): axum::Extension<SpecHandle>,
) -> impl IntoResponse {
    axum::Json((*spec.0).clone())
}

/// Test/debug seam: the generated spec on its own (no axum wiring). Used
/// by the route↔spec drift test to assert the operation set without
/// spinning a server.
pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    build_api_v1().split_for_parts().1
}
