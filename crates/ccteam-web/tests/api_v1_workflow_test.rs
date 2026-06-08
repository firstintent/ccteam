//! V0.4.0 F68 — workflow_summary JSON API integration tests.
//!
//! Covers the `workflow_summary` field on `GET /api/v1/projects/{slug}`:
//!
//! - Legacy project (no `workflow.yaml`): `workflow_summary` is null or
//!   an empty-shaped object (`workflow_name == ""`, `agents == []`,
//!   `artifact_counts == {}`, `gate_states == {}`); workflow.yaml
//!   absence is not a 500.
//! - V0.4.0 project (workflow.yaml in `.ccteam/`): `workflow_summary`
//!   carries the parsed name + every role from the spec as an agent
//!   card stub + `gate_states[role] == "waiting"` for each Gate role.
//! - Progress events flow through: a paired `agent_spawn` / `agent_done`
//!   adds `cost_usd` to `total_cost_usd`, and an `agent_spawn` with no
//!   live backing job (no `job_id` / `state.json`) is demoted out of
//!   `running_count` by the F80 liveness-aware accounting rather than
//!   counted as still-open.
//! - F67 regression guard (re-asserted at this level): the legacy
//!   `current_phase` / `decision_candidates` fields are NOT present on
//!   the response, no matter what the project shape is.

use std::fs;
use std::net::SocketAddr;

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState};
use serde_json::{json, Value};
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

/// Drop a minimal valid `workflow.yaml` into the project's `.ccteam/`
/// directory. Exercises the same load path the orchestrator uses.
fn write_workflow_yaml(paths: &CcteamPaths, slug: &str, body: &str) {
    let ccteam_dir = paths.project_ccteam_dir(slug);
    fs::create_dir_all(&ccteam_dir).unwrap();
    fs::write(ccteam_dir.join("workflow.yaml"), body).unwrap();
}

/// Append a JSON event line to the project's progress.jsonl (workflow
/// projects use the flat `<slug>.jsonl` shape per F66).
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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn workflow_summary_absent_workflow_yaml_returns_empty_default() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "legacy");

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/legacy"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    // Field is present even when there's no workflow.yaml — Some(default)
    // shape so the SPA gets a uniform, never-undefined contract.
    assert!(
        body.get("workflow_summary").is_some(),
        "workflow_summary must be present on the response shape",
    );
    let summary = &body["workflow_summary"];
    if !summary.is_null() {
        assert_eq!(summary["workflow_name"], "");
        assert!(summary["agents"].as_array().unwrap().is_empty());
        assert!(summary["artifact_counts"].as_object().unwrap().is_empty());
        assert!(summary["gate_states"].as_object().unwrap().is_empty());
    }
}

#[tokio::test]
async fn workflow_summary_with_yaml_populates_agents_and_gate_states() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-a");

    // Minimal valid spec: one Manual role + one Gate role with input.
    // Gate role's input dir doesn't need to exist for `workflow_summary`
    // — `count_files_in_dir` returns 0 on missing dir.
    write_workflow_yaml(
        &paths,
        "team-a",
        "\
name: team-a
agents:
  planner:
    trigger: manual
  reviewer:
    trigger: gate
    input: artifacts/plan
",
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/team-a"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    let summary = &body["workflow_summary"];
    assert_eq!(summary["workflow_name"], "team-a");

    let agents = summary["agents"].as_array().unwrap();
    let roles: Vec<&str> = agents.iter().map(|a| a["role"].as_str().unwrap()).collect();
    // Sorted by ASCII per queries.rs::workflow_summary
    assert_eq!(roles, vec!["planner", "reviewer"]);

    // Every agent card carries the zero defaults
    for a in agents {
        assert_eq!(a["running_count"], 0);
        assert_eq!(a["queued_count"], 0);
        assert!(a["total_cost_usd"].is_number());
        assert!(a["last_session_status"].is_null());
    }

    // The single Gate role surfaces in gate_states as "waiting"
    let gates = summary["gate_states"].as_object().unwrap();
    assert_eq!(gates["reviewer"], "waiting");
    // Non-gate roles must NOT appear in gate_states
    assert!(!gates.contains_key("planner"));
}

#[tokio::test]
async fn workflow_summary_reflects_agent_spawn_and_done_events() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "team-b");
    write_workflow_yaml(
        &paths,
        "team-b",
        "\
name: team-b
agents:
  coder:
    trigger: manual
",
    );

    // Older spawn-only session (no agent_done, no `job_id`). Under the
    // F80 liveness-aware accounting that `workflow_summary` wires in
    // (`current_agent_sessions_with_liveness` -> `probe_job`), an
    // `agent_spawn` whose `job_id` does not resolve to a live
    // `~/.claude/jobs/<id>/state.json` is a phantom row (a daemon
    // casualty that died without writing `agent_done`). `probe_job(None)`
    // returns `Terminal { killed }`, so this row is demoted out of
    // `running_count` instead of being counted as still-open.
    append_event(
        &paths,
        "team-b",
        json!({
            "event": "agent_spawn",
            "role": "coder",
            "session_id": "coder-000",
            "ts": "2026-05-14T08:00:00Z",
        }),
    );
    // Newer completed session — full spawn / done pair with cost.
    // `started_at` ascending: this session is the *latest* so it
    // determines `last_session_status`.
    append_event(
        &paths,
        "team-b",
        json!({
            "event": "agent_spawn",
            "role": "coder",
            "session_id": "coder-001",
            "ts": "2026-05-14T09:00:00Z",
        }),
    );
    append_event(
        &paths,
        "team-b",
        json!({
            "event": "agent_done",
            "role": "coder",
            "session_id": "coder-001",
            "status": "completed",
            "cost_usd": 0.42,
            "ts": "2026-05-14T09:01:00Z",
        }),
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/team-b"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let summary = &body["workflow_summary"];
    let agents = summary["agents"].as_array().unwrap();
    let coder = agents
        .iter()
        .find(|a| a["role"] == "coder")
        .expect("coder agent");
    // F80: the spawn-only `coder-000` row has no live backing job, so
    // it is demoted as a phantom rather than counted as running. Both
    // sessions are therefore terminal from the summary's point of view.
    assert_eq!(
        coder["running_count"], 0,
        "spawn-only row with no live job is demoted (F80 phantom cleanup)"
    );
    let cost = coder["total_cost_usd"].as_f64().unwrap();
    assert!(
        (cost - 0.42).abs() < 1e-6,
        "total_cost_usd must include the agent_done cost: got {cost}"
    );
    // The latest session by started_at terminated → last_session_status
    // surfaces as "done".
    assert_eq!(coder["last_session_status"]["status"], "done");
    assert_eq!(
        summary["total_cost_usd"].as_f64().unwrap(),
        0.42,
        "rollup cost matches the single agent_done",
    );
}

#[tokio::test]
async fn workflow_summary_field_is_redundant_safe_with_dropped_phase_fields() {
    // Regression guard for the F67 drop, re-asserted at the workflow
    // layer: even with a fully-populated workflow.yaml the response
    // must not regress to including the phase-era field names.
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "guard");
    write_workflow_yaml(
        &paths,
        "guard",
        "\
name: guard
agents:
  one:
    trigger: manual
",
    );

    let addr = spawn(AppState::new(paths)).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/guard"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("current_phase").is_none());
    assert!(body.get("decision_candidates").is_none());
    assert!(body.get("phase_state").is_none());
    assert!(body.get("workflow_summary").is_some());
}
