//! V0.4.0 F67 — workflow event aggregation tests.
//!
//! Cases per `docs/v0-4-0/dev-plan.md` §9.1 #8.5. Each case is a pure
//! function call against synthesised `Value` events — no tmux, no
//! claude binary, no inotify (so the F64 flake on WSL never touches
//! this suite).
//!
//! Red-line audit:
//! - The progress.rs API surface no longer exports phase-specific
//!   query helpers; t06 enforces this at compile time via the
//!   `use` import list.
//! - WorkflowSummary derives default behaviour for legacy / empty
//!   projects (t05 + t10); the workflow view never panics when the
//!   project's progress.jsonl is fresh.
//!
//! Architecture refs: `docs/v0-4-0/prd.md` §F67,
//! `docs/v0-4-0/dev-plan.md` §9,
//! `docs/dev-coupling-audit.md` F67.

use std::collections::HashMap;

use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::progress::{
    current_agent_sessions, escalation_count, workflow_cost_total, AgentSessionStatus,
};
// t06 compile-level audit: importing the previously-exported phase
// helpers would fail here (they were dropped in F60 + F67). Listed in
// the body as comments so the audit is greppable.
use ccteam_core::queries::workflow_summary;
use ccteam_core::state::ProjectState;
use ccteam_core::team::TeamKind;
use ccteam_core::CcteamPaths;

// =====================================================================
// helpers
// =====================================================================

fn spawn(role: &str, sid: &str, ts: &str) -> Value {
    json!({
        "event": "agent_spawn",
        "role": role,
        "session_id": sid,
        "executor": "claude",
        "ts": ts,
    })
}

fn done(role: &str, sid: &str, status: &str, cost_usd: f64, ts: &str) -> Value {
    json!({
        "event": "agent_done",
        "role": role,
        "session_id": sid,
        "status": status,
        "cost_usd": cost_usd,
        "ts": ts,
    })
}

fn escalation(role: &str, ts: &str) -> Value {
    json!({
        "event": "escalation",
        "kind": "spawn_failed",
        "role": role,
        "consecutive_failures": 1,
        "ts": ts,
    })
}

fn make_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join(".ccteam"),
        projects_root: tmp.path().join("projects"),
    }
}

/// Materialise a workflow project at `<projects_root>/<slug>/` with
/// the given workflow.yaml body and optional progress events.
fn make_project(
    tmp: &TempDir,
    slug: &str,
    workflow_yaml: &str,
    events: &[Value],
) -> (CcteamPaths, std::path::PathBuf) {
    let paths = make_paths(tmp);
    let project_dir = paths.project_dir(slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    std::fs::write(project_dir.join("workflow.yaml"), workflow_yaml).unwrap();
    ProjectState::initial(slug.to_string())
        .save(&paths.project_state(slug))
        .unwrap();
    let progress = paths.progress_jsonl(slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    let body: String = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    std::fs::write(&progress, body).unwrap();
    (paths, project_dir)
}

// =====================================================================
// test cases
// =====================================================================

#[test]
fn t01_workflow_cost_total() {
    let events = vec![
        spawn("planner", "planner-1", "2026-05-10T00:00:00Z"),
        done(
            "planner",
            "planner-1",
            "completed",
            0.1,
            "2026-05-10T00:00:01Z",
        ),
        spawn("fixer", "fixer-1", "2026-05-10T00:00:02Z"),
        done("fixer", "fixer-1", "completed", 0.2, "2026-05-10T00:00:03Z"),
        spawn("reviewer", "reviewer-1", "2026-05-10T00:00:04Z"),
        done(
            "reviewer",
            "reviewer-1",
            "completed",
            0.3,
            "2026-05-10T00:00:05Z",
        ),
    ];
    let total = workflow_cost_total(&events);
    // Float arithmetic: 0.1 + 0.2 + 0.3 = 0.6 within rounding tolerance.
    assert!((total - 0.6).abs() < 1e-9, "expected 0.6, got {total}");
}

#[test]
fn t02_current_agent_sessions_open() {
    // spawn without corresponding done — session is Running.
    let events = vec![spawn("planner", "planner-1", "2026-05-10T00:00:00Z")];
    let sessions = current_agent_sessions(&events);
    assert_eq!(sessions.len(), 1, "one open session expected");
    assert_eq!(sessions[0].role, "planner");
    assert_eq!(sessions[0].session_id, "planner-1");
    assert!(matches!(sessions[0].status, AgentSessionStatus::Running));
}

#[test]
fn t03_current_agent_sessions_closed() {
    // spawn + done → session resolves to Done, not Running.
    let events = vec![
        spawn("planner", "planner-1", "2026-05-10T00:00:00Z"),
        done(
            "planner",
            "planner-1",
            "completed",
            0.5,
            "2026-05-10T00:00:01Z",
        ),
    ];
    let sessions = current_agent_sessions(&events);
    assert_eq!(sessions.len(), 1);
    match &sessions[0].status {
        AgentSessionStatus::Done { cost_usd } => {
            assert!((cost_usd - 0.5).abs() < 1e-9);
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn t04_escalation_count() {
    let events = vec![
        spawn("fixer", "fixer-1", "2026-05-10T00:00:00Z"),
        escalation("fixer", "2026-05-10T00:00:01Z"),
        escalation("fixer", "2026-05-10T00:00:02Z"),
        // unrelated events must not contribute.
        json!({"event": "PostToolUse", "tool": "Edit", "ts": "2026-05-10T00:00:03Z"}),
    ];
    assert_eq!(escalation_count(&events), 2);
}

#[test]
fn t05_workflow_summary_empty() {
    // Empty event slice + no workflow.yaml → defaulted summary,
    // never panics.
    let tmp = TempDir::new().unwrap();
    let paths = make_paths(&tmp);
    let slug = "empty-proj";
    std::fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
    ProjectState::initial(slug.to_string())
        .save(&paths.project_state(slug))
        .unwrap();

    let summary = workflow_summary(slug, &paths).unwrap();
    assert_eq!(summary.workflow_name, "");
    assert!(summary.agents.is_empty());
    assert!(summary.artifact_counts.is_empty());
    assert!((summary.total_cost_usd - 0.0).abs() < 1e-9);
    assert_eq!(summary.escalation_count, 0);
    assert!(summary.gate_states.is_empty());
}

#[test]
fn t06_latest_terminal_event_removed() {
    // Compile-level audit: the F60 phase machinery left no
    // `latest_terminal_event_for_phase`, `phase_transition_events`,
    // or `phase_history` symbols on `ccteam_core::progress::*`.
    // If any of those re-appear, the following commented-out lines
    // would fail to compile when uncommented.
    //
    // use ccteam_core::progress::latest_terminal_event_for_phase;
    // use ccteam_core::progress::phase_transition_events;
    // use ccteam_core::progress::phase_history;
    //
    // Runtime check: the new aggregation surface uses only the new
    // event kinds and does not match any phase event names.
    let phase_events = vec![
        json!({"event": "phase_start", "phase": "implement"}),
        json!({"event": "phase_done", "phase": "implement", "cost_usd": 0.5}),
        json!({"event": "golden_rules_check", "phase": "implement"}),
    ];
    assert_eq!(
        workflow_cost_total(&phase_events),
        0.0,
        "legacy phase events MUST NOT contribute to workflow cost",
    );
    assert!(
        current_agent_sessions(&phase_events).is_empty(),
        "legacy phase events MUST NOT spawn agent sessions",
    );
    assert_eq!(
        escalation_count(&phase_events),
        0,
        "legacy phase events MUST NOT count as escalations",
    );
}

#[test]
fn t07_append_and_read_roundtrip() {
    // append_event + read_all_events round-trip — the SoT primitive
    // pair survives F67's refactor unchanged.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("progress.jsonl");
    let e1 = spawn("planner", "planner-1", "2026-05-10T00:00:00Z");
    let e2 = done(
        "planner",
        "planner-1",
        "completed",
        0.42,
        "2026-05-10T00:00:01Z",
    );
    ccteam_core::progress::append_event(&path, &e1).unwrap();
    ccteam_core::progress::append_event(&path, &e2).unwrap();

    let events = ccteam_core::progress::read_all_events(&path).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["session_id"], "planner-1");
    assert_eq!(events[1]["cost_usd"].as_f64().unwrap(), 0.42);
}

#[test]
fn t08_workflow_summary_with_artifacts() {
    // Fixture workflow.yaml + populated input/output dirs → counts
    // surface in the summary keyed by relative path.
    let tmp = TempDir::new().unwrap();
    let slug = "watch-proj";
    let yaml = "\
name: watcher
agents:
  fixer:
    executor: claude
    trigger: watch:issues
    parallelism: 1
    input: issues
    output: fixes
";
    let events = vec![spawn("fixer", "fixer-1", "2026-05-10T00:00:00Z")];
    let (paths, project_dir) = make_project(&tmp, slug, yaml, &events);

    let issues_dir = project_dir.join("issues");
    let fixes_dir = project_dir.join("fixes");
    std::fs::create_dir_all(&issues_dir).unwrap();
    std::fs::create_dir_all(&fixes_dir).unwrap();
    std::fs::write(issues_dir.join("a.md"), "a").unwrap();
    std::fs::write(issues_dir.join("b.md"), "b").unwrap();
    std::fs::write(fixes_dir.join("z.md"), "z").unwrap();

    let summary = workflow_summary(slug, &paths).unwrap();
    assert_eq!(summary.workflow_name, "watcher");
    let counts: HashMap<String, u64> = summary.artifact_counts.clone();
    assert_eq!(counts.get("issues").copied(), Some(2));
    assert_eq!(counts.get("fixes").copied(), Some(1));

    // Agent row should show one running session for `fixer`.
    let fixer = summary
        .agents
        .iter()
        .find(|a| a.role == "fixer")
        .expect("fixer agent row");
    assert_eq!(fixer.running_count, 1);
    assert_eq!(fixer.queued_count, 0);
}

#[test]
fn t09_cost_accumulation_from_multiple_agents() {
    // Three roles, each with one completed session — total_cost is
    // the sum.
    let tmp = TempDir::new().unwrap();
    let slug = "multi-role";
    let yaml = "\
name: multi
agents:
  planner:
    executor: claude
    trigger: manual
  fixer:
    executor: claude
    trigger: watch:issues
    input: issues
  reviewer:
    executor: claude
    trigger: gate
    input: candidates
";
    let events = vec![
        spawn("planner", "planner-1", "2026-05-10T00:00:00Z"),
        done(
            "planner",
            "planner-1",
            "completed",
            0.10,
            "2026-05-10T00:00:01Z",
        ),
        spawn("fixer", "fixer-1", "2026-05-10T00:00:02Z"),
        done(
            "fixer",
            "fixer-1",
            "completed",
            0.20,
            "2026-05-10T00:00:03Z",
        ),
        spawn("reviewer", "reviewer-1", "2026-05-10T00:00:04Z"),
        done(
            "reviewer",
            "reviewer-1",
            "completed",
            0.30,
            "2026-05-10T00:00:05Z",
        ),
    ];
    let (paths, _pdir) = make_project(&tmp, slug, yaml, &events);

    let summary = workflow_summary(slug, &paths).unwrap();
    assert!(
        (summary.total_cost_usd - 0.6).abs() < 1e-9,
        "expected 0.6, got {}",
        summary.total_cost_usd,
    );
    let by_role: HashMap<&str, f64> = summary
        .agents
        .iter()
        .map(|a| (a.role.as_str(), a.total_cost_usd))
        .collect();
    assert!((by_role["planner"] - 0.10).abs() < 1e-9);
    assert!((by_role["fixer"] - 0.20).abs() < 1e-9);
    assert!((by_role["reviewer"] - 0.30).abs() < 1e-9);
}

#[test]
fn t10_empty_progress_file_returns_defaults() {
    // workflow.yaml present + empty progress.jsonl → summary defaults
    // for counts/cost, agent rows present from the spec.
    let tmp = TempDir::new().unwrap();
    let slug = "fresh-proj";
    let yaml = "\
name: fresh
agents:
  planner:
    executor: claude
    trigger: manual
";
    let (paths, _pdir) = make_project(&tmp, slug, yaml, &[]);

    let summary = workflow_summary(slug, &paths).unwrap();
    assert_eq!(summary.workflow_name, "fresh");
    assert_eq!(summary.escalation_count, 0);
    assert!((summary.total_cost_usd - 0.0).abs() < 1e-9);
    assert_eq!(summary.agents.len(), 1);
    let planner = &summary.agents[0];
    assert_eq!(planner.role, "planner");
    assert_eq!(planner.running_count, 0);
    assert_eq!(planner.queued_count, 0);
    assert!(planner.last_session_status.is_none());
    assert!((planner.total_cost_usd - 0.0).abs() < 1e-9);
}

// ---------------- bonus coverage ----------------

#[test]
fn t11_gate_state_fired_after_event() {
    // Workflow with a gate role; a gate_triggered event flips the
    // gate_state map entry from "waiting" to "fired".
    let tmp = TempDir::new().unwrap();
    let slug = "gate-proj";
    let yaml = "\
name: gated
agents:
  reviewer:
    executor: claude
    trigger: gate
    input: candidates
";
    let events = vec![json!({
        "event": "gate_triggered",
        "role": "reviewer",
        "ts": "2026-05-10T00:00:00Z",
    })];
    let (paths, _pdir) = make_project(&tmp, slug, yaml, &events);
    let summary = workflow_summary(slug, &paths).unwrap();
    assert_eq!(
        summary.gate_states.get("reviewer").map(String::as_str),
        Some("fired"),
    );
}

#[test]
fn t12_flex_project_uses_session_streams() {
    // Flex project writes events to <slug>/<sid>.jsonl rather than
    // <slug>.jsonl — workflow_summary must still read them.
    let tmp = TempDir::new().unwrap();
    let paths = make_paths(&tmp);
    let slug = "flex-proj";
    let project_dir = paths.project_dir(slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    std::fs::write(
        project_dir.join("workflow.yaml"),
        "\
name: flexworkflow
agents:
  planner:
    executor: claude
    trigger: manual
",
    )
    .unwrap();

    let mut state = ProjectState::initial_for_team(slug.into(), "flex".into());
    state.team_kind = TeamKind::Flex;
    state.save(&paths.project_state(slug)).unwrap();

    let sid_path = paths.progress_jsonl_for_session(slug, "claude-1");
    std::fs::create_dir_all(sid_path.parent().unwrap()).unwrap();
    let body = serde_json::to_string(&done(
        "planner",
        "planner-1",
        "completed",
        0.42,
        "2026-05-10T00:00:00Z",
    ))
    .unwrap()
        + "\n";
    std::fs::write(&sid_path, body).unwrap();

    let summary = workflow_summary(slug, &paths).unwrap();
    assert!(
        (summary.total_cost_usd - 0.42).abs() < 1e-9,
        "flex session-stream cost expected 0.42, got {}",
        summary.total_cost_usd,
    );
}
