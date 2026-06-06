//! Integration tests for the BotSupervisor `signals/reset.signal`
//! operator session-reset contract. (The `ccteam__chat_reset` MCP tool
//! that once wrote this signal — and its `chat_reset_signal_path` helper
//! — were retired; the supervisor file-signal contract remains and is
//! covered here.)
//!
//! Coverage:
//! - The supervisor's `decide()` reads `signals/reset.signal` and
//!   returns `ResetSession`, independent of restart-budget state.
//! - `BotSupervisor::reset_session()` archives `turns.jsonl` to
//!   `archive/turns-<unix-ms>.jsonl`, clears the on-disk transcript
//!   cursor, deletes the signal file, and starts a fresh handle on
//!   the adapter.
//! - The V0.6.4 Bug B防线: post-reset, the on-disk transcript cursor is
//!   zeroed so the new session's first transcript bytes are NOT
//!   dedup-skipped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};
use ccteam_im::supervisor::{
    bot_dir, decide, BotState, BotSupervisor, SupervisorAction, RESET_SIGNAL,
};
use ccteam_im::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;

#[derive(Debug, Default)]
struct CountingAdapter {
    starts: AtomicUsize,
    closes: AtomicUsize,
}

#[async_trait]
impl HarnessAdapter for CountingAdapter {
    fn name(&self) -> &'static str {
        "counting"
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
            identity: format!("cnt-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("t"))
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
        workflow_slug: "demo".into(),
        role: "helper".into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "mcp".into(),
        im_chat_id: "0".into(),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    }
}

fn write_reset_signal(projects_root: &std::path::Path, r: &BotRegistration) {
    let sig = bot_dir(projects_root, r).join("signals").join(RESET_SIGNAL);
    std::fs::create_dir_all(sig.parent().unwrap()).unwrap();
    std::fs::write(&sig, format!("{}", chrono::Utc::now().timestamp_millis())).unwrap();
}

#[test]
fn decide_returns_reset_session_when_signal_present() {
    let tmp = TempDir::new().unwrap();
    let r = reg();
    write_reset_signal(tmp.path(), &r);
    // Provide a fresh heartbeat so the noop / restart path doesn't fire.
    let dir = bot_dir(tmp.path(), &r);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("heartbeat"), "x").unwrap();
    let st = BotState {
        handle: Some(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: "x".into(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        }),
        ..Default::default()
    };
    assert_eq!(
        decide(tmp.path(), &r, &st, SystemTime::now()),
        SupervisorAction::ResetSession
    );
}

#[test]
fn decide_reset_session_beats_restart_budget_exhaustion() {
    // V0.6.5 F147 design: reset is an intentional user action and
    // must succeed even when the per-hour restart budget is burnt.
    // We've routed the `RESET_SIGNAL` check *before* the budget check
    // in `decide()` for exactly this reason.
    let tmp = TempDir::new().unwrap();
    let r = reg();
    write_reset_signal(tmp.path(), &r);
    let st = BotState {
        handle: Some(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: "x".into(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        }),
        restarts: (0..ccteam_im::supervisor::MAX_RESTARTS_PER_HOUR)
            .map(|_| std::time::Instant::now())
            .collect(),
        ..Default::default()
    };
    assert_eq!(
        decide(tmp.path(), &r, &st, SystemTime::now()),
        SupervisorAction::ResetSession
    );
}

#[test]
fn decide_shutdown_still_beats_reset_signal() {
    // Shutdown is terminal — even an in-flight reset signal must
    // defer to it. Otherwise an admin could race a `chat_reset` past
    // a `@ccteam stop everything` and re-spawn the bot we just asked
    // to kill.
    let tmp = TempDir::new().unwrap();
    let r = reg();
    write_reset_signal(tmp.path(), &r);
    // ALSO drop shutdown.signal.
    let sig_dir = bot_dir(tmp.path(), &r).join("signals");
    std::fs::create_dir_all(&sig_dir).unwrap();
    std::fs::write(sig_dir.join("shutdown.signal"), "").unwrap();
    let st = BotState {
        handle: Some(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: "x".into(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        }),
        ..Default::default()
    };
    assert_eq!(
        decide(tmp.path(), &r, &st, SystemTime::now()),
        SupervisorAction::Shutdown
    );
}

#[tokio::test]
async fn reset_session_archives_turns_jsonl_and_clears_transcript_cursor() {
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let r = reg();
    let adapter = Arc::new(CountingAdapter::default());
    let sup = BotSupervisor::new(r.clone(), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

    // Seed turns.jsonl + transcript-cursor.json + outbound.cursor in
    // the bot dir so we can prove reset archives + clears them.
    let bd = bot_dir(&projects_root, &r);
    std::fs::create_dir_all(&bd).unwrap();
    std::fs::write(bd.join("turns.jsonl"), "{\"turn_id\":\"old\"}\n").unwrap();
    std::fs::write(
        bd.join("transcript-cursor.json"),
        r#"{"session_id":"old-sid","byte_offset":1234,"prior_offsets":{"old-sid":1234}}"#,
    )
    .unwrap();
    std::fs::write(bd.join("outbound.cursor"), r#"{"position":5000}"#).unwrap();

    // Drop the reset.signal file the MCP tool would have written.
    write_reset_signal(&projects_root, &r);

    let archived = sup.reset_session().await.unwrap();
    let archived_path = archived.expect("archive path returned");
    assert!(archived_path.exists(), "archived turns.jsonl must exist");
    assert!(
        archived_path.to_string_lossy().contains("/archive/turns-"),
        "archive lives under archive/ — got {}",
        archived_path.display()
    );
    // Old turns.jsonl is gone (renamed, not copied).
    assert!(!bd.join("turns.jsonl").exists());
    // V0.6.4 Bug B防线: transcript cursor + outbound cursor are
    // wiped so the new session restarts from byte 0 instead of
    // dedup-skipping the first burst.
    assert!(
        !bd.join("transcript-cursor.json").exists(),
        "transcript cursor must be cleared on reset"
    );
    assert!(
        !bd.join("outbound.cursor").exists(),
        "outbound cursor must be cleared on reset"
    );
    // Signal is unlinked so the next tick doesn't loop.
    let sig = bot_dir(&projects_root, &r)
        .join("signals")
        .join(RESET_SIGNAL);
    assert!(!sig.exists(), "reset.signal must be consumed");
    // Adapter saw close + fresh start.
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reset_session_on_fresh_bot_with_no_turns_jsonl_is_a_noop_archive() {
    // Edge case: user hits reset on a freshly-registered bot that
    // hasn't taken a turn yet. Reset must succeed (close + start)
    // without erroring on the missing turns.jsonl.
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let r = reg();
    let adapter = Arc::new(CountingAdapter::default());
    let sup = BotSupervisor::new(r.clone(), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();

    let archived = sup.reset_session().await.unwrap();
    assert!(
        archived.is_none(),
        "fresh bot reset returns None for archive path"
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 2);
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn apply_action_dispatches_reset_session() {
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let r = reg();
    let adapter = Arc::new(CountingAdapter::default());
    let sup = BotSupervisor::new(r.clone(), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();
    sup.apply_action(SupervisorAction::ResetSession)
        .await
        .unwrap();
    // Same observable shape as `reset_session()` direct call.
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 2);
}

#[test]
fn reset_signal_const_is_documented_filename() {
    // `RESET_SIGNAL` is the filename `signal_present` checks under
    // `<bot_dir>/signals/`. Lock the documented operator contract: a
    // session reset is triggered by writing `signals/reset.signal`.
    assert_eq!(RESET_SIGNAL, "reset.signal");
}
