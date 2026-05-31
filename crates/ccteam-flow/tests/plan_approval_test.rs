//! V0.6.1 F98 — plan-approval ↔ outbox 联动 integration test.
//!
//! Drives the full loop with a mock outbox:
//!   1. agent writes `<project>/.ccteam/plans/reviewer-<ts>.md`
//!   2. engine.scan_plans() → SendIm + EmitEvent(plan_pending)
//!   3. test pretends user replied "APPROVE" via IM
//!   4. engine.apply_decision() → WriteDecisionFile + EmitEvent(plan_decision)
//!   5. assert decision file body + progress.jsonl event ordering
//!   6. separate scenario: 60min timeout w/ on_timeout=escalate
//!
//! Acceptance per `docs/versions/v0-6-1/prd.md` §F98:
//!   ✓ workflow.yaml `plan_approval:` block round-trips through serde
//!   ✓ plan write → IM body contains plan head + APPROVE/REJECT/EDIT hint
//!   ✓ APPROVE → orchestrator inject decision + agent resume path
//!   ✓ 60min no reply + on_timeout=escalate → emit plan_timeout + ping body
//!   ✓ mock IM full loop pass

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use serde_json::Value;
use tempfile::TempDir;

use ccteam_core::plan_approval::{
    apply_action, parse_decision, PlanApprovalEngine, PlanApprovalEngineConfig, PlanDecision,
    PlanEngineAction, PlanId, PlanRecordState,
};
use ccteam_core::progress;
use ccteam_flow::workflow::{PlanApprovalOnTimeout, WorkflowSpec};

#[derive(Default, Clone)]
struct MockOutbox {
    sent: Arc<Mutex<Vec<(String, String)>>>,
    escalations: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockOutbox {
    fn dispatch(&self, action: &PlanEngineAction) {
        match action {
            PlanEngineAction::SendIm { outbox, body, .. } => {
                self.sent
                    .lock()
                    .unwrap()
                    .push((outbox.clone(), body.clone()));
            }
            PlanEngineAction::Escalate { agent, body, .. } => {
                self.escalations
                    .lock()
                    .unwrap()
                    .push((agent.clone(), body.clone()));
            }
            _ => {}
        }
    }

    fn ims(&self) -> Vec<(String, String)> {
        self.sent.lock().unwrap().clone()
    }
    fn escalations(&self) -> Vec<(String, String)> {
        self.escalations.lock().unwrap().clone()
    }
}

fn read_progress(path: &Path) -> Vec<Value> {
    progress::read_all_events(path).unwrap()
}

fn build_engine(
    project_dir: PathBuf,
    timeout_min: u32,
    on_timeout: PlanApprovalOnTimeout,
) -> PlanApprovalEngine {
    let cfg = PlanApprovalEngineConfig {
        project_dir,
        outbox: "telegram".to_string(),
        timeout: Duration::from_secs(u64::from(timeout_min) * 60),
        on_timeout,
    };
    PlanApprovalEngine::new(cfg)
}

#[test]
fn schema_round_trips_plan_approval_block() {
    let yaml = r#"
name: review-wf
agents:
  reviewer:
    trigger: manual
    plan_approval:
      enabled: true
      outbox: telegram
      timeout_min: 60
      on_timeout: escalate
"#;
    let spec: WorkflowSpec = serde_yaml::from_str(yaml).expect("parse");
    let agent = spec.agents.get("reviewer").expect("reviewer agent present");
    let pa = agent
        .plan_approval
        .as_ref()
        .expect("plan_approval block parsed");
    assert!(pa.enabled);
    assert_eq!(pa.outbox, "telegram");
    assert_eq!(pa.timeout_min, 60);
    assert_eq!(pa.on_timeout, PlanApprovalOnTimeout::Escalate);
}

#[test]
fn schema_defaults_when_partial() {
    // outbox is the only required field; the rest default.
    let yaml = r#"
name: review-wf
agents:
  reviewer:
    trigger: manual
    plan_approval:
      outbox: telegram
"#;
    let spec: WorkflowSpec = serde_yaml::from_str(yaml).expect("parse");
    let pa = spec
        .agents
        .get("reviewer")
        .unwrap()
        .plan_approval
        .as_ref()
        .unwrap();
    assert!(pa.enabled, "enabled defaults to true when block present");
    assert_eq!(pa.timeout_min, 60, "default 60 min");
    assert_eq!(pa.on_timeout, PlanApprovalOnTimeout::Escalate);
}

#[test]
fn parse_decision_recognises_canonical_replies() {
    assert_eq!(parse_decision("APPROVE"), Some(PlanDecision::Approve));
    assert_eq!(
        parse_decision("approve"),
        Some(PlanDecision::Approve),
        "case-insensitive"
    );
    assert_eq!(
        parse_decision("REJECT"),
        Some(PlanDecision::Reject { reason: None })
    );
    assert_eq!(
        parse_decision("REJECT plan is too risky"),
        Some(PlanDecision::Reject {
            reason: Some("plan is too risky".to_string())
        })
    );
    assert_eq!(
        parse_decision("EDIT add a rollback step"),
        Some(PlanDecision::Edit {
            comment: "add a rollback step".to_string()
        })
    );
    assert_eq!(parse_decision("hello, are you there?"), None);
}

#[test]
fn full_loop_approve_path() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let plans_dir = project_dir.join(".ccteam/plans");
    let progress_path = project_dir.join(".ccteam/progress.jsonl");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();

    // 1) Agent writes the plan.
    let plan_path = plans_dir.join("reviewer-20260519T1200.md");
    std::fs::write(
        &plan_path,
        "# Plan: refactor auth\n\n- Phase 1\n- Phase 2\n- Phase 3\n",
    )
    .unwrap();

    let mut engine = build_engine(project_dir.clone(), 60, PlanApprovalOnTimeout::Escalate);
    let outbox = MockOutbox::default();

    // 2) Scan picks up the new plan → IM + plan_pending event.
    let actions = engine
        .scan_plans(Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap())
        .unwrap();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, PlanEngineAction::SendIm { .. })),
        "must surface a SendIm action"
    );
    for action in &actions {
        outbox.dispatch(action);
        apply_action(action, &progress_path).unwrap();
    }
    let ims = outbox.ims();
    assert_eq!(ims.len(), 1, "exactly one IM notice");
    assert_eq!(ims[0].0, "telegram");
    assert!(
        ims[0].1.contains("reviewer-20260519T1200"),
        "IM body carries plan_id: {}",
        ims[0].1
    );
    assert!(
        ims[0].1.contains("APPROVE"),
        "IM body prompts APPROVE: {}",
        ims[0].1
    );
    assert!(
        ims[0].1.contains("refactor auth"),
        "IM body carries plan head"
    );

    // Idempotent rescan — no new actions.
    let again = engine
        .scan_plans(Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 5).unwrap())
        .unwrap();
    assert!(again.is_empty(), "second scan must be a no-op");

    // 3) User replies APPROVE via IM (the test plays the inbound side).
    let plan_id = PlanId("reviewer-20260519T1200".to_string());
    let decision = parse_decision("APPROVE").unwrap();
    let actions = engine.apply_decision(
        &plan_id,
        decision,
        Utc.with_ymd_and_hms(2026, 5, 19, 12, 5, 0).unwrap(),
    );
    for action in &actions {
        apply_action(action, &progress_path).unwrap();
    }

    // 4) Verify decision file body.
    let decision_path = project_dir
        .join(".ccteam/plan-decisions")
        .join("reviewer-20260519T1200.md");
    assert!(
        decision_path.exists(),
        "decision file written at {}",
        decision_path.display()
    );
    let body = std::fs::read_to_string(&decision_path).unwrap();
    assert!(body.contains("decision: approve"), "body: {body}");
    assert!(body.contains("APPROVED"), "body: {body}");

    // 5) Verify progress.jsonl ordering: plan_pending then plan_decision.
    let events = read_progress(&progress_path);
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(|s| s.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![progress::PLAN_PENDING, progress::PLAN_DECISION],
        "exactly plan_pending → plan_decision in order"
    );
    let decision_ev = events
        .iter()
        .find(|e| e["event"] == progress::PLAN_DECISION)
        .unwrap();
    assert_eq!(decision_ev["decision"], "approve");
    assert_eq!(decision_ev["agent"], "reviewer");

    // 6) Idempotent: a second APPROVE for the same plan is a no-op.
    let dup = engine.apply_decision(
        &plan_id,
        PlanDecision::Approve,
        Utc.with_ymd_and_hms(2026, 5, 19, 12, 6, 0).unwrap(),
    );
    assert!(dup.is_empty(), "duplicate APPROVE must be a no-op");
}

#[test]
fn full_loop_reject_with_reason_path() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let plans_dir = project_dir.join(".ccteam/plans");
    let progress_path = project_dir.join(".ccteam/progress.jsonl");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();

    let plan_path = plans_dir.join("planner-001.md");
    std::fs::write(&plan_path, "Plan body").unwrap();

    let mut engine = build_engine(project_dir.clone(), 60, PlanApprovalOnTimeout::Escalate);
    let now = Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap();
    for a in engine.scan_plans(now).unwrap() {
        apply_action(&a, &progress_path).unwrap();
    }

    let plan_id = PlanId("planner-001".to_string());
    let decision = parse_decision("REJECT plan is too risky").unwrap();
    let actions = engine.apply_decision(&plan_id, decision, now);
    for a in &actions {
        apply_action(a, &progress_path).unwrap();
    }

    let body = std::fs::read_to_string(plan_id.decision_path(&project_dir)).unwrap();
    assert!(body.contains("decision: reject"));
    assert!(body.contains("plan is too risky"));

    let events = read_progress(&progress_path);
    let decision_ev = events
        .iter()
        .find(|e| e["event"] == progress::PLAN_DECISION)
        .unwrap();
    assert_eq!(decision_ev["decision"], "reject");
    assert_eq!(decision_ev["comment"], "plan is too risky");
}

#[test]
fn timeout_escalate_emits_event_and_escalate_action() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let plans_dir = project_dir.join(".ccteam/plans");
    let progress_path = project_dir.join(".ccteam/progress.jsonl");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
    std::fs::write(plans_dir.join("reviewer-late.md"), "plan").unwrap();

    let mut engine = build_engine(project_dir.clone(), 60, PlanApprovalOnTimeout::Escalate);
    let outbox = MockOutbox::default();
    let t0 = Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap();
    for a in engine.scan_plans(t0).unwrap() {
        outbox.dispatch(&a);
        apply_action(&a, &progress_path).unwrap();
    }
    assert_eq!(outbox.ims().len(), 1);

    // Within 30 min: no timeout fired.
    let t30 = Utc.with_ymd_and_hms(2026, 5, 19, 12, 30, 0).unwrap();
    assert!(engine.tick_timeouts(t30).is_empty());

    // 61 min after notify: timeout fires.
    let t61 = Utc.with_ymd_and_hms(2026, 5, 19, 13, 1, 0).unwrap();
    let actions = engine.tick_timeouts(t61);
    for a in &actions {
        outbox.dispatch(a);
        apply_action(a, &progress_path).unwrap();
    }

    // Verify plan state.
    let plan_id = PlanId("reviewer-late".to_string());
    let rec = engine.plans().get(&plan_id).unwrap();
    assert!(
        matches!(rec.state, PlanRecordState::TimedOut),
        "state must be TimedOut, was {:?}",
        rec.state
    );

    // Verify event + escalate.
    let events = read_progress(&progress_path);
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(|s| s.as_str()))
        .collect();
    assert!(kinds.contains(&progress::PLAN_TIMEOUT));
    let escalations = outbox.escalations();
    assert_eq!(escalations.len(), 1, "one escalation push");
    assert_eq!(escalations[0].0, "reviewer");
    assert!(escalations[0].1.contains("reviewer-late"));
}

#[test]
fn timeout_auto_approve_synthesizes_decision() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let plans_dir = project_dir.join(".ccteam/plans");
    let progress_path = project_dir.join(".ccteam/progress.jsonl");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
    std::fs::write(plans_dir.join("nightly-1.md"), "plan").unwrap();

    let mut engine = build_engine(project_dir.clone(), 60, PlanApprovalOnTimeout::AutoApprove);
    let t0 = Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap();
    for a in engine.scan_plans(t0).unwrap() {
        apply_action(&a, &progress_path).unwrap();
    }
    let t_after = Utc.with_ymd_and_hms(2026, 5, 19, 14, 0, 0).unwrap();
    let actions = engine.tick_timeouts(t_after);
    for a in &actions {
        apply_action(a, &progress_path).unwrap();
    }

    // Decision file must exist with `decision: approve`.
    let plan_id = PlanId("nightly-1".to_string());
    let body = std::fs::read_to_string(plan_id.decision_path(&project_dir)).unwrap();
    assert!(body.contains("decision: approve"));

    let events = read_progress(&progress_path);
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(|s| s.as_str()))
        .collect();
    assert!(kinds.contains(&progress::PLAN_TIMEOUT));
    assert!(kinds.contains(&progress::PLAN_DECISION));
}

#[test]
fn timeout_zero_disables_timeout_logic() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let plans_dir = project_dir.join(".ccteam/plans");
    let progress_path = project_dir.join(".ccteam/progress.jsonl");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
    std::fs::write(plans_dir.join("forever-1.md"), "plan").unwrap();

    let mut engine = build_engine(project_dir, 0, PlanApprovalOnTimeout::Escalate);
    let t0 = Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap();
    for a in engine.scan_plans(t0).unwrap() {
        apply_action(&a, &progress_path).unwrap();
    }
    let t_far = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let actions = engine.tick_timeouts(t_far);
    assert!(
        actions.is_empty(),
        "timeout=0 must never fire, got {actions:?}"
    );
}

#[test]
fn apply_decision_unknown_plan_id_is_noop() {
    let tmp = TempDir::new().unwrap();
    let mut engine = build_engine(
        tmp.path().to_path_buf(),
        60,
        PlanApprovalOnTimeout::Escalate,
    );
    let plan_id = PlanId("never-existed".to_string());
    let actions = engine.apply_decision(
        &plan_id,
        PlanDecision::Approve,
        Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap(),
    );
    assert!(actions.is_empty());
}
