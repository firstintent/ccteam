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
//! - `cost_24h_usd` + `cost_24h_by_vendor`: sum the shared incremental
//!   progress projection across every registered project.
//! - `budget_cap_24h`: the aggregate 24h cap. Budgets are declared per project
//!   in `workflow.yaml::budgets_v060` (a [`ccteam_cost::Budgets`]); we sum each
//!   project's [`Budgets::aggregated_cost_cap_24h`] across all projects.
//!   `null` when **no** project configures a cap.
//!
//! Merged into the `/api/v1` [`OpenApiRouter`] (see [`super::openapi`]) so it
//! sits behind the same web-token gate as the rest of the resource API.
//! Daemon aggregates stay global; tenant session rows are filtered through the
//! project-owner ACL.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

type StatusResult = Result<StatusResponse, String>;

#[derive(Clone, Default)]
pub(crate) struct StatusSingleflight {
    inner: Arc<Mutex<SingleflightState>>,
}

#[derive(Default)]
struct SingleflightState {
    next_id: u64,
    in_flight: Option<StatusFlight>,
}

struct StatusFlight {
    id: u64,
    result_tx: tokio::sync::watch::Sender<Option<StatusResult>>,
}

impl StatusSingleflight {
    /// Run one blocking status computation for every overlapping set of
    /// callers. The producer lives in a detached task, so dropping any waiter
    /// cannot cancel it or leave `in_flight` occupied forever.
    async fn get_or_compute<F>(&self, compute: F) -> StatusResult
    where
        F: FnOnce() -> StatusResponse + Send + 'static,
    {
        let mut result_rx = {
            let mut state = self.lock_state();
            if let Some(flight) = &state.in_flight {
                flight.result_tx.subscribe()
            } else {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                let (result_tx, result_rx) = tokio::sync::watch::channel(None);
                state.in_flight = Some(StatusFlight {
                    id,
                    result_tx: result_tx.clone(),
                });

                let singleflight = self.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(compute)
                        .await
                        .map_err(|err| format!("status aggregation task failed: {err}"));
                    // Clear before publishing while holding one non-async lock:
                    // existing subscribers receive this result, while a caller
                    // arriving after completion starts a fresh computation.
                    let mut state = singleflight.lock_state();
                    if state.in_flight.as_ref().map(|flight| flight.id) == Some(id) {
                        state.in_flight = None;
                    }
                    let _ = result_tx.send(Some(result));
                });
                result_rx
            }
        };

        let result = match result_rx.wait_for(Option::is_some).await {
            Ok(result) => result
                .clone()
                .expect("watch predicate guarantees a status result"),
            Err(_) => Err("status aggregation ended without a result".to_string()),
        };
        result
    }

    fn lock_state(&self) -> MutexGuard<'_, SingleflightState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn subscriber_count(&self) -> usize {
        self.lock_state()
            .in_flight
            .as_ref()
            .map(|flight| flight.result_tx.receiver_count())
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn is_in_flight(&self) -> bool {
        self.lock_state().in_flight.is_some()
    }
}

/// `GET /api/v1/status` response — the daemon-wide snapshot the cost pill +
/// Status view render.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatusResponse {
    /// Monotonic revision of the projection snapshot. `null` while startup
    /// hydration is in progress, which also suppresses ETag/304 responses.
    #[serde(default)]
    pub version: Option<u64>,
    /// Startup hydration is still folding one or more project journals.
    #[serde(default)]
    pub warming_up: bool,
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
    /// v0.9.0 W2 (F2/F7) — delegation observability: current durable watches +
    /// the trailing-24h notification / denial counts.
    #[serde(default)]
    pub delegations: DelegationSummary,
}

/// v0.9.0 W2 (F2/F7) — the `delegations` block of `GET /status`: the A2A
/// delegation runtime's observability counters.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct DelegationSummary {
    /// Durable completion watches currently armed in the gateway's in-memory
    /// mirror (hydrated from `delegation.json` during startup reconciliation).
    pub active_watches: u32,
    /// Completion notifications delivered in the trailing 24h
    /// (`delegation_notified` progress events).
    pub notified_24h: u32,
    /// Delegations denied by a guardrail in the trailing 24h
    /// (`delegation_denied` progress events).
    pub denied_24h: u32,
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
    /// Vendor (`claude` / `codex` / `grok` / `opencode`).
    pub vendor: String,
    /// Host axis (`local` or satellite id) — multi-host cost attribution.
    pub host: String,
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
    headers: HeaderMap,
) -> Response {
    let compute_app = app.clone();
    let status = match app
        .status_singleflight
        .get_or_compute(move || aggregate_status(&compute_app))
        .await
    {
        Ok(status) => status,
        Err(err) => {
            tracing::error!(error = %err, "GET /api/v1/status aggregation failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err})),
            )
                .into_response();
        }
    };

    // Project ACL checks read project state; keep those per-identity reads off
    // async workers too, while the expensive global snapshot above stays shared.
    let filter_app = app.clone();
    match tokio::task::spawn_blocking(move || {
        let mut status = status;
        retain_visible_session_rows(&filter_app, &identity, &mut status.sessions);
        let etag = super::api_v1::snapshot_etag("status", status.version, &status);
        let mut response = Json(status).into_response();
        if let Some(etag) = etag {
            if headers
                .get(header::IF_NONE_MATCH)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
            {
                response = StatusCode::NOT_MODIFIED.into_response();
            }
            if let Ok(value) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, value);
            }
        }
        response
    })
    .await
    {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(error = %err, "GET /api/v1/status ACL task failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Compute the global snapshot synchronously inside `spawn_blocking`. The
/// gateway section is now a pure catalog snapshot; progress reads are byte-
/// cursor catch-ups outside that lock and normally reduce to metadata stats.
fn aggregate_status(app: &AppState) -> StatusResponse {
    let daemon_healthy = ccteam_core::check_daemon_health(&app.paths).is_healthy();

    // ── sessions: prefer the live gateway map; else the on-disk snapshot. ──
    // Either way we only need the COUNT of tracked sessions; the live/idle
    // split is daemon health (the gateway has no finer per-session bit).
    let mut active_watches = 0;
    let mut views = Vec::new();
    let tracked_count: u32 = if let Some(gw) = app.gateway.as_ref() {
        let guard = gw.blocking_lock();
        views = guard.session_views();
        active_watches = guard.armed_delegation_watch_count();
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
    let projects = ccteam_core::queries::collect_projects(&app.paths).unwrap_or_default();
    let mut projections = projects
        .iter()
        .map(|project| {
            let slug = project.state.slug.clone();
            let snapshot = app.progress_projection.project_snapshot(&slug);
            (slug, snapshot)
        })
        .collect::<std::collections::HashMap<_, _>>();
    for view in &views {
        projections
            .entry(view.project.clone())
            .or_insert_with(|| app.progress_projection.project_snapshot(&view.project));
    }
    let session_rows = build_session_cost_rows(&projections, &views);

    let mut cost_24h_usd = 0.0_f64;
    let mut cost_24h_by_vendor: BTreeMap<String, f64> = BTreeMap::new();
    let mut budget_cap_24h: Option<f64> = None;
    let mut notified_24h = 0u32;
    let mut denied_24h = 0u32;
    for project in projects {
        let slug = &project.state.slug;
        if let Some(snapshot) = projections.get(slug) {
            cost_24h_usd += snapshot.cost.cost_24h_usd;
            for (vendor, usd) in &snapshot.cost.cost_24h_by_vendor {
                *cost_24h_by_vendor.entry(vendor.clone()).or_insert(0.0) += usd;
            }
            notified_24h = notified_24h.saturating_add(snapshot.delegations.notified_24h);
            denied_24h = denied_24h.saturating_add(snapshot.delegations.denied_24h);
        }
        // Budget caps are additive across projects; a single configured cap
        // flips the aggregate from `None` to `Some`.
        if let Some(cap) = project_budget_cap_24h(&app.paths.project_dir(slug)) {
            budget_cap_24h = Some(budget_cap_24h.unwrap_or(0.0) + cap);
        }
    }

    StatusResponse {
        version: app.progress_projection.snapshot_version(),
        warming_up: app.progress_projection.warming_up(),
        daemon_healthy,
        sessions_live,
        sessions_idle,
        cost_24h_usd,
        cost_24h_by_vendor,
        budget_cap_24h,
        sessions: session_rows,
        delegations: DelegationSummary {
            active_watches,
            notified_24h,
            denied_24h,
        },
    }
}

/// Keep daemon-wide aggregates global while hiding per-session rows whose
/// project the caller does not own. Admins retain the unfiltered fleet view.
fn retain_visible_session_rows(
    app: &AppState,
    identity: &Identity,
    rows: &mut Vec<SessionCostRow>,
) {
    if identity.is_admin {
        return;
    }
    rows.retain(|row| crate::routes::api_v1::can_see_project(app, identity, &row.project));
}

/// Build one [`SessionCostRow`] per live gateway session, pricing each
/// **deterministically per-turn** by the turn's own canonical `model`. Reads
/// from the already-caught-up per-project snapshots. No journal is opened in
/// this function. A turn whose model is absent/unpriced is counted without a
/// fallback; zero priced turns yields `cost_usd: None` (rendered "—").
fn build_session_cost_rows(
    projections: &std::collections::HashMap<
        String,
        ccteam_im::progress_projection::ProjectProjectionSnapshot,
    >,
    views: &[ccteam_im::gateway::SessionView],
) -> Vec<SessionCostRow> {
    views
        .iter()
        .map(|v| {
            let pricing = projections
                .get(&v.project)
                .and_then(|projection| projection.sessions.get(&v.sid))
                .map(|session| session.pricing(agent_vendor_from_str(&v.vendor)))
                .unwrap_or_default();
            SessionCostRow {
                sid: v.sid.clone(),
                project: v.project.clone(),
                role: v.role.clone(),
                vendor: v.vendor.clone(),
                host: if v.host.is_empty() {
                    "local".into()
                } else {
                    v.host.clone()
                },
                status: v.status.clone(),
                cost_usd: pricing.cost_usd,
                unpriced_turns: pricing.unpriced_turns,
            }
        })
        .collect()
}

/// Map a session-view vendor token to the harness enum, defaulting to Claude.
fn agent_vendor_from_str(vendor: &str) -> ccteam_harness::AgentVendor {
    match vendor.trim().to_ascii_lowercase().as_str() {
        "codex" => ccteam_harness::AgentVendor::Codex,
        "grok" => ccteam_harness::AgentVendor::Grok,
        "opencode" => ccteam_harness::AgentVendor::Opencode,
        "kimi" => ccteam_harness::AgentVendor::Kimi,
        "pi" => ccteam_harness::AgentVendor::Pi,
        "dsh" => ccteam_harness::AgentVendor::Dsh,
        _ => ccteam_harness::AgentVendor::Claude,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Condvar;
    use std::time::Duration;

    #[derive(Default)]
    struct BlockingGate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl BlockingGate {
        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.changed.wait(open).unwrap();
            }
        }

        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    fn status_with_live_count(sessions_live: u32) -> StatusResponse {
        StatusResponse {
            version: Some(1),
            warming_up: false,
            daemon_healthy: false,
            sessions_live,
            sessions_idle: 0,
            cost_24h_usd: 0.0,
            cost_24h_by_vendor: BTreeMap::new(),
            budget_cap_24h: None,
            sessions: Vec::new(),
            delegations: DelegationSummary::default(),
        }
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition should become true promptly");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_singleflight_coalesces_concurrent_requests() {
        const REQUESTS: usize = 12;
        let singleflight = StatusSingleflight::default();
        let start = Arc::new(tokio::sync::Barrier::new(REQUESTS + 1));
        let compute_count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(BlockingGate::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let mut requests = Vec::new();

        for _ in 0..REQUESTS {
            let singleflight = singleflight.clone();
            let start = Arc::clone(&start);
            let compute_count = Arc::clone(&compute_count);
            let gate = Arc::clone(&gate);
            let started_tx = Arc::clone(&started_tx);
            requests.push(tokio::spawn(async move {
                start.wait().await;
                singleflight
                    .get_or_compute(move || {
                        compute_count.fetch_add(1, Ordering::SeqCst);
                        if let Some(started_tx) = started_tx.lock().unwrap().take() {
                            let _ = started_tx.send(());
                        }
                        gate.wait();
                        status_with_live_count(7)
                    })
                    .await
                    .unwrap()
            }));
        }

        start.wait().await;
        started_rx.await.unwrap();
        wait_until(|| singleflight.subscriber_count() == REQUESTS).await;
        gate.release();

        for request in requests {
            assert_eq!(request.await.unwrap().sessions_live, 7);
        }
        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_status_request_does_not_wedge_the_next_request() {
        let singleflight = StatusSingleflight::default();
        let compute_count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(BlockingGate::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let first = {
            let singleflight = singleflight.clone();
            let compute_count = Arc::clone(&compute_count);
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                singleflight
                    .get_or_compute(move || {
                        compute_count.fetch_add(1, Ordering::SeqCst);
                        let _ = started_tx.send(());
                        gate.wait();
                        status_with_live_count(1)
                    })
                    .await
            })
        };

        started_rx.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        gate.release();
        wait_until(|| !singleflight.is_in_flight()).await;

        let next_count = Arc::clone(&compute_count);
        let next = tokio::time::timeout(
            Duration::from_secs(1),
            singleflight.get_or_compute(move || {
                next_count.fetch_add(1, Ordering::SeqCst);
                status_with_live_count(2)
            }),
        )
        .await
        .expect("the request after cancellation must complete promptly")
        .unwrap();
        assert_eq!(next.sessions_live, 2, "completed results are not cached");
        assert_eq!(compute_count.load(Ordering::SeqCst), 2);
    }

    fn test_paths(root: &std::path::Path) -> ccteam_core::CcteamPaths {
        ccteam_core::CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    fn projection_snapshots(
        paths: &ccteam_core::CcteamPaths,
        slugs: &[&str],
    ) -> std::collections::HashMap<String, ccteam_im::progress_projection::ProjectProjectionSnapshot>
    {
        let projection = ccteam_im::progress_projection::ProgressProjection::new(paths.clone());
        slugs
            .iter()
            .map(|slug| ((*slug).to_string(), projection.project_snapshot(slug)))
            .collect()
    }

    fn view(sid: &str, project: &str, vendor: &str) -> ccteam_im::gateway::SessionView {
        ccteam_im::gateway::SessionView {
            driveable: true,
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
            title: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            model: None,
            waiting_approval: false,
            parent_sid: None,
            delegation_depth: 0,
        }
    }

    #[test]
    fn agent_vendor_from_str_maps_tokens() {
        assert_eq!(
            agent_vendor_from_str("claude"),
            ccteam_harness::AgentVendor::Claude
        );
        assert_eq!(
            agent_vendor_from_str("Codex"),
            ccteam_harness::AgentVendor::Codex
        );
        assert_eq!(
            agent_vendor_from_str("grok"),
            ccteam_harness::AgentVendor::Grok
        );
        assert_eq!(
            agent_vendor_from_str("opencode"),
            ccteam_harness::AgentVendor::Opencode
        );
        assert_eq!(agent_vendor_from_str("pi"), ccteam_harness::AgentVendor::Pi);
        assert_eq!(
            agent_vendor_from_str("dsh"),
            ccteam_harness::AgentVendor::Dsh
        );
        assert_eq!(
            agent_vendor_from_str("weird"),
            ccteam_harness::AgentVendor::Claude
        );
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

        let snapshots = projection_snapshots(&paths, &["demo"]);
        let rows = build_session_cost_rows(&snapshots, &[view("s1", "demo", "claude")]);
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
        let snapshots = projection_snapshots(&paths, &["demo"]);
        let rows = build_session_cost_rows(&snapshots, &[view("s9", "demo", "claude")]);
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

        let snapshots = projection_snapshots(&paths, &["demo"]);
        let rows = build_session_cost_rows(&snapshots, &[view("s1", "demo", "claude")]);
        assert!(rows[0].cost_usd.is_none(), "unknown model must not price");
        assert_eq!(rows[0].unpriced_turns, 1, "the synthetic turn is exposed");
    }

    #[test]
    fn build_session_cost_rows_stamps_host_axis() {
        // v0.8.24 multi-host: cost rows must surface the session host so the
        // Status view can attribute spend (satellite vs local), never drop it.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let mut remote = view("s2", "demo", "claude");
        remote.host = "sat-east".into();
        let snapshots = projection_snapshots(&paths, &["demo"]);
        let rows = build_session_cost_rows(&snapshots, &[view("s1", "demo", "claude"), remote]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].host, "local");
        assert_eq!(rows[1].sid, "s2");
        assert_eq!(rows[1].host, "sat-east");
    }

    #[test]
    fn tenant_status_rows_are_filtered_by_project_ownership() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        for (slug, owner) in [("alice", "user:u1"), ("bob", "user:u2")] {
            let path = paths.project_state(slug);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut state = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
            state.owner = Some(owner.into());
            state.save(&path).unwrap();
        }
        let app = AppState::new(paths);
        let snapshots = projection_snapshots(&app.paths, &["alice", "bob"]);
        let mut rows = build_session_cost_rows(
            &snapshots,
            &[view("s1", "alice", "claude"), view("s2", "bob", "codex")],
        );
        let mut admin_rows = rows.clone();
        retain_visible_session_rows(&app, &Identity::admin(), &mut admin_rows);
        assert_eq!(admin_rows.len(), 2, "admin fleet rows stay unfiltered");

        retain_visible_session_rows(&app, &Identity::tenant("u1".into()), &mut rows);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sid, "s1");
        assert_eq!(rows[0].project, "alice");
    }
}
