//! V0.6.8 F196 — integration tests for the SessionStart marker
//! self-heal state machine on `BotSupervisor`.
//!
//! Scenarios:
//! - threshold-crossing: 30 `record_marker_missing` calls without an
//!   intervening `record_marker_found` returns `Heal` on call 30,
//!   `Quiet` on calls 1..29.
//! - `record_marker_found` resets both `marker_missing_count` and
//!   `marker_self_heal_attempts` so a recovered bot starts fresh.
//! - After `MAX_MARKER_SELF_HEAL_ATTEMPTS` consecutive heal escalations
//!   the next threshold crossing returns `PermanentFailure` and the
//!   supervisor latches `marker_stuck`. Further missing reports
//!   short-circuit to `Quiet` — no more cycling.
//! - `attempt_marker_self_heal` (which calls the existing F192c
//!   `reset_session`) MUST preserve the F196 budget — otherwise every
//!   heal would zero `marker_self_heal_attempts` and the latch could
//!   never trip. The clear lives only in the operator-driven
//!   `apply_action(ResetSession)` path.
//! - The trait-impl path (registry lookup → `report_marker_missing`)
//!   spawns the heal task and produces identical state-machine
//!   transitions as the direct in-line `record_marker_missing` calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_im::supervisor::{
    BotSupervisor, MarkerHealAction, SupervisorAction, MARKER_MISSING_RESET_THRESHOLD,
    MAX_MARKER_SELF_HEAL_ATTEMPTS,
};
use ccteam_im::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;

/// Stub `HarnessAdapter` that just counts start/close calls. Used
/// where the test asserts state-machine semantics — heal sequence
/// includes a call into `reset_session` whose archive / unlink
/// branches we don't care about here.
#[derive(Debug, Default)]
struct StubAdapter {
    starts: AtomicUsize,
    closes: AtomicUsize,
}

#[async_trait]
impl HarnessAdapter for StubAdapter {
    fn name(&self) -> &'static str {
        "stub-marker"
    }
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }
    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("stub-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({"slug": ctx.slug, "role": spec.role}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("stub-turn"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(futures::stream::empty())
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "stub".into(),
        })
    }
    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn reg() -> BotRegistration {
    BotRegistration {
        workflow_slug: "dev-foo".into(),
        role: "lead".into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "mock".into(),
        im_chat_id: "1".into(),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    }
}

/// Pre-condition: F196 below-threshold reports stay `Quiet`. We
/// confirm the boundary: `MARKER_MISSING_RESET_THRESHOLD - 1`
/// missing reports do nothing, the `MARKER_MISSING_RESET_THRESHOLD`-th
/// returns `Heal`.
#[tokio::test]
async fn record_marker_missing_returns_quiet_below_threshold() {
    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter));
    // Register self_weak so the trait impl can run later (not used
    // here but needed once we transition to the Heal-path test).
    sup.register_as_marker_reporter();

    for i in 1..MARKER_MISSING_RESET_THRESHOLD {
        let action = sup.record_marker_missing().await;
        assert_eq!(
            action,
            MarkerHealAction::Quiet,
            "report #{i} below threshold should stay Quiet"
        );
    }
    let st = sup.state_snapshot().await;
    assert_eq!(st.marker_missing_count, MARKER_MISSING_RESET_THRESHOLD - 1);
    assert_eq!(st.marker_self_heal_attempts, 0);
    assert!(!st.marker_stuck);
}

#[tokio::test]
async fn threshold_crossing_returns_heal_and_resets_count() {
    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter));
    sup.register_as_marker_reporter();

    for _ in 0..(MARKER_MISSING_RESET_THRESHOLD - 1) {
        sup.record_marker_missing().await;
    }
    let action = sup.record_marker_missing().await;
    assert_eq!(
        action,
        MarkerHealAction::Heal,
        "report at threshold should return Heal"
    );
    let st = sup.state_snapshot().await;
    // Per-window counter zeroed so the next heal also waits a full
    // window (no back-to-back resets on the very next miss).
    assert_eq!(st.marker_missing_count, 0);
    assert_eq!(
        st.marker_self_heal_attempts, 1,
        "Heal escalation should bump heal-attempts by 1"
    );
    assert!(!st.marker_stuck);
}

#[tokio::test]
async fn record_marker_found_resets_both_counters() {
    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter));
    sup.register_as_marker_reporter();

    // Climb to threshold, escalating once (bumps heal-attempts to 1).
    for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
        sup.record_marker_missing().await;
    }
    let st_pre = sup.state_snapshot().await;
    assert_eq!(st_pre.marker_self_heal_attempts, 1);

    // Marker reappears (the real-world equivalent: SessionStart hook
    // finally fires on the new session). Both counters must zero.
    sup.record_marker_found().await;
    let st_post = sup.state_snapshot().await;
    assert_eq!(st_post.marker_missing_count, 0);
    assert_eq!(
        st_post.marker_self_heal_attempts, 0,
        "marker_found must reset the heal budget so a future flake gets a full window"
    );
    assert!(!st_post.marker_stuck);
}

#[tokio::test]
async fn three_failed_heals_latch_permanent_failure() {
    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter));
    sup.register_as_marker_reporter();

    // Burn through all three heal attempts: each is a full threshold
    // window of missing reports without an intervening `found`.
    for attempt in 1..=MAX_MARKER_SELF_HEAL_ATTEMPTS {
        for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
            let _ = sup.record_marker_missing().await;
        }
        // The Nth attempt's threshold-crossing must have returned
        // `Heal` and bumped heal-attempts. Confirm via snapshot.
        let st = sup.state_snapshot().await;
        assert_eq!(
            st.marker_self_heal_attempts, attempt,
            "after {attempt} full-window failures, heal-attempts should equal {attempt}"
        );
    }

    // Fourth threshold crossing — heal budget already at the cap, so
    // the supervisor latches marker_stuck and returns PermanentFailure.
    for _ in 0..(MARKER_MISSING_RESET_THRESHOLD - 1) {
        assert_eq!(
            sup.record_marker_missing().await,
            MarkerHealAction::Quiet,
            "ramp-up to 4th threshold should be Quiet"
        );
    }
    let final_action = sup.record_marker_missing().await;
    assert_eq!(
        final_action,
        MarkerHealAction::PermanentFailure,
        "after {MAX_MARKER_SELF_HEAL_ATTEMPTS} heal attempts, next crossing must permanently fail"
    );
    let st = sup.state_snapshot().await;
    assert!(
        st.marker_stuck,
        "PermanentFailure return MUST latch marker_stuck"
    );

    // Subsequent misses are silently dropped — supervisor refuses to
    // cycle the bot further. (Recovery requires operator reset.)
    for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
        let action = sup.record_marker_missing().await;
        assert_eq!(
            action,
            MarkerHealAction::Quiet,
            "latched marker_stuck must drop every subsequent missing report"
        );
    }
}

#[tokio::test]
async fn operator_reset_clears_marker_stuck_latch() {
    // Operator workflow: bot hits marker_stuck, operator restores
    // state.json + writes signals/reset.signal. The daemon's tick
    // observes the signal, returns `SupervisorAction::ResetSession`,
    // and `apply_action` is what clears the F196 latches (NOT
    // `reset_session` directly — that would short-circuit the heal
    // budget, see `attempt_marker_self_heal_drains_budget_to_latch`
    // for the proof).
    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter.clone()));
    sup.register_as_marker_reporter();
    sup.ensure_started().await.unwrap();

    // Force the supervisor into marker_stuck by emulating the burn
    // sequence (same as `three_failed_heals_latch_permanent_failure`).
    for _ in 0..(MAX_MARKER_SELF_HEAL_ATTEMPTS + 1) {
        for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
            let _ = sup.record_marker_missing().await;
        }
    }
    let st_pre = sup.state_snapshot().await;
    assert!(
        st_pre.marker_stuck,
        "test prereq: supervisor should be in marker_stuck before reset"
    );

    // Drive the operator reset path the way the daemon's tick does.
    sup.apply_action(SupervisorAction::ResetSession)
        .await
        .unwrap();
    let st_post = sup.state_snapshot().await;
    assert_eq!(st_post.marker_missing_count, 0);
    assert_eq!(st_post.marker_self_heal_attempts, 0);
    assert!(
        !st_post.marker_stuck,
        "ResetSession apply_action MUST clear marker_stuck so the supervisor re-arms"
    );
}

/// The critical test: drive the full production wiring (trait impl
/// via the global reporter registry, spawned heal task, real
/// `reset_session` call) and confirm that the heal budget actually
/// drains down to `marker_stuck` after `MAX_MARKER_SELF_HEAL_ATTEMPTS`
/// rounds. If `reset_session` mistakenly cleared the F196 counters
/// itself, this test would loop forever (or fail with attempts == 0
/// after each round).
#[tokio::test]
async fn attempt_marker_self_heal_drains_budget_to_latch() {
    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter.clone()));
    sup.register_as_marker_reporter();
    sup.ensure_started().await.unwrap();

    // Cross threshold once → `record_marker_missing` returns Heal,
    // which the test invokes directly (not via the spawn-trait path
    // — we want a deterministic assertion on the post-heal state).
    for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
        let _ = sup.record_marker_missing().await;
    }
    // attempt_marker_self_heal calls reset_session in-line. After
    // it returns, the budget must NOT have been wiped — otherwise
    // the next round would never advance attempts past 1.
    let n1 = sup.attempt_marker_self_heal().await;
    assert_eq!(n1, 1);
    let st1 = sup.state_snapshot().await;
    assert_eq!(
        st1.marker_self_heal_attempts, 1,
        "after heal #1 returns, attempts must remain at 1 (NOT zeroed by reset_session)"
    );
    // Counter zeroed by record_marker_missing's window-reset on the
    // crossing; reset_session must NOT have touched it.
    assert_eq!(st1.marker_missing_count, 0);

    // Round 2.
    for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
        let _ = sup.record_marker_missing().await;
    }
    let n2 = sup.attempt_marker_self_heal().await;
    assert_eq!(n2, 2);
    assert_eq!(sup.state_snapshot().await.marker_self_heal_attempts, 2);

    // Round 3 — last heal in the budget. After this, the next
    // threshold crossing returns PermanentFailure.
    for _ in 0..MARKER_MISSING_RESET_THRESHOLD {
        let _ = sup.record_marker_missing().await;
    }
    let n3 = sup.attempt_marker_self_heal().await;
    assert_eq!(n3, MAX_MARKER_SELF_HEAL_ATTEMPTS);

    // Final crossing → PermanentFailure latch.
    for _ in 0..(MARKER_MISSING_RESET_THRESHOLD - 1) {
        assert_eq!(
            sup.record_marker_missing().await,
            MarkerHealAction::Quiet,
            "ramp-up to 4th threshold (post-heal) should be Quiet"
        );
    }
    assert_eq!(
        sup.record_marker_missing().await,
        MarkerHealAction::PermanentFailure,
        "after {MAX_MARKER_SELF_HEAL_ATTEMPTS} drained heals, next crossing MUST permanently fail"
    );
    assert!(
        sup.state_snapshot().await.marker_stuck,
        "PermanentFailure return MUST latch marker_stuck"
    );
}

/// V0.6.8 F196 — exercise the *full* production wiring: register the
/// supervisor in the global reporter registry, look it up the way the
/// chat-mode tail loop does, fire `report_marker_missing` through the
/// trait, let the spawned heal task complete. Confirms the trait impl
/// + Arc<Self> recovery + tokio::spawn + state-machine integration
/// all line up end-to-end.
#[tokio::test]
async fn trait_impl_heal_path_runs_through_registry() {
    use std::time::Duration;

    let adapter = Arc::new(StubAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new(reg(), tmp.path(), adapter.clone()));
    sup.ensure_started().await.unwrap();
    sup.register_as_marker_reporter();

    // Look up the way the chat-mode tail loop does in production.
    let reporter = ccteam_core::execution::marker_reporter::lookup("dev-foo", "lead")
        .expect("registered supervisor should be found in the reporter registry");

    // Fire below-threshold reports through the trait — should be
    // entirely transparent (no spawn, no reset, state machine
    // alone bookkeeps).
    for _ in 0..(MARKER_MISSING_RESET_THRESHOLD - 1) {
        reporter.report_marker_missing().await;
    }
    let st_pre = sup.state_snapshot().await;
    assert_eq!(
        st_pre.marker_missing_count,
        MARKER_MISSING_RESET_THRESHOLD - 1
    );
    assert_eq!(st_pre.marker_self_heal_attempts, 0);

    // Threshold-crossing report — trait impl spawns the heal task.
    reporter.report_marker_missing().await;
    // Yield + sleep briefly so the spawned tokio task gets a chance
    // to run reset_session + complete. 100ms is enough on a stub
    // adapter (no IO past tempdir file ops).
    tokio::time::sleep(Duration::from_millis(100)).await;

    let st_post = sup.state_snapshot().await;
    assert_eq!(
        st_post.marker_self_heal_attempts, 1,
        "trait-impl-driven heal must have run and bumped attempts to 1 \
         (if this is 0 the budget was wiped by reset_session — the F196 bug)"
    );
    assert!(
        !st_post.marker_stuck,
        "single heal should not latch marker_stuck — three are needed"
    );

    // report_marker_found should reset both counters via the trait.
    reporter.report_marker_found().await;
    let st_found = sup.state_snapshot().await;
    assert_eq!(st_found.marker_missing_count, 0);
    assert_eq!(st_found.marker_self_heal_attempts, 0);
}
