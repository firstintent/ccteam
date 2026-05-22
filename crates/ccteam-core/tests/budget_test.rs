//! V0.4.6 F84 — `max_cost_usd_per_24h` + `max_agent_spawns_per_hour`
//! budget caps.
//!
//! Coverage matrix (4 cases) — see `docs/versions/v0-4-6/dev-plan.md` §F84 测试矩阵.
//!
//! ## What these tests verify
//!
//! - Trip on 24h cost cap → `budget_exceeded` event written + workflow.yaml
//!   `enabled: false` set + `workflow_done reason="budget_exceeded"` event
//!   written.
//! - Trip on 1h spawn-rate cap → same audit trail with kind="spawn_rate".
//! - No `budget` block on workflow.yaml → no-op (V0.4.5 behaviour
//!   preserved).
//! - Re-enable-without-cap-change → next tick re-trips (sliding 24h
//!   window still ≥ cap).
//!
//! Tests drive `Orchestrator::enforce_budget` directly through the
//! public API (the method itself is `pub`); no MockAdapter is needed
//! because budget enforcement is read-only over progress.jsonl + a
//! single workflow.yaml mutation.

#![cfg(feature = "test-util")]

use std::path::PathBuf;

use chrono::Utc;
use indexmap::IndexMap;
use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::orchestrator::{Orchestrator, OrchestratorConfig};
use ccteam_core::workflow::{AgentSpec, BudgetSpec, Executor, Trigger, WorkflowSpec};
use ccteam_core::CcteamPaths;

// =====================================================================
// Fixture helpers
// =====================================================================

fn make_project(workflow_yaml: &str) -> (TempDir, TempDir, PathBuf, CcteamPaths, PathBuf, String) {
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let slug = format!("f84-{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let project_dir = projects_root.path().join(&slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("workflow.yaml"), workflow_yaml).unwrap();

    let paths = CcteamPaths {
        root: ccteam_root.path().to_path_buf(),
        projects_root: projects_root.path().to_path_buf(),
    };
    let progress_path = paths.progress_jsonl(&slug);
    (
        projects_root,
        ccteam_root,
        project_dir,
        paths,
        progress_path,
        slug,
    )
}

fn manual_spec_with_budget(role: &str, budget: Option<BudgetSpec>) -> WorkflowSpec {
    let mut agents = IndexMap::new();
    agents.insert(
        role.into(),
        AgentSpec {
            executor: Executor::Claude,
            model: None,
            trigger: Trigger::Manual,
            parallelism: None,
            input: None,
            output: None,
            schedule: None,
            timeout: None,
            on_timeout: None,
            plan_approval: None,
        },
    );
    WorkflowSpec {
        name: "test-budget".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        agents,
    }
}

fn write_agent_done(progress_path: &std::path::Path, slug: &str, role: &str, cost_usd: f64) {
    let sid = format!("{role}-{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let evt = json!({
        "event": "agent_done",
        "role": role,
        "session_id": sid,
        "status": "completed",
        "cost_usd": cost_usd,
        "slug": slug,
        "ts": Utc::now().to_rfc3339(),
    });
    ccteam_core::progress::append_event(progress_path, &evt).unwrap();
}

fn write_agent_spawn(progress_path: &std::path::Path, slug: &str, role: &str) {
    let sid = format!("{role}-{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let evt = json!({
        "event": "agent_spawn",
        "role": role,
        "session_id": sid,
        "slug": slug,
        "ts": Utc::now().to_rfc3339(),
    });
    ccteam_core::progress::append_event(progress_path, &evt).unwrap();
}

fn read_events(path: &std::path::Path) -> Vec<Value> {
    ccteam_core::progress::read_all_events(path).unwrap_or_default()
}

fn build_orchestrator(paths: CcteamPaths) -> Orchestrator {
    Orchestrator::new(paths, OrchestratorConfig::default()).unwrap()
}

// =====================================================================
// t01 — 24h cost cap trips, writes budget_exceeded + workflow_done +
// flips enabled:false on workflow.yaml.
// =====================================================================
#[tokio::test]
async fn t01_budget_cost_24h_trips() {
    let yaml = "\
name: test-budget
enabled: true
budget:
  max_cost_usd_per_24h: 0.50
agents:
  explorer:
    executor: claude
    trigger: manual
";
    let (_pr, _cr, project_dir, paths, progress_path, slug) = make_project(yaml);

    // Push 6 * 0.10 = 0.60 USD in agent_done events (exceeds 0.50 cap).
    for _ in 0..6 {
        write_agent_done(&progress_path, &slug, "explorer", 0.10);
    }

    let orch = build_orchestrator(paths);
    let spec = manual_spec_with_budget(
        "explorer",
        Some(BudgetSpec {
            max_cost_usd_per_24h: Some(0.50),
            max_agent_spawns_per_hour: None,
        }),
    );
    let tripped = orch
        .enforce_budget(&slug, &spec, &project_dir, &progress_path)
        .await
        .expect("enforce_budget should succeed");
    assert!(tripped, "should trip on cost_24h ≥ cap");

    let events = read_events(&progress_path);
    let exceeded: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "budget_exceeded")
        .collect();
    assert_eq!(exceeded.len(), 1, "exactly one budget_exceeded written");
    assert_eq!(exceeded[0]["kind"], "cost_24h");
    assert!(exceeded[0]["value"].as_f64().unwrap() >= 0.50);
    assert_eq!(exceeded[0]["cap"].as_f64().unwrap(), 0.50);

    let done: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "workflow_done")
        .collect();
    assert_eq!(done.len(), 1, "one workflow_done emitted by auto_disable");
    assert_eq!(done[0]["reason"], "budget_exceeded");

    // workflow.yaml must now have `enabled: false`.
    let yaml_after = std::fs::read_to_string(project_dir.join("workflow.yaml")).unwrap();
    assert!(
        yaml_after.contains("enabled: false"),
        "workflow.yaml should be flipped to enabled: false, got:\n{yaml_after}"
    );
}

// =====================================================================
// t02 — 1h spawn-rate cap trips, kind="spawn_rate", auto-disable wires
// same path as cost trip.
// =====================================================================
#[tokio::test]
async fn t02_budget_spawn_rate_trips() {
    let yaml = "\
name: test-budget
enabled: true
budget:
  max_agent_spawns_per_hour: 5
agents:
  explorer:
    executor: claude
    trigger: manual
";
    let (_pr, _cr, project_dir, paths, progress_path, slug) = make_project(yaml);

    // 6 agent_spawn events within the last hour → exceeds cap=5.
    for _ in 0..6 {
        write_agent_spawn(&progress_path, &slug, "explorer");
    }

    let orch = build_orchestrator(paths);
    let spec = manual_spec_with_budget(
        "explorer",
        Some(BudgetSpec {
            max_cost_usd_per_24h: None,
            max_agent_spawns_per_hour: Some(5),
        }),
    );
    let tripped = orch
        .enforce_budget(&slug, &spec, &project_dir, &progress_path)
        .await
        .expect("enforce_budget should succeed");
    assert!(tripped, "should trip on spawn_rate ≥ cap");

    let events = read_events(&progress_path);
    let exceeded: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "budget_exceeded")
        .collect();
    assert_eq!(exceeded.len(), 1, "exactly one budget_exceeded written");
    assert_eq!(exceeded[0]["kind"], "spawn_rate");
    assert!(exceeded[0]["value"].as_u64().unwrap() >= 5);
    assert_eq!(exceeded[0]["cap"].as_u64().unwrap(), 5);

    let done: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "workflow_done")
        .collect();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["reason"], "spawn_rate_exceeded");

    let yaml_after = std::fs::read_to_string(project_dir.join("workflow.yaml")).unwrap();
    assert!(
        yaml_after.contains("enabled: false"),
        "workflow.yaml should be flipped to enabled: false, got:\n{yaml_after}"
    );
}

// =====================================================================
// t03 — Missing `budget` block → no-op (V0.4.5 behaviour preserved).
// =====================================================================
#[tokio::test]
async fn t03_no_budget_no_op() {
    let yaml = "\
name: test-budget
enabled: true
agents:
  explorer:
    executor: claude
    trigger: manual
";
    let (_pr, _cr, project_dir, paths, progress_path, slug) = make_project(yaml);

    // Even arbitrarily high cost shouldn't trip when no budget set.
    for _ in 0..50 {
        write_agent_done(&progress_path, &slug, "explorer", 10.0);
    }

    let orch = build_orchestrator(paths);
    // No budget spec at all.
    let spec = manual_spec_with_budget("explorer", None);
    let tripped = orch
        .enforce_budget(&slug, &spec, &project_dir, &progress_path)
        .await
        .expect("enforce_budget should succeed");
    assert!(!tripped, "no budget block → never trip");

    let events = read_events(&progress_path);
    assert!(
        events.iter().all(|e| e["event"] != "budget_exceeded"),
        "no budget_exceeded should be emitted"
    );
    assert!(
        events.iter().all(|e| e["event"] != "workflow_done"),
        "no workflow_done should be emitted by enforce_budget"
    );

    // workflow.yaml stays unchanged.
    let yaml_after = std::fs::read_to_string(project_dir.join("workflow.yaml")).unwrap();
    assert!(
        yaml_after.contains("enabled: true"),
        "workflow.yaml should remain enabled: true"
    );
}

// =====================================================================
// t04 — Re-enable workflow.yaml after a trip without changing cap →
// next tick re-trips (24h window still ≥ cap). Verifies budget is
// idempotent over re-enables (PRD F84 验收 #2).
// =====================================================================
#[tokio::test]
async fn t04_disabled_then_reenabled_immediate_retrip() {
    let yaml = "\
name: test-budget
enabled: true
budget:
  max_cost_usd_per_24h: 0.50
agents:
  explorer:
    executor: claude
    trigger: manual
";
    let (_pr, _cr, project_dir, paths, progress_path, slug) = make_project(yaml);

    // Seed cost over cap.
    for _ in 0..6 {
        write_agent_done(&progress_path, &slug, "explorer", 0.10);
    }

    let orch = build_orchestrator(paths);
    let spec = manual_spec_with_budget(
        "explorer",
        Some(BudgetSpec {
            max_cost_usd_per_24h: Some(0.50),
            max_agent_spawns_per_hour: None,
        }),
    );

    // First trip.
    let first = orch
        .enforce_budget(&slug, &spec, &project_dir, &progress_path)
        .await
        .unwrap();
    assert!(first, "first call should trip");

    // User manually re-enables.
    let yaml_re = std::fs::read_to_string(project_dir.join("workflow.yaml"))
        .unwrap()
        .replace("enabled: false", "enabled: true");
    std::fs::write(project_dir.join("workflow.yaml"), yaml_re).unwrap();

    // Second call (cost still over cap) re-trips and writes a fresh
    // pair of events.
    let second = orch
        .enforce_budget(&slug, &spec, &project_dir, &progress_path)
        .await
        .unwrap();
    assert!(second, "second call should re-trip (window still over cap)");

    let events = read_events(&progress_path);
    let exceeded: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "budget_exceeded")
        .collect();
    assert_eq!(
        exceeded.len(),
        2,
        "both trips should write budget_exceeded for audit"
    );

    // YAML is back to enabled: false after second trip.
    let yaml_after = std::fs::read_to_string(project_dir.join("workflow.yaml")).unwrap();
    assert!(
        yaml_after.contains("enabled: false"),
        "second trip should re-flip workflow.yaml to enabled: false"
    );
}
