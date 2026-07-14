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
/// Route at which the **vendored** Scalar standalone JS is served (inside the
/// auth gate, same-origin so the auth cookie carries). See [`SCALAR_JS`].
pub const SCALAR_JS_PATH: &str = "/api/docs/scalar-standalone.js";

/// v0.8.7 review-fix (R-M5) — the upstream `@scalar/api-reference` standalone
/// build, **vendored** into the binary instead of pulled from
/// `cdn.jsdelivr.net` at page load. ccteam's product target is a
/// self-hosted / air-gapped daemon (LAN NAS, firewalled box); a third-party
/// CDN `<script>` made `/api/docs` blank offline AND was an unpinned
/// supply-chain + CSP liability. The file is committed under
/// `crates/ccteam-web/assets/scalar-standalone.js` and served from
/// [`SCALAR_JS_PATH`]; [`scalar_docs_html`] points the page `<script src>` at
/// that local route, so the docs UI has **zero external hosts**.
///
/// Pinned version: **`@scalar/api-reference@1.58.0`** (`dist/browser/
/// standalone.js`). Refresh = chore: re-fetch the pinned tarball build and
/// overwrite the vendored file (mirrors the `workflow_templates/` vendoring
/// pattern). The standalone build
/// auto-bootstraps from the `<script id="api-reference" type="application/
/// json">` spec block on load (`window.Scalar` self-init).
pub const SCALAR_JS: &str = include_str!("../../assets/scalar-standalone.js");

/// The pinned upstream version of the vendored Scalar build (for the
/// `ccteam doctor` / refresh chore + a test assertion).
pub const SCALAR_VERSION: &str = "1.58.0";

/// Build the self-hosted Scalar docs HTML for [`DOCS_PATH`]. Same structure
/// as utoipa-scalar's default template (a `<script id="api-reference"
/// type="application/json">$spec</script>` block the standalone build
/// auto-initializes from) EXCEPT the loader `<script src>` points at the
/// local [`SCALAR_JS_PATH`] route — never an external CDN. The `$spec`
/// placeholder is substituted by `Scalar::to_html` at serve time.
fn scalar_docs_html() -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
    <title>ccteam API reference</title>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
</head>
<body>
<script id="api-reference" type="application/json">
    $spec
</script>
<script src="{SCALAR_JS_PATH}"></script>
</body>
</html>
"#
    )
}

/// `GET {SCALAR_JS_PATH}` — serve the vendored Scalar standalone JS with a
/// long-lived immutable cache (the bytes change only on a version bump). No
/// external fetch; fully offline.
async fn serve_scalar_js() -> impl IntoResponse {
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/javascript; charset=utf-8"),
            ),
            (
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        SCALAR_JS,
    )
}

/// Top-level OpenAPI metadata. Operations are contributed by the
/// per-handler `#[utoipa::path]` registrations in [`api_v1_router`]
/// (`nest`/`merge` collects them), NOT by a static `paths(...)` list — so
/// this derive only carries the info block + tags.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "ccteam resource API",
        description = "The `/api/v1` resource surface: capabilities, projects, roles, \
                       sessions (+ turn / events / stop), and workflow panels. \
                       Auth: the same web-token gate as every other `/api/v1` route \
                       (`Authorization: Bearer ccteam:<hex>` or the `ccteam_token` cookie). \
                       Also available (not REST / not in this spec): `POST /mcp` — \
                       streamable HTTP MCP (admin bearer always required).",
        version = env!("CARGO_PKG_VERSION"),
    ),
    tags(
        (name = "capabilities", description = "Harness vendor probe"),
        (name = "hosts", description = "Host registry + agent report (local + satellites: join-token/join; reverse control channel + exec dial-back ride WS at /hosts/channel and /hosts/exec/{nonce}; register-mcp; admin-only list)"),
        (name = "users", description = "档1 per-user web tenant management (admin-gated; web-first, no CLI — mint personal links, list, delete)"),
        (name = "status", description = "Daemon-wide status snapshot (health · sessions live/idle · 24h cost · budget cap · per-session cost)"),
        (name = "projects", description = "Project lifecycle + detail"),
        (name = "roles", description = "Project-scoped agent roles (`.claude/agents/<role>.md`)"),
        (name = "sessions", description = "Live gateway sessions (spawn / turn / events / stop)"),
        (name = "workflow", description = "Workflow dashboard panels (artifacts / cost / jobs)"),
        (name = "auth", description = "Web-token introspection"),
        (name = "config", description = "IM credential configuration (masked read; never echoes secrets)"),
        (name = "marketplace", description = "ccteam-hub plugin catalog (browse / body preview / per-project install)"),
        (name = "agents", description = "v0.9.0 W4 — team visualization: cross-session graph snapshot + global SSE"),
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
        // v0.8.18 柱1 — host-keyed agent report (list / detail / register-mcp)
        .routes(routes!(super::hosts::handle_hosts))
        .routes(routes!(super::hosts::handle_host_detail))
        .routes(routes!(super::hosts::handle_register_mcp))
        // v0.8.24 Track D — multi-host join / mint. GET (read newest valid)
        // + POST (mint) share `/hosts/join-token`. (The satellite keepalive
        // is no longer HTTP: reports ride the reverse `ccteam-host.v1` WS
        // control channel, mounted as a plain route in `api_v1_router`.)
        .routes(routes!(
            super::hosts::handle_mint_join_token,
            super::hosts::handle_get_join_token
        ))
        .routes(routes!(super::hosts::handle_host_join))
        // v0.8.18 档1 — per-user web tenant management (admin-gated)
        .routes(routes!(
            super::users::handle_create_user,
            super::users::handle_list_users
        ))
        .routes(routes!(super::users::handle_delete_user))
        // v0.8.20 F3 — admin re-reveals a tenant's personal login link.
        .routes(routes!(super::users::handle_user_link))
        // v0.8.20 F2 — per-user IM bot config (self-serve `/me/im` + admin).
        .routes(routes!(super::users::handle_put_me_im))
        .routes(routes!(
            super::users::handle_get_me_lark_open_id_candidates,
            super::users::handle_put_me_lark_allowed_users
        ))
        .routes(routes!(super::users::handle_put_user_im))
        // v0.8.9 Phase 4 — daemon-wide status aggregate (cost pill + Status view)
        .routes(routes!(super::status::handle_status))
        .routes(routes!(super::api_v1::handle_me))
        // v0.8.24 — admin self-serve web-token rotation (live, atomic).
        .routes(routes!(super::api_v1::handle_reset_token))
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
        // v0.8.24 F1.12 — project third-party MCP servers (GET list shares
        // the path with the admin-only POST register).
        .routes(routes!(
            super::mcp_servers::handle_list_mcp_servers,
            super::mcp_servers::handle_register_mcp_server
        ))
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
        .routes(routes!(super::compare::handle_compare))
        .routes(routes!(super::compare::handle_compare_history))
        .routes(routes!(super::evolution::handle_evolution))
        // v0.8.22 P1 — GET history + PATCH rename share `/api/v1/sessions/{sid}`.
        .routes(routes!(
            super::sessions_api::handle_session_history,
            super::sessions_api::handle_patch_session
        ))
        .routes(routes!(super::sessions_api::handle_session_status))
        .routes(routes!(super::sessions_api::handle_session_turn))
        // v0.8.7 review-fix (R-H1) — token-resolve for the web HITL approve/deny
        // path (same pending machinery as an IM click, NOT a turn).
        .routes(routes!(super::sessions_api::handle_session_resolve))
        .routes(routes!(super::sessions_api::handle_session_events))
        .routes(routes!(super::sessions_api::handle_session_stop))
        // v0.8.19 — interrupt the running turn WITHOUT destroying the session
        // (the non-destructive twin of /stop; keeps context for a /model switch).
        .routes(routes!(super::sessions_api::handle_session_interrupt))
        // v0.8.21 — history list, resume stopped session, external discover, import.
        .routes(routes!(super::sessions_api::handle_session_history_list))
        .routes(routes!(super::sessions_api::handle_session_resume))
        .routes(routes!(super::sessions_api::handle_external_sessions))
        .routes(routes!(super::sessions_api::handle_import_session))
        // v0.8.8 F4 — IM credential config (masked read + validate-before-persist
        // PUTs + async telegram chat_id capture). All inside the web-token gate.
        .routes(routes!(super::im_config::handle_get_im_config))
        .routes(routes!(super::im_config::handle_put_telegram))
        .routes(routes!(super::im_config::handle_telegram_chat_id_start))
        .routes(routes!(super::im_config::handle_telegram_chat_id_poll))
        .routes(routes!(super::im_config::handle_put_lark))
        // v0.8.9 Phase 2 — ccteam-hub plugin marketplace: global catalog +
        // body preview, plus per-project decorated catalog + install.
        .routes(routes!(super::marketplace::handle_marketplace))
        .routes(routes!(super::marketplace::handle_marketplace_body))
        .routes(routes!(super::marketplace::handle_project_marketplace))
        .routes(routes!(super::marketplace::handle_project_marketplace_install))
        // v0.9.0 W4 — team visualization graph snapshot + global SSE.
        .routes(routes!(super::agents::handle_agents_graph))
        .routes(routes!(super::agents::handle_agents_events))
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
        // v0.9.0 reverse-connection — WS endpoints (not OpenAPI-documented;
        // the shared auth layer still wraps them: agent-token bearer →
        // identity `host:<id>`).
        .route(
            "/api/v1/hosts/channel",
            axum::routing::get(super::hosts::handle_host_channel),
        )
        .route(
            "/api/v1/hosts/exec/{nonce}",
            axum::routing::get(super::hosts::handle_host_exec_dialback),
        )
        .route(OPENAPI_JSON_PATH, axum::routing::get(serve_openapi_json))
        // v0.8.7 review-fix (R-M5) — serve the SELF-HOSTED Scalar UI: a custom
        // HTML page whose loader `<script src>` points at the vendored JS route
        // below, NOT `cdn.jsdelivr.net`. Works offline / air-gapped.
        .route(SCALAR_JS_PATH, axum::routing::get(serve_scalar_js))
        .merge(Scalar::with_url(DOCS_PATH, api.clone()).custom_html(scalar_docs_html()))
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
