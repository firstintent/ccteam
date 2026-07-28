//! v0.9.11 TEAM-2 — per-project division-of-labor charter (`routing.md`).
//!
//! The charter is user-authored advisory markdown that agents PULL verbatim
//! through the MCP `status` tool (`ccteam_im::mcp::vendor_panel`); ccteam
//! never parses or injects it. This module is its web read/write face:
//!
//! - `GET /api/v1/projects/{slug}/routing` → the effective charter with
//!   fallback semantics: the project file (`<project>/.ccteam/routing.md`)
//!   when present, else the global `~/.ccteam/routing.md` (read-only in the
//!   web), else an honest `source: "none"`.
//! - `PUT /api/v1/projects/{slug}/routing` → atomically write the PROJECT
//!   file only. The global file's write surface stays CLI/filesystem (owner
//!   decision), so the web can never clobber the cross-project fallback.
//!
//! For a remote (satellite) project the project path resolves to the
//! daemon-side data home (`CcteamPaths::project_routing_notes`), so
//! read/write works uniformly — no host special-casing here.
//!
//! Auth: `/api/v1/projects/{slug}/…` rides the single project-ownership
//! choke point (`auth::project_acl_layer`); unknown project → 404.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::state::AppState;

/// Cap on a charter body written via PUT. Same bound as a role `.md`
/// (`roles::ROLE_BODY_LIMIT`): prose markdown, 256 KiB is generous headroom —
/// and the MCP `status` transport excerpts anything beyond ~4k chars anyway.
const ROUTING_BODY_LIMIT: usize = 256 * 1024;

/// Lower-hex sha256 of the content bytes (mirrors
/// `ccteam_im::mcp::vendor_panel::sha256_hex` / `hub::sha256_hex` so the web
/// face and the MCP `status` face report the same digest for the same file).
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// RFC3339 mtime of `path` (matches the MCP `status` `updated_at` rendering).
fn mtime_rfc3339(path: &std::path::Path) -> Option<String> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
}

/// Reject an unknown project with a 404 JSON body (same convention as
/// `roles::reject_unknown_project`).
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

/// `GET /api/v1/projects/{slug}/routing` response.
#[derive(Debug, Serialize, ToSchema)]
pub struct RoutingDoc {
    /// Whether a charter file exists at the reported `source` (false only
    /// when `source == "none"`).
    pub exists: bool,
    /// `"project"` (the project's own file) · `"global"` (the read-only
    /// `~/.ccteam/routing.md` fallback) · `"none"`.
    pub source: String,
    /// The PROJECT charter path — always the target a PUT writes. When
    /// `source == "project"` it is also the file being served.
    pub path: String,
    /// The global fallback file actually being served when
    /// `source == "global"`; null otherwise.
    pub fallback_path: Option<String>,
    /// Raw markdown (verbatim; empty when `source == "none"`).
    pub content: String,
    /// Lower-hex sha256 of `content` bytes; null when `source == "none"`.
    pub sha256: Option<String>,
    /// RFC3339 file mtime; null when `source == "none"` (or unreadable).
    pub updated_at: Option<String>,
}

/// `PUT /api/v1/projects/{slug}/routing` request body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RoutingPutBody {
    /// Full replacement markdown for the PROJECT charter.
    pub content: String,
}

/// `PUT /api/v1/projects/{slug}/routing` response: the written file's digest
/// + mtime (what a follow-up GET will report).
#[derive(Debug, Serialize, ToSchema)]
pub struct RoutingPutResult {
    pub sha256: String,
    pub updated_at: String,
}

/// `GET /api/v1/projects/{slug}/routing` — effective charter (project file,
/// else global fallback, else none).
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/routing",
    tag = "routing",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Effective charter + provenance", body = RoutingDoc),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Charter read failed"),
    ),
)]
pub(crate) async fn handle_get_routing(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    let project_path = app.paths.project_routing_notes(&slug);
    let global_path = app.paths.global_routing_notes();

    let (source, served, fallback_path) = if project_path.is_file() {
        ("project", Some(project_path.clone()), None)
    } else if global_path.is_file() {
        (
            "global",
            Some(global_path.clone()),
            Some(global_path.display().to_string()),
        )
    } else {
        ("none", None, None)
    };

    let Some(served) = served else {
        return Json(RoutingDoc {
            exists: false,
            source: source.into(),
            path: project_path.display().to_string(),
            fallback_path: None,
            content: String::new(),
            sha256: None,
            updated_at: None,
        })
        .into_response();
    };

    match std::fs::read(&served) {
        Ok(bytes) => Json(RoutingDoc {
            exists: true,
            source: source.into(),
            // Always the PUT target: for `source == "global"` this is the
            // project file a save would CREATE, not the file being served
            // (that one is `fallback_path`).
            path: project_path.display().to_string(),
            fallback_path,
            sha256: Some(sha256_hex(&bytes)),
            updated_at: mtime_rfc3339(&served),
            content: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .into_response(),
        Err(err) => {
            tracing::error!(slug, path = %served.display(), %err, "routing notes read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("read routing notes: {err}")})),
            )
                .into_response()
        }
    }
}

/// `PUT /api/v1/projects/{slug}/routing` — atomically create-or-replace the
/// PROJECT charter (never the global fallback).
#[utoipa::path(
    put,
    path = "/api/v1/projects/{slug}/routing",
    tag = "routing",
    params(("slug" = String, Path, description = "Project slug")),
    request_body = RoutingPutBody,
    responses(
        (status = 200, description = "Charter written", body = RoutingPutResult),
        (status = 404, description = "Unknown project"),
        (status = 413, description = "Body exceeds the 256 KiB cap"),
        (status = 500, description = "Charter write failed"),
    ),
)]
pub(crate) async fn handle_put_routing(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Json(body): Json<RoutingPutBody>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    if body.content.len() > ROUTING_BODY_LIMIT {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!(
                    "charter is {} bytes — the cap is 256 KiB; keep it advisory-sized \
                     (the MCP status transport excerpts beyond ~4k chars anyway)",
                    body.content.len()
                )
            })),
        )
            .into_response();
    }

    let path = app.paths.project_routing_notes(&slug);
    let write = || -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        ccteam_harness::execution::fs_atomic::atomic_write_durable(&path, body.content.as_bytes())
    };
    match write() {
        Ok(()) => Json(RoutingPutResult {
            sha256: sha256_hex(body.content.as_bytes()),
            updated_at: mtime_rfc3339(&path).unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        })
        .into_response(),
        Err(err) => {
            tracing::error!(slug, path = %path.display(), %err, "routing notes write failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("write routing notes: {err}")})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_hex_matches_known_vector() {
        // Same convention as `hub::sha256_hex` / vendor_panel: lower-hex.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
