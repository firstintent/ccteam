//! V0.2.2 F36 — orchestrator integration tests for the send-keys
//! sub-agent guard.
//!
//! - `dispatch_phase_with_state` writes `pending-inject.json` and skips
//!   send-keys when a sub-agent is active (no `phase_inject` event).
//! - Daemon-tick drain on `SubagentStop`: real dispatch + delete.
//! - Timeout path: pending older than `max_defer_minutes` →
//!   enriched outbox (`ccteam_classification: "inject_defer_timeout"`)
//!   + delete.
//! - Race fallback: F36 misses the open window → F35 `InjectLimbo`
//!   eventually re-injects (the integration shape of "F36 + F35
//!   coordination" PRD §5.3 spells out).
//! - F35 `attempt_limbo_reinject` skips when a F36 pending record is
//!   in flight (avoids burning the deterministic retry budget on a
//!   no-op).
//!
//! Side-effect-free unit tests live in
//! `crates/ccteam-core/src/pending_inject.rs::tests` and the
//! `subagent_active` cases in `progress.rs::tests`.

use std::path::Path;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    limbo_retry_path_in, load_limbo_retry_count, load_pending_inject, pending_inject_path_in,
    progress, save_pending_inject, write_global_phase_templates, CcteamPaths,
    DrainPendingOutcome, Orchestrator, OrchestratorConfig, Parallelism, PendingInject,
    PhaseState, ProjectState, TeamKind,
};
use serde_json::{json, Value};
use tempfile::TempDir;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug(test: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("test-f36-{test}-{pid}-{n}")
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

/// Identical fixture pattern as `silence_classifier_e2e_test::fixture`:
/// real tmux pane (so `dispatch_phase_with_state` can `send-keys`),
/// `dev` team templates, AutoLocked / `implement` phase. `silent_minutes`
/// shifts `created_at` / `last_progress_event_at` so silence-derived
/// classifications fire deterministically.
fn fixture(
    test: &str,
    silent_minutes: i64,
) -> Option<(TempDir, CcteamPaths, String, ScopedSession)> {
    if !tmux_available() {
        eprintln!("[skip] {test}: tmux not on PATH");
        return None;
    }
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = unique_slug(test);
    let project = paths.project_dir(&slug);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(paths.project_state(&slug).parent().unwrap()).unwrap();
    write_global_phase_templates(&paths.root, false).unwrap();

    let now = chrono::Utc::now();
    let stale = now - chrono::Duration::seconds(silent_minutes.saturating_mul(60).max(0));
    ProjectState {
        slug: slug.clone(),
        team: "dev".into(),
        team_kind: TeamKind::Workflow,
        created_at: stale,
        tmux_session: TmuxSession::for_slug(&slug).name().to_string(),
        claude_session_id: None,
        claude_pid: None,
        phase_state: PhaseState::AutoLocked,
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
        last_progress_event_at: Some(stale),
        last_event_type: Some("PreToolUse".into()),
        last_user_interaction_at: stale,
        user_attached: false,
        user_pause_pending: false,
        sessions: BTreeMap::new(),
        next_sid_seq: BTreeMap::new(),
    }
    .save(&paths.project_state(&slug))
    .unwrap();

    let scoped = ScopedSession::for_slug(&slug);
    scoped.session.start(&project, &["sh", "-i"]).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    Some((tmp, paths, slug, scoped))
}

fn write_event(progress_path: &Path, event: &Value) {
    progress::append_event(progress_path, event).unwrap();
}

fn count_phase_inject_events(progress_path: &Path) -> usize {
    progress::read_all_events(progress_path)
        .unwrap()
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("phase_inject"))
        .count()
}

fn build_orchestrator(paths: CcteamPaths) -> Orchestrator {
    Orchestrator::new(
        paths,
        OrchestratorConfig {
            tick_interval: Duration::from_millis(100),
            ready_timeout: Duration::from_millis(50),
            post_ready_warmup: Duration::from_millis(0),
            claude_argv: vec!["sh".into(), "-i".into()],
            skip_tool_check: true,
            subskill_argv: None,
        },
    )
    .unwrap()
}

fn read_outbox(project_dir: &Path) -> Value {
    let path = project_dir.join(".ccteam").join("needs_attention.outbox.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("expected outbox at {}", path.display()));
    serde_json::from_str(&body).unwrap()
}

#[test]
fn dispatch_with_active_subagent_writes_pending_inject_and_skips_send_keys() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("dispatch_defers", 0) else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);

    // Sub-agent is in flight: PreToolUse(Task) without a matching
    // SubagentStop. F36 must defer.
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "PreToolUse", "tool": "Task"}),
    );
    let phase_injects_before = count_phase_inject_events(&progress_path);

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.dispatch_phase_with_state(&slug, "implement", &state).unwrap();

    // No phase_inject event — the dispatch deferred.
    assert_eq!(
        count_phase_inject_events(&progress_path),
        phase_injects_before,
        "F36 deferred: no phase_inject event must be written",
    );
    // Pending record landed on disk with the right phase + budget.
    let pending_path = pending_inject_path_in(&project_dir);
    assert!(pending_path.exists(), "pending-inject.json must exist");
    let pending = load_pending_inject(&pending_path).unwrap().unwrap();
    assert_eq!(pending.slug, slug);
    assert_eq!(pending.phase, "implement");
    assert_eq!(pending.max_defer_minutes, 10);
}

#[test]
fn drain_after_subagent_stop_dispatches_and_deletes_pending() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("drain_after_stop", 0) else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);
    let pending_path = pending_inject_path_in(&project_dir);

    // Step 1: sub-agent active → defer.
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "PreToolUse", "tool": "Task"}),
    );
    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.dispatch_phase_with_state(&slug, "implement", &state).unwrap();
    assert!(pending_path.exists());

    // Step 2: SubagentStop arrives — drain must dispatch (write
    // phase_inject) + delete pending.
    let phase_injects_before = count_phase_inject_events(&progress_path);
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "SubagentStop"}),
    );
    let outcome = orch
        .drain_pending_inject_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    assert_eq!(outcome, DrainPendingOutcome::Drained);
    assert!(
        !pending_path.exists(),
        "pending-inject.json must be deleted after a successful drain",
    );
    assert_eq!(
        count_phase_inject_events(&progress_path),
        phase_injects_before + 1,
        "drain must write exactly one phase_inject event",
    );
}

#[test]
fn drain_with_subagent_still_active_keeps_pending() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("drain_still_blocked", 0) else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);
    let pending_path = pending_inject_path_in(&project_dir);

    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "PreToolUse", "tool": "Task"}),
    );
    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.dispatch_phase_with_state(&slug, "implement", &state).unwrap();
    assert!(pending_path.exists());

    // Sub-agent still active (no SubagentStop). Drain must be a no-op.
    let phase_injects_before = count_phase_inject_events(&progress_path);
    let outcome = orch
        .drain_pending_inject_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    assert_eq!(outcome, DrainPendingOutcome::StillBlocked);
    assert!(pending_path.exists(), "pending must persist while blocked");
    assert_eq!(
        count_phase_inject_events(&progress_path),
        phase_injects_before,
        "no phase_inject must be written while blocked",
    );
}

#[test]
fn drain_after_max_defer_minutes_writes_inject_defer_timeout_outbox() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("drain_timeout", 0) else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);
    let pending_path = pending_inject_path_in(&project_dir);

    // Forge a pending record from 30 minutes ago so the timeout path
    // fires regardless of test runtime. Sub-agent is also still active
    // — the timeout takes priority.
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "PreToolUse", "tool": "Task"}),
    );
    let stale =
        chrono::Utc::now() - chrono::Duration::minutes(30);
    save_pending_inject(
        &pending_path,
        &PendingInject::new(slug.clone(), "implement", vec![], stale, 10),
    )
    .unwrap();

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    let outcome = orch
        .drain_pending_inject_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    assert_eq!(outcome, DrainPendingOutcome::TimedOut);
    assert!(
        !pending_path.exists(),
        "pending-inject.json must be deleted after timeout",
    );

    let outbox = read_outbox(&project_dir);
    assert_eq!(
        outbox["ccteam_classification"].as_str(),
        Some("inject_defer_timeout"),
        "outbox classification field",
    );
    assert_eq!(outbox["event_kind"].as_str(), Some("escalation"));
    assert_eq!(outbox["priority"].as_str(), Some("high"));
    assert!(
        outbox.get("ccteam_pane_tail").is_some(),
        "F35 enriched schema reused: pane_tail field present",
    );
    assert!(
        outbox["body"]
            .as_str()
            .unwrap_or("")
            .contains("implement"),
        "body must mention the deferred phase",
    );
}

#[test]
fn drain_no_pending_record_returns_none() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("drain_none", 0) else {
        return;
    };
    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    let outcome = orch
        .drain_pending_inject_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    assert_eq!(outcome, DrainPendingOutcome::None);
}

#[test]
fn drain_skips_evergreen_team() {
    use ccteam_core::write_all_global_team_templates;
    let Some((_tmp, paths, slug, _scoped)) = fixture("drain_evergreen_skip", 0) else {
        return;
    };
    // Seed the full team set (incl. `teams/meta-agent/team.yaml`
    // with `evergreen: true`) so `is_evergreen("meta-agent")` actually
    // returns true. The fixture's dev-only seed isn't enough.
    write_all_global_team_templates(&paths.root, false).unwrap();

    let project_dir = paths.project_dir(&slug);
    let pending_path = pending_inject_path_in(&project_dir);
    save_pending_inject(
        &pending_path,
        &PendingInject::new(
            slug.clone(),
            "implement",
            vec![],
            chrono::Utc::now() - chrono::Duration::minutes(30),
            10,
        ),
    )
    .unwrap();

    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    state.team = "meta-agent".into();
    state.save(&paths.project_state(&slug)).unwrap();

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    let outcome = orch
        .drain_pending_inject_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    assert_eq!(
        outcome,
        DrainPendingOutcome::None,
        "evergreen team must skip drain even when a pending record exists",
    );
    // Outbox must NOT have been written (timeout path skipped).
    let outbox_path =
        project_dir.join(".ccteam").join("needs_attention.outbox.json");
    assert!(
        !outbox_path.exists(),
        "evergreen short-circuit must not surface inject_defer_timeout",
    );
    assert!(
        pending_path.exists(),
        "evergreen short-circuit leaves the pending record alone",
    );
}

#[test]
fn f36_race_miss_is_caught_by_f35_inject_limbo() {
    // F36 race scenario PRD §5.3:
    // - dispatch ran when no Task PreToolUse was visible (subagent_active = false)
    // - phase_inject landed in progress.jsonl
    // - sub-agent emerged and emitted PreToolUse(Task) only after the
    //   send-keys (so the inject ended up in the wrong context)
    //
    // F35 sees: tail event = phase_inject (the meta-event PreToolUse(Task)
    // doesn't mask it because the classifier walks back past
    // `inbox_consumed` etc., but a real PreToolUse(Task) would). For
    // the test we keep the tail at `phase_inject` after enough silence
    // — F35 must classify as `InjectLimbo` and run the deterministic
    // re-inject (counter 0 → 1).
    let Some((_tmp, paths, slug, _scoped)) =
        fixture("f36_race_falls_back_to_f35", 10) // > warn threshold
    else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);

    // Direct phase_inject (simulating "F36 sent the keys"). No follow
    // up — the assistant is blocked on a sub-agent that never emitted
    // a PreToolUse(Task) event in time.
    write_event(
        &progress_path,
        &json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "event": "phase_inject",
            "phase": "implement",
            "idle": true,
        }),
    );

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    let counter =
        load_limbo_retry_count(&limbo_retry_path_in(&project_dir), "implement").unwrap();
    assert_eq!(
        counter.count, 1,
        "F35 must back F36's race window: InjectLimbo deterministic re-inject ran",
    );
}

#[test]
fn f35_limbo_reinject_skipped_when_pending_inject_exists() {
    // F36 × F35 coordination: when a pending-inject is already in
    // flight (sub-agent really is busy), F35's `attempt_limbo_reinject`
    // must NOT bump its retry counter — the deterministic budget
    // would burn on a no-op (the dispatch path would just rewrite the
    // pending record).
    let Some((_tmp, paths, slug, _scoped)) =
        fixture("f35_skips_when_f36_pending", 10)
    else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);
    let pending_path = pending_inject_path_in(&project_dir);

    // Plant: a Task PreToolUse so dispatch defers, plus a phase_inject
    // tail so F35 classifies as InjectLimbo. (Order matters for tail
    // detection: phase_inject is the final event so the classifier
    // sees it as the tail.)
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "PreToolUse", "tool": "Task"}),
    );
    write_event(
        &progress_path,
        &json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "event": "phase_inject",
            "phase": "implement",
        }),
    );
    save_pending_inject(
        &pending_path,
        &PendingInject::new(slug.clone(), "implement", vec![], chrono::Utc::now(), 10),
    )
    .unwrap();

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();

    let retry = limbo_retry_path_in(&project_dir);
    let count = if retry.exists() {
        load_limbo_retry_count(&retry, "implement")
            .unwrap()
            .count
    } else {
        0
    };
    assert_eq!(
        count, 0,
        "F35 must not bump retry counter while F36 pending-inject is in flight",
    );
}
