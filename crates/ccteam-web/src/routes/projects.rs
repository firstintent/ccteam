//! v0.8.6 W5b ResDisk — project resource lifecycle endpoints.
//!
//! This module owns the **mutating** project resource verbs:
//!
//! - `POST   /api/v1/projects`        → create (bootstrap + register) → 201
//! - `DELETE /api/v1/projects/{slug}` → deregister + stop its sessions → 200
//!
//! The **read** verbs (`GET /api/v1/projects` list, `GET
//! /api/v1/projects/{slug}` detail) are served by the pre-existing
//! [`super::api_v1`] handlers (`handle_projects` → `DashboardRow[]` via
//! `ccteam_core::collect_projects`; `handle_project` → `ProjectSummary`).
//! axum merges this router's `POST` / `DELETE` method handlers onto the
//! same paths as api_v1's `GET`s — different HTTP methods on one path do
//! not collide. We deliberately do **not** re-register the GETs here: the
//! SPA already consumes api_v1's richer shapes, and a second GET handler
//! on the same path would panic at router build time.
//!
//! **DELETE semantics (locked W5b decision)**: deregister only —
//! `config::remove_project(slug)` plus, when a gateway is attached, stop
//! every live session for that project via the spine. It never
//! file-purges the working tree; destructive purge stays the CLI op
//! `ccteam project rm --purge`.
//!
//! Auth: merged into [`super::stateful_router`], so the existing
//! `auth_layer` gate applies.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_core::ProjectEntry;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

/// POST body — `slug` (required), `path` (required, absolute or
/// `~`-relative working-tree dir), `team` (optional, defaults `dev`).
///
/// Wire note: accepted as either `application/json` or
/// `application/x-www-form-urlencoded` (the [`FormOrJson`] extractor).
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProjectForm {
    pub slug: String,
    pub path: String,
    /// Project execution host. Omitted/empty means `local`.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
}

/// 201 response body for a created project.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedProject {
    pub slug: String,
    pub host: String,
    pub path: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ImportProjectForm {
    pub host: String,
    pub remote_slug: String,
    #[serde(default)]
    pub slug: Option<String>,
}

/// `POST /api/v1/projects`
///
/// Mirrors `Gateway::create_project`'s sequence (validate slug →
/// bootstrap at the resolved dir → register in config.yaml) but does
/// **not** require a gateway: project scaffolding is pure disk/config
/// work. When a gateway *is* attached, a later `POST .../sessions` call
/// will lazily load the freshly-registered project (`/cd`-style), so we
/// don't need to push it into the in-memory roster here.
#[utoipa::path(
    post,
    path = "/api/v1/projects",
    tag = "projects",
    request_body(content = CreateProjectForm, description = "Project to create (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 201, description = "Project created + registered", body = CreatedProject),
        (status = 400, description = "Invalid slug or path"),
        (status = 409, description = "Slug already registered"),
        (status = 500, description = "Scaffold or registry write failed"),
    ),
)]
pub(crate) async fn handle_create_project(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    FormOrJson(form, mode): FormOrJson<CreateProjectForm>,
) -> Response {
    let team = form
        .team
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("dev")
        .to_string();

    let host = form
        .host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .unwrap_or(ccteam_core::LOCAL_HOST)
        .to_string();

    // Validate the slug grammar and reserve a daemon-catalog identity. The
    // satellite may independently suffix its own wire slug.
    let base_slug = match ccteam_core::validate_slug_format(&form.slug) {
        Ok(s) => s,
        Err(err) => return create_error(StatusCode::BAD_REQUEST, format!("{err}"), mode),
    };
    let slug = match ccteam_core::pick_unused_project_slug(&app.paths.root, &base_slug) {
        Ok(slug) => slug,
        Err(err) => {
            return create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("registry read failed: {err}"),
                mode,
            )
        }
    };

    if host != ccteam_core::LOCAL_HOST {
        return create_remote_project(&app, &identity, form, mode, slug, host, team).await;
    }

    // Resolve the local working-tree dir. `~`-expansion + absolute-path
    //    enforcement; we keep this local (the gateway's `expand_project_
    //    path` is private) but apply the same rule: must be absolute after
    //    expansion.
    let abs = match expand_project_path(&form.path) {
        Ok(p) => p,
        Err(err) => return create_error(StatusCode::BAD_REQUEST, format!("{err}"), mode),
    };

    // 4. Bootstrap on disk (leaves existing user files alone; creates the
    //    dir when empty) then register in config.yaml.
    let paths = app.paths.clone();
    let slug_for_blocking = slug.clone();
    let abs_for_blocking = abs.clone();
    let team_for_blocking = team.clone();
    // v0.8.18 档1 — bind the new project to its creating web user
    // (`user:<id>`; admin → the shared `web-api` pool). Project is the unit of
    // ownership; its sessions inherit it.
    let owner_for_blocking = identity.owner_tag();
    let scaffold = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        ccteam_core::bootstrap_project_at_dir(
            &paths,
            &abs_for_blocking,
            &slug_for_blocking,
            "(created from web resource API)",
            &team_for_blocking,
        )?;
        // Owner stamp — bind the project to its creator so the tenant can see
        // its own project. Use the KNOWN project path (`abs`), NOT
        // `paths.project_state(slug)`: that resolves the dir through the config
        // registry, which does not yet contain this project (the upsert is
        // below) → it would fall back to the wrong path, the load would miss,
        // and the owner would never persist → the creating tenant could not see
        // its own project (404). `abs` IS the project working tree.
        let state_path = ccteam_core::CcteamPaths::project_state_in(&abs_for_blocking);
        if let Ok(mut state) = ccteam_core::ProjectState::load(&state_path) {
            state.owner = Some(owner_for_blocking.clone());
            if let Err(err) = state.save(&state_path) {
                tracing::warn!(slug = %slug_for_blocking, error = %err, "set project owner failed");
            }
        }
        ccteam_core::upsert_project_in_config(
            &paths.root,
            ProjectEntry {
                slug: slug_for_blocking.clone(),
                path: abs_for_blocking.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: team_for_blocking.clone(),
                installed_at: chrono::Utc::now(),
            },
        )?;
        Ok(())
    })
    .await;

    match scaffold {
        Ok(Ok(())) => {
            let body = CreatedProject {
                slug: slug.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                path: abs.display().to_string(),
            };
            match mode {
                // Both modes return 201 with the created resource — the
                // form path here is API (not htmx), so a redirect would be
                // wrong; a 201 + JSON body is the honest REST shape.
                InputMode::Form | InputMode::Json => {
                    (StatusCode::CREATED, Json(body)).into_response()
                }
            }
        }
        Ok(Err(err)) => {
            tracing::error!(%slug, %err, "create_project scaffold/register failed");
            create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("create failed: {err}"),
                mode,
            )
        }
        Err(err) => {
            tracing::error!(%slug, ?err, "create_project worker failed");
            create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "create worker failed".to_string(),
                mode,
            )
        }
    }
}

async fn create_remote_project(
    app: &AppState,
    identity: &crate::auth::Identity,
    form: CreateProjectForm,
    mode: InputMode,
    slug: String,
    host: String,
    team: String,
) -> Response {
    let remote_path = std::path::PathBuf::from(form.path.trim());
    if !remote_path.is_absolute() {
        return create_error(
            StatusCode::BAD_REQUEST,
            "path must be absolute on the satellite".to_string(),
            mode,
        );
    }
    let registry = match ccteam_core::HostRegistry::load(&app.paths.host_registry_path()) {
        Ok(registry) => registry,
        Err(err) => {
            return create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("host registry read failed: {err}"),
                mode,
            )
        }
    };
    let Some(record) = registry.get(&host) else {
        return create_error(StatusCode::NOT_FOUND, format!("unknown host: {host}"), mode);
    };
    if !record.is_online(ccteam_core::DEFAULT_HEARTBEAT_TTL_SECS) {
        return create_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("host `{host}` is offline"),
            mode,
        );
    }
    if !app.host_hub.is_connected(&host) {
        return create_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("host `{host}` has no live control channel"),
            mode,
        );
    }

    let result = match app
        .host_hub
        .request_project_init(&host, form.path.trim(), &slug)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            let message = format!("remote project creation failed: {err}");
            let status = if message.contains("did not respond") {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            return create_error(status, message, mode);
        }
    };
    if !result.ok {
        return create_error(
            StatusCode::BAD_GATEWAY,
            format!(
                "satellite project creation failed: {}",
                result.error.unwrap_or_else(|| "unknown error".to_string())
            ),
            mode,
        );
    }
    let Some(remote_slug) = result.slug.filter(|value| !value.is_empty()) else {
        return create_error(
            StatusCode::BAD_GATEWAY,
            "satellite returned success without a slug".to_string(),
            mode,
        );
    };
    let Some(remote_path) = result.path.map(std::path::PathBuf::from) else {
        return create_error(
            StatusCode::BAD_GATEWAY,
            "satellite returned success without a path".to_string(),
            mode,
        );
    };
    if !remote_path.is_absolute() {
        return create_error(
            StatusCode::BAD_GATEWAY,
            "satellite returned a non-absolute project path".to_string(),
            mode,
        );
    }

    let data_home = app.paths.projects_root.join(&slug);
    let owner = identity.owner_tag();
    let root = app.paths.root.clone();
    let slug_for_write = slug.clone();
    let host_for_write = host.clone();
    let remote_slug_for_write = remote_slug.clone();
    let remote_path_for_write = remote_path.clone();
    let data_home_for_write = data_home.clone();
    let write = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        ccteam_core::ensure_project_data_home(&data_home_for_write, &slug_for_write, Some(owner))?;
        ccteam_core::upsert_project_in_config(
            &root,
            ProjectEntry {
                slug: slug_for_write,
                path: data_home_for_write,
                host: host_for_write,
                remote_slug: Some(remote_slug_for_write),
                remote_path: Some(remote_path_for_write),
                team,
                installed_at: chrono::Utc::now(),
            },
        )?;
        Ok(())
    })
    .await;
    match write {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(CreatedProject {
                slug,
                host,
                path: data_home.display().to_string(),
            }),
        )
            .into_response(),
        Ok(Err(err)) => create_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("catalog registration failed: {err}"),
            mode,
        ),
        Err(err) => create_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("catalog registration worker failed: {err}"),
            mode,
        ),
    }
}

/// `POST /api/v1/projects/import` — catalog a project already registered on
/// a satellite. This is a collection operation: it creates the daemon-side
/// data home and ownership state but never contacts or mutates the satellite.
#[utoipa::path(
    post,
    path = "/api/v1/projects/import",
    tag = "projects",
    request_body(content = ImportProjectForm),
    responses(
        (status = 200, description = "Already cataloged", body = CreatedProject),
        (status = 201, description = "Imported into daemon catalog", body = CreatedProject),
        (status = 400, description = "Invalid host or slug"),
        (status = 404, description = "Host or remote project not reported"),
    ),
)]
pub(crate) async fn handle_import_project(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    FormOrJson(form, mode): FormOrJson<ImportProjectForm>,
) -> Response {
    let host = form.host.trim();
    if host.is_empty() || host == ccteam_core::LOCAL_HOST {
        return create_error(
            StatusCode::BAD_REQUEST,
            "import requires a satellite host".to_string(),
            mode,
        );
    }
    let remote_slug = match ccteam_core::validate_slug_format(&form.remote_slug) {
        Ok(slug) => slug,
        Err(err) => return create_error(StatusCode::BAD_REQUEST, format!("{err}"), mode),
    };
    let config = match ccteam_core::load_ccteam_config(&app.paths.root) {
        Ok(config) => config,
        Err(err) => {
            return create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("registry read failed: {err}"),
                mode,
            )
        }
    };
    if let Some(existing) = config.projects.iter().find(|entry| {
        entry.host == host && entry.remote_slug.as_deref() == Some(remote_slug.as_str())
    }) {
        let owner = ccteam_core::ProjectState::load(&ccteam_core::CcteamPaths::project_state_in(
            &existing.path,
        ))
        .ok()
        .and_then(|state| state.owner);
        if !identity.can_see_owner(owner.as_deref()) {
            return create_error(
                StatusCode::NOT_FOUND,
                "remote project is already cataloged by another owner".to_string(),
                mode,
            );
        }
        return (
            StatusCode::OK,
            Json(CreatedProject {
                slug: existing.slug.clone(),
                host: existing.host.clone(),
                path: existing.path.display().to_string(),
            }),
        )
            .into_response();
    }

    let registry = match ccteam_core::HostRegistry::load(&app.paths.host_registry_path()) {
        Ok(registry) => registry,
        Err(err) => {
            return create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("host registry read failed: {err}"),
                mode,
            )
        }
    };
    let Some(remote) = registry
        .get(host)
        .and_then(|record| record.projects.iter().find(|p| p.slug == remote_slug))
    else {
        return create_error(
            StatusCode::NOT_FOUND,
            format!("project `{remote_slug}` is not reported by host `{host}`"),
            mode,
        );
    };
    let base_slug = match form.slug.as_deref() {
        Some(slug) => match ccteam_core::validate_slug_format(slug) {
            Ok(slug) => slug,
            Err(err) => return create_error(StatusCode::BAD_REQUEST, format!("{err}"), mode),
        },
        None => remote_slug.clone(),
    };
    let slug = match ccteam_core::pick_unused_project_slug(&app.paths.root, &base_slug) {
        Ok(slug) => slug,
        Err(err) => {
            return create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("registry read failed: {err}"),
                mode,
            )
        }
    };
    let remote_path = std::path::PathBuf::from(&remote.path);
    let data_home = app.paths.projects_root.join(&slug);
    let owner = identity.owner_tag();
    let entry = ProjectEntry {
        slug: slug.clone(),
        path: data_home.clone(),
        host: host.to_string(),
        remote_slug: Some(remote_slug),
        remote_path: Some(remote_path),
        team: "dev".to_string(),
        installed_at: chrono::Utc::now(),
    };
    let root = app.paths.root.clone();
    let write_home = data_home.clone();
    let write_slug = slug.clone();
    let write = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        ccteam_core::ensure_project_data_home(&write_home, &write_slug, Some(owner))?;
        ccteam_core::upsert_project_in_config(&root, entry)?;
        Ok(())
    })
    .await;
    match write {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(CreatedProject {
                slug,
                host: host.to_string(),
                path: data_home.display().to_string(),
            }),
        )
            .into_response(),
        Ok(Err(err)) => create_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("import failed: {err}"),
            mode,
        ),
        Err(err) => create_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("import worker failed: {err}"),
            mode,
        ),
    }
}

/// `DELETE /api/v1/projects/{slug}`
///
/// Deregister-only: remove the slug from `config.yaml`, then (when a
/// gateway is attached) stop every live session belonging to that
/// project via the spine. 404 when the slug is not registered. Never
/// file-purges. Returns `{ "removed": true }` on success.
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{slug}",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug to deregister")),
    responses(
        (status = 200, description = "Deregistered; `{removed, sessions_stopped[]}`", body = serde_json::Value),
        (status = 404, description = "Slug not registered"),
        (status = 500, description = "Deregister failed"),
    ),
)]
pub(crate) async fn handle_delete_project(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    // 1. Deregister from config.yaml. `false` = slug wasn't present → 404.
    match ccteam_core::remove_project_from_config(&app.paths.root, &slug) {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("project not registered: {slug}")})),
            )
                .into_response();
        }
        Err(err) => {
            tracing::error!(%slug, %err, "remove_project_from_config failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("deregister failed: {err}")})),
            )
                .into_response();
        }
    }

    // 2. Stop every live session for this project via the spine, if a
    //    gateway is attached. Snapshot the sids under one lock (session_
    //    views is sync), then stop them (each stop_session is async, held
    //    under the same lock — the gateway access pattern from the spine).
    //    No gateway (standalone internal-web path) ⇒ skip; deregister
    //    alone is the meaningful effect there.
    let mut stopped: Vec<String> = Vec::new();
    if let Some(gw) = app.gateway.as_ref() {
        let mut guard = gw.lock().await;
        let sids: Vec<String> = guard
            .session_views()
            .into_iter()
            .filter(|v| v.project == slug)
            .map(|v| v.sid)
            .collect();
        for sid in sids {
            match guard.stop_session(&sid).await {
                Ok(()) => stopped.push(sid),
                Err(err) => {
                    // Best-effort: a session that vanished mid-teardown
                    // shouldn't fail the deregister (already removed from
                    // config). Log + continue.
                    tracing::warn!(%slug, %sid, %err, "stop_session during project delete failed");
                }
            }
        }
    }

    Json(serde_json::json!({
        "removed": true,
        "sessions_stopped": stopped,
    }))
    .into_response()
}

/// Shared POST error responder honoring the [`FormOrJson`] mode
/// convention: form ⇒ plain text, JSON ⇒ `{ "ok": false, "error": ... }`.
fn create_error(status: StatusCode, msg: String, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => (status, msg).into_response(),
        InputMode::Json => {
            (status, Json(serde_json::json!({"ok": false, "error": msg}))).into_response()
        }
    }
}

/// Expand a `~`-relative or relative project path to an absolute
/// `PathBuf`, mirroring `Gateway::create_project`'s `expand_project_path`
/// contract (the gateway's helper is private to ccteam-im). Rules:
///
/// - `~` / `~/...` expands against `$HOME`.
/// - The result must be absolute (a bare relative path with no `~` is
///   rejected — the API caller must be explicit about where the working
///   tree lives).
fn expand_project_path(raw: &str) -> anyhow::Result<std::path::PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("path must be non-empty");
    }
    let expanded: std::path::PathBuf = if trimmed == "~" {
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
            .join(rest)
    } else {
        std::path::PathBuf::from(trimmed)
    };
    if !expanded.is_absolute() {
        anyhow::bail!("path must be absolute (or ~-relative); got {:?}", trimmed);
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_project_path_rejects_relative() {
        assert!(expand_project_path("some/rel/dir").is_err());
        assert!(expand_project_path("").is_err());
    }

    #[test]
    fn expand_project_path_keeps_absolute() {
        let p = expand_project_path("/abs/dir").unwrap();
        assert_eq!(p, std::path::PathBuf::from("/abs/dir"));
    }

    #[test]
    fn expand_project_path_expands_home() {
        if let Some(home) = dirs::home_dir() {
            let p = expand_project_path("~/work/x").unwrap();
            assert_eq!(p, home.join("work/x"));
            let bare = expand_project_path("~").unwrap();
            assert_eq!(bare, home);
        }
    }
}
