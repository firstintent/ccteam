//! V0.6.1 F136 — verifies [`BotSupervisor::ensure_started`] spawns the
//! per-bot heartbeat-writer task and that the writer touches
//! `<projects_root>/<slug>/.ccteam/chat/<role>/heartbeat` within a few
//! seconds of start.
//!
//! Before F136, `supervisor::decide` would observe the missing
//! heartbeat → return `SupervisorAction::Restart` → the daemon would
//! tear down + recreate the tmux session every ~65s, wiping any
//! `send-keys` payloads queued by the F132 inbound pipeline.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_imd::supervisor::BotSupervisor;
use ccteam_imd::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;

#[derive(Debug, Default)]
struct NoopAdapter {
    starts: AtomicUsize,
    closes: AtomicUsize,
}

#[async_trait]
impl HarnessAdapter for NoopAdapter {
    fn name(&self) -> &'static str {
        "noop"
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
            identity: format!("noop-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({"slug": ctx.slug, "role": spec.role}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("noop-turn"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // Pending forever so the F137 consumer task stays alive but
        // never emits — keeps the heartbeat assertion isolated from
        // the events-consumer side of `ensure_started`.
        Box::pin(futures::stream::pending())
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "noop".into(),
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

#[tokio::test]
async fn ensure_started_spawns_heartbeat_writer_that_touches_file() {
    let tmp = TempDir::new().unwrap();
    let adapter = Arc::new(NoopAdapter::default());
    let sup = BotSupervisor::new(reg(), tmp.path(), adapter.clone());

    sup.ensure_started().await.unwrap();
    let hb = tmp
        .path()
        .join("dev-foo")
        .join(".ccteam")
        .join("chat")
        .join("lead")
        .join("heartbeat");

    // `tokio::time::interval`'s first tick fires immediately, so the
    // heartbeat file should appear well under a second. Poll up to 3s
    // to keep the test robust on busy CI runners.
    let started = Instant::now();
    while !hb.exists() && started.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hb.exists(),
        "heartbeat file {} should have been written by the F136 task",
        hb.display()
    );

    // mtime should be fresh — well inside the supervisor's
    // STALE_THRESHOLD (60s), proving `decide()` would now return NoOp
    // instead of Restart.
    let meta = std::fs::metadata(&hb).unwrap();
    let age = SystemTime::now()
        .duration_since(meta.modified().unwrap())
        .unwrap_or_default();
    assert!(
        age < Duration::from_secs(30),
        "heartbeat mtime should be < 30s old; got {age:?}"
    );

    // Body should be parseable as RFC3339 so operators eyeballing the
    // file get a useful timestamp, not raw debug output.
    let body = std::fs::read_to_string(&hb).unwrap();
    chrono::DateTime::parse_from_rfc3339(body.trim())
        .expect("heartbeat body should be RFC3339 timestamp");

    // Shutdown should abort the heartbeat task — subsequent file
    // writes should stop. We verify by snapshotting mtime, sleeping
    // briefly, then asserting mtime didn't advance.
    sup.shutdown().await.unwrap();
    let mtime_before = std::fs::metadata(&hb).unwrap().modified().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mtime_after = std::fs::metadata(&hb).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "heartbeat writer should be aborted after shutdown"
    );
}
