//! V0.3.2 F52 — JSON API parity layer.
//!
//! Exposes JSON endpoints that mirror the data the V0.3 askama HTML
//! views used to render. The SPA (F54+) consumes these directly;
//! V0.3.2 F59 retired the HTML routes (now 301-redirect into `/app/...`).
//!
//! Endpoints:
//!
//! - `GET /api/v1/projects` → `Vec<DashboardRow>`.
//! - `GET /api/v1/projects/{slug}` → [`ProjectSummary`].
//! - `GET /api/v1/projects/{slug}/sessions/{sid}` → [`SessionDetail`].
//! - `GET /api/v1/auth/token` → `{"wire_token": "ccteam:<hex>" | null}`.
//!
//! The composite DTOs (`ProjectSummary` / `SessionDetail` /
//! [`AuthToken`]) are defined here. `auth_wire_token` is syntactically
//! impossible to leak from the project / session JSON (the field does
//! not exist on the DTOs; auth state lives behind `/api/v1/auth/token`).
//!
//! Auth: this module merges into [`super::stateful_router`] so the
//! existing `auth_layer` middleware in `lib::router_with_state`
//! applies for free — no separate gate.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ccteam_core::{HarnessKind, HarnessSnapshot, ProjectState, TeamKind};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::decisions::scan_candidates;
use crate::queries::{
    event_ts_label, events_to_rows, outbox_rows, recent_event_summary, session_outbox_rows,
    session_recent_events, slug_recent_events, DEFAULT_OUTBOX_LIMIT, PROJECT_EVENT_DISPLAY_LIMIT,
    STATUS_EVENT_LIMIT,
};
use crate::state::AppState;
use crate::status::status_badge;
use crate::views::{DashboardRow, EventRow, HarnessSnapshotView, OutboxRow, SessionCard};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects", get(handle_projects))
        .route("/api/v1/projects/{slug}", get(handle_project))
        .route(
            "/api/v1/projects/{slug}/sessions/{sid}",
            get(handle_session),
        )
        .route("/api/v1/auth/token", get(handle_auth_token))
}

/// JSON returned by `GET /api/v1/projects/{slug}`.
///
/// Two deliberate shape choices vs. the V0.3 (retired) askama project
/// template payload:
///
/// 1. `state_json_pretty: String` → `state: serde_json::Value` — the
///    SPA picks its own formatting; pretty-printing is presentation.
/// 2. `auth_wire_token` / `auth_enabled` are **not** on this struct.
///    Tokens belong on `/api/v1/auth/token` (single explicit endpoint)
///    so listing API responses cannot leak them.
#[derive(Serialize)]
pub struct ProjectSummary {
    pub slug: String,
    pub team: String,
    pub kind: String,
    pub is_flex: bool,
    pub current_phase: String,
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
    pub created_at: String,
    pub sessions: Vec<SessionCard>,
    pub state: serde_json::Value,
    pub events: Vec<EventRow>,
    pub outbox: Vec<OutboxRow>,
    pub decision_candidates: Vec<String>,
}

/// JSON returned by `GET /api/v1/projects/{slug}/sessions/{sid}`.
///
/// Shape matches the V0.3 (retired) askama session template payload
/// minus the auth fields (same rationale as [`ProjectSummary`]).
#[derive(Serialize)]
pub struct SessionDetail {
    pub slug: String,
    pub sid: String,
    pub team: String,
    pub kind: String,
    pub harness: String,
    pub harness_class: &'static str,
    pub tmux_session: String,
    pub started_at: String,
    pub status_class: &'static str,
    pub status_label: &'static str,
    pub cost_label: String,
    pub events: Vec<EventRow>,
    pub outbox: Vec<OutboxRow>,
    pub decision_candidates: Vec<String>,
    pub harness_snapshot: Option<HarnessSnapshotView>,
}

/// JSON returned by `GET /api/v1/auth/token`.
///
/// `wire_token` is `Some("ccteam:<hex>")` when auth is enabled; `null`
/// when the server runs with auth disabled (loopback default /
/// `--no-auth`). The SPA uses this to decide whether the token-entry
/// flow is required before fetching protected resources.
#[derive(Serialize)]
pub struct AuthToken {
    pub wire_token: Option<String>,
}

async fn handle_projects(State(app): State<AppState>) -> impl IntoResponse {
    match build_projects(&app) {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => {
            tracing::error!(?err, "GET /api/v1/projects build failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

fn build_projects(app: &AppState) -> anyhow::Result<Vec<DashboardRow>> {
    let summaries = ccteam_core::collect_projects(&app.paths)?;
    let mut rows = Vec::with_capacity(summaries.len());
    for s in summaries {
        let events = slug_recent_events(&app.paths, &s.state.slug, STATUS_EVENT_LIMIT);
        let badge = status_badge(&s.state, &events, s.stall_silent_seconds);
        let last_event_label = match s.state.last_progress_event_at {
            Some(ts) => recent_event_summary(ts, s.stall_silent_seconds),
            None => "—".to_string(),
        };
        rows.push(DashboardRow {
            slug: s.state.slug.clone(),
            team: s.state.team.clone(),
            kind: team_kind_label(s.state.team_kind).to_string(),
            current_phase: if s.state.team_kind == TeamKind::Flex {
                String::new()
            } else {
                s.state.current_phase.clone()
            },
            last_event_label,
            badge_class: badge.css_class(),
            badge_label: badge.label(),
            cost_label: format!("{:.2}", s.state.cost_used_usd),
        });
    }
    Ok(rows)
}

async fn handle_project(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    let state_path = app.paths.project_state(&slug);
    if !state_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }
    let state = match ProjectState::load(&state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(slug, error = %err, "GET /api/v1/projects/{{slug}} load failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("state.json load failed for {slug}: {err}")
                })),
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

    let state_value = match serde_json::to_value(&state) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(slug, error = %err, "state serialize failed");
            serde_json::Value::Null
        }
    };

    let decision_candidates: Vec<String> = scan_candidates(&app.paths, &slug)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();

    let summary = ProjectSummary {
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
        state: state_value,
        events: event_rows,
        outbox,
        decision_candidates,
    };
    Json(summary).into_response()
}

async fn handle_session(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
) -> impl IntoResponse {
    let state_path = app.paths.project_state(&slug);
    if !state_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }
    let state = match ProjectState::load(&state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(slug, error = %err, "session: ProjectState::load failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("state.json load failed for {slug}: {err}")
                })),
            )
                .into_response();
        }
    };
    if state.team_kind != TeamKind::Flex {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("project {slug} is not a flex project")
            })),
        )
            .into_response();
    }
    let Some(record) = state.sessions.get(&sid) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("session not found: {slug}/{sid}")
            })),
        )
            .into_response();
    };

    let status_events = session_recent_events(&app.paths, &slug, &sid, STATUS_EVENT_LIMIT);
    let display_start = status_events
        .len()
        .saturating_sub(PROJECT_EVENT_DISPLAY_LIMIT);
    let silent = status_events
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
    let badge = status_badge(&state, &status_events, silent);
    let decision_candidates: Vec<String> = scan_candidates(&app.paths, &slug)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();
    let snapshot = load_harness_snapshot(&app, &slug, &sid);
    let cost_label = snapshot
        .as_ref()
        .map(|snap| format!("{:.2}", snap.cost_usd_total))
        .unwrap_or_else(|| format!("{:.2}", state.cost_used_usd));

    let detail = SessionDetail {
        slug: state.slug.clone(),
        sid: sid.clone(),
        team: state.team.clone(),
        kind: "flex".to_string(),
        harness: harness_label(record.harness).to_string(),
        harness_class: harness_class(record.harness),
        tmux_session: record.tmux_session.clone(),
        started_at: record.started_at.to_rfc3339(),
        status_class: badge.css_class(),
        status_label: badge.label(),
        cost_label,
        events: events_to_rows(&status_events[display_start..]),
        outbox: session_outbox_rows(&app.paths, &slug, &sid, DEFAULT_OUTBOX_LIMIT),
        decision_candidates,
        harness_snapshot: snapshot.map(snapshot_view),
    };
    Json(detail).into_response()
}

async fn handle_auth_token(State(app): State<AppState>) -> impl IntoResponse {
    Json(AuthToken {
        wire_token: app.auth.wire_token(),
    })
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

fn snapshot_view(snapshot: HarnessSnapshot) -> HarnessSnapshotView {
    HarnessSnapshotView {
        model: snapshot.model_display_name,
        context_used_pct: format!("{}%", snapshot.context_used_pct),
        cost_usd_total: format!("{:.2}", snapshot.cost_usd_total),
        rate_limit_pct: snapshot
            .rate_limit_pct
            .map(|pct| format!("{pct}%"))
            .unwrap_or_else(|| "—".to_string()),
        captured_at: snapshot.captured_at.to_rfc3339(),
    }
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
