//! v0.8.9 Phase 4 — daemon-wide status aggregate for the unified-shell
//! **cost pill** + **Status view**.
//!
//! - `GET /api/v1/status` → `{daemon_healthy, sessions_live, sessions_idle,
//!   cost_24h_usd, cost_24h_by_vendor, budget_cap_24h}`
//!
//! These are daemon-wide aggregates the CLI computes in-process in
//! `ccteam-cli`'s `run_status`; this handler mirrors that so a browser (the
//! cost pill + Status view) can read the same snapshot. The shape is a
//! **best-effort glance**: a missing daemon / unreadable project degrades to
//! `daemon_healthy: false` + zeroed cost rather than a 500.
//!
//! ## Field sourcing
//!
//! - `daemon_healthy`: [`ccteam_core::check_daemon_health`]`.is_healthy()`
//!   (the same MCP-socket probe `ccteam status` prints).
//! - `sessions_live` / `sessions_idle`: the gateway session map. We prefer the
//!   **live** map ([`Gateway::session_views`]) when [`AppState::gateway`] is
//!   attached (the daemon process), else fall back to the on-disk
//!   [`tracked_chat_sessions`] snapshot (mirrors how `run_status` reads
//!   sessions out-of-process). Split (matching `run_status`): a tracked session
//!   counts **live** when the daemon is healthy, **idle** otherwise — the
//!   gateway carries no finer per-session live/idle bit (`SessionView::status`
//!   is `"live"` for any tracked session), so daemon health is the split.
//! - `cost_24h_usd` + `cost_24h_by_vendor`: sum
//!   [`ccteam_core::queries::cost_summary`]`.cost_24h_usd` and merge
//!   `.cost_24h_by_vendor` across every [`ccteam_core::queries::collect_projects`]
//!   project (same per-project cost surface the workflow cost panel uses).
//! - `budget_cap_24h`: the aggregate 24h cap. Budgets are declared per project
//!   in `workflow.yaml::budgets_v060` (a [`ccteam_cost::Budgets`]); we sum each
//!   project's [`Budgets::aggregated_cost_cap_24h`] across all projects.
//!   `null` when **no** project configures a cap.
//!
//! Merged into the `/api/v1` [`OpenApiRouter`] (see [`super::openapi`]) so it
//! sits behind the same web-token gate as the rest of the resource API — this
//! module writes **zero** auth code.

use std::collections::BTreeMap;

use axum::{extract::State, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::state::AppState;

/// `GET /api/v1/status` response — the daemon-wide snapshot the cost pill +
/// Status view render.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatusResponse {
    /// Whether the daemon MCP socket is reachable (`ccteam status`'s daemon
    /// line). `false` from a standalone web process or when the daemon is down.
    pub daemon_healthy: bool,
    /// Tracked sessions counted **live** (daemon healthy).
    pub sessions_live: u32,
    /// Tracked sessions counted **idle** (daemon down → none are live).
    pub sessions_idle: u32,
    /// Sum of every project's rolling-24h cost (USD).
    pub cost_24h_usd: f64,
    /// Per-vendor breakdown of [`Self::cost_24h_usd`] (keys: `"claude"`,
    /// `"codex"`, …). Empty when no project recorded a vendor-tagged cost in
    /// the window.
    pub cost_24h_by_vendor: BTreeMap<String, f64>,
    /// The aggregate 24h budget cap (USD) summed across every project's
    /// `workflow.yaml` budget. `null` when no project configures a cap.
    pub budget_cap_24h: Option<f64>,
}

/// Minimal `workflow.yaml` projection: only the `budgets_v060` block (the
/// per-vendor [`ccteam_cost::Budgets`]). Deserializing just this field — with
/// `#[serde(default)]` and no other keys — lets the status handler read a
/// project's budget cap without pulling the full `ccteam-flow` `WorkflowSpec`
/// (and its orchestrator) into `ccteam-web`. Matches the on-disk key name
/// (`budgets_v060`, no rename) so it stays in lockstep with the authoring path.
#[derive(Debug, Default, Deserialize)]
struct WorkflowBudgetView {
    #[serde(default)]
    budgets_v060: Option<ccteam_cost::Budgets>,
}

/// Read a project's `workflow.yaml` (nested `.ccteam/` first, then the project
/// root — same precedence as `ccteam_core`'s workflow reader) and return its
/// aggregate 24h budget cap, if any. Any read / parse miss is non-fatal →
/// `None` (a project without a workflow / budget simply contributes no cap).
fn project_budget_cap_24h(project_dir: &std::path::Path) -> Option<f64> {
    let nested = project_dir.join(".ccteam").join("workflow.yaml");
    let direct = project_dir.join("workflow.yaml");
    let path = if nested.exists() { nested } else { direct };
    let raw = std::fs::read_to_string(&path).ok()?;
    let view: WorkflowBudgetView = serde_yaml::from_str(&raw).ok()?;
    view.budgets_v060
        .as_ref()
        .and_then(ccteam_cost::Budgets::aggregated_cost_cap_24h)
}

/// `GET /api/v1/status` — the daemon-wide status snapshot.
///
/// Best-effort: every sub-computation degrades to a zeroed / `false` / `None`
/// contribution rather than erroring, so the Status view always renders.
#[utoipa::path(
    get,
    path = "/api/v1/status",
    tag = "status",
    responses(
        (status = 200, description = "Daemon-wide status snapshot", body = StatusResponse),
    ),
)]
pub(crate) async fn handle_status(State(app): State<AppState>) -> impl IntoResponse {
    let daemon_healthy = ccteam_core::check_daemon_health(&app.paths).is_healthy();

    // ── sessions: prefer the live gateway map; else the on-disk snapshot. ──
    // Either way we only need the COUNT of tracked sessions; the live/idle
    // split is daemon health (the gateway has no finer per-session bit).
    let tracked_count: u32 = if let Some(gw) = app.gateway.as_ref() {
        // session_views() is sync once we hold the lock (no `.await` under it).
        let guard = gw.lock().await;
        guard.session_views().len() as u32
    } else {
        // Standalone web (no daemon gateway): fall back to the persisted route
        // table the CLI's `run_status` reads. A missing / unreadable file is an
        // empty list, never an error.
        ccteam_im::gateway::tracked_chat_sessions(&ccteam_im::default_gateway_state_path())
            .map(|rows| rows.len() as u32)
            .unwrap_or(0)
    };
    let (sessions_live, sessions_idle) = if daemon_healthy {
        (tracked_count, 0)
    } else {
        (0, tracked_count)
    };

    // ── cost: sum each project's 24h cost + merge the per-vendor breakdown. ──
    let mut cost_24h_usd = 0.0_f64;
    let mut cost_24h_by_vendor: BTreeMap<String, f64> = BTreeMap::new();
    let mut budget_cap_24h: Option<f64> = None;
    for project in ccteam_core::queries::collect_projects(&app.paths).unwrap_or_default() {
        let slug = &project.state.slug;
        let summary =
            ccteam_core::queries::cost_summary(slug, &app.paths.progress_jsonl(slug), &app.paths)
                .unwrap_or_default();
        cost_24h_usd += summary.cost_24h_usd;
        for (vendor, usd) in summary.cost_24h_by_vendor {
            *cost_24h_by_vendor.entry(vendor).or_insert(0.0) += usd;
        }
        // Budget caps are additive across projects; a single configured cap
        // flips the aggregate from `None` to `Some`.
        if let Some(cap) = project_budget_cap_24h(&app.paths.project_dir(slug)) {
            budget_cap_24h = Some(budget_cap_24h.unwrap_or(0.0) + cap);
        }
    }

    Json(StatusResponse {
        daemon_healthy,
        sessions_live,
        sessions_idle,
        cost_24h_usd,
        cost_24h_by_vendor,
        budget_cap_24h,
    })
}
