//! v0.8.6 W5b ResDisk — project-scoped agent role endpoints.
//!
//! Roles are Claude Code subagent definitions at
//! `<project_dir>/.claude/agents/<role>.md`. These endpoints are the
//! network face of the core read reader (`ccteam_core::roles`) and the
//! core write primitive (`ccteam_core::write_role`).
//!
//! - `GET  /api/v1/projects/{slug}/roles`        → `[{role, description, model}]`
//! - `GET  /api/v1/projects/{slug}/roles/{role}` → `{role, frontmatter, body}` or 404
//! - `PUT  /api/v1/projects/{slug}/roles/{role}` → write the `.md`, `{ "ok": true }`
//!
//! The PUT body is either `application/json` `{"content": "..."}` /
//! form-encoded `content=...` (via [`FormOrJson`]) **or** a raw request
//! body (any other content-type, e.g. `text/markdown`) — handled by a
//! dedicated extractor below so an SPA can `PUT` the markdown verbatim.
//!
//! Unknown project → 404 (mirrors the `api_v1` project 404 short-circuit
//! on `project_state(slug)`). Auth: merged into
//! [`super::stateful_router`] so the existing `auth_layer` gate applies.

use axum::{
    body,
    extract::{FromRequest, Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

/// Cap on a role `.md` body written via PUT. Persona files are prose +
/// a small YAML frontmatter; 256 KiB is generous headroom while bounding
/// the request.
const ROLE_BODY_LIMIT: usize = 256 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects/{slug}/roles", get(handle_list_roles))
        .route(
            "/api/v1/projects/{slug}/roles/{role}",
            get(handle_get_role).put(handle_put_role),
        )
}

/// Reject an unknown project with a 404 JSON body. Returns `Some(resp)`
/// when the project is unknown so callers can `return` it directly.
fn reject_unknown_project(app: &AppState, slug: &str) -> Option<Response> {
    if app.paths.project_state(slug).exists() {
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

/// `GET /api/v1/projects/{slug}/roles`
async fn handle_list_roles(State(app): State<AppState>, Path(slug): Path<String>) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    let project_dir = app.paths.project_dir(&slug);
    match ccteam_core::list_roles(&project_dir) {
        Ok(roles) => Json(roles).into_response(),
        Err(err) => {
            tracing::error!(slug, %err, "list_roles failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/projects/{slug}/roles/{role}` → frontmatter + body or 404.
async fn handle_get_role(
    State(app): State<AppState>,
    Path((slug, role)): Path<(String, String)>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    let project_dir = app.paths.project_dir(&slug);
    match ccteam_core::read_role(&project_dir, &role) {
        Ok(Some(detail)) => Json(detail).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("role not found: {slug}/{role}")})),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(slug, role, %err, "read_role failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// JSON / form PUT body shape: `{"content": "..."}` / `content=...`.
#[derive(Debug, Deserialize)]
pub struct RoleContentForm {
    pub content: String,
}

/// PUT body: either a structured `content` field (JSON / form) or a raw
/// body (any other content-type). `mode` mirrors the [`FormOrJson`]
/// success-shape convention — Form / raw ⇒ `200 OK` (empty), JSON ⇒
/// `{ "ok": true }`. We surface raw as [`InputMode::Form`] so a `curl`
/// `--data-binary @role.md` gets a plain 200.
struct RoleBody {
    content: String,
    mode: InputMode,
}

impl<S> FromRequest<S> for RoleBody
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let ct = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .map(|s| {
                s.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        // JSON or form → reuse FormOrJson<RoleContentForm> so the wire
        // shape + error handling matches the rest of the resource API.
        if ct == "application/json" || ct == "application/x-www-form-urlencoded" {
            let FormOrJson(form, mode) =
                FormOrJson::<RoleContentForm>::from_request(req, state).await?;
            return Ok(RoleBody {
                content: form.content,
                mode,
            });
        }
        // Anything else (text/markdown, text/plain, no content-type): take
        // the raw body verbatim as the role markdown.
        let bytes = body::to_bytes(req.into_body(), ROLE_BODY_LIMIT)
            .await
            .map_err(|err| {
                (StatusCode::BAD_REQUEST, format!("invalid body: {err}")).into_response()
            })?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|_| (StatusCode::BAD_REQUEST, "body must be valid UTF-8").into_response())?;
        Ok(RoleBody {
            content,
            mode: InputMode::Form,
        })
    }
}

/// `PUT /api/v1/projects/{slug}/roles/{role}` — create-or-replace the
/// role's `.md`. Body via [`RoleBody`] (JSON `{content}` / form / raw).
async fn handle_put_role(
    State(app): State<AppState>,
    Path((slug, role)): Path<(String, String)>,
    body: RoleBody,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    let project_dir = app.paths.project_dir(&slug);
    match ccteam_core::write_role(&project_dir, &role, &body.content) {
        Ok(_) => match body.mode {
            InputMode::Json => Json(serde_json::json!({"ok": true})).into_response(),
            InputMode::Form => StatusCode::OK.into_response(),
        },
        Err(err) => {
            tracing::warn!(slug, role, %err, "write_role failed");
            // Bad role name / empty content are client errors (400); the
            // underlying message is honest enough for a single-user tool.
            let status = StatusCode::BAD_REQUEST;
            match body.mode {
                InputMode::Json => (
                    status,
                    Json(serde_json::json!({"ok": false, "error": format!("{err}")})),
                )
                    .into_response(),
                InputMode::Form => (status, format!("write_role failed: {err}")).into_response(),
            }
        }
    }
}
