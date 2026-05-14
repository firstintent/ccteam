//! V0.3 M5.3 — `POST /api/<slug>/{btw,inject_decision,pause,resume}`.
//!
//! V0.3.2 F52 update: each handler now accepts **either**
//! `application/x-www-form-urlencoded` (V0.3 default — keeps the
//! htmx flow + 303 redirect-back contract) **or** `application/json`
//! (new SPA flow — body `{ "text": "..." }` etc., response
//! `{ "ok": true }` / `{ "ok": false, "error": "..." }` with 4xx/5xx).
//! The content-type dispatch goes through the [`FormOrJson`] extractor
//! below.
//!
//! Each handler is a thin adapter that:
//!
//! 1. extracts the request body via [`FormOrJson<T>`] (form-encoded ⇒
//!    [`InputMode::Form`]; JSON ⇒ [`InputMode::Json`]),
//! 2. validates input (length caps, path-traversal sanity for
//!    `inject_decision`),
//! 3. calls into the corresponding `ccteam_core::actions::*` helper
//!    (M5.0 promote — see `docs/dev-coupling-audit.md` F45),
//! 4. on success: form ⇒ 303 See Other → `/project/<slug>` (existing
//!    htmx contract), JSON ⇒ `{ "ok": true }` (200),
//! 5. on validation failure: form ⇒ `400` + plain-text reason, JSON ⇒
//!    `400` + `{ "ok": false, "error": "..." }`,
//! 6. on unexpected error: form ⇒ `500` plain-text, JSON ⇒ `500` +
//!    `{ "ok": false, "error": "..." }`.
//!
//! Pause / resume have **no body** — [`FormOrJson<T = ()>`] still
//! dispatches correctly: with no `Content-Type` it defaults to
//! [`InputMode::Form`] (preserving the existing htmx + 303 contract).
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
    body,
    extract::{FromRequest, Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::post,
    Json, Router,
};
use ccteam_core::actions::{self, DecisionInput};
use ccteam_core::{
    inbox_filename, next_inbox_seq, InboxFrontMatter, InboxMessage, ProjectState, SessionMailbox,
    TeamKind, LATEST_SCHEMA_VERSION,
};
use chrono::Utc;
use serde::{de::DeserializeOwned, Deserialize};

use crate::state::AppState;

/// Maximum length of `/btw` text payload (chars). Mirrors PRD §6.2.2
/// — keeps a single inbox message small enough that the orchestrator
/// can deliver it via tmux send-keys without wrapping pathologies.
const BTW_MAX: usize = 4000;

/// Maximum length of an `inject_decision` body (chars).
const DECISION_BODY_MAX: usize = 8000;

/// Form submissions in this surface are tiny (one text field or two
/// decision fields). Keep the extractor bounded even though the later
/// field validators enforce the user-facing caps.
const FORM_BODY_LIMIT: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct BtwForm {
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct InjectDecisionForm {
    pub path: String,
    pub body: String,
}

/// Empty body marker for pause / resume. Form-encoded "no body"
/// deserializes via `serde_urlencoded` to a unit struct; JSON `{}`
/// likewise deserializes via `serde_json` to `()`. Centralised so
/// the FormOrJson<()> path works for both encodings.
#[derive(Debug, Deserialize)]
pub struct EmptyBody {}

/// Which content-type the client sent. Handlers use this to decide
/// between the htmx 303-redirect response shape (`Form`) and the
/// SPA `{"ok":true}` JSON shape (`Json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// `application/x-www-form-urlencoded` or no body (htmx / pause /
    /// resume) — handler responds with `303 See Other`.
    Form,
    /// `application/json` — handler responds with `{ "ok": true }` on
    /// success, `{ "ok": false, "error": "..." }` on failure.
    Json,
}

/// Content-type-dispatching extractor. Body type `T` must implement
/// `Deserialize` for both `serde_urlencoded` (form) and `serde_json`
/// (json) — `BtwForm`, `InjectDecisionForm`, `EmptyBody` all do.
pub struct FormOrJson<T>(pub T, pub InputMode);

impl<S, T> FromRequest<S> for FormOrJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send + 'static,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let mode = detect_mode(&req);
        match mode {
            InputMode::Json => match Json::<T>::from_request(req, state).await {
                Ok(Json(value)) => Ok(FormOrJson(value, InputMode::Json)),
                Err(rejection) => Err(json_error(rejection.status(), &rejection.body_text())),
            },
            InputMode::Form => {
                let bytes = body::to_bytes(req.into_body(), FORM_BODY_LIMIT)
                    .await
                    .map_err(|err| {
                        (StatusCode::BAD_REQUEST, format!("invalid form body: {err}"))
                            .into_response()
                    })?;
                match serde_urlencoded::from_bytes::<T>(&bytes) {
                    Ok(value) => Ok(FormOrJson(value, InputMode::Form)),
                    Err(err) => Err(
                        (StatusCode::BAD_REQUEST, format!("invalid form body: {err}"))
                            .into_response(),
                    ),
                }
            }
        }
    }
}

fn detect_mode(req: &Request) -> InputMode {
    let Some(ct) = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
    else {
        return InputMode::Form;
    };
    let head = ct.split(';').next().unwrap_or("").trim();
    if head.eq_ignore_ascii_case("application/json") {
        InputMode::Json
    } else {
        InputMode::Form
    }
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({"ok": false, "error": msg}))).into_response()
}

/// Success response — Form ⇒ 303 redirect back to project page,
/// JSON ⇒ `{"ok":true}` with 200.
fn success_project(slug: &str, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => Redirect::to(&format!("/project/{slug}")).into_response(),
        InputMode::Json => Json(serde_json::json!({"ok": true})).into_response(),
    }
}

/// Session-scoped success — Form ⇒ 303 redirect back to session page,
/// JSON ⇒ `{"ok":true}`.
fn success_session(slug: &str, sid: &str, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => Redirect::to(&format!("/session/{slug}/{sid}")).into_response(),
        InputMode::Json => Json(serde_json::json!({"ok": true})).into_response(),
    }
}

/// Error response — Form ⇒ plain text body, JSON ⇒ `{"ok":false,"error":...}`.
fn error(status: StatusCode, msg: String, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => (status, msg).into_response(),
        InputMode::Json => json_error(status, &msg),
    }
}

/// Reject text payloads that are empty / whitespace-only / over the cap.
fn validate_text(field: &str, value: &str, max: usize) -> Result<(), (StatusCode, String)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{field} must not be empty"),
        ));
    }
    if value.len() > max {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{field} exceeds max length {max}"),
        ));
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
fn validate_decision_path(
    app: &AppState,
    slug: &str,
    raw: &str,
) -> Result<PathBuf, (StatusCode, String)> {
    let candidate = PathBuf::from(raw);
    if !candidate.is_absolute() {
        return Err((StatusCode::BAD_REQUEST, "path must be absolute".to_string()));
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must not contain `..` components".to_string(),
        ));
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
        ));
    }
    Ok(candidate)
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

async fn handle_btw(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    FormOrJson(form, mode): FormOrJson<BtwForm>,
) -> Response {
    if let Err((status, msg)) = validate_text("text", &form.text, BTW_MAX) {
        return error(status, msg, mode);
    }
    match actions::send_to_session(&app.paths, &slug, &form.text) {
        Ok(_) => success_project(&slug, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "send_to_session failed");
            // The most common case is "no project named <slug>" which
            // is a client problem (404), but distinguishing it from
            // genuine IO is brittle; 400 with the underlying message
            // is honest enough for a single-user dev tool.
            error(StatusCode::BAD_REQUEST, format!("btw failed: {err}"), mode)
        }
    }
}

async fn handle_session_btw(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
    FormOrJson(form, mode): FormOrJson<BtwForm>,
) -> Response {
    if let Err((status, msg)) = validate_text("text", &form.text, BTW_MAX) {
        return error(status, msg, mode);
    }
    match send_to_registered_session(&app, &slug, &sid, &form.text) {
        Ok(_) => success_session(&slug, &sid, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, sid = %sid, error = %err, "send_to_registered_session failed");
            error(StatusCode::BAD_REQUEST, format!("btw failed: {err}"), mode)
        }
    }
}

async fn handle_inject_decision(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    FormOrJson(form, mode): FormOrJson<InjectDecisionForm>,
) -> Response {
    if let Err((status, msg)) = validate_text("body", &form.body, DECISION_BODY_MAX) {
        return error(status, msg, mode);
    }
    let path = match validate_decision_path(&app, &slug, &form.path) {
        Ok(p) => p,
        Err((status, msg)) => return error(status, msg, mode),
    };
    let decision = DecisionInput {
        path,
        body: form.body,
    };
    match actions::inject_decision(&app.paths, &slug, decision) {
        Ok(_) => success_project(&slug, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "inject_decision failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("inject_decision failed: {err}"),
                mode,
            )
        }
    }
}

async fn handle_pause(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    FormOrJson(_empty, mode): FormOrJson<EmptyBody>,
) -> Response {
    match actions::pause(&app.paths, &slug) {
        Ok(_) => success_project(&slug, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "pause failed");
            error(
                StatusCode::BAD_REQUEST,
                format!("pause failed: {err}"),
                mode,
            )
        }
    }
}

async fn handle_session_pause(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
    FormOrJson(_empty, mode): FormOrJson<EmptyBody>,
) -> Response {
    if let Err((status, msg)) = validate_known_session(&app, &slug, &sid) {
        return error(status, msg, mode);
    }
    match actions::pause(&app.paths, &slug) {
        Ok(_) => success_session(&slug, &sid, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, sid = %sid, error = %err, "pause failed");
            error(
                StatusCode::BAD_REQUEST,
                format!("pause failed: {err}"),
                mode,
            )
        }
    }
}

async fn handle_resume(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    FormOrJson(_empty, mode): FormOrJson<EmptyBody>,
) -> Response {
    match actions::resume(&app.paths, &slug) {
        Ok(_) => success_project(&slug, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, error = %err, "resume failed");
            error(
                StatusCode::BAD_REQUEST,
                format!("resume failed: {err}"),
                mode,
            )
        }
    }
}

async fn handle_session_resume(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
    FormOrJson(_empty, mode): FormOrJson<EmptyBody>,
) -> Response {
    if let Err((status, msg)) = validate_known_session(&app, &slug, &sid) {
        return error(status, msg, mode);
    }
    match actions::resume(&app.paths, &slug) {
        Ok(_) => success_session(&slug, &sid, mode),
        Err(err) => {
            tracing::warn!(slug = %slug, sid = %sid, error = %err, "resume failed");
            error(
                StatusCode::BAD_REQUEST,
                format!("resume failed: {err}"),
                mode,
            )
        }
    }
}

fn validate_known_session(
    app: &AppState,
    slug: &str,
    sid: &str,
) -> Result<(), (StatusCode, String)> {
    let state = ProjectState::load(&app.paths.project_state(slug)).map_err(|err| {
        (
            StatusCode::NOT_FOUND,
            format!("project not found or unreadable: {slug}: {err}"),
        )
    })?;
    if state.team_kind != TeamKind::Flex {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("project {slug} is not a flex project"),
        ));
    }
    if !state.sessions.contains_key(sid) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("session not found: {slug}/{sid}"),
        ));
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
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_decision_path_rejects_dotdot_components() {
        let (_tmp, app) = fake_app();
        let raw = format!(
            "{}/../../../etc/passwd",
            app.paths.project_ccteam_dir("demo").display()
        );
        let err = validate_decision_path(&app, "demo", &raw).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_decision_path_rejects_outside_ccteam_dir() {
        let (_tmp, app) = fake_app();
        let err = validate_decision_path(&app, "demo", "/etc/passwd").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
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
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_text_rejects_overlong() {
        let s = "x".repeat(5000);
        let err = validate_text("text", &s, 4000).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_text_accepts_normal() {
        validate_text("text", "hello", 4000).unwrap();
    }
}
