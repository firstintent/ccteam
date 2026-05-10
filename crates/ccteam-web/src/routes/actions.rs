//! V0.3 M5.3 — `POST /api/<slug>/{btw,inject_decision,pause,resume}`.
//!
//! Each handler is a thin adapter that:
//!
//! 1. extracts a typed `Form<...>` body (axum's
//!    `application/x-www-form-urlencoded` deserialization),
//! 2. validates input (length caps, path-traversal sanity for
//!    `inject_decision`),
//! 3. calls into the corresponding `ccteam_core::actions::*` helper
//!    (M5.0 promote — see `docs/dev-coupling-audit.md` F45),
//! 4. on success → 303 See Other → `/project/<slug>` so the user lands
//!    back on the detail page,
//! 5. on validation failure → `400 Bad Request` + plain-text reason,
//! 6. on unexpected error → `500 Internal Server Error` + plain-text.
//!
//! The handlers do **not** kill tmux sessions, parse pane output, or
//! mutate `progress.jsonl` (CLAUDE.md §三 red lines). They only write
//! inbox / decision / state files; the orchestrator's existing
//! inotify + idle-aware dispatcher picks the change up.
//!
//! Auth is enforced by the `auth_layer` middleware mounted on the
//! whole stateful router in `lib::router_with_state`, so handlers
//! here trust they only run for authenticated callers (when auth is
//! enabled at all).

use std::path::{Component, PathBuf};

use anyhow::{bail, Context};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::post,
    Form, Router,
};
use ccteam_core::actions::{self, DecisionInput};
use ccteam_core::{
    inbox_filename, next_inbox_seq, InboxFrontMatter, InboxMessage, ProjectState, SessionMailbox,
    TeamKind, LATEST_SCHEMA_VERSION,
};
use chrono::Utc;
use serde::Deserialize;

use crate::state::AppState;

/// Maximum length of `/btw` text payload (chars). Mirrors PRD §6.2.2
/// — keeps a single inbox message small enough that the orchestrator
/// can deliver it via tmux send-keys without wrapping pathologies.
const BTW_MAX: usize = 4000;

/// Maximum length of an `inject_decision` body (chars).
const DECISION_BODY_MAX: usize = 8000;

#[derive(Debug, Deserialize)]
pub struct BtwForm {
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct InjectDecisionForm {
    pub path: String,
    pub body: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/{slug}/btw", post(handle_btw))
        .route("/api/{slug}/{sid}/btw", post(handle_session_btw))
        .route("/api/{slug}/inject_decision", post(handle_inject_decision))
        .route("/api/{slug}/pause", post(handle_pause))
        .route("/api/{slug}/{sid}/pause", post(handle_session_pause))
        .route("/api/{slug}/resume", post(handle_resume))
        .route("/api/{slug}/{sid}/resume", post(handle_session_resume))
}

/// Common success → 303 See Other → /project/<slug>.
fn redirect_back(slug: &str) -> Response {
    Redirect::to(&format!("/project/{slug}")).into_response()
}

/// Session-scoped success → 303 See Other → /session/<slug>/<sid>.
fn redirect_back_session(slug: &str, sid: &str) -> Response {
    Redirect::to(&format!("/session/{slug}/{sid}")).into_response()
}

/// Reject text payloads that are empty / whitespace-only / over the cap.
#[allow(clippy::result_large_err)] // Err = axum Response (boxed body); fine for handler fast-fail.
fn validate_text(field: &str, value: &str, max: usize) -> Result<(), Response> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{field} must not be empty"),
        )
            .into_response());
    }
    if value.len() > max {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{field} exceeds max length {max}"),
        )
            .into_response());
    }
    Ok(())
}

/// Path-traversal sanity for `inject_decision`. Brief §3.3 says: the
/// decision file MUST be absolute and live under
/// `~/projects/<slug>/.ccteam/`. We:
///
/// 1. parse the supplied string as a `PathBuf`,
/// 2. reject any `..` component (defeats `~/projects/<slug>/.ccteam/../../../etc/passwd`),
/// 3. require absolute path,
/// 4. require `starts_with(project_ccteam_dir(slug))` against the
///    resolved (existing) ancestor.
#[allow(clippy::result_large_err)] // Err = axum Response (boxed body); fine for handler fast-fail.
fn validate_decision_path(app: &AppState, slug: &str, raw: &str) -> Result<PathBuf, Response> {
    let candidate = PathBuf::from(raw);
    if !candidate.is_absolute() {
        return Err((StatusCode::BAD_REQUEST, "path must be absolute").into_response());
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must not contain `..` components",
        )
            .into_response());
    }
    let ccteam_dir = app.paths.project_ccteam_dir(slug);
    if !candidate.starts_with(&ccteam_dir) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "path must live under {} (got {})",
                ccteam_dir.display(),
                candidate.display(),
            ),
        )
            .into_response());
    }
    Ok(candidate)
}

async fn handle_btw(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<BtwForm>,
) -> Response {
    if let Err(resp) = validate_text("text", &form.text, BTW_MAX) {
        return resp;
    }
    match actions::send_to_session(&app.paths, &slug, &form.text) {
        Ok(_) => redirect_back(&slug),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "send_to_session failed");
            // The most common case is "no project named <slug>" which
            // is a client problem (404), but distinguishing it from
            // genuine IO is brittle; 400 with the underlying message
            // is honest enough for a single-user dev tool.
            (StatusCode::BAD_REQUEST, format!("btw failed: {err}")).into_response()
        }
    }
}

async fn handle_session_btw(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
    Form(form): Form<BtwForm>,
) -> Response {
    if let Err(resp) = validate_text("text", &form.text, BTW_MAX) {
        return resp;
    }
    match send_to_registered_session(&app, &slug, &sid, &form.text) {
        Ok(_) => redirect_back_session(&slug, &sid),
        Err(err) => {
            tracing::warn!(slug = %slug, sid = %sid, error = %err, "send_to_registered_session failed");
            (StatusCode::BAD_REQUEST, format!("btw failed: {err}")).into_response()
        }
    }
}

async fn handle_inject_decision(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Form(form): Form<InjectDecisionForm>,
) -> Response {
    if let Err(resp) = validate_text("body", &form.body, DECISION_BODY_MAX) {
        return resp;
    }
    let path = match validate_decision_path(&app, &slug, &form.path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let decision = DecisionInput {
        path,
        body: form.body,
    };
    match actions::inject_decision(&app.paths, &slug, decision) {
        Ok(_) => redirect_back(&slug),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "inject_decision failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("inject_decision failed: {err}"),
            )
                .into_response()
        }
    }
}

async fn handle_pause(State(app): State<AppState>, Path(slug): Path<String>) -> Response {
    match actions::pause(&app.paths, &slug) {
        Ok(_) => redirect_back(&slug),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "pause failed");
            (StatusCode::BAD_REQUEST, format!("pause failed: {err}")).into_response()
        }
    }
}

async fn handle_session_pause(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
) -> Response {
    if let Err(resp) = validate_known_session(&app, &slug, &sid) {
        return resp;
    }
    match actions::pause(&app.paths, &slug) {
        Ok(_) => redirect_back_session(&slug, &sid),
        Err(err) => {
            tracing::warn!(slug = %slug, sid = %sid, error = %err, "pause failed");
            (StatusCode::BAD_REQUEST, format!("pause failed: {err}")).into_response()
        }
    }
}

async fn handle_resume(State(app): State<AppState>, Path(slug): Path<String>) -> Response {
    match actions::resume(&app.paths, &slug) {
        Ok(_) => redirect_back(&slug),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "resume failed");
            (StatusCode::BAD_REQUEST, format!("resume failed: {err}")).into_response()
        }
    }
}

async fn handle_session_resume(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
) -> Response {
    if let Err(resp) = validate_known_session(&app, &slug, &sid) {
        return resp;
    }
    match actions::resume(&app.paths, &slug) {
        Ok(_) => redirect_back_session(&slug, &sid),
        Err(err) => {
            tracing::warn!(slug = %slug, sid = %sid, error = %err, "resume failed");
            (StatusCode::BAD_REQUEST, format!("resume failed: {err}")).into_response()
        }
    }
}

#[allow(clippy::result_large_err)] // Err = axum Response; handler fast-fail.
fn validate_known_session(app: &AppState, slug: &str, sid: &str) -> Result<(), Response> {
    let state = ProjectState::load(&app.paths.project_state(slug)).map_err(|err| {
        (
            StatusCode::NOT_FOUND,
            format!("project not found or unreadable: {slug}: {err}"),
        )
            .into_response()
    })?;
    if state.team_kind != TeamKind::Flex {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("project {slug} is not a flex project"),
        )
            .into_response());
    }
    if !state.sessions.contains_key(sid) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("session not found: {slug}/{sid}"),
        )
            .into_response());
    }
    Ok(())
}

fn send_to_registered_session(
    app: &AppState,
    slug: &str,
    sid: &str,
    body: &str,
) -> anyhow::Result<()> {
    let state = ProjectState::load(&app.paths.project_state(slug))
        .with_context(|| format!("load state for {slug}"))?;
    if state.team_kind != TeamKind::Flex {
        bail!("project {slug} is not a flex project");
    }
    if !state.sessions.contains_key(sid) {
        bail!("session not found: {slug}/{sid}");
    }

    let mailbox = SessionMailbox::for_ccteam_dir(&app.paths.project_session_dir(slug, sid));
    mailbox.ensure_dirs()?;
    let now = Utc::now();
    let filename = inbox_filename(now, next_inbox_seq(&mailbox)?);
    let inbox_path = mailbox.inbox.join(&filename);
    let msg = InboxMessage {
        front: InboxFrontMatter {
            schema_version: LATEST_SCHEMA_VERSION,
            source: "ccteam-web".into(),
            source_chat_id: None,
            source_msg_id: None,
            source_user: "web".into(),
            created_at: now,
            ingested_at: now,
            content_type: "text".into(),
            attachments: Vec::new(),
        },
        body: format!("{}\n", body.trim_end_matches('\n')),
    };
    msg.save(&inbox_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::CcteamPaths;
    use tempfile::TempDir;

    fn fake_app() -> (TempDir, AppState) {
        let tmp = TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        (tmp, AppState::new(paths))
    }

    #[test]
    fn validate_decision_path_rejects_relative() {
        let (_tmp, app) = fake_app();
        let err = validate_decision_path(&app, "demo", "decision.md").unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_decision_path_rejects_dotdot_components() {
        let (_tmp, app) = fake_app();
        let raw = format!(
            "{}/../../../etc/passwd",
            app.paths.project_ccteam_dir("demo").display()
        );
        let err = validate_decision_path(&app, "demo", &raw).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_decision_path_rejects_outside_ccteam_dir() {
        let (_tmp, app) = fake_app();
        let err = validate_decision_path(&app, "demo", "/etc/passwd").unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_decision_path_accepts_within_ccteam_dir() {
        let (_tmp, app) = fake_app();
        let inside = app.paths.project_ccteam_dir("demo").join("decision-x.md");
        let ok = validate_decision_path(&app, "demo", &inside.display().to_string()).unwrap();
        assert_eq!(ok, inside);
    }

    #[test]
    fn validate_text_rejects_empty() {
        let err = validate_text("text", "   ", 100).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_text_rejects_overlong() {
        let s = "x".repeat(5000);
        let err = validate_text("text", &s, 4000).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_text_accepts_normal() {
        validate_text("text", "hello", 4000).unwrap();
    }
}
