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
    #[serde(default)]
    pub team: Option<String>,
}

/// 201 response body for a created project.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedProject {
    pub slug: String,
    pub path: String,
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

    // 1. Validate the slug grammar ([a-z0-9-]+, ≤60, no edge dashes).
    let slug = match ccteam_core::validate_slug_format(&form.slug) {
        Ok(s) => s,
        Err(err) => return create_error(StatusCode::BAD_REQUEST, format!("{err}"), mode),
    };

    // 2. On a slug collision, AUTO-APPEND (demo → demo2 → demo3 …) instead of
    //    rejecting — the same rule `ccteam init` uses. Two users (or two
    //    different working trees) can then both create a "demo": their paths
    //    differ; the numeric suffix just disambiguates the globally-unique slug
    //    (the REST/key identity). The 201 returns the slug actually used.
    //    Bounded so a pathological registry can't spin forever.
    let slug = {
        let base = slug;
        let mut candidate = base.clone();
        let mut n = 1u32;
        loop {
            match ccteam_core::lookup_project_in_config(&app.paths.root, &candidate) {
                Ok(None) => break candidate,
                Ok(Some(_)) => {
                    n += 1;
                    if n > 999 {
                        return create_error(
                            StatusCode::CONFLICT,
                            format!("too many projects named {base}"),
                            mode,
                        );
                    }
                    candidate = format!("{base}{n}");
                }
                Err(err) => {
                    tracing::error!(slug = %candidate, %err, "lookup_project_in_config failed");
                    return create_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("registry read failed: {err}"),
                        mode,
                    );
                }
            }
        }
    };

    // 3. Resolve the working-tree dir. `~`-expansion + absolute-path
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
