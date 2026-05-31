//! V0.6.1 F134 — daemon-level outbound wiring integration test.
//!
//! Before F134 the daemon spawned no `outbound` forwarder task — even
//! though `outbound::forward_new_rows` existed with unit-test coverage,
//! `run_daemon_with_shutdown` never called it. Symmetric gap with
//! F132 (inbound): the bot tmux session would receive user messages
//! (post-F132) but any reply it wrote to `turns.jsonl` was stranded.
//!
//! This integration test stitches together the production wiring:
//!
//!   pre-seeded `turns.jsonl` (one assistant row)
//!   → daemon tick → `drain_outboxes` → `forward_new_rows`
//!   → `MockChannel::send` (recipient = bot.im_chat_id).
//!
//! Two assertions:
//! 1. The mock channel's outbox holds the assistant row addressed to
//!    the registered `im_chat_id`.
//! 2. The cursor file is persisted so a hypothetical restart wouldn't
//!    double-forward.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_im::daemon::{run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs};
use ccteam_im::outbound;
use ccteam_im::register_bot;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::Channel;
use ccteam_im::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;

/// Build a transient `BotRegistration` for path-helper calls. F185
/// path resolvers take `&BotRegistration` so they can honor a per-bot
/// `project_dir`; these tests stay on the slug-fallback layout
/// (`project_dir = None`), matching the legacy `<projects_root>/<slug>/`
/// shape register_bot() persists.
fn bot_reg(slug: &str, role: &str) -> BotRegistration {
    BotRegistration {
        workflow_slug: slug.into(),
        role: role.into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "telegram".into(),
        im_chat_id: "0".into(),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    }
}

// ----- env isolation helpers (mirrors tests/inbound_wiring_test.rs) ---

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn isolate_home() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("HOME", tmp.path());
    tmp
}

// ----- stub adapter — quiet (supervisor calls start_thread; we ignore) -

#[derive(Debug, Default)]
struct QuietAdapter {
    starts: AtomicUsize,
}

#[async_trait]
impl HarnessAdapter for QuietAdapter {
    fn name(&self) -> &'static str {
        "f134-quiet"
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
            identity: format!("quiet-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("quiet-turn"))
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
        Ok(())
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_forwards_turns_jsonl_to_channel() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    register_bot(
        "dev-foo",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "chat-42",
    )
    .unwrap();

    // Pre-seed the bot's turns.jsonl with two assistant rows + one user
    // row. drain_outboxes should forward only the two assistant rows.
    let chat_dir = projects_root.join("dev-foo/.ccteam/chat/lead");
    std::fs::create_dir_all(&chat_dir).unwrap();
    let turns_path = chat_dir.join("turns.jsonl");
    std::fs::write(
        &turns_path,
        concat!(
            r#"{"role":"assistant","content":"hello from the bot"}"#,
            "\n",
            r#"{"role":"user","content":"echoed user line — should not forward"}"#,
            "\n",
            r#"{"role":"assistant","content":"second reply"}"#,
            "\n",
        ),
    )
    .unwrap();

    let mock = Arc::new(MockChannel::new());
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(QuietAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(600)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };

    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    // Outbox holds the two assistant rows, both addressed to chat-42.
    let outbox = mock.outbox().await;
    assert_eq!(
        outbox.len(),
        2,
        "expected 2 assistant rows forwarded (got {}): {:?}",
        outbox.len(),
        outbox
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(outbox[0].content, "hello from the bot");
    assert_eq!(outbox[0].recipient, "chat-42");
    assert_eq!(outbox[1].content, "second reply");
    assert_eq!(outbox[1].recipient, "chat-42");

    // Cursor file was persisted so a daemon restart wouldn't re-forward.
    let cursor_path = outbound::outbound_cursor_path(&projects_root, &bot_reg("dev-foo", "lead"));
    assert!(
        cursor_path.exists(),
        "cursor file should exist at {}",
        cursor_path.display()
    );
    let body = std::fs::read_to_string(&cursor_path).unwrap();
    let cursor: outbound::TailCursor = serde_json::from_str(&body).unwrap();
    let turns_len = std::fs::metadata(&turns_path).unwrap().len();
    assert_eq!(
        cursor.position, turns_len,
        "cursor should advance to EOF of turns.jsonl ({} bytes)",
        turns_len
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_no_op_when_turns_jsonl_missing() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    register_bot(
        "dev-bar",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "chat-99",
    )
    .unwrap();
    // Deliberately NOT creating turns.jsonl.

    let mock = Arc::new(MockChannel::new());
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(QuietAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(300)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };

    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    let outbox = mock.outbox().await;
    assert!(
        outbox.is_empty(),
        "expected no forwards when turns.jsonl missing (got {:?})",
        outbox
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
}

/// Regression: turns.jsonl truncated below the persisted cursor must
/// not produce an unbounded re-forward loop on each safety-net tick.
///
/// Before the OutboundCursor refactor, the dispatcher's `save_cursor`
/// had an in-function monotonic guard that silently dropped writes
/// smaller than the on-disk value. That guard incidentally killed
/// `drain_outboxes`'s truncation-rewind branch — the cursor could
/// never decrease, so every subsequent drain saw the stale large
/// cursor, `read_new_rows` rewound to 0 (because cursor > len),
/// re-read the post-truncation rows, re-forwarded them, and the
/// rejected `save_cursor(small)` left the cursor stale for the next
/// tick. Repeat forever → flood of duplicates on the IM channel.
///
/// Fix: `OutboundCursor::force_set` is the explicit truncation escape
/// hatch; `drain_outboxes` detects shrink via `file_len < cursor.current`
/// and calls it before forwarding. After this, the cursor sits at the
/// new EOF and subsequent ticks find nothing new.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_handles_truncation_without_repeat_forwarding() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    register_bot(
        "dev-trunc",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "chat-99",
    )
    .unwrap();

    // Seed turns.jsonl with one short post-truncation row.
    let chat_dir = projects_root.join("dev-trunc/.ccteam/chat/lead");
    std::fs::create_dir_all(&chat_dir).unwrap();
    let turns_path = chat_dir.join("turns.jsonl");
    let row = r#"{"role":"assistant","content":"post-rotation reply"}"#;
    std::fs::write(&turns_path, format!("{row}\n")).unwrap();
    let new_eof = std::fs::metadata(&turns_path).unwrap().len();

    // Seed an outbound.cursor that points well past the new EOF —
    // simulates a turns.jsonl that was much longer before and got
    // rotated / truncated while the daemon was offline.
    let cursor_path = outbound::outbound_cursor_path(&projects_root, &bot_reg("dev-trunc", "lead"));
    std::fs::write(&cursor_path, r#"{"position": 10000}"#).unwrap();
    assert!(
        new_eof < 10000,
        "test pre-condition: new EOF < stale cursor"
    );

    let mock = Arc::new(MockChannel::new());
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );
    let adapter = Arc::new(QuietAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(600)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    // The post-truncation row is forwarded EXACTLY ONCE, not on every
    // tick. (Pre-fix this would be 1 forward per safety-net pass — and
    // since the safety_net_ticker tokio::interval fires immediately on
    // first poll, even a short test run would see >= 1 duplicate.)
    let outbox = mock.outbox().await;
    assert_eq!(
        outbox.len(),
        1,
        "expected exactly 1 forward post-truncation (got {}): {:?}",
        outbox.len(),
        outbox
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );

    // Cursor should sit at the new EOF — not stuck at the stale 10000
    // and not stuck at 0 (which would re-forward next tick).
    let body = std::fs::read_to_string(&cursor_path).unwrap();
    let cursor: outbound::TailCursor = serde_json::from_str(&body).unwrap();
    assert_eq!(
        cursor.position, new_eof,
        "cursor should land at the new EOF ({} bytes)",
        new_eof
    );
}

/// Regression: pre-seeded cursor at EOF must not produce a single
/// forward on the drain pass. Models the steady-state case where the
/// fast-path dispatcher already delivered every row and persisted the
/// cursor; the safety-net drain should see "nothing new" and stay
/// quiet. Pre-fix, a race between the fast-path's per-row save and
/// the drain's batch save could rewind the cursor under the EOF and
/// produce a duplicate forward — this exercises the dedup-against-
/// current-cursor invariant from the OutboundCursor refactor.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_no_op_when_cursor_already_at_eof() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    register_bot(
        "dev-eof",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "chat-eof",
    )
    .unwrap();

    let chat_dir = projects_root.join("dev-eof/.ccteam/chat/lead");
    std::fs::create_dir_all(&chat_dir).unwrap();
    let turns_path = chat_dir.join("turns.jsonl");
    std::fs::write(
        &turns_path,
        concat!(
            r#"{"role":"assistant","content":"already delivered 1"}"#,
            "\n",
            r#"{"role":"assistant","content":"already delivered 2"}"#,
            "\n",
        ),
    )
    .unwrap();
    let eof = std::fs::metadata(&turns_path).unwrap().len();

    // Cursor pre-positioned at EOF — simulates the steady-state where
    // the fast-path dispatcher already shipped every row.
    let cursor_path = outbound::outbound_cursor_path(&projects_root, &bot_reg("dev-eof", "lead"));
    std::fs::write(&cursor_path, format!(r#"{{"position": {eof}}}"#)).unwrap();

    let mock = Arc::new(MockChannel::new());
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );
    let adapter = Arc::new(QuietAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(400)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    let outbox = mock.outbox().await;
    assert!(
        outbox.is_empty(),
        "drain must not re-forward when cursor is already at EOF (got {:?})",
        outbox
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );

    // Cursor should remain at EOF — drain saw no new rows so it didn't
    // call try_advance; the value is preserved.
    let body = std::fs::read_to_string(&cursor_path).unwrap();
    let cursor: outbound::TailCursor = serde_json::from_str(&body).unwrap();
    assert_eq!(cursor.position, eof);
}
