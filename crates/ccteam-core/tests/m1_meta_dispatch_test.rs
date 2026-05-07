//! M1 integration tests for meta-agent dispatch + inbox/outbox + concurrency.
//!
//! These exercise the orchestrator-side changes in m1/meta-agent-dispatch:
//!
//! - `process_meta_project` does NOT inject phase prompts
//! - `process_session_inbox` drains inbox files into the session
//! - `MAX_CONCURRENT_PROJECTS` gates regular project dispatch
//! - meta projects don't count against the concurrency budget
//!
//! tmux-dependent tests skip gracefully when tmux is not on PATH.

use std::sync::OnceLock;
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    bootstrap_meta_project, bootstrap_project, disable_tool_surface_bootstrap_for_tests,
    inbox_filename, write_all_global_team_templates, write_global_phase_templates, CcteamPaths,
    InboxFrontMatter, InboxMessage, Orchestrator, OrchestratorConfig, PhaseState, ProjectState,
    SessionMailbox, MAX_CONCURRENT_PROJECTS, META_TEAM_NAME,
};
use chrono::Utc;
use tempfile::TempDir;

static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
fn isolation() {
    DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
}

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

// (TmuxServer helper kept simple — current tests target the developer
// tmux server, isolated by per-test session names that include
// std::process::id(). Reserve a real `-L <server>` shim for M1.5
// integration tests of `ccteam stop`.)

/// Set up phase templates so Orchestrator::new succeeds. Used by tests
/// that exercise paths reaching `decide_tick` for non-meta projects.
fn write_solo_phases(paths: &CcteamPaths) {
    write_global_phase_templates(&paths.root, true).unwrap();
}

/// Seed every shipped team (`teams/<name>/team.yaml`) so
/// `Orchestrator::new`'s `load_team_runtimes` registers them. Tests
/// exercising the V0.2 evergreen flag (meta-agent dispatched off
/// `TeamSpec::evergreen` rather than a string compare) need this.
fn seed_all_teams(paths: &CcteamPaths) {
    write_all_global_team_templates(&paths.root, true).unwrap();
}

/// Build an `Orchestrator` with all shipped teams seeded so
/// `count_active_regular` / `is_evergreen` work for meta-agent.
fn orch_with_all_teams(paths: &CcteamPaths) -> Orchestrator {
    seed_all_teams(paths);
    Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap()
}

#[test]
fn count_active_regular_excludes_meta_team() {
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = orch_with_all_teams(&paths);

    let mut a = ProjectState::initial("p-1".into());
    a.phase_state = PhaseState::InFlight;
    let mut b = ProjectState::initial("p-2".into());
    b.phase_state = PhaseState::AutoLocked;
    let mut idle = ProjectState::initial("p-3".into());
    idle.phase_state = PhaseState::Idle;
    let mut meta = ProjectState::initial_for_team("rob-meta".into(), META_TEAM_NAME.into());
    meta.phase_state = PhaseState::InFlight; // even if mislabeled, it shouldn't count

    let projects = vec![
        ("p-1".into(), a),
        ("p-2".into(), b),
        ("p-3".into(), idle),
        ("rob-meta".into(), meta),
    ];
    assert_eq!(orch.count_active_regular(&projects), 2);
}

#[test]
fn process_session_inbox_consumes_messages_and_emits_progress_event() {
    if !tmux_available() {
        eprintln!("[skip] tmux not available");
        return;
    }
    isolation();

    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_solo_phases(&paths);
    let slug = format!("inbox-test-{}", std::process::id());
    bootstrap_project(&paths, &slug, "test", "dev").unwrap();

    // Spin a tmux session named exactly state.tmux_session ("ccteam-<slug>").
    let project_dir = paths.project_dir(&slug);
    let ready = project_dir.join(".ccteam/ready");
    std::fs::write(&ready, b"").unwrap();
    let session = TmuxSession::for_slug(&slug);
    session
        .start(&project_dir, &["sh", "-c", "sleep 30"])
        .unwrap();

    // Drop a well-formed inbox file.
    let cc = paths.project_ccteam_dir(&slug);
    let mailbox = SessionMailbox::for_ccteam_dir(&cc);
    mailbox.ensure_dirs().unwrap();
    let now = Utc::now();
    let path = mailbox.inbox.join(inbox_filename(now, 1));
    let msg = InboxMessage {
        front: InboxFrontMatter {
            schema_version: 1,
            source: "cli".into(),
            source_chat_id: None,
            source_msg_id: None,
            source_user: "rob".into(),
            created_at: now,
            ingested_at: now,
            content_type: "text".into(),
            attachments: Vec::new(),
        },
        body: "hello from inbox\n".into(),
    };
    msg.save(&path).unwrap();

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.process_session_inbox(&slug, &state).unwrap();

    // The inbox file should be gone (idempotent ack).
    assert!(!path.exists(), "consumed inbox file must be deleted");

    // progress.jsonl should contain an `inbox_consumed` event.
    let body = std::fs::read_to_string(paths.progress_jsonl(&slug)).unwrap();
    assert!(body.contains("\"event\":\"inbox_consumed\""), "got: {body}");
    assert!(body.contains("\"source\":\"cli\""), "got: {body}");

    let _ = session.kill();
}

#[test]
fn process_session_inbox_leaves_malformed_files_in_place() {
    if !tmux_available() {
        eprintln!("[skip] tmux not available");
        return;
    }
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_solo_phases(&paths);
    let slug = format!("inbox-bad-{}", std::process::id());
    bootstrap_project(&paths, &slug, "test", "dev").unwrap();

    let project_dir = paths.project_dir(&slug);
    let ready = project_dir.join(".ccteam/ready");
    std::fs::write(&ready, b"").unwrap();
    let session = TmuxSession::for_slug(&slug);
    session
        .start(&project_dir, &["sh", "-c", "sleep 30"])
        .unwrap();

    let cc = paths.project_ccteam_dir(&slug);
    let mailbox = SessionMailbox::for_ccteam_dir(&cc);
    mailbox.ensure_dirs().unwrap();
    let path = mailbox.inbox.join("msg-bogus.md");
    std::fs::write(&path, "not even YAML\n").unwrap();

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    // Must NOT panic / propagate; bad file is logged + skipped.
    orch.process_session_inbox(&slug, &state).unwrap();
    assert!(path.exists(), "malformed file should remain for inspection");

    let _ = session.kill();
}

#[test]
fn meta_project_skips_phase_dispatch() {
    if !tmux_available() {
        eprintln!("[skip] tmux not available");
        return;
    }
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    // V0.2 §6.4 candidate 5: seed every shipped team so meta-agent's
    // `evergreen: true` flag drives the dispatch.
    seed_all_teams(&paths);
    // Per-test user handle so concurrent tmux session names don't collide.
    let user = format!("rob-skip-{}", std::process::id());
    let report = bootstrap_meta_project(&paths, &user).unwrap();

    // ensure_session deletes any pre-existing ready marker, so the
    // shell stand-in for `claude` must touch it back. Mirror what the
    // SessionStart hook would do in production.
    let ready = report.project_dir.join(".ccteam/ready");
    let argv = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "touch {} && exec sh -c 'sleep 60'",
            ready.to_str().unwrap().replace(' ', r"\ "),
        ),
    ];
    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            claude_argv: argv,
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    let state = ProjectState::load(&paths.project_state(&report.slug)).unwrap();
    let updated = orch.process_project(&report.slug, state).unwrap();

    // Meta path: current_phase stays empty, phase_state stays Idle, no
    // `phase_inject` event is appended.
    assert_eq!(updated.current_phase, "");
    assert_eq!(updated.phase_state, PhaseState::Idle);

    let progress = paths.progress_jsonl(&report.slug);
    if progress.exists() {
        let body = std::fs::read_to_string(&progress).unwrap();
        assert!(
            !body.contains("phase_inject"),
            "meta-agent path must not emit phase_inject events; got: {body}",
        );
    }

    // tmux session should have been spawned under ccteam-meta-<user>.
    let session = TmuxSession::from_name(updated.tmux_session.clone());
    let expected = format!("ccteam-meta-{}", user);
    assert_eq!(session.name(), expected);
    let _ = session.kill();
}

#[test]
fn poll_tick_caps_active_regular_projects_at_three() {
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = orch_with_all_teams(&paths);

    // We don't actually want to spin tmux for 5 projects in a unit
    // test, so we exercise the budget arithmetic directly. The pure
    // counter is the load-bearing assertion; the dispatch path itself
    // is covered by the existing dispatch_test.rs harness.
    let mut active1 = ProjectState::initial("p1".into());
    active1.phase_state = PhaseState::InFlight;
    let mut active2 = ProjectState::initial("p2".into());
    active2.phase_state = PhaseState::AutoLocked;
    let mut active3 = ProjectState::initial("p3".into());
    active3.phase_state = PhaseState::InFlight;
    let q1 = ProjectState::initial("q1".into());
    let q2 = ProjectState::initial("q2".into());

    let projects = vec![
        ("p1".into(), active1),
        ("p2".into(), active2),
        ("p3".into(), active3),
        ("q1".into(), q1),
        ("q2".into(), q2),
    ];
    let active = orch.count_active_regular(&projects);
    assert_eq!(active, 3);
    let budget = MAX_CONCURRENT_PROJECTS.saturating_sub(active);
    assert_eq!(
        budget, 0,
        "with 3 active and cap=3, no further dispatch slots remain",
    );
}

#[test]
fn meta_context_reset_appends_progress_summary_to_claude_md() {
    if !tmux_available() {
        eprintln!("[skip] tmux not available");
        return;
    }
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    // V0.2 §6.4 candidate 5: seed every shipped team so meta-agent's
    // `evergreen: true` flag is loadable when Orchestrator::new walks
    // ~/.ccteam/teams/. Without this, `is_evergreen("meta-agent")`
    // returns false and `process_project` skips the meta dispatch
    // path entirely.
    seed_all_teams(&paths);
    let user = format!("rob-reset-{}", std::process::id());
    let report = bootstrap_meta_project(&paths, &user).unwrap();

    // Force the over-threshold condition so process_meta_project
    // triggers reset_context.
    let state_path = paths.project_state(&report.slug);
    let mut state = ProjectState::load(&state_path).unwrap();
    state.context_tokens_used = state.context_reset_threshold_tokens + 1;
    state.save(&state_path).unwrap();

    // Stand-in for `claude` that touches the ready marker so
    // reset_context's wait_for_ready unblocks.
    let ready = report.project_dir.join(".ccteam/ready");
    let argv = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "touch {} && exec sh -c 'sleep 60'",
            ready.to_str().unwrap().replace(' ', r"\ "),
        ),
    ];
    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            claude_argv: argv,
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    let state = ProjectState::load(&state_path).unwrap();
    let updated = orch.process_project(&report.slug, state).unwrap();

    assert_eq!(
        updated.context_reset_count, 1,
        "meta-agent should have triggered exactly one reset",
    );
    let claude_md = std::fs::read_to_string(&report.claude_md).unwrap();
    assert!(
        claude_md.contains("当前进度"),
        "reset_context should append the progress section to CLAUDE.md",
    );
    // Role prompt body must survive the append (we still want the
    // dispatcher rules visible to the new session).
    assert!(claude_md.contains("决策树"), "role prompt should remain intact");

    let session = TmuxSession::from_name(updated.tmux_session.clone());
    let _ = session.kill();
}

#[test]
fn meta_project_does_not_consume_concurrency_budget() {
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = orch_with_all_teams(&paths);

    let mut a = ProjectState::initial("p1".into());
    a.phase_state = PhaseState::InFlight;
    let mut b = ProjectState::initial("p2".into());
    b.phase_state = PhaseState::InFlight;
    let mut meta = ProjectState::initial_for_team("rob-meta".into(), META_TEAM_NAME.into());
    meta.phase_state = PhaseState::InFlight;

    let projects = vec![
        ("p1".into(), a),
        ("p2".into(), b),
        ("rob-meta".into(), meta),
    ];
    assert_eq!(orch.count_active_regular(&projects), 2);
    assert!(
        MAX_CONCURRENT_PROJECTS - orch.count_active_regular(&projects) > 0,
        "meta should leave at least one budget slot",
    );
}

