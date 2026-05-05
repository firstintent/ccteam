//! Integration tests for the M0.12 fix-loop wiring inside
//! `parse_phase_end`. The handler reads / mutates / deletes
//! `<project>/.ccteam/fix-loop.state.md` and returns a `ParseDecision`
//! the CLI translates into the Stop-hook stdout JSON.

use std::path::PathBuf;

use chrono::Utc;
use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::fix_loop::{self, FixLoopState};
use ccteam_core::{progress, CcteamPaths, Parallelism, PhaseState, ProjectState};
use ccteam_hooks::{parse_phase_end, ParseDecision};

struct Fixture {
    _tmp: TempDir,
    paths: CcteamPaths,
    project_dir: PathBuf,
    transcript_path: PathBuf,
    slug: String,
}

impl Fixture {
    fn new(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let project_dir = paths.project_dir(slug);
        std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
        let now = Utc::now();
        ProjectState {
            slug: slug.into(),
            created_at: now,
            tmux_session: format!("ccteam-{slug}"),
            claude_session_id: None,
            claude_pid: None,
            phase_state: PhaseState::FixLocked,
            current_phase: "fix".into(),
            parallelism: Parallelism::Solo,
            phase_history: Vec::new(),
            fix_cycle_count: 0,
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
        }
        .save(&paths.project_state(slug))
        .unwrap();

        Self {
            transcript_path: tmp.path().join("transcript.jsonl"),
            _tmp: tmp,
            paths,
            project_dir,
            slug: slug.into(),
        }
    }

    fn write_transcript(&self, lines: &[Value]) {
        let body: String = lines
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&self.transcript_path, body + "\n").unwrap();
    }

    fn put_fix_loop(&self, iter: u32) -> FixLoopState {
        let mut state = FixLoopState::new(
            self.slug.clone(),
            "请按 @.ccteam/phases/06-fix.md 完成本阶段...".into(),
            3,
        );
        state.front.iteration = iter;
        fix_loop::write(&fix_loop::path_in(&self.project_dir), &state).unwrap();
        state
    }

    fn fix_loop_path(&self) -> PathBuf {
        fix_loop::path_in(&self.project_dir)
    }

    fn progress_lines(&self) -> Vec<Value> {
        let p = self.paths.progress_jsonl(&self.slug);
        if !p.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&p)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn assistant_msg(text: &str) -> Value {
    json!({
        "type": "message",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
        }
    })
}

#[test]
fn fix_loop_blocks_and_bumps_iteration_when_signal_absent() {
    let fx = Fixture::new("fix-iter1");
    let _state = fx.put_fix_loop(1);
    fx.write_transcript(&[assistant_msg("Tried option A. tests still red.")]);

    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    match decision {
        ParseDecision::Block { reason } => {
            assert!(reason.contains("06-fix.md"));
        }
        other => panic!("expected Block, got {other:?}"),
    }

    let s2 = fix_loop::read(&fx.fix_loop_path()).unwrap().unwrap();
    assert_eq!(s2.front.iteration, 2);
    assert!(fx.progress_lines().is_empty(), "no event written on block");
}

#[test]
fn fix_loop_allows_exit_and_clears_state_when_tests_green() {
    let fx = Fixture::new("fix-green");
    fx.put_fix_loop(2);
    fx.write_transcript(&[assistant_msg(
        "Re-ran cargo test. all passing.\nTESTS_GREEN\n\nPHASE_DONE: fix",
    )]);

    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert_eq!(decision, ParseDecision::Continue);
    assert!(
        !fx.fix_loop_path().exists(),
        "state.md must be deleted after successful exit",
    );
    let events = fx.progress_lines();
    assert_eq!(events.last().unwrap()["event"], "phase_done");
    assert_eq!(events.last().unwrap()["phase"], "fix");
}

#[test]
fn fix_loop_emits_escalate_when_iteration_cap_reached_without_signal() {
    let fx = Fixture::new("fix-cap");
    fx.put_fix_loop(3); // already at cap
    fx.write_transcript(&[assistant_msg(
        "Three rounds of triage; root cause unclear. Stopping.",
    )]);

    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert_eq!(decision, ParseDecision::Continue);
    assert!(
        !fx.fix_loop_path().exists(),
        "state.md must be cleared at iteration cap",
    );
    let events = fx.progress_lines();
    let last = events.last().unwrap();
    assert_eq!(last["event"], "escalate");
    let reason = last["reason"].as_str().unwrap();
    assert!(reason.contains("3"));
    assert!(reason.contains("TESTS_GREEN"));
}

#[test]
fn parse_phase_end_falls_through_to_normal_parsing_when_no_state_md() {
    let fx = Fixture::new("normal");
    fx.write_transcript(&[assistant_msg("All good.\n\nPHASE_DONE: implement")]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert_eq!(decision, ParseDecision::Continue);
    let events = fx.progress_lines();
    assert_eq!(events.last().unwrap()["event"], "phase_done");
    assert_eq!(events.last().unwrap()["phase"], "implement");
}

#[test]
fn fix_loop_writes_with_ccteam_dir_already_present() {
    // Smoke test that the round-trip from orchestrator-write ↔
    // hook-read ↔ hook-write works against a real on-disk state file
    // (catches any divergence between writer and reader).
    let fx = Fixture::new("rt");
    fx.put_fix_loop(1);
    fx.write_transcript(&[assistant_msg("still working.")]);

    let stdin = json!({"cwd": fx.project_dir, "transcript_path": fx.transcript_path});
    parse_phase_end(&fx.paths, &stdin).unwrap();
    let s = fix_loop::read(&fx.fix_loop_path()).unwrap().unwrap();
    assert_eq!(s.front.iteration, 2);
    assert_eq!(s.front.completion_signal, "TESTS_GREEN");
    assert!(s.prompt.contains("06-fix.md"));
}
