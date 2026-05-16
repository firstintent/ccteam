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
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ccteam_core::{
    ActiveSessionInfo, ArtifactQueueEntry, CostHistoryBucket, HarnessKind, HarnessSnapshot,
    ProjectState, TeamKind, WorkflowSummary,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
        // V0.4.6 F90 — WorkflowView panel endpoints.
        .route(
            "/api/v1/projects/{slug}/artifact_queue",
            get(handle_artifact_queue),
        )
        .route(
            "/api/v1/projects/{slug}/cost_history",
            get(handle_cost_history),
        )
        .route(
            "/api/v1/projects/{slug}/sessions/active",
            get(handle_active_sessions),
        )
        .route(
            "/api/v1/projects/{slug}/jobs/{job_id}/log",
            get(handle_job_log),
        )
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
///
/// V0.4.0 F67: `current_phase` and `decision_candidates` removed —
/// phase machinery was retired in F60. The new `workflow_summary`
/// field replaces both (the SPA wires it in F68; current Rust
/// callers leave it `None` for legacy projects without a
/// workflow.yaml).
#[derive(Serialize)]
pub struct ProjectSummary {
    pub slug: String,
    pub team: String,
    pub kind: String,
    pub is_flex: bool,
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
    pub created_at: String,
    pub sessions: Vec<SessionCard>,
    pub state: serde_json::Value,
    pub events: Vec<EventRow>,
    pub outbox: Vec<OutboxRow>,
    pub workflow_summary: Option<WorkflowSummary>,
}

/// JSON returned by `GET /api/v1/projects/{slug}/sessions/{sid}`.
///
/// Shape matches the V0.3 (retired) askama session template payload
/// minus the auth fields (same rationale as [`ProjectSummary`]).
///
/// V0.4.0 F67: `decision_candidates` removed — phase decision graph
/// was retired in F60.
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
        // V0.4.6 F91 — dashboard cost column reads `cost_total_usd`
        // from `cost_summary` (progress.jsonl + live claude state.json)
        // instead of the now-frozen `state.cost_used_usd`. A missing
        // progress file folds to 0.00 — same shape pre-F91 fresh
        // projects displayed.
        let cost_total = ccteam_core::cost_summary(
            &s.state.slug,
            &app.paths.progress_jsonl(&s.state.slug),
            &app.paths,
        )
        .map(|c| c.cost_total_usd)
        .unwrap_or(0.0);
        rows.push(DashboardRow {
            slug: s.state.slug.clone(),
            team: s.state.team.clone(),
            kind: team_kind_label(s.state.team_kind).to_string(),
            last_event_label,
            badge_class: badge.css_class(),
            badge_label: badge.label(),
            cost_label: format!("{:.2}", cost_total),
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

    let workflow_summary = match ccteam_core::workflow_summary(&slug, &app.paths) {
        Ok(s) => Some(s),
        Err(err) => {
            tracing::warn!(slug, error = %err, "workflow_summary build failed");
            None
        }
    };

    // V0.4.6 F91 — cost_label sources `cost_total_usd` from
    // `cost_summary` (progress.jsonl + live state.json). Pre-F91 this
    // line read `state.cost_used_usd`, which is now frozen.
    let cost_total = ccteam_core::cost_summary(&slug, &app.paths.progress_jsonl(&slug), &app.paths)
        .map(|c| c.cost_total_usd)
        .unwrap_or(0.0);
    let summary = ProjectSummary {
        slug: state.slug.clone(),
        team: state.team.clone(),
        kind: team_kind_label(state.team_kind).to_string(),
        is_flex: state.team_kind == TeamKind::Flex,
        badge_class: badge.css_class(),
        badge_label: badge.label(),
        cost_label: format!("{:.2}", cost_total),
        created_at: state.created_at.to_rfc3339(),
        sessions,
        state: state_value,
        events: event_rows,
        outbox,
        workflow_summary,
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
    let snapshot = load_harness_snapshot(&app, &slug, &sid);
    // V0.4.6 F91 — when no live harness snapshot is available, fall
    // back to `cost_total_usd` from `cost_summary` instead of the
    // frozen `state.cost_used_usd`. Both numbers are project-wide
    // (not session-specific) — session-granular cost is the snapshot's
    // job.
    let cost_label = snapshot
        .as_ref()
        .map(|snap| format!("{:.2}", snap.cost_usd_total))
        .unwrap_or_else(|| {
            let total =
                ccteam_core::cost_summary(&slug, &app.paths.progress_jsonl(&slug), &app.paths)
                    .map(|c| c.cost_total_usd)
                    .unwrap_or(0.0);
            format!("{:.2}", total)
        });

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
            // V0.4.6 F91 — fallback path tracks `cost_total_usd` from
            // `cost_summary` instead of the frozen state field.
            let cost_label = load_harness_snapshot(app, &state.slug, sid)
                .map(|snap| format!("{:.2}", snap.cost_usd_total))
                .unwrap_or_else(|| {
                    let total = ccteam_core::cost_summary(
                        &state.slug,
                        &app.paths.progress_jsonl(&state.slug),
                        &app.paths,
                    )
                    .map(|c| c.cost_total_usd)
                    .unwrap_or(0.0);
                    format!("{:.2}", total)
                });
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

// ---------------- V0.4.6 F90 — WorkflowView panel endpoints ----------------

/// `GET /api/v1/projects/<slug>/artifact_queue`
///
/// Response: `Vec<ArtifactQueueEntry>` — one entry per
/// `Trigger::Watch(<path>)` declared in `workflow.yaml`. Returns an
/// empty array (200 OK) for legacy projects or workflows without
/// watch triggers.
async fn handle_artifact_queue(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    if !app.paths.project_state(&slug).exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }
    match ccteam_core::artifact_queue(&slug, &app.paths) {
        Ok(entries) => Json(entries).into_response(),
        Err(err) => {
            tracing::error!(slug, %err, "artifact_queue build failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// Query parameters for `cost_history`. `window=24h` (default) or
/// `window=7d` per PRD §F90. Anything else falls back to `24h`.
#[derive(Debug, Deserialize)]
pub struct CostHistoryQuery {
    #[serde(default)]
    pub window: Option<String>,
}

/// JSON payload returned by `GET /api/v1/projects/<slug>/cost_history`.
#[derive(Serialize)]
pub struct CostHistoryResponse {
    pub window: String,
    pub buckets: Vec<CostHistoryBucket>,
}

/// `GET /api/v1/projects/<slug>/cost_history?window=24h|7d`
///
/// Returns hour-bucketed `agent_done.cost_usd` totals for the given
/// rolling window. Bucket count = `window_hours`; sparse hours appear
/// with `cost_usd = 0.0` so the SPA sparkline has even x-axis spacing.
async fn handle_cost_history(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<CostHistoryQuery>,
) -> impl IntoResponse {
    if !app.paths.project_state(&slug).exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }
    let raw = q.window.as_deref().unwrap_or("24h");
    let (window_hours, normalized) = match raw {
        "7d" | "168h" => (24 * 7u32, "7d"),
        _ => (24u32, "24h"),
    };
    match ccteam_core::cost_history_buckets(&slug, &app.paths, window_hours) {
        Ok(buckets) => Json(CostHistoryResponse {
            window: normalized.to_string(),
            buckets,
        })
        .into_response(),
        Err(err) => {
            tracing::error!(slug, %err, "cost_history build failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/projects/<slug>/sessions/active`
///
/// Returns one entry per still-open `agent_spawn` (no matching
/// `agent_done`), decorated with `state.json` live data (cwd, cost).
async fn handle_active_sessions(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    if !app.paths.project_state(&slug).exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }
    match ccteam_core::active_sessions(&slug, &app.paths) {
        Ok(sessions) => Json(sessions).into_response(),
        Err(err) => {
            tracing::error!(slug, %err, "active_sessions build failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

/// Query parameters for `jobs/<job_id>/log`. `tail` is the line count;
/// clamped to `[1, 5000]` server-side. Default 200.
#[derive(Debug, Deserialize)]
pub struct JobLogQuery {
    #[serde(default)]
    pub tail: Option<u32>,
}

/// JSON payload returned by `GET /api/v1/projects/<slug>/jobs/<job_id>/log`.
#[derive(Serialize)]
pub struct JobLogResponse {
    pub job_id: String,
    /// Total line count in `output.log` (so the SPA can render
    /// "showing last N of M" hints).
    pub total_lines: u64,
    /// Trailing `tail` lines, joined with `\n`. Empty string when
    /// `output.log` is missing.
    pub tail: String,
}

/// `GET /api/v1/projects/<slug>/jobs/<job_id>/log?tail=200`
///
/// Read-only access to a claude bg job's `output.log`. Read-only, no
/// PTY — the SPA's `FailureInspector` modal just displays the text.
/// Project ownership is **not** validated against the job_id (the
/// state.json holds the cwd, but probing it adds I/O for no gain);
/// the `<slug>` in the URL is used only for the 404 short-circuit on
/// unknown projects.
async fn handle_job_log(
    State(app): State<AppState>,
    Path((slug, job_id)): Path<(String, String)>,
    Query(q): Query<JobLogQuery>,
) -> impl IntoResponse {
    if !app.paths.project_state(&slug).exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }
    // Reject obvious path-traversal attempts. job_id is a hex-ish
    // hash on the wire; `/` and `..` should never appear.
    if job_id.contains('/') || job_id.contains("..") || job_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid job_id"})),
        )
            .into_response();
    }
    let tail = q.tail.unwrap_or(200);
    match ccteam_core::job_log_tail(&job_id, tail) {
        Ok((body, total_lines)) => Json(JobLogResponse {
            job_id,
            total_lines,
            tail: body,
        })
        .into_response(),
        Err(err) => {
            tracing::error!(%job_id, %err, "job_log read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

// Silence "unused" lint while the imports are kept in scope for
// downstream consumers (the SPA-side shapes mirror these structs 1:1
// in `crates/ccteam-web/web/src/lib/workflowPanels.ts`).
#[allow(dead_code)]
fn _workflow_panel_dto_anchor(
    _a: ArtifactQueueEntry,
    _b: ActiveSessionInfo,
    _c: CostHistoryBucket,
) {
}
