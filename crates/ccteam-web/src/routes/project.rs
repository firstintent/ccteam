//! `GET /project/<slug>` — project detail page.
//!
//! Loads `~/.ccteam/projects/<slug>/.ccteam/state.json`, tails the
//! last 200 progress events, scans the outbox dir for the 20 newest
//! messages, and renders the askama `project.html` template. Unknown
//! slug returns 404.
//!
//! Read-only — no write endpoints, no escalation, no state mutation.
//! M5.3 will add forms here; for M5.1 the screenshot panel is a
//! commented placeholder.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use ccteam_core::ProjectState;
use chrono::Utc;

use crate::decisions::scan_candidates;
use crate::queries::{
    events_to_rows, outbox_rows, slug_recent_events, DEFAULT_EVENT_LIMIT, DEFAULT_OUTBOX_LIMIT,
};
use crate::state::AppState;
use crate::status::status_badge;
use crate::views::{HtmlTemplate, ProjectTemplate};

pub fn router() -> Router<AppState> {
    Router::new().route("/project/{slug}", get(handle_project))
}

async fn handle_project(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    let state_path = app.paths.project_state(&slug);
    if !state_path.exists() {
        return (StatusCode::NOT_FOUND, format!("project not found: {slug}")).into_response();
    }
    let state = match ProjectState::load(&state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(slug, error = %err, "ProjectState::load failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("state.json load failed for {slug}: {err}"),
            )
                .into_response();
        }
    };

    let raw_events = slug_recent_events(&app.paths, &slug, DEFAULT_EVENT_LIMIT);
    let event_rows = events_to_rows(&raw_events);
    let outbox = outbox_rows(&app.paths, &slug, DEFAULT_OUTBOX_LIMIT);

    let silent = state
        .last_progress_event_at
        .map(|t| Utc::now().signed_duration_since(t).num_seconds().max(0) as u64)
        .unwrap_or(0);
    let badge = status_badge(&state, &raw_events, silent);

    let state_json_pretty = match serde_json::to_string_pretty(&state) {
        Ok(s) => s,
        Err(err) => format!("(serialize error: {err})"),
    };

    // V0.3 M5.3 — write-action context: decision candidates list +
    // auth state echoed into `hx-headers` so same-origin form
    // submits carry the bearer token (CSRF defense, PRD §6.2.5).
    let decision_candidates: Vec<String> = scan_candidates(&app.paths, &slug)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();

    let tpl = ProjectTemplate {
        version: env!("CARGO_PKG_VERSION"),
        slug: state.slug.clone(),
        team: state.team.clone(),
        current_phase: state.current_phase.clone(),
        badge_class: badge.css_class(),
        badge_label: badge.label(),
        cost_label: format!("{:.2}", state.cost_used_usd),
        state_json_pretty,
        events: event_rows,
        outbox,
        auth_enabled: app.auth.enabled,
        auth_wire_token: app.auth.wire_token(),
        decision_candidates,
    };
    HtmlTemplate(tpl).into_response()
}
