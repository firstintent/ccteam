//! M0.15 — end-to-end happy path. Walks a single project from
//! `ccteam new` through every phase in the M0 DAG (plan-eng →
//! implement → test-author → test-run → fix → ship) by simulating
//! the assistant's PHASE_DONE sigils directly into progress.jsonl.
//! This bypasses tmux/claude (those are tested separately in
//! dispatch_test, fix_loop_test, etc.) and focuses on verifying the
//! orchestrator's decision layer + state.json transitions hold up
//! across all 6 phases without leaks or mis-routes.

use std::sync::OnceLock;

use chrono::{SecondsFormat, Utc};
use serde_json::json;
use tempfile::TempDir;

use ccteam_core::{
    bootstrap_project, decide_tick, dev_dag, disable_tool_surface_bootstrap_for_tests,
    progress, slugify, CcteamPaths, PhaseHistoryEntry, PhaseState, ProjectState, TickAction,
};

/// These tests don't care about the tool-surface side effects of
/// bootstrap_project — they want it to write project files and
/// nothing else. Disable the ~/.claude/ mutation for the whole
/// binary so cargo test doesn't pollute the developer's real
/// agents/ + skills/ dirs.
static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
fn ensure_isolation() {
    DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
}

const M0_DAG: &[&str] = &[
    "plan-eng",
    "implement",
    "test-author",
    "test-run",
    "fix",
    "ship",
];

fn fresh(paths: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: paths.path().join("home"),
        projects_root: paths.path().join("projects"),
    }
}

/// Pretend the orchestrator dispatched `phase`. `decide_tick` treats
/// InFlight and FixLocked identically (both block AdvancePhase until
/// a `phase_done` event lands), so this fixture uses InFlight
/// uniformly — the FixLocked branch's actual transition is covered
/// by the orchestrator tests in `state_machine_test.rs` and the
/// hooks integration tests.
fn fake_dispatch(state: &mut ProjectState, phase: &str) {
    state.current_phase = phase.into();
    state.phase_state = PhaseState::InFlight;
}

fn fake_phase_done(paths: &CcteamPaths, slug: &str, phase: &str) {
    progress::append_event(
        &paths.progress_jsonl(slug),
        &json!({
            "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            "event": "phase_done",
            "phase": phase,
        }),
    )
    .unwrap();
}

#[test]
fn full_pipeline_advances_through_every_m0_phase() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh(&tmp);
    let request = "Build a tiny CLI greeter that prints hello";

    let slug = slugify(request);
    ensure_isolation();
    bootstrap_project(&paths, &slug, request).unwrap();

    let dag = dev_dag();

    let state_path = paths.project_state(&slug);
    let state = ProjectState::load(&state_path).unwrap();
    assert!(state.current_phase.is_empty());
    assert_eq!(state.phase_state, PhaseState::Idle);

    // First decision on a fresh project: dispatch the DAG entry node.
    let action = decide_tick(&dag, &state, None);
    assert_eq!(
        action,
        TickAction::DispatchPhase {
            phase: dag.entry().into(),
        },
    );

    // Walk the DAG. For each phase: simulate dispatch (state mutation),
    // then phase_done event, then assert decide_tick wants AdvancePhase,
    // then apply the advance.
    for phase in M0_DAG {
        // simulate dispatch
        let mut s = ProjectState::load(&state_path).unwrap();
        fake_dispatch(&mut s, phase);
        s.save(&state_path).unwrap();

        // simulate phase_done from claude
        fake_phase_done(&paths, &slug, phase);

        // orchestrator's next decision should be AdvancePhase
        let s = ProjectState::load(&state_path).unwrap();
        let last = progress::last_event(&paths.progress_jsonl(&slug))
            .unwrap()
            .unwrap();
        let action = decide_tick(&dag, &s, Some(&last));
        let expected_to = dag.next_on_done(phase).map(String::from);
        assert_eq!(
            action,
            TickAction::AdvancePhase {
                from: phase.to_string(),
                to: expected_to.clone(),
            },
            "phase {phase} did not advance as expected",
        );

        // apply advance (same logic as Orchestrator::process_project)
        let mut s = ProjectState::load(&state_path).unwrap();
        s.phase_history.push(PhaseHistoryEntry {
            phase: phase.to_string(),
            status: "passed".into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
        s.phase_state = PhaseState::Idle;
        s.current_phase = expected_to.unwrap_or_default();
        s.save(&state_path).unwrap();
    }

    let final_state = ProjectState::load(&state_path).unwrap();
    assert!(
        dag.is_terminal_state(&final_state),
        "project must be terminal after the DAG endpoint",
    );
    let last_history = final_state.phase_history.last().unwrap();
    assert!(
        dag.is_terminal_phase(&last_history.phase),
        "last history phase must be a DAG endpoint, got {}",
        last_history.phase,
    );
    assert_eq!(last_history.status, "passed");
    assert_eq!(
        final_state.phase_history.len(),
        M0_DAG.len(),
        "history should contain one entry per phase",
    );
}

#[test]
fn full_pipeline_runs_three_times_without_leaking_state() {
    // Per dev-plan, M0 closure requires three e2e passes. Run the
    // walk thrice in fresh tempdirs to confirm reproducibility — no
    // global state leak should produce divergent histories.
    let dag = dev_dag();
    for run in 1..=3 {
        let tmp = TempDir::new().unwrap();
        let paths = fresh(&tmp);
        let slug = slugify(&format!("smoke {run}"));
        ensure_isolation();
        bootstrap_project(&paths, &slug, "smoke").unwrap();
        let state_path = paths.project_state(&slug);

        for phase in M0_DAG {
            let mut s = ProjectState::load(&state_path).unwrap();
            fake_dispatch(&mut s, phase);
            s.save(&state_path).unwrap();
            fake_phase_done(&paths, &slug, phase);

            let s = ProjectState::load(&state_path).unwrap();
            let last = progress::last_event(&paths.progress_jsonl(&slug))
                .unwrap()
                .unwrap();
            let action = decide_tick(&dag, &s, Some(&last));
            let expected_to = dag.next_on_done(phase).map(String::from);
            assert_eq!(
                action,
                TickAction::AdvancePhase {
                    from: phase.to_string(),
                    to: expected_to.clone(),
                },
                "run {run}, phase {phase}: unexpected action",
            );
            let mut s = ProjectState::load(&state_path).unwrap();
            s.phase_history.push(PhaseHistoryEntry {
                phase: phase.to_string(),
                status: "passed".into(),
                duration_s: 0,
                cost_usd: 0.0,
            });
            s.phase_state = PhaseState::Idle;
            s.current_phase = expected_to.unwrap_or_default();
            s.save(&state_path).unwrap();
        }

        let final_state = ProjectState::load(&state_path).unwrap();
        assert!(
            dag.is_terminal_state(&final_state),
            "run {run}: project must terminate at the DAG endpoint",
        );
        assert_eq!(
            final_state.phase_history.len(),
            M0_DAG.len(),
            "run {run}: phase_history length should equal DAG length",
        );
    }
}

#[test]
fn pipeline_halts_on_escalate_event_mid_dag() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh(&tmp);
    let slug = slugify("escalate-test");
    ensure_isolation();
    bootstrap_project(&paths, &slug, "escalate test").unwrap();
    let state_path = paths.project_state(&slug);

    let dag = dev_dag();

    // Advance to test-run, then claude escalates.
    for phase in &M0_DAG[..3] {
        let mut s = ProjectState::load(&state_path).unwrap();
        fake_dispatch(&mut s, phase);
        s.save(&state_path).unwrap();
        fake_phase_done(&paths, &slug, phase);
        // advance
        let mut s = ProjectState::load(&state_path).unwrap();
        s.phase_history.push(PhaseHistoryEntry {
            phase: phase.to_string(),
            status: "passed".into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
        s.phase_state = PhaseState::Idle;
        s.current_phase = dag.next_on_done(phase).unwrap_or("").into();
        s.save(&state_path).unwrap();
    }

    // Dispatch test-run, then escalate.
    let mut s = ProjectState::load(&state_path).unwrap();
    fake_dispatch(&mut s, "test-run");
    s.save(&state_path).unwrap();
    progress::append_event(
        &paths.progress_jsonl(&slug),
        &json!({
            "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            "event": "escalate",
            "reason": "compiler segfault on every revision",
        }),
    )
    .unwrap();

    let s = ProjectState::load(&state_path).unwrap();
    let last = progress::last_event(&paths.progress_jsonl(&slug))
        .unwrap()
        .unwrap();
    let action = decide_tick(&dag, &s, Some(&last));
    assert_eq!(
        action,
        TickAction::Escalated {
            phase: "test-run".into(),
            reason: "compiler segfault on every revision".into(),
        },
    );
}
