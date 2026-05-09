//! V0.2.2 F35 — orchestrator integration tests for the event-aware
//! silence classifier. Covers:
//!
//! - `MidToolHung` writes the enriched `needs_attention.outbox.json`
//!   with all four F35 fields (`ccteam_classification` /
//!   `ccteam_silent_seconds` / `ccteam_last_event` / `ccteam_pane_tail`)
//! - `PostStopLimbo` triggers a deterministic re-inject (one
//!   `phase_inject` progress event added) and bumps
//!   `limbo-retry-count.json::count` to 1
//! - Second `PostStopLimbo` after the cap is reached writes an enriched
//!   outbox with `ccteam_classification: "limbo_capped"` instead of
//!   re-injecting again
//! - Phase advance resets the retry counter so the next phase gets a
//!   fresh budget (red-line: per-phase, not global)
//! - Meta-agent / evergreen project skipped (defense-in-depth — the
//!   `poll_tick` filter is the primary guard)
//!
//! Side-effect-free unit tests live in
//! `crates/ccteam-core/src/silence_classifier.rs::tests` (21 cases).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    limbo_retry_path_in, load_limbo_retry_count, progress,
    write_global_phase_templates, CcteamPaths, Orchestrator, OrchestratorConfig, Parallelism,
    PhaseState, ProjectState,
};
use serde_json::{json, Value};
use tempfile::TempDir;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug(test: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("test-f35-{test}-{pid}-{n}")
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

/// Spin up the same paths / state shape `dispatch_test.rs` uses, plus
/// a real tmux pane so `dispatch_phase` (which `attempt_limbo_reinject`
/// calls) can actually `send-keys`. Returns `None` when tmux isn't
/// installed so the test is skipped instead of failing on CI.
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
    let stale =
        now - chrono::Duration::seconds(silent_minutes.saturating_mul(60).max(0));
    ProjectState {
        slug: slug.clone(),
        team: "dev".into(),
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

fn read_outbox(project_dir: &Path) -> Value {
    let path = project_dir.join(".ccteam").join("needs_attention.outbox.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("expected outbox at {}", path.display()));
    serde_json::from_str(&body).unwrap()
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

#[test]
fn mid_tool_hung_writes_enriched_outbox() {
    let Some((_tmp, paths, slug, _scoped)) =
        fixture("mid_tool_hung", 10) // silent for 10min, default warn = 5min
    else {
        return;
    };

    let progress_path = paths.progress_jsonl(&slug);
    // Tail event = PreToolUse with non-Task tool. Above the warn
    // threshold but below escalate (default 30min) → MidToolHung.
    write_event(
        &progress_path,
        &json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "event": "PreToolUse",
            "tool": "Read",
            "path": "src/db.rs",
        }),
    );

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();

    let outbox = read_outbox(&paths.project_dir(&slug));
    assert_eq!(
        outbox["ccteam_classification"].as_str(),
        Some("mid_tool_hung"),
        "outbox classification field",
    );
    let secs = outbox["ccteam_silent_seconds"].as_u64().unwrap();
    assert!(secs >= 9 * 60, "silent_seconds reported: {secs}");
    let last = &outbox["ccteam_last_event"];
    assert_eq!(last["event"].as_str(), Some("PreToolUse"));
    assert_eq!(last["tool"].as_str(), Some("Read"));
    assert!(
        outbox.get("ccteam_pane_tail").is_some(),
        "pane_tail key present (best-effort string)",
    );
    assert_eq!(outbox["priority"].as_str(), Some("high"));
    assert_eq!(outbox["event_kind"].as_str(), Some("escalation"));
}

#[test]
fn post_stop_limbo_re_injects_once_then_caps() {
    let Some((_tmp, paths, slug, _scoped)) =
        fixture("post_stop_limbo_caps", 10)
    else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);

    // Tail = SubagentStop, silence past warn → PostStopLimbo.
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "SubagentStop"}),
    );

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();

    // First tick: should re-inject (counter 0 → 1).
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    let counter =
        load_limbo_retry_count(&limbo_retry_path_in(&project_dir), "implement").unwrap();
    assert_eq!(
        counter.count, 1,
        "first tick must re-inject and bump counter to 1",
    );
    let events = progress::read_all_events(&progress_path).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("phase_inject")),
        "first tick must append a phase_inject event (deterministic re-inject ran)",
    );

    // Second tick (still PostStopLimbo): cap exhausted → enriched
    // escalate with `limbo_capped` classification, no second
    // phase_inject.
    let phase_injects_before = events
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("phase_inject"))
        .count();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    let outbox = read_outbox(&project_dir);
    assert_eq!(
        outbox["ccteam_classification"].as_str(),
        Some("limbo_capped"),
        "second tick after cap must escalate, not re-inject",
    );
    let phase_injects_after = progress::read_all_events(&progress_path)
        .unwrap()
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("phase_inject"))
        .count();
    assert_eq!(
        phase_injects_after, phase_injects_before,
        "no new phase_inject after cap (deterministic budget exhausted)",
    );
}

#[test]
fn inject_limbo_re_injects_once() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("inject_limbo", 10) else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    let project_dir = paths.project_dir(&slug);

    // Tail = phase_inject, silence past warn → InjectLimbo. The F36
    // case (send-keys went to a sub-agent's context).
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
    assert_eq!(counter.count, 1);
}

#[test]
fn healthy_project_writes_no_outbox() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("healthy", 0) else {
        return;
    };
    let progress_path = paths.progress_jsonl(&slug);
    write_event(
        &progress_path,
        &json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "event": "PostToolUse",
            "tool": "Read",
        }),
    );

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    let outbox_path = paths
        .project_dir(&slug)
        .join(".ccteam")
        .join("needs_attention.outbox.json");
    assert!(
        !outbox_path.exists(),
        "Healthy classification must not write outbox",
    );
}

#[test]
fn empty_progress_log_writes_no_outbox() {
    let Some((_tmp, paths, slug, _scoped)) = fixture("empty_log", 99) else {
        return;
    };
    // No events written to progress.jsonl — empty log → Healthy.
    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    let outbox_path = paths
        .project_dir(&slug)
        .join(".ccteam")
        .join("needs_attention.outbox.json");
    assert!(!outbox_path.exists());
}

#[test]
fn evergreen_team_classifier_is_noop() {
    // Defense in depth — `poll_tick` already filters evergreen teams,
    // but the classifier itself must short-circuit too.
    let Some((_tmp, paths, slug, _scoped)) = fixture("evergreen_skip", 30) else {
        return;
    };
    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    state.team = "meta-agent".into();
    state.save(&paths.project_state(&slug)).unwrap();

    let progress_path = paths.progress_jsonl(&slug);
    write_event(
        &progress_path,
        &json!({"ts": chrono::Utc::now().to_rfc3339(), "event": "Stop"}),
    );

    let orch = build_orchestrator(paths.clone());
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    orch.classify_and_act_on_silence_for_test(&slug, &state, chrono::Utc::now())
        .unwrap();
    let outbox_path = paths
        .project_dir(&slug)
        .join(".ccteam")
        .join("needs_attention.outbox.json");
    assert!(
        !outbox_path.exists(),
        "evergreen team must short-circuit before writing outbox",
    );
}
