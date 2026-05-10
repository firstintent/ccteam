//! `GET /project/<slug>` — project detail page.
//!
//! Loads `~/.ccteam/projects/<slug>/.ccteam/state.json`, tails the
//! last 10 progress events for display, scans the outbox dir for the
//! 20 newest messages, and renders the askama `project.html` template.
//! Unknown slug returns 404.
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
use ccteam_core::{HarnessKind, HarnessSnapshot, ProjectState, TeamKind};
use chrono::{DateTime, Utc};

use crate::decisions::scan_candidates;
use crate::queries::{
    event_ts_label, events_to_rows, outbox_rows, session_recent_events, slug_recent_events,
    DEFAULT_OUTBOX_LIMIT, PROJECT_EVENT_DISPLAY_LIMIT, STATUS_EVENT_LIMIT,
};
use crate::state::AppState;
use crate::status::status_badge;
use crate::views::{HtmlTemplate, ProjectTemplate, SessionCard};

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

    let status_events = slug_recent_events(&app.paths, &slug, STATUS_EVENT_LIMIT);
    let display_start = status_events
        .len()
        .saturating_sub(PROJECT_EVENT_DISPLAY_LIMIT);
    let event_rows = events_to_rows(&status_events[display_start..]);
    let outbox = outbox_rows(&app.paths, &slug, DEFAULT_OUTBOX_LIMIT);

    let silent = state
        .last_progress_event_at
        .map(|t| Utc::now().signed_duration_since(t).num_seconds().max(0) as u64)
        .unwrap_or(0);
    let badge = status_badge(&state, &status_events, silent);
    let sessions = if state.team_kind == TeamKind::Flex {
        session_cards(&app, &state)
    } else {
        Vec::new()
    };

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
        kind: team_kind_label(state.team_kind).to_string(),
        is_flex: state.team_kind == TeamKind::Flex,
        current_phase: state.current_phase.clone(),
        badge_class: badge.css_class(),
        badge_label: badge.label(),
        cost_label: format!("{:.2}", state.cost_used_usd),
        created_at: state.created_at.to_rfc3339(),
        sessions,
        state_json_pretty,
        events: event_rows,
        outbox,
        auth_enabled: app.auth.enabled,
        auth_wire_token: app.auth.wire_token(),
        decision_candidates,
    };
    HtmlTemplate(tpl).into_response()
}

fn session_cards(app: &AppState, state: &ProjectState) -> Vec<SessionCard> {
    state
        .sessions
        .iter()
        .map(|(sid, record)| {
            let events = session_recent_events(&app.paths, &state.slug, sid, STATUS_EVENT_LIMIT);
            let last_event_label = events
                .last()
                .and_then(event_ts_label)
                .unwrap_or_else(|| "—".to_string());
            let silent = events
                .last()
                .and_then(|event| event.get("ts").and_then(|s| s.as_str()))
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|ts| {
                    Utc::now()
                        .signed_duration_since(ts.with_timezone(&Utc))
                        .num_seconds()
                        .max(0) as u64
                })
                .unwrap_or(0);
            let badge = status_badge(state, &events, silent);
            let cost_label = load_harness_snapshot(app, &state.slug, sid)
                .map(|snap| format!("{:.2}", snap.cost_usd_total))
                .unwrap_or_else(|| format!("{:.2}", state.cost_used_usd));
            SessionCard {
                sid: sid.clone(),
                harness: harness_label(record.harness).to_string(),
                harness_class: harness_class(record.harness),
                tmux_session: record.tmux_session.clone(),
                status_class: badge.css_class(),
                status_label: badge.label(),
                last_event_label,
                cost_label,
                detail_href: format!("/session/{}/{}", state.slug, sid),
                screenshot_href: format!("/screenshot/{}-{}.png", state.slug, sid),
                attach_command: format!("tmux attach -t {}", record.tmux_session),
            }
        })
        .collect()
}

fn load_harness_snapshot(app: &AppState, slug: &str, sid: &str) -> Option<HarnessSnapshot> {
    let path = app.paths.harness_dir().join(format!("{slug}-{sid}.json"));
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn team_kind_label(kind: TeamKind) -> &'static str {
    match kind {
        TeamKind::Workflow => "workflow",
        TeamKind::MultiWorkflow => "multi_workflow",
        TeamKind::Flex => "flex",
    }
}

fn harness_label(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "claude",
        HarnessKind::Codex => "codex",
    }
}

fn harness_class(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "harness-claude",
        HarnessKind::Codex => "harness-codex",
    }
}
