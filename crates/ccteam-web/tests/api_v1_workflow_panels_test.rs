//! V0.4.6 F90 — WorkflowView panel endpoints integration tests.
//!
//! Covers the four new endpoints introduced for the enhanced
//! workflow view (PRD v0-4-6 §F90):
//!
//! - `GET /api/v1/projects/<slug>/artifact_queue` — lists every
//!   `Trigger::Watch(<path>)` agent + the fs file count and oldest
//!   file age in the watched directory.
//! - `GET /api/v1/projects/<slug>/cost_history?window=24h|7d` — hour
//!   bucketed `agent_done.cost_usd` sums over the rolling window.
//! - `GET /api/v1/projects/<slug>/jobs/<job_id>/log?tail=N` — read
//!   tail of `~/.claude/jobs/<job_id>/output.log` (clamped). Reads
//!   honor `$CCTEAM_CLAUDE_JOBS_DIR` so the test can sandbox without
//!   hitting the real `~/.claude` directory.
//! - `GET /api/v1/projects/<slug>/sessions/active` — open
//!   `agent_spawn` rows decorated with live state.json (`cwd`,
//!   `cost_usd`).

use std::fs;
use std::net::SocketAddr;

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState};
use serde_json::{json, Value};
use serial_test::serial;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_project(paths: &CcteamPaths, slug: &str) {
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(paths, slug, "demo request", "dev").unwrap();
}

fn write_workflow_yaml(paths: &CcteamPaths, slug: &str, body: &str) {
    let ccteam_dir = paths.project_ccteam_dir(slug);
    fs::create_dir_all(&ccteam_dir).unwrap();
    fs::write(ccteam_dir.join("workflow.yaml"), body).unwrap();
}

fn append_event(paths: &CcteamPaths, slug: &str, event: Value) {
    let path = paths.progress_jsonl(slug);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut line = serde_json::to_string(&event).unwrap();
    line.push('\n');
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

/// reqwest client that ignores `HTTP_PROXY` / `HTTPS_PROXY` env vars.
/// In some shells those resolve `127.0.0.1:<random>` to a corporate
/// proxy which then returns 502; the tests bypass with `no_proxy()`.
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

// ----- t01: artifact_queue -----

#[tokio::test]
async fn t01_artifact_queue_lists_watch_paths_with_file_count() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-a");

    // workflow.yaml declares one Watch trigger. Create three matching
    // files so the endpoint reports file_count=3.
    write_workflow_yaml(
        &paths,
        "team-a",
        "\
name: team-a
agents:
  explorer:
    trigger: watch:.ccteam/explore-requests/
    parallelism: 3
  reviewer:
    trigger: manual
",
    );
    let watch_dir = paths
        .project_dir("team-a")
        .join(".ccteam")
        .join("explore-requests");
    fs::create_dir_all(&watch_dir).unwrap();
    fs::write(watch_dir.join("a.md"), "a").unwrap();
    fs::write(watch_dir.join("b.md"), "b").unwrap();
    fs::write(watch_dir.join("c.md"), "c").unwrap();

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-a/artifact_queue"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().expect("response is an array");
    // Only the Watch trigger contributes — `reviewer: manual` is filtered.
    assert_eq!(arr.len(), 1, "exactly one Watch trigger declared");
    let entry = &arr[0];
    assert_eq!(entry["role"], "explorer");
    assert_eq!(entry["path"], ".ccteam/explore-requests/");
    assert_eq!(entry["file_count"], 3);
    // age must be a non-negative number; freshness is non-null.
    assert!(entry["oldest_age_seconds"].is_number());
    assert!(entry["newest_filename"].is_string());
}

// ----- t02: cost_history -----

#[tokio::test]
async fn t02_cost_history_buckets_by_hour() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-b");

    // Two agent_done events: one within the 24h window (now-30min),
    // one outside (now-25h). Window=24h, so cost_24h sums only the first.
    let now = chrono::Utc::now();
    let inside = now - chrono::Duration::minutes(30);
    let outside = now - chrono::Duration::hours(25);
    append_event(
        &paths,
        "team-b",
        json!({
            "event": "agent_done",
            "role": "coder",
            "session_id": "s-1",
            "status": "completed",
            "cost_usd": 0.25,
            "ts": inside.to_rfc3339(),
        }),
    );
    append_event(
        &paths,
        "team-b",
        json!({
            "event": "agent_done",
            "role": "coder",
            "session_id": "s-2",
            "status": "completed",
            "cost_usd": 1.50,
            "ts": outside.to_rfc3339(),
        }),
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-b/cost_history?window=24h"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["window"], "24h");
    let buckets = body["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 24, "24h window → 24 hour buckets");
    let total: f64 = buckets
        .iter()
        .map(|b| b["cost_usd"].as_f64().unwrap_or(0.0))
        .sum();
    assert!(
        (total - 0.25).abs() < 1e-6,
        "only the inside-window event contributes: total={total}"
    );

    // 7d window picks up the outside event too.
    let resp7 = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-b/cost_history?window=7d"
        ))
        .send()
        .await
        .unwrap();
    let body7: Value = resp7.json().await.unwrap();
    assert_eq!(body7["window"], "7d");
    let buckets7 = body7["buckets"].as_array().unwrap();
    assert_eq!(buckets7.len(), 24 * 7);
    let total7: f64 = buckets7
        .iter()
        .map(|b| b["cost_usd"].as_f64().unwrap_or(0.0))
        .sum();
    assert!(
        (total7 - 1.75).abs() < 1e-6,
        "both events fall in 7d: total7={total7}"
    );
}

// ----- t03: job log tail -----

#[tokio::test]
#[serial(claude_jobs_env)]
async fn t03_job_log_returns_tail() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-c");

    // Sandbox the claude jobs dir + write a fake output.log with 1000
    // lines so the SPA-style tail clamp can be exercised.
    let jobs_root = tmp.path().join("claude-jobs");
    let job_id = "abc123";
    let job_dir = jobs_root.join(job_id);
    fs::create_dir_all(&job_dir).unwrap();
    let mut body = String::new();
    for i in 0..1000 {
        body.push_str(&format!("line {i}\n"));
    }
    fs::write(job_dir.join("output.log"), &body).unwrap();
    // Also a state.json so the optional resolver doesn't blow up if
    // a future implementation reads it before the log.
    fs::write(
        job_dir.join("state.json"),
        r#"{"state":"working","cost_usd":0.1}"#,
    )
    .unwrap();
    std::env::set_var("CCTEAM_CLAUDE_JOBS_DIR", &jobs_root);

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-c/jobs/{job_id}/log?tail=200"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["job_id"], job_id);
    assert_eq!(body["total_lines"], 1000);
    let tail = body["tail"].as_str().unwrap();
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 200, "tail returns 200 lines");
    assert_eq!(lines[0], "line 800", "tail starts at line 800");
    assert_eq!(lines[199], "line 999", "tail ends at line 999");

    // Empty path traversal must be rejected.
    let bad = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-c/jobs/..%2Fetc/log"
        ))
        .send()
        .await
        .unwrap();
    // axum normalizes URL-encoded slashes — path will be missing the
    // job_id segment OR contain `..`. We accept 400 OR 404 as long as
    // it's not a 500 / 200.
    assert!(
        bad.status() == 400 || bad.status() == 404,
        "path traversal returns 4xx, got {}",
        bad.status()
    );

    std::env::remove_var("CCTEAM_CLAUDE_JOBS_DIR");
}

// ----- t04: active sessions with state.json cost -----

#[tokio::test]
#[serial(claude_jobs_env)]
async fn t04_active_sessions_with_state_json_cost() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-d");
    write_workflow_yaml(
        &paths,
        "team-d",
        "\
name: team-d
agents:
  coder:
    trigger: manual
",
    );

    // Two open agent_spawn events (no matching agent_done); each
    // carries a unique job_id pointing into the sandboxed jobs dir.
    let jobs_root = tmp.path().join("claude-jobs");
    fs::create_dir_all(&jobs_root).unwrap();
    let make_state = |id: &str, cost: f64, cwd: &str| {
        let job_dir = jobs_root.join(id);
        fs::create_dir_all(&job_dir).unwrap();
        fs::write(
            job_dir.join("state.json"),
            format!(
                r#"{{"state":"working","cost_usd":{cost},"cwd":"{cwd}"}}"#,
                cost = cost,
                cwd = cwd,
            ),
        )
        .unwrap();
    };
    make_state("job-1", 0.10, "/tmp/team-d");
    make_state("job-2", 0.20, "/tmp/team-d");
    std::env::set_var("CCTEAM_CLAUDE_JOBS_DIR", &jobs_root);

    append_event(
        &paths,
        "team-d",
        json!({
            "event": "agent_spawn",
            "role": "coder",
            "session_id": "coder-001",
            "job_id": "job-1",
            "ts": "2026-05-15T10:00:00Z",
        }),
    );
    append_event(
        &paths,
        "team-d",
        json!({
            "event": "agent_spawn",
            "role": "coder",
            "session_id": "coder-002",
            "job_id": "job-2",
            "ts": "2026-05-15T10:05:00Z",
        }),
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-d/sessions/active"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Sort: role ASC then started_at ASC → coder-001 first.
    assert_eq!(arr[0]["session_id"], "coder-001");
    assert_eq!(arr[1]["session_id"], "coder-002");
    assert!((arr[0]["cost_usd"].as_f64().unwrap() - 0.10).abs() < 1e-6);
    assert!((arr[1]["cost_usd"].as_f64().unwrap() - 0.20).abs() < 1e-6);
    assert_eq!(arr[0]["cwd"], "/tmp/team-d");
    assert_eq!(arr[0]["role"], "coder");
    assert_eq!(arr[0]["job_id"], "job-1");

    std::env::remove_var("CCTEAM_CLAUDE_JOBS_DIR");
}

// ----- V0.5.1 F103a — aggregate /api/v1/sessions/active -----

/// No projects → empty array (200 OK).
#[tokio::test]
async fn t05_sessions_active_aggregate_empty_when_no_projects() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/sessions/active"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

/// One project with an open agent_spawn → aggregate returns 1 row
/// with the project's slug attached. Mirrors t04's fixture shape so
/// the new handler exercises the same probe path.
#[tokio::test]
#[serial(claude_jobs_env)]
async fn t06_sessions_active_aggregate_flattens_across_projects() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-e");
    write_workflow_yaml(
        &paths,
        "team-e",
        "\
name: team-e
agents:
  planner:
    trigger: manual
",
    );

    let jobs_root = tmp.path().join("claude-jobs");
    fs::create_dir_all(&jobs_root).unwrap();
    let job_dir = jobs_root.join("job-e1");
    fs::create_dir_all(&job_dir).unwrap();
    fs::write(
        job_dir.join("state.json"),
        r#"{"state":"working","cost_usd":0.55,"cwd":"/tmp/team-e"}"#,
    )
    .unwrap();
    std::env::set_var("CCTEAM_CLAUDE_JOBS_DIR", &jobs_root);

    append_event(
        &paths,
        "team-e",
        json!({
            "event": "agent_spawn",
            "role": "planner",
            "session_id": "planner-e1",
            "job_id": "job-e1",
            "ts": "2026-05-17T10:00:00Z",
        }),
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/sessions/active"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let row = &arr[0];
    assert_eq!(row["slug"], "team-e");
    assert_eq!(row["session_id"], "planner-e1");
    assert_eq!(row["role"], "planner");
    assert_eq!(row["job_id"], "job-e1");
    assert!((row["cost_usd"].as_f64().unwrap() - 0.55).abs() < 1e-6);
    assert_eq!(row["cwd"], "/tmp/team-e");

    std::env::remove_var("CCTEAM_CLAUDE_JOBS_DIR");
}

// ----- V0.5.1 F103c — workflow SessionDetail -----

/// Workflow project (default team_kind) with one `agent_spawn` event
/// → SessionDetail returns 200 with `kind="workflow"`, `harness=null`,
/// and started_at sourced from the spawn event.
#[tokio::test]
async fn t08_session_detail_workflow_branch_returns_200() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-f");
    append_event(
        &paths,
        "team-f",
        json!({
            "event": "agent_spawn",
            "role": "planner",
            "session_id": "planner-f1",
            "job_id": "job-f1",
            "ts": "2026-05-17T11:00:00Z",
        }),
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-f/sessions/planner-f1"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["kind"], "workflow");
    assert_eq!(body["slug"], "team-f");
    assert_eq!(body["sid"], "planner-f1");
    assert!(body["harness_snapshot"].is_null());
    assert_eq!(body["started_at"], "2026-05-17T11:00:00Z");
    // Events should include the agent_spawn row for the session.
    let events = body["events"].as_array().unwrap();
    assert!(
        events.iter().any(|e| e["event"] == "agent_spawn"),
        "events list includes the agent_spawn row"
    );
}

/// Workflow project with no matching agent_spawn for the requested
/// sid → 404 (we don't synthesise a SessionDetail from nothing).
#[tokio::test]
async fn t09_session_detail_workflow_404_on_unknown_sid() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-g");
    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/projects/team-g/sessions/ghost"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
