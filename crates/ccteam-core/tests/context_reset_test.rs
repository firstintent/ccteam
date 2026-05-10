//! Tests for M0.10 context reset.

use std::path::Path;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use tempfile::TempDir;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    append_progress_summary, build_progress_summary, write_global_phase_templates,
    CcteamPaths, Orchestrator, OrchestratorConfig, Parallelism, PhaseHistoryEntry,
    PhaseState, ProjectState, TeamKind,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug(test_name: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("test-rst-{test_name}-{pid}-{n}")
}

fn fresh_state(slug: &str, current_phase: &str) -> ProjectState {
    let now = Utc::now();
    ProjectState {
        slug: slug.into(),
        team: "dev".into(),
        team_kind: TeamKind::Workflow,
        created_at: now,
        tmux_session: TmuxSession::for_slug(slug).name().into(),
        claude_session_id: None,
        claude_pid: None,
        phase_state: PhaseState::Idle,
        current_phase: current_phase.into(),
        parallelism: Parallelism::Solo,
        phase_history: vec![
            PhaseHistoryEntry {
                phase: "plan-eng".into(),
                status: "passed".into(),
                duration_s: 90,
                cost_usd: 0.12,
            },
            PhaseHistoryEntry {
                phase: "implement".into(),
                status: "passed".into(),
                duration_s: 600,
                cost_usd: 1.10,
            },
        ],
        auto_loop_cycle_count: 0,
        cost_used_usd: 1.22,
        soft_warn_threshold_usd: 20.0,
        hard_kill_threshold_usd: 200.0,
        context_tokens_used: 700_000,
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
fn build_progress_summary_lists_phase_history_and_current_phase() {
    let state = fresh_state("demo", "test-author");
    let s = build_progress_summary(&state);
    assert!(s.contains("当前 phase: test-author"));
    assert!(s.contains("- plan-eng (passed)"));
    assert!(s.contains("- implement (passed)"));
    assert!(s.contains("$1.22"));
}

#[test]
fn append_progress_summary_creates_file_with_header_when_missing() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("CLAUDE.md");
    append_progress_summary(&path, "## hello\n").unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("# CLAUDE.md (auto-managed by ccteam)"));
    assert!(body.contains("## hello"));
}

#[test]
fn append_progress_summary_appends_to_existing_file_without_clobbering_header() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("CLAUDE.md");
    std::fs::write(&path, "# my header\n\nuser-authored content\n").unwrap();
    append_progress_summary(&path, "## appended\nmore\n").unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("# my header"));
    assert!(body.contains("user-authored content"));
    assert!(body.contains("## appended"));
}

struct ScopedSession(TmuxSession);
impl Drop for ScopedSession {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[test]
fn reset_context_recycles_tmux_and_resets_counter() {
    if !tmux_available() {
        eprintln!("[skip] reset_context_recycles_tmux_and_resets_counter: no tmux");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = unique_slug("recycle");
    let project_dir = paths.project_dir(&slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    // Orchestrator::new now requires a non-empty phase template list
    // (M3.1 F2: DAG inference replaces M0_PHASE_DAG constant).
    write_global_phase_templates(&paths.root, false).unwrap();

    // Persist initial state with tokens above threshold.
    let mut state = fresh_state(&slug, "test-author");
    state.save(&paths.project_state(&slug)).unwrap();

    // Simulate the original session that the reset routine will kill.
    let original = TmuxSession::for_slug(&slug);
    original.start(&project_dir, &["sh", "-c", "sleep 60"]).unwrap();

    // claude_argv writes the ready marker first thing, mimicking the
    // SessionStart hook chain. After ready is touched, sleep so the
    // session stays alive long enough for the test to finish + Drop to clean up.
    let ready_cmd = format!(
        "touch {} ; sleep 60",
        Path::new(".ccteam").join("ready").display()
    );
    let config = OrchestratorConfig {
        claude_argv: vec!["sh".into(), "-c".into(), ready_cmd],
        ready_timeout: Duration::from_secs(5),
        skip_tool_check: true,
        ..OrchestratorConfig::default()
    };
    let orch = Orchestrator::new(paths.clone(), config).unwrap();

    let _scoped = ScopedSession(TmuxSession::for_slug(&slug));
    orch.reset_context(&slug, &mut state).unwrap();

    assert_eq!(state.context_tokens_used, 0);
    assert_eq!(state.context_reset_count, 1);

    let claude_md = project_dir.join("CLAUDE.md");
    let body = std::fs::read_to_string(&claude_md).unwrap();
    assert!(body.contains("# CLAUDE.md (auto-managed by ccteam)"));
    assert!(body.contains("## 当前进度"));

    // session should be alive and the ready marker (re)created.
    assert!(TmuxSession::for_slug(&slug).exists());
    assert!(project_dir.join(".ccteam/ready").exists());
}

#[test]
fn reset_context_times_out_when_ready_marker_never_appears() {
    if !tmux_available() {
        eprintln!("[skip] reset_context_times_out_when_ready_marker_never_appears");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = unique_slug("timeout");
    let project_dir = paths.project_dir(&slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    write_global_phase_templates(&paths.root, false).unwrap();

    let mut state = fresh_state(&slug, "implement");
    state.save(&paths.project_state(&slug)).unwrap();

    let config = OrchestratorConfig {
        // pane process does NOT touch ready
        claude_argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
        ready_timeout: Duration::from_millis(400),
        skip_tool_check: true,
        ..OrchestratorConfig::default()
    };
    let orch = Orchestrator::new(paths.clone(), config).unwrap();

    let _scoped = ScopedSession(TmuxSession::for_slug(&slug));
    let err = orch.reset_context(&slug, &mut state).unwrap_err();
    assert!(format!("{err:#}").contains("ready"), "got: {err:#}");
}
