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
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

/// Cap on a role `.md` body written via PUT. Persona files are prose +
/// a small YAML frontmatter; 256 KiB is generous headroom while bounding
/// the request.
const ROLE_BODY_LIMIT: usize = 256 * 1024;

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
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/roles",
    tag = "roles",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Role summaries `[{role, description, model}]`", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Role read failed"),
    ),
)]
pub(crate) async fn handle_list_roles(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
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

/// Reject a `{role}` path param that is empty or contains anything
/// outside `[a-z0-9_-]`. axum percent-decodes the path param before it
/// reaches us, so a `..%2f..%2f.claude%2fCLAUDE.md` traversal arrives as
/// literal `../../.claude/CLAUDE.md` and is caught by the `/` / `.`
/// rejection — defense in depth alongside `ccteam_core::read_role`'s own
/// guard, but here we can return a clean `400` instead of a `500`. Must
/// mirror the write/PUT path validator (`admin_actions::validate_bot_name`,
/// reused via `write_role`) so a name accepted on read is accepted on
/// write and vice versa.
fn role_name_is_valid(role: &str) -> bool {
    !role.is_empty()
        && role
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

/// `GET /api/v1/projects/{slug}/roles/{role}` → frontmatter + body or 404.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/roles/{role}",
    tag = "roles",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("role" = String, Path, description = "Role name ([a-z0-9_-]+)"),
    ),
    responses(
        (status = 200, description = "Role `{role, frontmatter, body}`", body = serde_json::Value),
        (status = 400, description = "Invalid role name"),
        (status = 404, description = "Unknown project or role"),
    ),
)]
pub(crate) async fn handle_get_role(
    State(app): State<AppState>,
    Path((slug, role)): Path<(String, String)>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    if !role_name_is_valid(&role) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("invalid role name: {role}")})),
        )
            .into_response();
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
            // The early `role_name_is_valid` guard catches traversal, so a
            // core `Err` here is a genuine read failure (or, defense in
            // depth, a name the core guard rejects) → 400, never 500, and
            // never leaks a path.
            tracing::warn!(slug, role, %err, "read_role failed");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// JSON / form PUT body shape: `{"content": "..."}` / `content=...`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RoleContentForm {
    pub content: String,
}

/// PUT body: either a structured `content` field (JSON / form) or a raw
/// body (any other content-type). `mode` mirrors the [`FormOrJson`]
/// success-shape convention — Form / raw ⇒ `200 OK` (empty), JSON ⇒
/// `{ "ok": true }`. We surface raw as [`InputMode::Form`] so a `curl`
/// `--data-binary @role.md` gets a plain 200.
pub(crate) struct RoleBody {
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
///
/// Wire note: the request body is **tri-modal** — `application/json`
/// `{content}`, `application/x-www-form-urlencoded` `content=...`, or a
/// raw `text/markdown` body taken verbatim (256 KiB cap). The schema
/// below documents the structured `{content}` form; a raw markdown PUT
/// is accepted with the same effect.
#[utoipa::path(
    put,
    path = "/api/v1/projects/{slug}/roles/{role}",
    tag = "roles",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("role" = String, Path, description = "Role name ([a-z0-9_-]+)"),
    ),
    request_body(content = RoleContentForm, description = "Role markdown (JSON `{content}` / form / or raw text/markdown body)"),
    responses(
        (status = 200, description = "Role written (`{ok:true}` for JSON, empty 200 for form/raw)"),
        (status = 400, description = "Write failed / invalid role name / bad body"),
        (status = 404, description = "Unknown project"),
    ),
)]
pub(crate) async fn handle_put_role(
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

#[cfg(test)]
mod tests {
    use super::role_name_is_valid;

    #[test]
    fn role_name_guard_rejects_traversal_and_accepts_normal() {
        // Normal subagent names pass.
        for ok in ["cto", "reviewer", "code-reviewer", "bot_2", "a"] {
            assert!(role_name_is_valid(ok), "expected `{ok}` to be valid");
        }
        // axum percent-decodes the path param, so these are the literal
        // strings a percent-encoded traversal delivers to the handler.
        for bad in [
            "",
            "../secret",
            "../../etc/passwd",
            "../../../.claude/CLAUDE.md",
            "..\\..\\windows",
            "/etc/passwd",
            ".hidden",
            "a/b",
            "Bad Name",
            "UPPER",
        ] {
            assert!(!role_name_is_valid(bad), "expected `{bad}` to be rejected");
        }
    }
}
