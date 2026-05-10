//! `GET /session/<slug>/<sid>` — flex session detail page.
//!
//! Read path is session-scoped: state registry lookup, per-session
//! progress tail, optional harness snapshot, and project-level outbox.
//! Write forms reuse the existing project-level action endpoints until
//! core exposes session-level inbox writers.

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
    events_to_rows, session_outbox_rows, session_recent_events, DEFAULT_OUTBOX_LIMIT,
    PROJECT_EVENT_DISPLAY_LIMIT, STATUS_EVENT_LIMIT,
};
use crate::state::AppState;
use crate::status::status_badge;
use crate::views::{HarnessSnapshotView, HtmlTemplate, SessionTemplate};

pub fn router() -> Router<AppState> {
    Router::new().route("/session/{slug}/{sid}", get(handle_session))
}

async fn handle_session(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
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
    if state.team_kind != TeamKind::Flex {
        return (
            StatusCode::NOT_FOUND,
            format!("project {slug} is not a flex project"),
        )
            .into_response();
    }
    let Some(record) = state.sessions.get(&sid) else {
        return (
            StatusCode::NOT_FOUND,
            format!("session not found: {slug}/{sid}"),
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

    let tpl = SessionTemplate {
        version: env!("CARGO_PKG_VERSION"),
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
        auth_enabled: app.auth.enabled,
        auth_wire_token: app.auth.wire_token(),
        decision_candidates,
        harness_snapshot: snapshot.map(snapshot_view),
    };
    HtmlTemplate(tpl).into_response()
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
