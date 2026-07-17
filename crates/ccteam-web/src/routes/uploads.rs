//! Web composer attachments + project skill picker.
//!
//! - `POST /api/v1/projects/{slug}/uploads?name=<file>` → save the raw request
//!   body to `<project_dir>/.ccteam/uploads/`, reply `{path, kind, name, size}`
//! - `GET  /api/v1/projects/{slug}/skills`              → `[{skill, description}]`
//!
//! The upload is step 1 of the two-step attachment flow (vendor-generic by
//! construction): the SPA uploads each picked file here the moment it is
//! attached, then names the returned `path` in the turn's `attachments[]`
//! (`POST /sessions/{sid}/turn`), which appends the SAME
//! `[attachment image_path|file_path="…"]` turn-text lines the IM inbound path
//! emits — every vendor session already knows to `Read` those paths (ccteam
//! MCP instructions), so no per-vendor plumbing exists anywhere.
//!
//! Files land under the project's ccteam-owned dotdir (`.ccteam/uploads/`, in
//! the project's `.gitignore` via init), NOT the global IM staging dir, so a
//! project's uploads live and die with the project and the per-project ACL
//! (`auth::project_acl_layer`, automatic for every `/projects/{slug}/*` route)
//! is the exact upload permission.
//!
//! Remote-host projects (v0.9.2 host-binding) are rejected with a readable
//! error: the daemon-side file would not exist on the satellite that executes
//! the session. Same honesty posture as the exec layer's explicit
//! NotImplemented verdicts — no silent half-support.

use axum::{
    body,
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::state::AppState;

/// Cap on one uploaded attachment. Matches the "screenshots + logs + small
/// docs" chat use case while bounding the request; the IM side (Telegram bot
/// API) tops out at 20 MB, so parity plus headroom.
pub(crate) const UPLOAD_BODY_LIMIT: usize = 25 * 1024 * 1024;

/// Query params for the upload POST. `name` is the original client file name
/// (sanitized server-side before it touches the filesystem).
#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    name: Option<String>,
}

/// Reject an unknown project with a 404 JSON body (mirrors `roles.rs`).
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

/// The project's catalog host binding (`"local"` when unregistered — every
/// test fixture and pre-v0.9.2 entry deserializes as local).
fn project_host(app: &AppState, slug: &str) -> String {
    ccteam_core::config::load(&app.paths.root)
        .ok()
        .and_then(|cfg| cfg.projects.into_iter().find(|p| p.slug == slug))
        .map(|p| p.host)
        .unwrap_or_else(|| "local".to_string())
}

/// Classify an upload as image vs. generic file — content-type first
/// (`image/*`), file extension as fallback. Pure.
pub(crate) fn classify_upload(
    content_type: &str,
    name: &str,
) -> ccteam_im::transport::AttachmentKind {
    if content_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/")
    {
        return ccteam_im::transport::AttachmentKind::Image;
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => {
            ccteam_im::transport::AttachmentKind::Image
        }
        _ => ccteam_im::transport::AttachmentKind::File,
    }
}

/// `POST /api/v1/projects/{slug}/uploads?name=<file>`
///
/// Saves the raw request body (any content-type; 25 MiB cap) to
/// `<project_dir>/.ccteam/uploads/<millis>-<sanitized-name>` and replies
/// `201 {path, kind, name, size}`. The returned `path` is what the SPA later
/// names in the turn's `attachments[]`. Auth: the project ACL middleware
/// covers this route like every `/projects/{slug}/*` face.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/uploads",
    tag = "sessions",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("name" = Option<String>, Query, description = "Original file name (sanitized server-side)"),
    ),
    request_body(content = String, description = "Raw file bytes (any content-type; 25 MiB cap)"),
    responses(
        (status = 201, description = "Stored. `{path, kind: \"image\"|\"file\", name, size}`", body = serde_json::Value),
        (status = 400, description = "Empty body / remote-host project"),
        (status = 404, description = "Unknown project"),
        (status = 413, description = "Body exceeds the 25 MiB cap"),
    ),
)]
pub(crate) async fn handle_project_upload(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<UploadQuery>,
    req: Request,
) -> Response {
    if let Some(deny) = reject_unknown_project(&app, &slug) {
        return deny;
    }
    // Remote-host project → the saved file would not exist on the satellite
    // that executes this project's sessions. Fail fast and readable.
    let host = project_host(&app, &slug);
    if host != "local" {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "project `{slug}` runs on remote host `{host}` — attachments are not yet \
                     supported for remote projects"
                )
            })),
        )
            .into_response();
    }
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = match body::to_bytes(req.into_body(), UPLOAD_BODY_LIMIT).await {
        Ok(b) => b,
        Err(err) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({"error": format!("body too large or unreadable: {err}")})),
            )
                .into_response();
        }
    };
    if bytes.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "empty upload body"})),
        )
            .into_response();
    }
    let name =
        ccteam_im::transport::sanitize_attachment_name(q.name.as_deref().unwrap_or("upload.bin"));
    let kind = classify_upload(&content_type, &name);
    let dir = app.paths.project_dir(&slug).join(".ccteam").join("uploads");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("create uploads dir: {err}")})),
        )
            .into_response();
    }
    // Millis prefix keeps names unique + chronologically sorted; on the rare
    // same-ms same-name collision, bump a numeric suffix instead of clobbering.
    let millis = chrono::Utc::now().timestamp_millis();
    let mut path = dir.join(format!("{millis}-{name}"));
    let mut bump = 0u32;
    while path.exists() {
        bump += 1;
        path = dir.join(format!("{millis}-{bump}-{name}"));
    }
    let tmp = path.with_extension("part");
    if let Err(err) = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &path)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("store upload: {err}")})),
        )
            .into_response();
    }
    let kind_str = match kind {
        ccteam_im::transport::AttachmentKind::Image => "image",
        ccteam_im::transport::AttachmentKind::File => "file",
    };
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "path": path.to_string_lossy(),
            "kind": kind_str,
            "name": name,
            "size": bytes.len(),
        })),
    )
        .into_response()
}

/// `GET /api/v1/projects/{slug}/skills`
///
/// The project's installed skills (`.claude/skills/<id>/SKILL.md`), for the
/// composer's attach-skill picker. `[{skill, description}]`, sorted by id;
/// empty for a project with no skills dir. Read-only; the hub marketplace
/// remains the install face.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/skills",
    tag = "roles",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "`[{skill, description}]` sorted by id", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
    ),
)]
pub(crate) async fn handle_list_skills(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    if let Some(deny) = reject_unknown_project(&app, &slug) {
        return deny;
    }
    let project_dir = app.paths.project_dir(&slug);
    match ccteam_core::list_skills(&project_dir) {
        Ok(skills) => (StatusCode::OK, Json(skills)).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("list skills: {err}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::classify_upload;
    use ccteam_im::transport::AttachmentKind;

    #[test]
    fn classify_prefers_content_type_then_extension() {
        assert_eq!(classify_upload("image/png", "x.bin"), AttachmentKind::Image);
        assert_eq!(classify_upload("IMAGE/JPEG", "x"), AttachmentKind::Image);
        assert_eq!(
            classify_upload("application/octet-stream", "shot.PNG"),
            AttachmentKind::Image
        );
        assert_eq!(
            classify_upload("application/pdf", "doc.pdf"),
            AttachmentKind::File
        );
        assert_eq!(classify_upload("", "notes.txt"), AttachmentKind::File);
    }
}
