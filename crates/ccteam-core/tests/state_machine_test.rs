//! Pure-logic + integration tests for the M0.9 state machine.

use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use tempfile::TempDir;

use ccteam_core::{
    decide_tick, decide_tick_from_events, dev_dag, progress, write_global_phase_templates,
    CcteamPaths, Orchestrator, OrchestratorConfig, Parallelism, PhaseHistoryEntry, PhaseState,
    ProjectState, TeamKind, TickAction,
};

fn fresh_state(current_phase: &str, phase_state: PhaseState) -> ProjectState {
    let now = Utc::now();
    ProjectState {
        slug: "demo".into(),
        team: "dev".into(),
        team_kind: TeamKind::Workflow,
        created_at: now,
        tmux_session: "ccteam-demo".into(),
        claude_session_id: None,
        claude_pid: None,
        phase_state,
        current_phase: current_phase.into(),
        parallelism: Parallelism::Solo,
        phase_history: Vec::new(),
        auto_loop_cycle_count: 0,
        cost_used_usd: 0.0,
        soft_warn_threshold_usd: 20.0,
        hard_kill_threshold_usd: 200.0,
        context_tokens_used: 0,
        context_reset_threshold_tokens: 600_000,
        context_reset_count: 0,
        last_progress_event_at: None,
        last_event_type: None,
        last_user_interaction_at: now,
        user_attached: false,
        user_pause_pending: false,
        sessions: BTreeMap::new(),
        next_sid_seq: BTreeMap::new(),
    }
}

#[test]
fn dev_dag_walks_through_every_phase() {
    let dag = dev_dag();
    let chain = ["plan-eng", "implement", "test-author", "test-run", "fix", "ship"];
    for w in chain.windows(2) {
        assert_eq!(dag.next_on_done(w[0]), Some(w[1]));
    }
    assert_eq!(dag.next_on_done("ship"), None);
    assert_eq!(dag.next_on_done("not-a-real-phase"), None);
    assert_eq!(dag.entry(), "plan-eng");
    assert!(dag.is_terminal_phase("ship"));
}

#[test]
fn decide_tick_dispatches_first_phase_for_fresh_project() {
    let dag = dev_dag();
    let state = fresh_state("", PhaseState::Idle);
    assert_eq!(
        decide_tick(&dag, &state, None),
        TickAction::DispatchPhase {
            phase: dag.entry().into(),
        },
    );
}

#[test]
fn decide_tick_advances_when_phase_done_matches() {
    let dag = dev_dag();
    let state = fresh_state("implement", PhaseState::InFlight);
    let event = json!({"event": "phase_done", "phase": "implement"});
    assert_eq!(
        decide_tick(&dag, &state, Some(&event)),
        TickAction::AdvancePhase {
            from: "implement".into(),
            to: Some("test-author".into()),
        },
    );
}

#[test]
fn decide_tick_advances_to_none_after_ship() {
    let dag = dev_dag();
    let state = fresh_state("ship", PhaseState::InFlight);
    let event = json!({"event": "phase_done", "phase": "ship"});
    assert_eq!(
        decide_tick(&dag, &state, Some(&event)),
        TickAction::AdvancePhase {
            from: "ship".into(),
            to: None,
        },
    );
}

#[test]
fn decide_tick_ignores_phase_done_for_a_different_phase() {
    let dag = dev_dag();
    let state = fresh_state("implement", PhaseState::InFlight);
    let stale = json!({"event": "phase_done", "phase": "plan-eng"});
    assert_eq!(decide_tick(&dag, &state, Some(&stale)), TickAction::NoOp);
}

#[test]
fn decide_tick_classifies_busy_events_as_noop() {
    let dag = dev_dag();
    let state = fresh_state("implement", PhaseState::InFlight);
    for kind in ["PreToolUse", "PostToolUse", "phase_inject", "SubagentStop"] {
        let e = json!({"event": kind});
        assert_eq!(
            decide_tick(&dag, &state, Some(&e)),
            TickAction::NoOp,
            "{kind} must be NoOp",
        );
    }
}

#[test]
fn decide_tick_advances_through_phase_done_then_subagent_stop_sequence() {
    // E2E 2026-05-06 F1+F2 regression: when the finished turn used Task,
    // Claude Code emits Stop, parse-phase-end appends phase_done, and
    // SubagentStop fires 2–5 s later. The literal last event in
    // progress.jsonl is SubagentStop, but the project did finish — the
    // tick must still resolve to AdvancePhase. Pair this with the
    // is_idle change so the subsequent inject is sent bare instead of
    // wrapped in `/btw` (which would spawn a toolless side-agent).
    let dag = dev_dag();
    let state = fresh_state("plan-eng", PhaseState::InFlight);
    let events = vec![
        json!({"event": "phase_inject", "phase": "plan-eng"}),
        json!({"event": "PostToolUse", "tool": "Write"}),
        json!({"event": "Stop"}),
        json!({"event": "phase_done", "phase": "plan-eng"}),
        json!({"event": "SubagentStop"}),
    ];
    assert_eq!(
        decide_tick_from_events(&dag, &state, &events),
        TickAction::AdvancePhase {
            from: "plan-eng".into(),
            to: Some("implement".into()),
        },
    );
    // And the dispatcher's idle classifier must read the trailing
    // SubagentStop as idle so the next prompt is sent bare.
    let last = events.last();
    assert!(progress::is_idle(last));
}

#[test]
fn decide_tick_emits_escalated_on_escalate_event() {
    let dag = dev_dag();
    let state = fresh_state("fix", PhaseState::InFlight);
    let e = json!({"event": "escalate", "reason": "fix-cycle 撞 3 顶"});
    assert_eq!(
        decide_tick(&dag, &state, Some(&e)),
        TickAction::Escalated {
            phase: "fix".into(),
            reason: "fix-cycle 撞 3 顶".into(),
        },
    );
}

#[test]
fn is_terminal_state_true_after_dag_endpoint_passes() {
    let dag = dev_dag();
    let mut state = fresh_state("ship", PhaseState::Idle);
    state.phase_history.push(PhaseHistoryEntry {
        phase: "ship".into(),
        status: "passed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(dag.is_terminal_state(&state));
}

#[test]
fn is_terminal_state_true_after_any_escalation() {
    let dag = dev_dag();
    let mut state = fresh_state("implement", PhaseState::Idle);
    state.phase_history.push(PhaseHistoryEntry {
        phase: "implement".into(),
        status: "escalated".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(dag.is_terminal_state(&state));
}

#[test]
fn is_terminal_state_false_after_resumed_follows_escalated() {
    // E2E 2026-05-06 F8: an escalated entry followed by a resumed
    // entry must lift the terminal flag — otherwise `decide_tick`
    // returns NoOp forever and the daemon stops dispatching the
    // project even after `ccteam resume` clears phase_state.
    let dag = dev_dag();
    let mut state = fresh_state("fix", PhaseState::Idle);
    state.phase_history.push(PhaseHistoryEntry {
        phase: "fix".into(),
        status: "escalated".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(dag.is_terminal_state(&state), "escalated alone is terminal");

    state.phase_history.push(PhaseHistoryEntry {
        phase: "fix".into(),
        status: "resumed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(
        !dag.is_terminal_state(&state),
        "escalated + resumed must lift the terminal flag",
    );

    // Re-escalate on a later phase: terminal again.
    state.phase_history.push(PhaseHistoryEntry {
        phase: "ship".into(),
        status: "escalated".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(dag.is_terminal_state(&state), "later escalation re-arms");

    // Resume again: non-terminal.
    state.phase_history.push(PhaseHistoryEntry {
        phase: "ship".into(),
        status: "resumed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(!dag.is_terminal_state(&state), "second resume lifts again");
}

#[test]
fn is_terminal_state_true_after_passed_endpoint_even_following_resumed() {
    // A successful ship after a resume cycle is still terminal —
    // resume only clears the escalated flag, it doesn't override the
    // ship-passed terminal.
    let dag = dev_dag();
    let mut state = fresh_state("ship", PhaseState::Idle);
    for status in ["escalated", "resumed"] {
        state.phase_history.push(PhaseHistoryEntry {
            phase: "fix".into(),
            status: status.into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
    }
    state.phase_history.push(PhaseHistoryEntry {
        phase: "ship".into(),
        status: "passed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(dag.is_terminal_state(&state));
}

#[test]
fn is_terminal_state_false_when_non_endpoint_passes() {
    // After F4: "passed" on a non-endpoint phase is NOT terminal.
    // Old logic only checked for "ship" string match — this test
    // guards against regression.
    let dag = dev_dag();
    let mut state = fresh_state("implement", PhaseState::Idle);
    state.phase_history.push(PhaseHistoryEntry {
        phase: "implement".into(),
        status: "passed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    assert!(!dag.is_terminal_state(&state));
}

#[test]
fn process_project_writes_escalation_md_and_marks_history() {
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = "esc-demo";
    let project = paths.project_dir(slug);
    std::fs::create_dir_all(project.join(".ccteam")).unwrap();

    let mut state = fresh_state("fix", PhaseState::InFlight);
    state.slug = slug.into();
    state.save(&paths.project_state(slug)).unwrap();

    progress::append_event(
        &paths.progress_jsonl(slug),
        &json!({"event": "escalate", "reason": "tests still red after 3 cycles"}),
    )
    .unwrap();

    write_global_phase_templates(&paths.root, false).unwrap();
    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
    let new_state = orch
        .process_project(slug, ProjectState::load(&paths.project_state(slug)).unwrap())
        .unwrap();

    assert_eq!(new_state.phase_state, PhaseState::Idle);
    let last = new_state.phase_history.last().unwrap();
    assert_eq!(last.phase, "fix");
    assert_eq!(last.status, "escalated");

    let esc_path = paths.project_ccteam_dir(slug).join("escalation.md");
    let body = std::fs::read_to_string(&esc_path).unwrap();
    assert!(body.contains("phase: fix"));
    assert!(body.contains("tests still red after 3 cycles"));

    // No new dispatch attempt: terminal state. Last progress event
    // should still be `escalate` (no `phase_inject` appended).
    let last_event = progress::last_event(&paths.progress_jsonl(slug))
        .unwrap()
        .unwrap();
    assert_eq!(last_event["event"], "escalate");
}

#[test]
fn process_project_advances_history_when_phase_done_observed_without_tmux_dispatch() {
    // Same as above but the new phase would be `ship` — and dispatch
    // requires tmux. We pre-mark history so the project becomes
    // terminal immediately after advance, avoiding the dispatch step.
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = "advance-demo";
    let project = paths.project_dir(slug);
    std::fs::create_dir_all(project.join(".ccteam")).unwrap();

    let mut state = fresh_state("ship", PhaseState::InFlight);
    state.slug = slug.into();
    state.save(&paths.project_state(slug)).unwrap();

    progress::append_event(
        &paths.progress_jsonl(slug),
        &json!({"event": "phase_done", "phase": "ship"}),
    )
    .unwrap();

    write_global_phase_templates(&paths.root, false).unwrap();
    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
    let new_state = orch
        .process_project(slug, ProjectState::load(&paths.project_state(slug)).unwrap())
        .unwrap();

    assert_eq!(new_state.phase_state, PhaseState::Idle);
    assert!(new_state.current_phase.is_empty());
    let last = new_state.phase_history.last().unwrap();
    assert_eq!(last.phase, "ship");
    assert_eq!(last.status, "passed");
}
