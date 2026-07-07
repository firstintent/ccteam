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

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::{deny_non_admin, Identity};
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
    /// v0.8.18 柱1 — one row per LIVE gateway session (the fleet view): the
    /// loop-ops console skeleton. Empty on the standalone (no-gateway) web
    /// path. The loop version grows this same row with oracle/gate columns.
    #[serde(default)]
    pub sessions: Vec<SessionCostRow>,
}

/// v0.8.18 柱1 — one live session's fleet-view row. `cost_usd` is priced
/// **deterministically per-turn**: each `chat_turn_completed` event mirrored
/// to `progress.jsonl` carries its turn's canonical `model` (the stream-json
/// translator fills it from the transcript's `message.model`), so the row
/// sums `estimate_cost(usage, vendor, model)` over the session's turns.
///
/// `cost_usd` is `Some` only when at least one turn priced against a
/// table-matched model; it is `None` (rendered "—" in the UI) when the
/// session has priced no turn — e.g. a session that emitted no usage yet,
/// a tmux session (whose hook path carries no per-turn usage/model), or
/// turns whose model is not in the pricing table. There is **no silent
/// fallback** to a wrong model's rate. `unpriced_turns` exposes how many
/// turns were skipped for lacking a priceable model.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionCostRow {
    /// Gateway session id (`s{n}`).
    pub sid: String,
    /// Project slug the session runs in.
    pub project: String,
    /// Agent role (empty for a roleless session).
    pub role: String,
    /// Vendor (`claude` / `codex`).
    pub vendor: String,
    /// Cheap liveness label from the gateway (`live`).
    pub status: String,
    /// Deterministic per-turn accrued cost (USD), or `null` when nothing
    /// priceable — see the struct doc. `null` renders "—", never a faked 0.
    pub cost_usd: Option<f64>,
    /// Count of turns skipped because their model wasn't in the pricing
    /// table (exposes partial pricing; `0` when fully priced / no turns).
    #[serde(default)]
    pub unpriced_turns: usize,
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
pub(crate) async fn handle_status(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    // v0.8.18 档1 — the daemon-wide status/cost fleet is an operator/admin view.
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let daemon_healthy = ccteam_core::check_daemon_health(&app.paths).is_healthy();

    // ── sessions: prefer the live gateway map; else the on-disk snapshot. ──
    // Either way we only need the COUNT of tracked sessions; the live/idle
    // split is daemon health (the gateway has no finer per-session bit).
    let mut session_rows: Vec<SessionCostRow> = Vec::new();
    let tracked_count: u32 = if let Some(gw) = app.gateway.as_ref() {
        // session_views() is sync once we hold the lock (no `.await` under it).
        let views = {
            let guard = gw.lock().await;
            guard.session_views()
        };
        // v0.8.18 柱1 — decorate each live session with its best-effort cost.
        session_rows = build_session_cost_rows(&app.paths, &views);
        views.len() as u32
    } else {
        // Standalone web (no daemon gateway): fall back to the persisted route
        // table the CLI's `run_status` reads. A missing / unreadable file is an
        // empty list, never an error. No live map ⇒ no per-session fleet rows.
        ccteam_im::gateway::tracked_chat_sessions(&app.paths.root)
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
        sessions: session_rows,
    })
    .into_response()
}

/// Per-session priced accumulator — sum of priced turns + a count of turns
/// skipped for lacking a table-matched model.
#[derive(Default, Clone, Copy)]
struct SessionPriced {
    cost_usd: f64,
    priced_turns: usize,
    unpriced_turns: usize,
}

/// Build one [`SessionCostRow`] per live gateway session, pricing each
/// **deterministically per-turn** by the turn's own canonical `model`. Reads
/// `progress.jsonl` ONCE per project that has a live session (not every
/// project); for each `chat_turn_completed` event it prices that turn's
/// `usage × model` via [`ccteam_cost::estimate_cost`] and sums the `Some`
/// results per sid. A turn whose `model` is absent / not in the table is
/// skipped (counted) — there is no fallback to a wrong rate. A session with
/// zero priced turns yields `cost_usd: None` (rendered "—").
fn build_session_cost_rows(
    paths: &ccteam_core::CcteamPaths,
    views: &[ccteam_im::gateway::SessionView],
) -> Vec<SessionCostRow> {
    use std::collections::{BTreeSet, HashMap};

    // Read progress only for projects with a live session.
    let live_projects: BTreeSet<&str> = views.iter().map(|v| v.project.as_str()).collect();
    // Vendor per sid (from the view) so each turn prices against the
    // session's vendor table; default Claude for the rare missing case.
    let vendor_by_sid: HashMap<&str, ccteam_cost::Vendor> = views
        .iter()
        .map(|v| (v.sid.as_str(), vendor_from_str(&v.vendor)))
        .collect();

    let mut priced_by_sid: HashMap<String, SessionPriced> = HashMap::new();
    for slug in live_projects {
        let events =
            ccteam_core::progress::read_all_events(&paths.progress_jsonl(slug)).unwrap_or_default();
        for ev in &events {
            if ev.get("event").and_then(|v| v.as_str())
                != Some(ccteam_core::progress::CHAT_TURN_COMPLETED)
            {
                continue;
            }
            let Some(sid) = ev.get("sid").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(usage) = ev.get("usage").and_then(|u| {
                serde_json::from_value::<ccteam_cost::UnifiedTokenUsage>(u.clone()).ok()
            }) else {
                continue;
            };
            // The turn's canonical model (written by the pump from the
            // stream-json translator). Absent → unpriceable (exposed).
            let model = ev.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let vendor = vendor_by_sid
                .get(sid)
                .copied()
                .unwrap_or(ccteam_cost::Vendor::Claude);
            let acc = priced_by_sid.entry(sid.to_string()).or_default();
            match ccteam_cost::estimate_cost(&usage, vendor, model) {
                Some(cost) => {
                    acc.cost_usd += cost;
                    acc.priced_turns += 1;
                }
                None => acc.unpriced_turns += 1,
            }
        }
    }

    views
        .iter()
        .map(|v| {
            let acc = priced_by_sid.get(&v.sid).copied().unwrap_or_default();
            // `Some` only when a real, table-matched model priced a turn.
            let cost_usd = (acc.priced_turns > 0).then_some(acc.cost_usd);
            SessionCostRow {
                sid: v.sid.clone(),
                project: v.project.clone(),
                role: v.role.clone(),
                vendor: v.vendor.clone(),
                status: v.status.clone(),
                cost_usd,
                unpriced_turns: acc.unpriced_turns,
            }
        })
        .collect()
}

/// Map a `SessionView` vendor token to the pricing [`ccteam_cost::Vendor`]
/// (defaulting to Claude for an unknown token — the dominant vendor).
fn vendor_from_str(vendor: &str) -> ccteam_cost::Vendor {
    match vendor.trim().to_ascii_lowercase().as_str() {
        "codex" => ccteam_cost::Vendor::Codex,
        _ => ccteam_cost::Vendor::Claude,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &std::path::Path) -> ccteam_core::CcteamPaths {
        ccteam_core::CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    fn view(sid: &str, project: &str, vendor: &str) -> ccteam_im::gateway::SessionView {
        ccteam_im::gateway::SessionView {
            sid: sid.into(),
            project: project.into(),
            role: "cto".into(),
            vendor: vendor.into(),
            permission_mode: "skip".into(),
            protocol: "stream-json".into(),
            host: "local".into(),
            current: true,
            status: "live".into(),
            last_activity_seconds: None,
            created_at: String::new(),
            last_active: String::new(),
        }
    }

    #[test]
    fn vendor_from_str_maps_tokens() {
        assert_eq!(vendor_from_str("claude"), ccteam_cost::Vendor::Claude);
        assert_eq!(vendor_from_str("Codex"), ccteam_cost::Vendor::Codex);
        assert_eq!(vendor_from_str("weird"), ccteam_cost::Vendor::Claude);
    }

    #[test]
    fn build_session_cost_rows_prices_per_turn_canonical_model() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        // Two turns on DIFFERENT canonical models — they must price at their
        // OWN rate, not one collapsed fallback. 1M output @ opus-4-8 = $25,
        // 1M output @ sonnet-4-6 = $15 → sum $40 (not 2×fallback).
        let m_out = ccteam_cost::UnifiedTokenUsage {
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let opus = ccteam_core::progress::build_chat_turn_completed_event(
            "cto",
            "s1",
            "t1",
            &m_out,
            Some("claude-opus-4-8"),
        );
        let sonnet = ccteam_core::progress::build_chat_turn_completed_event(
            "cto",
            "s1",
            "t2",
            &m_out,
            Some("claude-sonnet-4-6"),
        );
        ccteam_core::progress::append_event(&paths.progress_jsonl("demo"), &opus).unwrap();
        ccteam_core::progress::append_event(&paths.progress_jsonl("demo"), &sonnet).unwrap();

        let rows = build_session_cost_rows(&paths, &[view("s1", "demo", "claude")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sid, "s1");
        let cost = rows[0].cost_usd.expect("priced");
        assert!(
            (cost - 40.0).abs() < 0.01,
            "per-turn canonical pricing: opus $25 + sonnet $15 = $40, got {cost}",
        );
        assert_eq!(rows[0].unpriced_turns, 0);
    }

    #[test]
    fn build_session_cost_rows_none_when_no_priceable_turn() {
        // A live session that has emitted no chat_turn_completed usage yet is
        // still listed, at cost_usd: None → rendered "—" (never a faked 0).
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let rows = build_session_cost_rows(&paths, &[view("s9", "demo", "claude")]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].cost_usd.is_none(), "no turns ⇒ None, not 0.0");
        assert_eq!(rows[0].unpriced_turns, 0);
    }

    #[test]
    fn build_session_cost_rows_exposes_unpriced_synthetic_model() {
        // A turn whose model isn't in the table (e.g. `<synthetic>`) is
        // skipped + counted, NOT billed at a fallback rate. With ONLY such a
        // turn the session is unpriced (None) with unpriced_turns == 1.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let m_out = ccteam_cost::UnifiedTokenUsage {
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let synthetic = ccteam_core::progress::build_chat_turn_completed_event(
            "cto",
            "s1",
            "t1",
            &m_out,
            Some("<synthetic>"),
        );
        ccteam_core::progress::append_event(&paths.progress_jsonl("demo"), &synthetic).unwrap();

        let rows = build_session_cost_rows(&paths, &[view("s1", "demo", "claude")]);
        assert!(rows[0].cost_usd.is_none(), "unknown model must not price");
        assert_eq!(rows[0].unpriced_turns, 1, "the synthetic turn is exposed");
    }
}
