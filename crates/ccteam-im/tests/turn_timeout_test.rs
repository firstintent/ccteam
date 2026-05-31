//! V0.6.8 F195 — per-turn timeout watchdog integration test.
//!
//! Scenario: a bot's `submit_turn` succeeds but the harness never
//! emits the matching `ItemCompleted/AgentMessage` (simulating a
//! silently-hung claude session / broken Stop hook / stuck tail loop).
//! Today this leaves the user with zero feedback. F195 arms a
//! per-turn watchdog on `handle_inbound`; at `1×` `turn_timeout_sec`
//! the daemon emits `chat_turn_running_long` + IM "still working"
//! reply; at `2×` it emits `chat_turn_timeout` + IM "stuck" reply.
//! Third tick onward: nothing (latches suppress repeats).
//!
//! The test uses a tight `turn_timeout_sec = 1` so the windows fit
//! into a 3.5s `max_runtime` budget.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_im::supervisor::{BotSupervisor, TurnWatchdogNotice};
use ccteam_im::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;

#[derive(Debug, Default)]
struct StuckAdapter {
    submits: AtomicUsize,
}

#[async_trait]
impl HarnessAdapter for StuckAdapter {
    fn name(&self) -> &'static str {
        "f195-stuck"
    }
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }
    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("stuck-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        Ok(TurnId::new("stuck-turn-1"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // Never emit anything → simulates a hung Stop hook / silent
        // claude turn. The watchdog has to surface the stall on its own.
        Box::pin(futures::stream::pending())
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "stuck".into(),
        })
    }
    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }
}

fn mk_reg() -> BotRegistration {
    BotRegistration {
        workflow_slug: "dev-foo".into(),
        role: "lead".into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "mock".into(),
        im_chat_id: "chat-1".into(),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    }
}

/// Drive a fresh BotSupervisor through the F195 threshold sequence
/// using a tight 1s timeout. Asserts:
/// 1. Before submit_turn → no notice.
/// 2. After submit_turn, before 1× → no notice yet.
/// 3. After 1× crossing → RunningLong fires (once).
/// 4. Subsequent calls before 2× → no notice (latched).
/// 5. After 2× crossing → Timeout fires (once).
/// 6. Subsequent calls → no notice (both latches set).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watchdog_fires_running_long_then_timeout_then_suppresses() {
    let stub = Arc::new(StuckAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new_with_turn_timeout(
        mk_reg(),
        tmp.path(),
        stub.clone(),
        std::collections::HashMap::new(),
        1,
    ));

    sup.ensure_started().await.unwrap();
    // Step 1: pre-submit → no notice.
    assert_eq!(sup.check_turn_watchdog().await, None);

    // Step 2: submit, then poll immediately → no notice yet (0s elapsed).
    let id = sup.handle_inbound("hi".into(), 0).await.unwrap();
    assert_eq!(id.0, "stuck-turn-1");
    assert_eq!(stub.submits.load(Ordering::SeqCst), 1);
    let immediately = sup.check_turn_watchdog().await;
    assert!(
        immediately.is_none(),
        "watchdog must not fire before 1x threshold (got {immediately:?})"
    );
    let snap = sup.active_turn_snapshot().await.expect("turn armed");
    assert_eq!(snap.turn_id.0, "stuck-turn-1");
    assert!(!snap.long_emitted);
    assert!(!snap.timeout_emitted);

    // Step 3: wait past 1× threshold.
    let t0 = Instant::now();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let notice = sup
        .check_turn_watchdog()
        .await
        .expect("RunningLong must fire after 1.1s with timeout=1s");
    match notice {
        TurnWatchdogNotice::RunningLong {
            turn_id,
            elapsed_sec,
        } => {
            assert_eq!(turn_id, "stuck-turn-1");
            assert!(
                elapsed_sec >= 1,
                "elapsed_sec >= timeout; got {elapsed_sec}"
            );
        }
        other => panic!("expected RunningLong, got {other:?}"),
    }

    // Step 4: poll again before 2× → no notice (long_emitted latched).
    let immediately = sup.check_turn_watchdog().await;
    assert!(
        immediately.is_none(),
        "RunningLong must NOT re-fire while long_emitted latched (got {immediately:?})"
    );

    // Step 5: wait past 2× threshold (total ~2.2s since submit).
    let elapsed_so_far = t0.elapsed();
    if elapsed_so_far < Duration::from_millis(2200) {
        tokio::time::sleep(Duration::from_millis(2200) - elapsed_so_far).await;
    }
    let notice = sup
        .check_turn_watchdog()
        .await
        .expect("Timeout must fire after 2.2s with timeout=1s");
    match notice {
        TurnWatchdogNotice::Timeout {
            turn_id,
            elapsed_sec,
        } => {
            assert_eq!(turn_id, "stuck-turn-1");
            assert!(
                elapsed_sec >= 2,
                "elapsed_sec >= 2x timeout; got {elapsed_sec}"
            );
        }
        other => panic!("expected Timeout, got {other:?}"),
    }

    // Step 6: 3× tick (~3.3s) → still no further notice.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let third = sup.check_turn_watchdog().await;
    assert!(
        third.is_none(),
        "no third notice (both latches set); got {third:?}"
    );

    // R5 sanity: the supervisor is still running. We never killed it.
    assert!(
        sup.is_started().await,
        "R5: watchdog must not kill the long session"
    );
}

/// Clearing `active_turn` (the events consumer's behaviour when the
/// harness finally emits `ItemCompleted`) must disarm the watchdog so
/// subsequent ticks return None even past the 1× / 2× thresholds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watchdog_clears_on_turn_completion_signal() {
    let stub = Arc::new(StuckAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new_with_turn_timeout(
        mk_reg(),
        tmp.path(),
        stub.clone(),
        std::collections::HashMap::new(),
        1,
    ));

    sup.ensure_started().await.unwrap();
    sup.handle_inbound("hi".into(), 0).await.unwrap();

    // Cross the 1× threshold + fire RunningLong.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let notice = sup.check_turn_watchdog().await;
    assert!(matches!(
        notice,
        Some(TurnWatchdogNotice::RunningLong { .. })
    ));

    // Simulate the events consumer's clear (what fires on
    // `ItemCompleted/AgentMessage`).
    sup.clear_active_turn().await;
    assert!(sup.active_turn_snapshot().await.is_none());

    // Wait past 2× threshold; nothing should fire.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        sup.check_turn_watchdog().await,
        None,
        "completed turn must not surface a post-completion Timeout"
    );
}

/// `shutdown` / `reset_session` must also clear the deadline so a
/// stale turn doesn't survive a session teardown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watchdog_cleared_by_shutdown_and_reset() {
    let stub = Arc::new(StuckAdapter::default());
    let tmp = TempDir::new().unwrap();
    let sup = Arc::new(BotSupervisor::new_with_turn_timeout(
        mk_reg(),
        tmp.path(),
        stub.clone(),
        std::collections::HashMap::new(),
        1,
    ));

    sup.ensure_started().await.unwrap();
    sup.handle_inbound("hi".into(), 0).await.unwrap();
    assert!(sup.active_turn_snapshot().await.is_some());
    sup.shutdown().await.unwrap();
    assert!(
        sup.active_turn_snapshot().await.is_none(),
        "shutdown must wipe the watchdog deadline"
    );

    sup.ensure_started().await.unwrap();
    sup.handle_inbound("hi again".into(), 0).await.unwrap();
    assert!(sup.active_turn_snapshot().await.is_some());
    sup.reset_session().await.unwrap();
    assert!(
        sup.active_turn_snapshot().await.is_none(),
        "reset_session must wipe the watchdog deadline"
    );
}
