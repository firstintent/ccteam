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
    body::{self, Body},
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use tokio_util::io::ReaderStream;

use crate::state::AppState;

/// Cap on one uploaded attachment. Matches the "screenshots + logs + small
/// docs" chat use case while bounding the request; the IM side (Telegram bot
/// API) tops out at 20 MB, so parity plus headroom.
pub(crate) const UPLOAD_BODY_LIMIT: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadPathError {
    NotFound,
    OutsideUploads,
}

/// Resolve one existing upload and prove that its canonical target stays
/// under this project's canonical uploads directory. This is the single path
/// guard for both the turn write side and the browser read side, including
/// symlinks and `..` components.
pub(crate) fn canonical_project_upload(
    project_dir: &std::path::Path,
    candidate: &std::path::Path,
) -> Result<std::path::PathBuf, UploadPathError> {
    let canonical = candidate
        .canonicalize()
        .map_err(|_| UploadPathError::NotFound)?;
    // Resolve the candidate first. If it exists but the project has no
    // uploads directory, it is necessarily outside the allowed root. This
    // preserves the turn-write face's readable outside-uploads contract.
    let root = ccteam_im::transport::project_uploads_dir(project_dir)
        .canonicalize()
        .map_err(|_| UploadPathError::OutsideUploads)?;
    if !canonical.starts_with(&root) {
        return Err(UploadPathError::OutsideUploads);
    }
    if !canonical.is_file() {
        return Err(UploadPathError::NotFound);
    }
    Ok(canonical)
}

/// Raster formats safe to render in an `<img>` on our authenticated origin.
/// SVG is intentionally absent: upload classification may call it an image,
/// but same-origin browser serving treats it as a download.
fn inline_image_content_type(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

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
pub(crate) fn project_host(app: &AppState, slug: &str) -> String {
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
    let project_dir = app.paths.project_dir(&slug);
    let requested_name = q.name.as_deref().unwrap_or("upload.bin");
    let dir = ccteam_im::transport::project_uploads_dir(&project_dir);
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
    let (path, name) =
        ccteam_im::transport::next_project_upload_path(&project_dir, requested_name, millis);
    let kind = classify_upload(&content_type, &name);
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

/// `GET /api/v1/projects/{slug}/uploads/{name}`
///
/// Streams one project-owned attachment after canonical containment checks.
/// Safe raster extensions are served with an image content type; every other
/// file (including SVG) is forced to an opaque download. The route shape is
/// deliberately project-scoped so `auth::project_acl_layer` gates it.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/uploads/{name}",
    tag = "sessions",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("name" = String, Path, description = "Stored upload basename"),
    ),
    responses(
        (status = 200, description = "Raw attachment bytes (safe raster inline; all other types download)"),
        (status = 400, description = "Invalid/outside-uploads path or remote-host project"),
        (status = 404, description = "Unknown project or missing attachment"),
    ),
)]
pub(crate) async fn handle_get_project_upload(
    State(app): State<AppState>,
    Path((slug, name)): Path<(String, String)>,
) -> Response {
    if let Some(deny) = reject_unknown_project(&app, &slug) {
        return deny;
    }
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

    let project_dir = app.paths.project_dir(&slug);
    let candidate = ccteam_im::transport::project_uploads_dir(&project_dir).join(&name);
    let path = match canonical_project_upload(&project_dir, &candidate) {
        Ok(path) => path,
        Err(UploadPathError::OutsideUploads) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid upload path"})),
            )
                .into_response();
        }
        Err(UploadPathError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "attachment not found"})),
            )
                .into_response();
        }
    };

    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "attachment not found"})),
            )
                .into_response();
        }
    };
    let size = file.metadata().await.ok().map(|meta| meta.len());
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(content_type) = inline_image_content_type(&path) {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    } else {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    if let Some(size) = size {
        if let Ok(value) = HeaderValue::from_str(&size.to_string()) {
            response.headers_mut().insert(header::CONTENT_LENGTH, value);
        }
    }
    response
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
    use super::*;
    use ccteam_core::{CcteamPaths, ProjectState};
    use ccteam_im::transport::AttachmentKind;

    fn paths(root: &std::path::Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join("home"),
            projects_root: root.join("projects"),
        }
    }

    fn seed_project(paths: &CcteamPaths, slug: &str, owner: Option<String>) -> std::path::PathBuf {
        let project = paths.project_dir(slug);
        std::fs::create_dir_all(project.join(".ccteam")).unwrap();
        let mut state = ProjectState::initial(slug.to_string());
        state.owner = owner;
        state
            .save(&CcteamPaths::project_state_in(&project))
            .unwrap();
        project
    }

    async fn serve(
        paths: CcteamPaths,
        auth: crate::AuthState,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = crate::router_with_state(AppState::with_auth(paths, auth));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (addr, task)
    }

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

    #[tokio::test]
    async fn read_face_enforces_containment_download_safety_and_missing_404() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths(tmp.path());
        let project = seed_project(&paths, "demo", None);
        let uploads = ccteam_im::transport::project_uploads_dir(&project);
        std::fs::create_dir_all(&uploads).unwrap();
        std::fs::write(uploads.join("1-safe.png"), b"png bytes").unwrap();
        std::fs::write(uploads.join("2-vector.svg"), b"<svg></svg>").unwrap();
        let outside = project.join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        assert_eq!(
            canonical_project_upload(&project, &outside),
            Err(UploadPathError::OutsideUploads)
        );

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, uploads.join("escape.txt")).unwrap();

        let (addr, server) = serve(paths, crate::AuthState::disabled()).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let get =
            |name: &str| client.get(format!("http://{addr}/api/v1/projects/demo/uploads/{name}"));

        let png = get("1-safe.png").send().await.unwrap();
        assert_eq!(png.status(), StatusCode::OK);
        assert_eq!(png.headers()[header::CONTENT_TYPE], "image/png");
        assert!(png.headers().get(header::CONTENT_DISPOSITION).is_none());

        let svg = get("2-vector.svg").send().await.unwrap();
        assert_eq!(svg.status(), StatusCode::OK);
        assert_eq!(
            svg.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(svg.headers()[header::CONTENT_DISPOSITION], "attachment");

        let missing = get("missing.bin").send().await.unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        #[cfg(unix)]
        {
            let escaped = get("escape.txt").send().await.unwrap();
            assert_eq!(escaped.status(), StatusCode::BAD_REQUEST);
        }
        server.abort();
    }

    #[tokio::test]
    async fn project_acl_denies_non_owner_upload_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths(tmp.path());
        let mut users = ccteam_core::tenants::TenantRegistry::default();
        let owner = users.add("owner");
        let stranger = users.add("stranger");
        users.save(&paths.users_dir()).unwrap();

        let project = seed_project(&paths, "private", Some(format!("user:{}", owner.id)));
        let uploads = ccteam_im::transport::project_uploads_dir(&project);
        std::fs::create_dir_all(&uploads).unwrap();
        std::fs::write(uploads.join("1-chart.png"), b"png").unwrap();

        let (addr, server) = serve(paths, crate::AuthState::enabled("admin-token".into())).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{addr}/api/v1/projects/private/uploads/1-chart.png");
        let denied = client
            .get(&url)
            .header(
                header::AUTHORIZATION,
                format!("Bearer ccteam:{}", stranger.web_token),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);

        let allowed = client
            .get(url)
            .header(
                header::AUTHORIZATION,
                format!("Bearer ccteam:{}", owner.web_token),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        server.abort();
    }
}
