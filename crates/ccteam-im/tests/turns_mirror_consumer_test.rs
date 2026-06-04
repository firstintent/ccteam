//! V0.6.1 F137 — verifies that `BotSupervisor::ensure_started` spawns a
//! task that drains the adapter's `events()` stream and appends every
//! `ItemCompleted(AgentMessage)` row to the bot's `turns.jsonl`.
//!
//! Before F137, `turns_mirror::append_turn` was only called by unit
//! tests; the production ccteam-im daemon never consumed the
//! `events()` stream returned by `ClaudeTuiAdapter`. Consequence:
//! `<project>/.ccteam/chat/<role>/turns.jsonl` stayed empty and the
//! F134 outbound forwarder had no source rows to dispatch.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ccteam_harness::execution::turns_mirror::{self, TurnRecord};
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, ThreadStatus,
    TurnId, TurnInput,
};
use ccteam_im::supervisor::BotSupervisor;
use ccteam_im::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Stub adapter that hands the consumer task a stream containing two
/// `ItemCompleted(AgentMessage)` events plus one non-text event that
/// must be ignored. The supervisor task should append exactly two
/// rows to turns.jsonl.
#[derive(Debug, Default)]
struct ScriptedAdapter {
    /// Wrapped in Mutex+Option so `events()` (sync) can consume the
    /// scripted vec exactly once.
    scripted: Mutex<Option<Vec<ThreadEvent>>>,
}

impl ScriptedAdapter {
    fn with_events(evts: Vec<ThreadEvent>) -> Self {
        Self {
            scripted: Mutex::new(Some(evts)),
        }
    }
}

#[async_trait]
impl HarnessAdapter for ScriptedAdapter {
    fn name(&self) -> &'static str {
        "scripted"
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
            identity: format!("scripted-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({"slug": ctx.slug, "role": spec.role}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("scripted-turn"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // Pop the scripted vec via try_lock (events() is sync; the
        // unit-test path is single-threaded so no contention). Fall
        // back to empty if already consumed.
        let evts = self
            .scripted
            .try_lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_default();
        Box::pin(futures::stream::iter(evts))
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "scripted".into(),
        })
    }
    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn handle_directive(
        &self,
        _h: &ThreadHandle,
        _d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Rejected {
            reason: "test double".to_string(),
        })
    }

    async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
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

fn agent_msg(id: &str, text: &str) -> ThreadEvent {
    ThreadEvent::ItemCompleted {
        item: ThreadItem {
            id: id.to_string(),
            details: ThreadItemDetails::AgentMessage(text.to_string()),
        },
    }
}

#[tokio::test]
async fn events_consumer_appends_two_assistant_rows() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("dev-foo");

    let evts = vec![
        agent_msg("a-1", "hello world"),
        // Non-text event must be skipped (Reasoning has no role in the
        // outbound forwarder's contract).
        ThreadEvent::ItemUpdated {
            item: ThreadItem {
                id: "r-1".into(),
                details: ThreadItemDetails::Reasoning("thinking…".into()),
            },
        },
        agent_msg("a-2", "second reply"),
    ];
    let adapter = Arc::new(ScriptedAdapter::with_events(evts));
    let sup = BotSupervisor::new(reg(), tmp.path(), adapter.clone());

    sup.ensure_started().await.unwrap();

    // Poll for the two expected rows for up to 3s — the consumer task
    // drains an in-memory stream so this typically completes in < 50ms,
    // but tests on a busy CI box can lag.
    let started = Instant::now();
    let rows = loop {
        let rows = turns_mirror::read_all_turns(&project_dir, "lead").unwrap_or_default();
        if rows.len() >= 2 || started.elapsed() > Duration::from_secs(3) {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    assert_eq!(
        rows.len(),
        2,
        "expected exactly 2 assistant rows in turns.jsonl, got {}: {rows:?}",
        rows.len()
    );

    // Validate schema: turn_id pass-through from ThreadItem.id, vendor
    // tagged from BotRegistration, role matches the bot, assistant
    // body matches the scripted AgentMessage text.
    let r0: &TurnRecord = &rows[0];
    assert_eq!(r0.turn_id, "a-1");
    assert_eq!(r0.vendor, "claude");
    assert_eq!(r0.role, "lead");
    assert_eq!(r0.assistant, "hello world");
    assert_eq!(r0.user, "");

    let r1: &TurnRecord = &rows[1];
    assert_eq!(r1.turn_id, "a-2");
    assert_eq!(r1.assistant, "second reply");
}

#[tokio::test]
async fn events_consumer_aborts_on_shutdown() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("dev-foo");

    // Empty stream — the consumer task should exit on its own once
    // the stream ends; we just confirm shutdown is a clean no-op
    // afterwards (no double-abort panic, no leftover task) and that
    // no rows were written.
    let adapter = Arc::new(ScriptedAdapter::with_events(Vec::new()));
    let sup = BotSupervisor::new(reg(), tmp.path(), adapter.clone());

    sup.ensure_started().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    sup.shutdown().await.unwrap();

    let rows = turns_mirror::read_all_turns(&project_dir, "lead").unwrap_or_default();
    assert!(rows.is_empty(), "no events → no rows; got {rows:?}");
}
