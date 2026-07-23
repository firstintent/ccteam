//! v0.8.9 Phase 2 — ccteam-hub plugin marketplace REST surface.
//!
//! The network face of [`ccteam_im::hub`] (the curated plugin catalog read +
//! install backend). Four handlers, all merged into the `/api/v1`
//! [`OpenApiRouter`] (see [`super::openapi`]) so they sit behind the same
//! web-token gate as the rest of the resource API — this module writes **zero**
//! auth code.
//!
//! - `GET  /api/v1/marketplace`                          → global catalog (`HubIndex`)
//! - `GET  /api/v1/marketplace/{id}/body`                → plugin body preview (install-time review)
//! - `GET  /api/v1/projects/{slug}/marketplace`          → catalog decorated with per-project `installed_status`
//! - `POST /api/v1/projects/{slug}/marketplace/install`  → install a plugin into the project
//!
//! ## Hub base (test seam)
//!
//! Every backend call takes a `base` (the hub raw-content root); we always pass
//! [`ccteam_im::hub::hub_base`], which honours the `CCTEAM_HUB_BASE` env
//! override so integration tests point at an in-process fake hub and `cargo
//! test` never touches github.
//!
//! ## Error mapping
//!
//! A failed hub fetch is **not** a server bug — it is an upstream / network /
//! integrity problem. [`hub_error_status`] maps each [`ccteam_im::hub::HubError`]
//! variant to an honest HTTP status (see its doc); the catalog/body/install
//! handlers all route their `Err` through it.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_im::hub::{self, HubError};
use serde::Deserialize;
use utoipa::ToSchema;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

/// Reject an unknown project with a 404 JSON body (mirrors
/// [`super::roles`]'s guard — keyed on the project's `state.json`). Returns
/// `Some(resp)` when the project is unknown so callers can `return` it.
fn reject_unknown_project(
    app: &AppState,
    identity: &crate::auth::Identity,
    slug: &str,
) -> Option<Response> {
    // v0.8.18 档1 — 404 an unknown project AND one the caller can't see: the
    // marketplace is project-scoped (install writes into that project's
    // `.claude`), so it follows the project-ownership ACL.
    if app.paths.project_state(slug).exists()
        && crate::routes::api_v1::can_see_project(app, identity, slug)
    {
        return None;
    }
    Some(
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response(),
    )
}

/// Map a [`HubError`] to its HTTP status. The single mapping point shared by
/// the catalog / body / install handlers:
///
/// - [`HubError::UnknownId`] → `404` — no such plugin in the catalog.
/// - [`HubError::Exists`] → `409` — a file already at the install target
///   (caller can retry with `force`).
/// - [`HubError::UnsupportedType`] → `400` — a recognised but not-yet-installable
///   type (e.g. `workflow`); also covers a bad install stem surfaced as a
///   client error.
/// - [`HubError::ShaMismatch`] / [`HubError::Http`] / [`HubError::BadStatus`] /
///   [`HubError::BadIndex`] / [`HubError::BadModels`] /
///   [`HubError::ModelsCacheShaMismatch`] / [`HubError::EmptyBody`] /
///   [`HubError::TooLarge`]
///   → `502` — the upstream hub failed us (transport, non-success status,
///   malformed/empty/oversized body, or a failed integrity check). Bad Gateway
///   is the honest code: ccteam is healthy, its upstream is not.
/// - [`HubError::BadStem`] → `400` — the install stem (plugin id / `--as`
///   override) sanitizes to an invalid name (client error).
/// - [`HubError::Write`] → `500` — a local disk write / cache read failed (our side).
fn hub_error_status(err: &HubError) -> StatusCode {
    match err {
        HubError::UnknownId(_) => StatusCode::NOT_FOUND,
        HubError::Exists(_) => StatusCode::CONFLICT,
        HubError::UnsupportedType(_) | HubError::BadStem(_) => StatusCode::BAD_REQUEST,
        HubError::ShaMismatch { .. }
        | HubError::Http { .. }
        | HubError::BadStatus { .. }
        | HubError::HostNotAllowed { .. }
        | HubError::BadIndex(_)
        | HubError::BadModels(_)
        | HubError::ModelsCacheShaMismatch { .. }
        // A malformed `type:"plugin"` row (missing marketplace pointer) is bad
        // upstream catalog data, like BadIndex.
        | HubError::InvalidPlugin(_)
        | HubError::EmptyBody(_)
        | HubError::TooLarge { .. } => StatusCode::BAD_GATEWAY,
        HubError::Write(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// `?refresh=true` query for the global / decorated catalog GETs.
#[derive(Debug, Default, Deserialize)]
pub struct CatalogQuery {
    /// When `true`, bypass the `~/.ccteam/hub-cache/` and re-fetch the index
    /// from the hub. Default `false` (offline-friendly cached browse).
    #[serde(default)]
    pub refresh: bool,
}

/// POST install body — `id` (required hub plugin id) + optional `force`
/// (overwrite an existing file at the install target). Accepted as JSON or
/// `application/x-www-form-urlencoded` (the [`FormOrJson`] extractor).
#[derive(Debug, Deserialize, ToSchema)]
pub struct InstallForm {
    /// The hub plugin id to install (the catalog key).
    pub id: String,
    /// Overwrite an existing file at the install target. Default `false`.
    #[serde(default)]
    pub force: Option<bool>,
}

/// `GET /api/v1/marketplace` — the global plugin catalog from the hub.
///
/// Loads (cached, unless `?refresh=true`) the hub `index.json` and returns it
/// verbatim as a [`ccteam_im::hub::HubIndex`]. A fetch / parse failure is a
/// `502` (the upstream hub failed); see [`hub_error_status`].
#[utoipa::path(
    get,
    path = "/api/v1/marketplace",
    tag = "marketplace",
    params(("refresh" = Option<bool>, Query, description = "Bypass the hub cache and re-fetch the index")),
    responses(
        (status = 200, description = "Hub catalog `{version, name, description, generated_at, plugins[]}`", body = serde_json::Value),
        (status = 502, description = "Hub fetch / parse failed"),
    ),
)]
pub(crate) async fn handle_marketplace(
    State(app): State<AppState>,
    Query(q): Query<CatalogQuery>,
) -> Response {
    match hub::load_catalog(&hub::hub_base(), &app.paths, q.refresh).await {
        Ok(index) => Json(index).into_response(),
        Err(err) => {
            tracing::warn!(%err, "marketplace catalog load failed");
            (
                hub_error_status(&err),
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/marketplace/{id}/body` — the plugin body, for install-time
/// review (the operator reads the markdown before installing it).
///
/// Resolves `id` in the (cached) catalog → fetches the body (which the backend
/// **sha256-verifies** against the index) → returns `{id, body}`. Unknown id →
/// `404`; a fetch / integrity failure → `502`.
#[utoipa::path(
    get,
    path = "/api/v1/marketplace/{id}/body",
    tag = "marketplace",
    params(("id" = String, Path, description = "Hub plugin id")),
    responses(
        (status = 200, description = "Plugin body `{id, body}`", body = serde_json::Value),
        (status = 404, description = "Unknown plugin id"),
        (status = 502, description = "Hub fetch / integrity check failed"),
    ),
)]
pub(crate) async fn handle_marketplace_body(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    // Resolve against the cached catalog (no forced refresh — a body preview
    // shouldn't pay a network round-trip for the index too).
    let index = match hub::load_catalog(&hub::hub_base(), &app.paths, false).await {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(%err, "marketplace body: catalog load failed");
            return (
                hub_error_status(&err),
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let Some(plugin) = index.find(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::json!({"error": format!("no plugin `{id}` in the ccteam-hub catalog")}),
            ),
        )
            .into_response();
    };
    match hub::fetch_plugin_body(plugin).await {
        Ok(body) => Json(serde_json::json!({"id": id, "body": body})).into_response(),
        Err(err) => {
            tracing::warn!(%id, %err, "marketplace body fetch failed");
            (
                hub_error_status(&err),
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/projects/{slug}/marketplace` — the catalog decorated, per
/// plugin, with its [`ccteam_im::hub::InstalledStatus`] in the target project.
///
/// `reject_unknown_project` first (404), then load the catalog and attach
/// `installed_status` (computed on-the-fly from the file on disk vs. the
/// index `content_sha`) to each entry. Shape:
/// `{version, name, description, generated_at, plugins:[{...plugin, installed_status}]}`.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/marketplace",
    tag = "marketplace",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("refresh" = Option<bool>, Query, description = "Bypass the hub cache and re-fetch the index"),
    ),
    responses(
        (status = 200, description = "Decorated catalog `{..., plugins:[{...plugin, installed_status}]}`", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 502, description = "Hub fetch / parse failed"),
    ),
)]
pub(crate) async fn handle_project_marketplace(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
    Query(q): Query<CatalogQuery>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &identity, &slug) {
        return resp;
    }
    let index = match hub::load_catalog(&hub::hub_base(), &app.paths, q.refresh).await {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(%slug, %err, "project marketplace catalog load failed");
            return (
                hub_error_status(&err),
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let project_dir = app.paths.project_dir(&slug);
    let library_root = app.paths.skills_dir();
    // Decorate each entry with its on-disk installed status. We serialize the
    // plugin to a JSON object and splice in `installed_status` so the wire
    // shape is `{...plugin, installed_status}` without needing a parallel DTO
    // (and without adding ToSchema to the foreign `HubPlugin` type).
    let plugins: Vec<serde_json::Value> = index
        .plugins
        .iter()
        .map(|plugin| {
            let status = hub::installed_status(&project_dir, &library_root, plugin);
            let mut obj = serde_json::to_value(plugin).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(map) = obj.as_object_mut() {
                map.insert(
                    "installed_status".to_string(),
                    serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
                );
            }
            obj
        })
        .collect();

    Json(serde_json::json!({
        "version": index.version,
        "name": index.name,
        "description": index.description,
        "generated_at": index.generated_at,
        "plugins": plugins,
    }))
    .into_response()
}

/// `POST /api/v1/projects/{slug}/marketplace/install` — install a hub plugin
/// into the project (`.claude/agents/<id>.md` for an agent).
///
/// `reject_unknown_project` (404) → resolve `id` in the catalog (404 unknown
/// plugin) → [`ccteam_im::hub::install_plugin`] (default stem = the plugin id).
/// Maps the outcome via [`hub_error_status`]: Ok → `201 {id,type,path,
/// overwrote}`; `Exists` → `409`; `UnsupportedType`/bad-stem → `400`;
/// integrity/transport → `502`; local write → `500`.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/marketplace/install",
    tag = "marketplace",
    params(("slug" = String, Path, description = "Project slug")),
    request_body(content = InstallForm, description = "Plugin to install (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 201, description = "Installed `{id, type, path, overwrote}`", body = serde_json::Value),
        (status = 400, description = "Unsupported plugin type / bad install stem"),
        (status = 404, description = "Unknown project or plugin id"),
        (status = 409, description = "Already installed (retry with force)"),
        (status = 500, description = "Local write failed"),
        (status = 502, description = "Hub fetch / integrity check failed"),
    ),
)]
pub(crate) async fn handle_project_marketplace_install(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
    FormOrJson(form, mode): FormOrJson<InstallForm>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &identity, &slug) {
        return resp;
    }
    // Resolve the id in the (cached) catalog before any install I/O so an
    // unknown plugin is a clean 404, not a confusing downstream error.
    let index = match hub::load_catalog(&hub::hub_base(), &app.paths, false).await {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(%slug, %err, "install: catalog load failed");
            return install_error(hub_error_status(&err), format!("{err}"), mode);
        }
    };
    let Some(plugin) = index.find(&form.id) else {
        return install_error(
            StatusCode::NOT_FOUND,
            format!("no plugin `{}` in the ccteam-hub catalog", form.id),
            mode,
        );
    };
    let project_dir = app.paths.project_dir(&slug);
    let library_root = app.paths.skills_dir();
    let force = form.force.unwrap_or(false);
    match hub::install_plugin(&project_dir, &library_root, plugin, None, force).await {
        Ok(result) => {
            let body = serde_json::json!({
                "id": result.id,
                "type": result.type_,
                "path": result.path.display().to_string(),
                "overwrote": result.overwrote,
            });
            // Both modes return 201 with the created resource (REST shape;
            // the form path here is API, not htmx, so no redirect).
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Err(err) => {
            tracing::warn!(%slug, id = %form.id, %err, "marketplace install failed");
            install_error(hub_error_status(&err), format!("{err}"), mode)
        }
    }
}

/// Shared install error responder honoring the [`FormOrJson`] mode convention
/// (mirrors `projects::create_error`): form ⇒ plain text, JSON ⇒
/// `{ "ok": false, "error": ... }`.
fn install_error(status: StatusCode, msg: String, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => (status, msg).into_response(),
        InputMode::Json => {
            (status, Json(serde_json::json!({"ok": false, "error": msg}))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_error_status_maps_each_variant() {
        assert_eq!(
            hub_error_status(&HubError::UnknownId("x".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            hub_error_status(&HubError::Exists("x".into())),
            StatusCode::CONFLICT
        );
        assert_eq!(
            hub_error_status(&HubError::UnsupportedType("workflow".into())),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            hub_error_status(&HubError::BadStem(
                "role name `!!!` sanitizes to empty".into()
            )),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            hub_error_status(&HubError::BadIndex("bad".into())),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            hub_error_status(&HubError::EmptyBody("x".into())),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            hub_error_status(&HubError::TooLarge {
                what: "x".into(),
                max: 1
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            hub_error_status(&HubError::BadStatus {
                what: "x".into(),
                status: 404,
                url: "u".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            hub_error_status(&HubError::ShaMismatch {
                id: "x".into(),
                expected: "a".into(),
                actual: "b".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            hub_error_status(&HubError::Write("write hub cache foo: oops".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
