//! End-to-end integration test for `Orchestrator::dispatch_phase`:
//! spin up a real tmux session running an interactive shell, dispatch
//! a phase, and verify (a) the prompt landed in the pane and (b) the
//! `phase_inject` event got appended to progress.jsonl. Skipped when
//! tmux is unavailable.

use std::path::Path;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    progress, write_global_phase_templates, CcteamPaths, Orchestrator, OrchestratorConfig,
    Parallelism, PhaseState, ProjectState, TeamKind,
};
use serde_json::json;
use tempfile::TempDir;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug(test_name: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("test-disp-{test_name}-{pid}-{n}")
}

struct ScopedSession {
    session: TmuxSession,
}

impl ScopedSession {
    fn for_slug(slug: &str) -> Self {
        Self {
            session: TmuxSession::for_slug(slug),
        }
    }
}

impl Drop for ScopedSession {
    fn drop(&mut self) {
        let _ = self.session.kill();
    }
}

fn fixture(test_name: &str) -> Option<(TempDir, CcteamPaths, String, ScopedSession)> {
    if !tmux_available() {
        eprintln!("[skip] {test_name}: tmux not on PATH");
        return None;
    }
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = unique_slug(test_name);
    let project = paths.project_dir(&slug);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(paths.project_state(&slug).parent().unwrap()).unwrap();
    // M3.1 F2: Orchestrator::new now requires phase templates to build
    // a DAG. Tests that previously got away with empty `~/.ccteam/phases/`
    // must populate it via the shipped dev templates.
    write_global_phase_templates(&paths.root, false).unwrap();
    let now = chrono::Utc::now();
    ProjectState {
        slug: slug.clone(),
        team: "dev".into(),
        team_kind: TeamKind::Workflow,
        created_at: now,
        tmux_session: TmuxSession::for_slug(&slug).name().to_string(),
        claude_session_id: None,
        claude_pid: None,
        phase_state: PhaseState::Idle,
        current_phase: "implement".into(),
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
    .save(&paths.project_state(&slug))
    .unwrap();

    let scoped = ScopedSession::for_slug(&slug);
    scoped
        .session
        .start(&project, &["sh", "-i"])
        .unwrap();
    // give the shell a beat to come up
    std::thread::sleep(Duration::from_millis(200));

    Some((tmp, paths, slug, scoped))
}

#[test]
fn dispatch_phase_sends_idle_prompt_and_appends_event_when_no_history() {
    let Some((_tmp, paths, slug, _scoped)) =
        fixture("idle-no-history")
    else {
        return;
    };
    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    orch.dispatch_phase(&slug, "implement").unwrap();

    let last = progress::last_event(&paths.progress_jsonl(&slug))
        .unwrap()
        .expect("phase_inject must be appended");
    assert_eq!(last["event"], "phase_inject");
    assert_eq!(last["phase"], "implement");
    assert_eq!(last["idle"], true);
}

#[test]
fn dispatch_phase_uses_btw_when_busy() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("busy-uses-btw") else {
        return;
    };
    // Seed progress.jsonl so the last event is non-idle (PreToolUse).
    progress::append_event(
        &paths.progress_jsonl(&slug),
        &json!({"event": "PreToolUse", "ts": "2026-05-05T00:00:00Z", "tool": "Edit"}),
    )
    .unwrap();

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
    orch.dispatch_phase(&slug, "implement").unwrap();

    let last = progress::last_event(&paths.progress_jsonl(&slug))
        .unwrap()
        .expect("phase_inject appended");
    assert_eq!(last["event"], "phase_inject");
    assert_eq!(last["idle"], false);
}

#[test]
fn dispatch_phase_payload_lands_in_tmux_pane() {
    let Some((_tmp, paths, slug, scoped)) = fixture("payload-in-pane") else {
        return;
    };
    let orch = Orchestrator::new(
        paths,
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
    let session = TmuxSession::for_slug(&slug);
    assert!(
        session.exists(),
        "session must be alive before dispatch_phase",
    );
    orch.dispatch_phase(&slug, "implement").unwrap();
    assert!(
        session.exists(),
        "session must still be alive after dispatch_phase",
    );
    let mut last_capture = String::new();
    for _ in 0..30 {
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-p", "-t", session.name()])
            .output()
            .unwrap();
        last_capture = String::from_utf8_lossy(&output.stdout).into_owned();
        // The full path may wrap to a new line; checking for the phase
        // name fragment alone is enough to confirm the prompt landed.
        if last_capture.contains("implement.md") || last_capture.contains("PHASE_DONE") {
            drop(scoped);
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(scoped);
    panic!(
        "phase prompt fragment never appeared in tmux pane.\nlast capture:\n{last_capture}"
    );
}

#[test]
fn idle_aware_message_via_module_picks_btw_for_busy() {
    // Pure unit-style sanity check that doesn't need tmux. Useful as a
    // smoke test on hosts where tmux is unavailable so this file isn't
    // entirely skipped.
    let _ = Path::new("/dev/null");
    let prompt = progress::build_phase_prompt("ship");
    assert!(!progress::idle_aware_message(&prompt, false).starts_with(&prompt));
    assert!(progress::idle_aware_message(&prompt, true) == prompt);
}
