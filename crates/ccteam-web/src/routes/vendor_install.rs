//! VENDOR-INSTALL-1 — admin one-click vendor install/update on the LOCAL host.
//!
//! `POST /api/v1/hosts/{host}/vendors/{vendor}/install` starts (or joins) an
//! install job for one npm-packaged vendor; `GET
//! /api/v1/hosts/{host}/vendors/{vendor}/install/{job_id}` polls it. The
//! recipe argv comes from `AgentProbeSpec::install_recipe` — the single
//! registry table — and is executed with `std::process::Command` argv-style
//! (no shell), so a request can never influence the command line beyond
//! choosing WHICH vendor row runs. kimi/pi carry no recipe and get a 400
//! with their manual-install link.
//!
//! Honesty: the job runs as the daemon's OS user. A permission failure
//! (npm's global prefix not writable) surfaces the process's own stderr
//! tail verbatim — no sudo, no fallback package manager. Jobs are
//! process-lifetime state (like the dsh-web supervisor): a daemon restart
//! simply forgets them, and a running job dedups a second POST for the same
//! vendor instead of double-installing.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_core::host_registry::AgentProbeSpec;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::auth::{deny_non_admin, Identity};
use crate::state::AppState;

use super::hosts::LOCAL_HOST;

/// A job abandoned past this wall-clock budget is killed and marked failed.
/// npm global installs of a vendor CLI finish in well under a minute on a
/// healthy link; ten minutes is the "something is wedged" line.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// How many trailing output lines a job keeps for the poll response.
const OUTPUT_TAIL_LINES: usize = 24;

/// Cap on retained jobs (finished included) so a click-happy admin cannot
/// grow the map without bound over a daemon's lifetime.
const MAX_RETAINED_JOBS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallJobState {
    Running,
    Ok,
    Failed,
}

/// One install attempt. `output_tail` is the merged stdout+stderr trailing
/// window (line order across the two streams is NOT preserved — the tail is
/// for human diagnosis, not parsing).
#[derive(Debug)]
pub struct InstallJob {
    pub vendor: String,
    pub state: InstallJobState,
    pub exit_code: Option<i32>,
    pub output_tail: VecDeque<String>,
}

impl InstallJob {
    fn view(&self, job_id: &str) -> InstallJobView {
        InstallJobView {
            job_id: job_id.to_string(),
            vendor: self.vendor.clone(),
            state: self.state,
            exit_code: self.exit_code,
            output_tail: self
                .output_tail
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// The wire shape both endpoints return (POST always 202 + this body; GET
/// 200 + this body).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct InstallJobView {
    pub job_id: String,
    pub vendor: String,
    /// `running` | `ok` | `failed`.
    pub state: InstallJobState,
    /// Process exit code once finished (`null` while running or when the
    /// process never spawned / was killed on timeout).
    pub exit_code: Option<i32>,
    /// Trailing merged stdout+stderr window (last 24 lines).
    pub output_tail: String,
}

/// Process-lifetime job table. Insert/poll take the lock for microseconds;
/// the child runs on a detached task that re-locks only to append output
/// lines and to publish the terminal state — the lock is never held across
/// an `.await` of the child.
#[derive(Debug, Default)]
pub struct VendorInstallManager {
    jobs: Mutex<BTreeMap<String, InstallJob>>,
}

impl VendorInstallManager {
    /// Returns the id of the vendor's currently-running job, if any (the
    /// same-vendor dedup: a second POST joins, never duplicates).
    fn running_job_for(&self, vendor: &str) -> Option<String> {
        let jobs = self.jobs.lock().ok()?;
        jobs.iter()
            .find(|(_, job)| job.vendor == vendor && job.state == InstallJobState::Running)
            .map(|(id, _)| id.clone())
    }

    fn insert_running(&self, vendor: &str) -> Option<String> {
        let mut jobs = self.jobs.lock().ok()?;
        // Prune the oldest FINISHED jobs once the table is full (running
        // jobs are never pruned — there is at most one per vendor).
        while jobs.len() >= MAX_RETAINED_JOBS {
            let Some(oldest_finished) = jobs
                .iter()
                .filter(|(_, job)| job.state != InstallJobState::Running)
                .map(|(id, _)| id.clone())
                .next()
            else {
                break;
            };
            jobs.remove(&oldest_finished);
        }
        let job_id = ccteam_core::session_secret::mint();
        jobs.insert(
            job_id.clone(),
            InstallJob {
                vendor: vendor.to_string(),
                state: InstallJobState::Running,
                exit_code: None,
                output_tail: VecDeque::new(),
            },
        );
        Some(job_id)
    }

    fn push_output(&self, job_id: &str, line: String) {
        if let Ok(mut jobs) = self.jobs.lock() {
            if let Some(job) = jobs.get_mut(job_id) {
                if job.output_tail.len() == OUTPUT_TAIL_LINES {
                    job.output_tail.pop_front();
                }
                job.output_tail.push_back(line);
            }
        }
    }

    fn finish(&self, job_id: &str, state: InstallJobState, exit_code: Option<i32>) {
        if let Ok(mut jobs) = self.jobs.lock() {
            if let Some(job) = jobs.get_mut(job_id) {
                job.state = state;
                job.exit_code = exit_code;
            }
        }
    }

    fn view(&self, vendor: &str, job_id: &str) -> Option<InstallJobView> {
        let jobs = self.jobs.lock().ok()?;
        let job = jobs.get(job_id)?;
        if job.vendor != vendor {
            return None;
        }
        Some(job.view(job_id))
    }
}

/// Execute the recipe with merged-output capture + timeout. Runs on a
/// detached task; publishes its terminal state into the manager.
async fn run_install_job(manager: Arc<VendorInstallManager>, job_id: String, vendor: String) {
    // Look the recipe up AGAIN by vendor token — the job carries no argv
    // from the request path at all; the registry table is the only source.
    let Some(spec) = AgentProbeSpec::by_vendor(&vendor) else {
        manager.finish(&job_id, InstallJobState::Failed, None);
        return;
    };
    let Some(argv) = spec.install_recipe else {
        manager.finish(&job_id, InstallJobState::Failed, None);
        return;
    };
    manager.push_output(&job_id, format!("$ {}", argv.join(" ")));

    let spawned = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            manager.push_output(
                &job_id,
                format!("failed to spawn `{}`: {err} (is it on PATH?)", argv[0]),
            );
            manager.finish(&job_id, InstallJobState::Failed, None);
            return;
        }
    };
    if let Some(stdout) = child.stdout.take() {
        spawn_tail_reader(&manager, &job_id, stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_tail_reader(&manager, &job_id, stderr);
    }

    match tokio::time::timeout(INSTALL_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => {
            manager.push_output(&job_id, "install finished successfully".to_string());
            manager.finish(&job_id, InstallJobState::Ok, status.code());
        }
        Ok(Ok(status)) => {
            manager.push_output(&job_id, format!("install exited with {status}"));
            manager.finish(&job_id, InstallJobState::Failed, status.code());
        }
        Ok(Err(err)) => {
            manager.push_output(&job_id, format!("failed to wait on installer: {err}"));
            manager.finish(&job_id, InstallJobState::Failed, None);
        }
        Err(_) => {
            // Timeout: kill the child (kill_on_drop alone only fires when the
            // Child value drops; make the kill explicit so the npm tree is
            // reaped before we publish the failure).
            let _ = child.kill().await;
            manager.push_output(
                &job_id,
                format!("install timed out after {}s", INSTALL_TIMEOUT.as_secs()),
            );
            manager.finish(&job_id, InstallJobState::Failed, None);
        }
    }
}

fn spawn_tail_reader<R>(manager: &Arc<VendorInstallManager>, job_id: &str, reader: R)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let job_id = job_id.to_string();
    let manager = manager.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            manager.push_output(&job_id, line);
        }
    });
}

/// Validate the `{host, vendor}` pair shared by both endpoints. `Ok(spec)`
/// is the recipe-bearing spec; `Err(response)` is the ready-made denial
/// (404 non-local host, 404 unknown vendor, 400 no-recipe vendor). Boxed —
/// a `Response` is far too big to ride the `Err` variant inline.
fn resolve_install_target(
    host: &str,
    vendor: &str,
) -> Result<&'static AgentProbeSpec, Box<Response>> {
    if host != LOCAL_HOST {
        return Err(Box::new(
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("unknown host: {host}")})),
            )
                .into_response(),
        ));
    }
    let Some(spec) = AgentProbeSpec::by_vendor(vendor) else {
        return Err(Box::new(
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("unknown vendor: {vendor}")})),
            )
                .into_response(),
        ));
    };
    if spec.install_recipe.is_none() {
        let manual = spec.manual_install_url.unwrap_or("the vendor's docs");
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "vendor {vendor} has no one-click install recipe — install it manually: {manual}"
                    )
                })),
            )
                .into_response(),
        ));
    }
    Ok(spec)
}

/// `POST /api/v1/hosts/{host}/vendors/{vendor}/install` — start (or join the
/// running) install/update job for one vendor on THIS machine. Admin-only:
/// the install runs as the daemon's OS user and writes that user's global
/// npm prefix — owner-scoped, never a tenant action.
#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host}/vendors/{vendor}/install",
    tag = "hosts",
    params(
        ("host" = String, Path, description = "Host id (`local` only)"),
        ("vendor" = String, Path, description = "Vendor token with an install recipe (claude/codex/grok/opencode/dsh)"),
    ),
    responses(
        (status = 202, description = "Install job running (started or joined); poll `GET .../install/{job_id}`", body = InstallJobView),
        (status = 400, description = "Vendor has no install recipe (kimi/pi — install manually)"),
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown host or vendor"),
    ),
)]
pub(crate) async fn handle_vendor_install(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((host, vendor)): Path<(String, String)>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let spec = match resolve_install_target(&host, &vendor) {
        Ok(spec) => spec,
        Err(deny) => return *deny,
    };
    let manager = &app.vendor_installs;
    // Same-vendor dedup: a running job is returned, never duplicated.
    if let Some(job_id) = manager.running_job_for(spec.vendor) {
        if let Some(view) = manager.view(spec.vendor, &job_id) {
            return (StatusCode::ACCEPTED, Json(view)).into_response();
        }
    }
    let Some(job_id) = manager.insert_running(spec.vendor) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "install job table unavailable"})),
        )
            .into_response();
    };
    // Detached: cancelling the HTTP request must not kill the install (the
    // detached task owns the `kill_on_drop` Child).
    tokio::spawn(run_install_job(
        manager.clone(),
        job_id.clone(),
        spec.vendor.to_string(),
    ));
    let view = manager
        .view(spec.vendor, &job_id)
        .expect("job inserted above");
    (StatusCode::ACCEPTED, Json(view)).into_response()
}

/// `GET /api/v1/hosts/{host}/vendors/{vendor}/install/{job_id}` — poll one
/// install job. Same admin gate as the POST: the output tail can carry local
/// paths from the installer.
#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host}/vendors/{vendor}/install/{job_id}",
    tag = "hosts",
    params(
        ("host" = String, Path, description = "Host id (`local` only)"),
        ("vendor" = String, Path, description = "Vendor token"),
        ("job_id" = String, Path, description = "Job id returned by the POST"),
    ),
    responses(
        (status = 200, description = "Job snapshot `{state, exit_code?, output_tail}`", body = InstallJobView),
        (status = 400, description = "Vendor has no install recipe"),
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown host, vendor, or job id"),
    ),
)]
pub(crate) async fn handle_vendor_install_job(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((host, vendor, job_id)): Path<(String, String, String)>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    if let Err(deny) = resolve_install_target(&host, &vendor) {
        return *deny;
    }
    match app.vendor_installs.view(&vendor, &job_id) {
        Some(view) => Json(view).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown install job: {job_id}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_target_validation_matrix() {
        // Non-local host → 404 (installs never target a satellite).
        let err = resolve_install_target("sat-1", "claude").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        // Unknown vendor → 404.
        let err = resolve_install_target(LOCAL_HOST, "gemini").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        // Recipe-less vendors → 400 with the manual link.
        for vendor in ["kimi", "pi"] {
            let err = resolve_install_target(LOCAL_HOST, vendor).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{vendor}");
        }
        // Every npm-backed vendor resolves.
        for vendor in ["claude", "codex", "grok", "opencode", "dsh"] {
            assert!(
                resolve_install_target(LOCAL_HOST, vendor).is_ok(),
                "{vendor}"
            );
        }
    }

    #[test]
    fn same_vendor_running_job_dedups() {
        let manager = VendorInstallManager::default();
        let first = manager.insert_running("claude").unwrap();
        assert_eq!(
            manager.running_job_for("claude").as_deref(),
            Some(first.as_str())
        );
        // A different vendor is independent.
        assert!(manager.running_job_for("codex").is_none());
        // Once finished, the dedup no longer fires.
        manager.finish(&first, InstallJobState::Ok, Some(0));
        assert!(manager.running_job_for("claude").is_none());
    }

    #[test]
    fn output_tail_is_bounded_and_view_is_scoped_to_vendor() {
        let manager = VendorInstallManager::default();
        let job_id = manager.insert_running("codex").unwrap();
        for n in 0..(OUTPUT_TAIL_LINES + 10) {
            manager.push_output(&job_id, format!("line {n}"));
        }
        let view = manager.view("codex", &job_id).unwrap();
        assert_eq!(view.output_tail.lines().count(), OUTPUT_TAIL_LINES);
        assert!(view.output_tail.starts_with("line 10\n"));
        // The job is invisible under another vendor's token (404, not leak).
        assert!(manager.view("claude", &job_id).is_none());
    }

    #[test]
    fn retained_jobs_are_capped_and_running_jobs_survive_pruning() {
        let manager = VendorInstallManager::default();
        let running = manager.insert_running("claude").unwrap();
        for _ in 0..(MAX_RETAINED_JOBS + 8) {
            let id = manager.insert_running("codex").unwrap();
            manager.finish(&id, InstallJobState::Failed, Some(1));
        }
        let jobs = manager.jobs.lock().unwrap();
        assert!(jobs.len() <= MAX_RETAINED_JOBS);
        assert!(jobs.contains_key(&running));
    }
}
