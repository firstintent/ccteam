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
    Extension, Json,
};
use ccteam_core::{
    cost_history_buckets, cost_summary, ActiveSessionInfo, ArtifactQueueEntry, CostHistoryBucket,
    HarnessKind, ProjectState, TeamKind, WorkflowSummary,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::queries::{
    events_to_rows, outbox_rows, recent_event_summary, slug_recent_events, DEFAULT_OUTBOX_LIMIT,
    PROJECT_EVENT_DISPLAY_LIMIT, STATUS_EVENT_LIMIT,
};
use crate::state::AppState;
use crate::status::status_badge;
use crate::views::{DashboardRow, EventRow, HarnessSnapshotView, OutboxRow};

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
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
    pub created_at: String,
    pub state: serde_json::Value,
    pub events: Vec<EventRow>,
    pub outbox: Vec<OutboxRow>,
    pub workflow_summary: Option<WorkflowSummary>,
    /// V0.6.0 Wave 3 F112 — per-vendor 24h cost breakdown
    /// (`{"claude": <f64>, "codex": <f64>}`). Pre-V0.6 `agent_done`
    /// events lacked a `vendor` field and contribute only to the
    /// aggregate `cost_label`; this map is empty when the project
    /// hasn't run any V0.6+ vendor-tagged turns yet.
    #[serde(default)]
    pub cost_24h_by_vendor: std::collections::BTreeMap<String, f64>,
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
#[derive(Serialize, ToSchema)]
pub struct AuthToken {
    pub wire_token: Option<String>,
}

/// `GET /api/v1/projects` → dashboard rows.
#[utoipa::path(
    get,
    path = "/api/v1/projects",
    tag = "projects",
    responses(
        (status = 200, description = "Registered projects (dashboard rows)", body = Vec<DashboardRow>),
        (status = 500, description = "Project collect failed"),
    ),
)]
pub(crate) async fn handle_projects(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
) -> impl IntoResponse {
    match build_projects(&app, &identity) {
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

fn build_projects(
    app: &AppState,
    identity: &crate::auth::Identity,
) -> anyhow::Result<Vec<DashboardRow>> {
    let summaries = ccteam_core::collect_projects(&app.paths)?;
    let mut rows = Vec::with_capacity(summaries.len());
    for s in summaries {
        // v0.8.18 档1 — per-user project isolation: the admin sees every
        // project; a tenant sees only the ones it owns (`web:<id>`).
        if !identity.can_see_owner(s.state.owner.as_deref()) {
            continue;
        }
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
        let cost_total = cost_summary(
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

/// `GET /api/v1/me` response — the authenticated caller's identity, so the SPA
/// shows admin-only surfaces (user management, IM credentials, hosts, status)
/// only to the owner.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    /// `"admin"` for the owner (bootstrap token), else the tenant id.
    pub id: String,
    /// Display handle: `"owner"` for the admin, else the tenant's handle.
    pub handle: String,
    pub is_admin: bool,
}

/// `GET /api/v1/me` — who am I? Lets the SPA branch the UI by identity.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    tag = "status",
    responses((status = 200, description = "The caller's identity", body = MeResponse)),
)]
pub(crate) async fn handle_me(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
) -> impl IntoResponse {
    let handle = if identity.is_admin {
        "owner".to_string()
    } else {
        ccteam_core::tenants::TenantRegistry::load(&app.paths.tenants_json())
            .by_id(&identity.id)
            .map(|t| t.handle.clone())
            .unwrap_or_else(|| identity.id.clone())
    };
    Json(MeResponse {
        id: identity.id.clone(),
        handle,
        is_admin: identity.is_admin,
    })
}

/// v0.8.18 档1 — whether `identity` may see project `slug`. Project is the unit
/// of ownership; a session's visibility derives from its project's. Reads the
/// project's `state.json` owner: the admin sees every project, a tenant only
/// one it owns (`web:<id>`). A missing/unreadable project is not visible to a
/// tenant. The single source the session endpoints consult to scope by project.
pub(crate) fn can_see_project(
    app: &AppState,
    identity: &crate::auth::Identity,
    slug: &str,
) -> bool {
    if identity.is_admin {
        return true;
    }
    match ProjectState::load(&app.paths.project_state(slug)) {
        Ok(state) => identity.can_see_owner(state.owner.as_deref()),
        Err(_) => false,
    }
}

/// `GET /api/v1/projects/{slug}` → project detail summary.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Project summary `{slug, team, kind, state, events, outbox, workflow_summary, ...}`", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "state.json load failed"),
    ),
)]
pub(crate) async fn handle_project(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
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

    // v0.8.18 档1 — per-user isolation: a tenant may only see its own projects.
    // 404 (not 403) so an unowned slug's existence isn't revealed.
    if !identity.can_see_owner(state.owner.as_deref()) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("project not found: {slug}")})),
        )
            .into_response();
    }

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
    // V0.6.0 Wave 3 F112 — also surface `cost_24h_by_vendor` to drive
    // the SPA's per-vendor split and `/ccteam-advise` UI.
    let cost =
        cost_summary(&slug, &app.paths.progress_jsonl(&slug), &app.paths).unwrap_or_default();
    let cost_total = cost.cost_total_usd;
    let cost_24h_by_vendor = cost.cost_24h_by_vendor.clone();
    let summary = ProjectSummary {
        slug: state.slug.clone(),
        team: state.team.clone(),
        kind: team_kind_label(state.team_kind).to_string(),
        badge_class: badge.css_class(),
        badge_label: badge.label(),
        cost_label: format!("{:.2}", cost_total),
        created_at: state.created_at.to_rfc3339(),
        state: state_value,
        events: event_rows,
        outbox,
        workflow_summary,
        cost_24h_by_vendor,
    };
    Json(summary).into_response()
}

/// `GET /api/v1/projects/{slug}/sessions/{sid}` → workflow session detail.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/sessions/{sid}",
    tag = "projects",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("sid" = String, Path, description = "Workflow session id (from `agent_spawn`)"),
    ),
    responses(
        (status = 200, description = "Session detail `{slug, sid, harness, events, outbox, ...}`", body = serde_json::Value),
        (status = 404, description = "Unknown project or session"),
        (status = 500, description = "progress.jsonl read failed"),
    ),
)]
pub(crate) async fn handle_session(
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
    // Synthesize a SessionDetail from the `agent_spawn` event in
    // progress.jsonl so the SPA can deep-link into sessions.
    build_workflow_session_detail(&app, &state, &slug, &sid)
}

/// Build a SessionDetail for a workflow/multi_workflow project session.
/// Sources:
///
/// - progress.jsonl `agent_spawn` event for `<sid>` provides `role` +
///   `started_at` (+ `job_id` when present).
/// - `~/.claude/jobs/<job_id>/state.json` (when job_id known) provides
///   model + live cost.
///
/// `harness_snapshot` is left `null` — workflow projects don't write
/// the F90 mirror file, and the SPA hides the HarnessPanel + terminal
/// mount for workflow sessions. Outbox / event rows come from the
/// project-wide progress.jsonl, filtered down to entries carrying the
/// matching `session_id`.
fn build_workflow_session_detail(
    app: &AppState,
    state: &ProjectState,
    slug: &str,
    sid: &str,
) -> axum::response::Response {
    let progress_path = app.paths.progress_jsonl(slug);
    if !progress_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("session not found: {slug}/{sid}")
            })),
        )
            .into_response();
    }
    let events: Vec<serde_json::Value> = match std::fs::read_to_string(&progress_path) {
        Ok(body) => body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(err) => {
            tracing::error!(slug, sid, %err, "workflow session: progress.jsonl read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("progress.jsonl read failed for {slug}: {err}")
                })),
            )
                .into_response();
        }
    };

    // Find the `agent_spawn` event for <sid>. If we can't find one,
    // 404 — synthesising a SessionDetail from later events would lose
    // the role / started_at anchor the SPA expects.
    let Some(spawn) = events.iter().find(|e| {
        e.get("event").and_then(|s| s.as_str()) == Some("agent_spawn")
            && e.get("session_id").and_then(|s| s.as_str()) == Some(sid)
    }) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("session not found: {slug}/{sid}")
            })),
        )
            .into_response();
    };
    let role = spawn
        .get("role")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let started_at = spawn
        .get("ts")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();

    // Probe state.json for live cost when we know the job_id. We
    // re-use the active_sessions row for this session if it's still
    // open — that's already decorated with model + cwd + cost.
    let live = ccteam_core::active_sessions(slug, &app.paths)
        .ok()
        .and_then(|rows| rows.into_iter().find(|r| r.session_id == sid));
    let cost_label = live
        .as_ref()
        .map(|r| format!("{:.2}", r.cost_usd))
        .unwrap_or_else(|| "0.00".to_string());
    // `tmux_session` is the project-level session (workflow project
    // shares one tmux session across spawns). `harness` is "claude"
    // since workflow spawns go through `claude --bg --agent <role>`.
    let tmux_session = state.tmux_session.clone();

    // Filter events down to those carrying this session_id (covers
    // agent_spawn / agent_done / any optional ad-hoc events that
    // happen to be sid-scoped). Returns the project tail when no
    // sid-scoped rows exist so the SPA's EventsLive isn't empty.
    let sid_events: Vec<serde_json::Value> = events
        .iter()
        .filter(|e| e.get("session_id").and_then(|s| s.as_str()) == Some(sid))
        .cloned()
        .collect();
    let event_rows = if sid_events.is_empty() {
        events_to_rows(&events[events.len().saturating_sub(PROJECT_EVENT_DISPLAY_LIMIT)..])
    } else {
        events_to_rows(&sid_events[sid_events.len().saturating_sub(PROJECT_EVENT_DISPLAY_LIMIT)..])
    };

    // Status: still-open spawn → "ok" (live); otherwise default to
    // "ok" too — workflow `agent_done.status` already maps via the
    // project-wide badge. Fine-grained per-session badging is a flex
    // feature we deliberately don't replicate here.
    let label: &'static str = if live.is_some() { "live" } else { "done" };
    let cls: &'static str = if live.is_some() {
        "badge-ok"
    } else {
        "badge-neutral"
    };

    let kind = team_kind_label(state.team_kind).to_string();
    let detail = SessionDetail {
        slug: slug.to_string(),
        sid: sid.to_string(),
        team: state.team.clone(),
        kind,
        harness: format!(
            "claude{}",
            if role.is_empty() {
                String::new()
            } else {
                format!("/{role}")
            }
        ),
        harness_class: "harness-claude",
        tmux_session,
        started_at,
        status_class: cls,
        status_label: label,
        cost_label,
        events: event_rows,
        outbox: outbox_rows(&app.paths, slug, DEFAULT_OUTBOX_LIMIT),
        harness_snapshot: None,
    };
    Json(detail).into_response()
}

/// `GET /api/v1/auth/token` → the wire token (or null when auth is off).
#[utoipa::path(
    get,
    path = "/api/v1/auth/token",
    tag = "auth",
    responses((status = 200, description = "`{wire_token: \"ccteam:<hex>\" | null}`", body = AuthToken)),
)]
pub(crate) async fn handle_auth_token(State(app): State<AppState>) -> impl IntoResponse {
    Json(AuthToken {
        wire_token: app.auth.wire_token(),
    })
}

fn team_kind_label(kind: TeamKind) -> &'static str {
    match kind {
        TeamKind::Workflow => "workflow",
        TeamKind::MultiWorkflow => "multi_workflow",
    }
}

// W5 harness facet: HarnessKind → label / css class. Retained for the
// W5b/W5c session pages, which re-key the session detail off the harness
// vendor. Not wired into the current workflow-only session detail.
#[allow(dead_code)]
fn harness_label(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "claude",
        HarnessKind::Codex => "codex",
    }
}

#[allow(dead_code)]
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
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/artifact_queue",
    tag = "workflow",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Watch-trigger artifact queue entries", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Build failed"),
    ),
)]
pub(crate) async fn handle_artifact_queue(
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

/// `GET /api/v1/projects/<slug>/artifact_status`
///
/// Response: `Vec<ArtifactStatusGroup>` — one entry per non-infra
/// subdir of `<project>/.ccteam/` containing `*.json` files with a
/// top-level string `.status` field. Counts are grouped by status
/// value. Empty result is valid (200 OK).
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/artifact_status",
    tag = "workflow",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Artifact status groups (counts by `.status`)", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Build failed"),
    ),
)]
pub(crate) async fn handle_artifact_status(
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
    match ccteam_core::artifact_status(&slug, &app.paths) {
        Ok(groups) => Json(groups).into_response(),
        Err(err) => {
            tracing::error!(slug, %err, "artifact_status build failed");
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
#[derive(Debug, Deserialize, utoipa::IntoParams)]
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
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/cost_history",
    tag = "workflow",
    params(
        ("slug" = String, Path, description = "Project slug"),
        CostHistoryQuery,
    ),
    responses(
        (status = 200, description = "Hour-bucketed cost `{window, buckets[]}`", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Build failed"),
    ),
)]
pub(crate) async fn handle_cost_history(
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
    match cost_history_buckets(&slug, &app.paths, window_hours) {
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
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/sessions/active",
    tag = "workflow",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Still-open `agent_spawn` sessions (live cost/cwd)", body = serde_json::Value),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Build failed"),
    ),
)]
pub(crate) async fn handle_active_sessions(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    // v0.8.18 档1 — per-user isolation: 404 a project the caller can't see
    // (covers both the unknown-project and the not-visible-to-tenant cases).
    if !app.paths.project_state(&slug).exists() || !can_see_project(&app, &identity, &slug) {
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

/// V0.5.1 F103a — aggregate active sessions row decorated with the
/// owning project slug. Shape on the wire is
/// `ActiveSessionInfo & { slug: String }` flattened, so the SPA can
/// route directly to `/p/<slug>/s/<session_id>` without a second
/// lookup.
#[derive(Serialize)]
pub struct ActiveSessionWithSlug {
    pub slug: String,
    #[serde(flatten)]
    pub session: ActiveSessionInfo,
}

/// `GET /api/v1/sessions/active`
///
/// V0.5.1 F103a — flattens [`ccteam_core::active_sessions`] across
/// every registered project so the SPA's `/sessions` top-level tab
/// can render one global card list without coordinating per-project
/// fetches. Per-project errors are logged (`tracing::warn`) but do
/// not fail the request — the response carries every project's rows
/// that loaded successfully.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/active",
    tag = "sessions",
    responses(
        (status = 200, description = "Active sessions across every project, each carrying its `slug`", body = serde_json::Value),
        (status = 500, description = "collect_projects failed"),
    ),
)]
pub(crate) async fn handle_active_sessions_aggregate(
    State(app): State<AppState>,
) -> impl IntoResponse {
    let summaries = match ccteam_core::collect_projects(&app.paths) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(?err, "GET /api/v1/sessions/active collect_projects failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let mut out: Vec<ActiveSessionWithSlug> = Vec::new();
    for s in summaries {
        let slug = s.state.slug;
        match ccteam_core::active_sessions(&slug, &app.paths) {
            Ok(rows) => {
                for r in rows {
                    out.push(ActiveSessionWithSlug {
                        slug: slug.clone(),
                        session: r,
                    });
                }
            }
            Err(err) => {
                tracing::warn!(
                    slug = %slug,
                    %err,
                    "aggregate active_sessions: per-project build failed",
                );
            }
        }
    }
    Json(out).into_response()
}

/// Query parameters for `jobs/<job_id>/log`. `tail` is the line count;
/// clamped to `[1, 5000]` server-side. Default 200.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct JobLogQuery {
    #[serde(default)]
    pub tail: Option<u32>,
}

/// JSON payload returned by `GET /api/v1/projects/<slug>/jobs/<job_id>/log`.
#[derive(Serialize, ToSchema)]
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
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/jobs/{job_id}/log",
    tag = "workflow",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("job_id" = String, Path, description = "Claude bg job id"),
        JobLogQuery,
    ),
    responses(
        (status = 200, description = "Job log tail `{job_id, total_lines, tail}`", body = JobLogResponse),
        (status = 400, description = "Invalid job_id"),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Log read failed"),
    ),
)]
pub(crate) async fn handle_job_log(
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
