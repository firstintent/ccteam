//! V0.6.0 Wave 3 e2e wiring — mock-IM integration test.
//!
//! Stitches every Wave 2 + Wave 3 ccteam-imd component together
//! without a real tmux process: MockChannel → inbound pipeline →
//! mailbox file → BotSupervisor + stub HarnessAdapter →
//! simulated `turns.jsonl` append → outbound tailer → MockChannel
//! outbox.
//!
//! Real-tmux + real-claude validation is Wave 4 host-probe (a user
//! pastes a Telegram bot token and we drive a live session); this
//! suite proves the wiring logic.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_imd::inbound::{process_inbound, DefaultMailboxResolver, InboundOutcome};
use ccteam_imd::outbound::{forward_new_rows, read_new_rows, TailCursor};
use ccteam_imd::router::HandleMap;
use ccteam_imd::supervisor::BotSupervisor;
use ccteam_imd::three_layer_sec::ThreeLayerSec;
use ccteam_imd::transport::providers::mock::MockChannel;
use ccteam_imd::transport::{Channel, ChannelMessage};
use ccteam_imd::BotRegistration;
use futures::stream::BoxStream;
use tempfile::TempDir;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------
// Stub adapter — pretends to be ClaudeTuiAdapter, records calls, and
// writes the "assistant reply" into turns.jsonl so the outbound tailer
// has something to forward.
// ---------------------------------------------------------------------

#[derive(Debug, Default)]
struct StubAdapter {
    starts: AtomicUsize,
    submits: AtomicUsize,
    closes: AtomicUsize,
    /// Where to mirror the assistant reply (set in `start_thread`).
    turns_path: tokio::sync::Mutex<Option<std::path::PathBuf>>,
    /// Per-submit canned assistant reply (defaults to echo of input).
    canned_reply: Option<String>,
}

impl StubAdapter {
    fn new() -> Self {
        Self::default()
    }
    fn with_canned_reply(reply: impl Into<String>) -> Self {
        Self {
            canned_reply: Some(reply.into()),
            ..Self::default()
        }
    }
}

#[async_trait]
impl HarnessAdapter for StubAdapter {
    fn name(&self) -> &'static str {
        "stub-claude-tui"
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
        let chat_dir = ctx
            .project_dir
            .join(".ccteam")
            .join("chat")
            .join(&spec.role);
        std::fs::create_dir_all(&chat_dir).unwrap();
        let path = chat_dir.join("turns.jsonl");
        *self.turns_path.lock().await = Some(path.clone());
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
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        // Simulate the real TUI adapter's turns_mirror append: drop
        // one `assistant` row that the outbound tailer will pick up.
        let user_text = match input {
            TurnInput::UserText(s) => s,
            other => format!("{other:?}"),
        };
        let reply = self
            .canned_reply
            .clone()
            .unwrap_or_else(|| format!("echo: {user_text}"));
        if let Some(path) = self.turns_path.lock().await.as_ref() {
            use std::io::Write as _;
            let row = serde_json::json!({
                "role": "assistant",
                "content": reply,
            });
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(f, "{row}").unwrap();
        }
        Ok(TurnId::new(format!(
            "stub-turn-{}",
            self.submits.load(Ordering::SeqCst)
        )))
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

// ---------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------

fn reg(slug: &str, role: &str) -> BotRegistration {
    BotRegistration {
        workflow_slug: slug.into(),
        role: role.into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "mock".into(),
        im_chat_id: format!("chat-{slug}-{role}"),
        chat_handle: None,
        created_at: chrono::Utc::now(),
    }
}

fn im_msg(payload: &str) -> ChannelMessage {
    ChannelMessage {
        id: "im-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: payload.into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    }
}

async fn drain_inbox_into_supervisor(
    projects_root: &Path,
    slug: &str,
    role: &str,
    sup: &BotSupervisor,
) -> usize {
    let inbox = projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("inbox");
    if !inbox.exists() {
        return 0;
    }
    let mut count = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let body = std::fs::read_to_string(entry.path()).unwrap();
        let env = ccteam_imd::inbound::parse_envelope(&body).unwrap();
        sup.handle_inbound(env.payload).await.unwrap();
        std::fs::remove_file(entry.path()).unwrap();
        count += 1;
    }
    count
}

// ---------------------------------------------------------------------
// 1. Happy path — IM in → bot replies → IM out.
// ---------------------------------------------------------------------

#[tokio::test]
async fn happy_path_im_to_bot_to_im() {
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let slug = "dev-foo";
    let role = "lead";

    // Wire inbound: write @lead message to mailbox.
    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(Default::default())));
    let mailbox = DefaultMailboxResolver::with_projects_root(projects_root.clone());
    let mut handles = HandleMap::new();
    handles.insert("lead", slug, role);

    let res = process_inbound(&im_msg("@lead what's up?"), &sec, &handles, &mailbox, 0, 1)
        .await
        .unwrap();
    assert!(matches!(res, InboundOutcome::DroppedToBot { .. }));

    // Wire supervisor + adapter; start the thread.
    let adapter = Arc::new(StubAdapter::new());
    let sup = BotSupervisor::new(reg(slug, role), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

    // Drain mailbox → submit_turn (the daemon's per-tick job).
    let drained = drain_inbox_into_supervisor(&projects_root, slug, role, &sup).await;
    assert_eq!(drained, 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);

    // Outbound: tail turns.jsonl, push assistant rows to MockChannel.
    let turns_path = projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("turns.jsonl");
    let (rows, _) = read_new_rows(&turns_path, &TailCursor::default()).unwrap();
    assert_eq!(rows.len(), 1, "stub adapter wrote one assistant row");

    let channel = MockChannel::new();
    let sent = forward_new_rows(&rows, &channel, "alice", &[]).await;
    assert_eq!(sent, 1);
    let out = channel.outbox().await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].content, "echo: what's up?");
    assert_eq!(out[0].recipient, "alice");
}

// ---------------------------------------------------------------------
// 2. Error recovery — adapter fails first start, restart() recovers.
// ---------------------------------------------------------------------

#[tokio::test]
async fn restart_recovers_from_close_failure() {
    // Adapter that succeeds start but `close_thread` returns Err on
    // first call (simulating a dead tmux session). restart() must
    // proceed past the close failure and call start_thread again.
    #[derive(Debug, Default)]
    struct FlakyCloseAdapter {
        starts: AtomicUsize,
        closes: AtomicUsize,
    }
    #[async_trait]
    impl HarnessAdapter for FlakyCloseAdapter {
        fn name(&self) -> &'static str {
            "flaky-close"
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
                identity: format!("flaky-{}-{}", ctx.slug, spec.role),
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
            let n = self.closes.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(HarnessError::ShutdownFailed("simulated".into()))
            } else {
                Ok(())
            }
        }
    }
    let tmp = TempDir::new().unwrap();
    let adapter = Arc::new(FlakyCloseAdapter::default());
    let sup = BotSupervisor::new(reg("dev-foo", "lead"), tmp.path(), adapter.clone());
    sup.ensure_started().await.unwrap();
    sup.restart().await.unwrap();
    assert_eq!(
        adapter.starts.load(Ordering::SeqCst),
        2,
        "restart starts again"
    );
    assert_eq!(
        adapter.closes.load(Ordering::SeqCst),
        1,
        "close attempted once"
    );
    assert!(sup.is_started().await);
}

// ---------------------------------------------------------------------
// 3. close_thread cleanup — shutdown is idempotent + the supervisor
//    refuses to handle inbound after shutdown.
// ---------------------------------------------------------------------

#[tokio::test]
async fn close_thread_cleanup_idempotent() {
    let tmp = TempDir::new().unwrap();
    let adapter = Arc::new(StubAdapter::new());
    let sup = BotSupervisor::new(reg("dev-foo", "lead"), tmp.path(), adapter.clone());
    sup.ensure_started().await.unwrap();
    assert!(sup.is_started().await);
    sup.shutdown().await.unwrap();
    sup.shutdown().await.unwrap();
    sup.shutdown().await.unwrap();
    assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
    assert!(!sup.is_started().await);
    // handle_inbound should refuse after shutdown (no handle).
    assert!(sup.handle_inbound("ignored".into()).await.is_err());
}

// ---------------------------------------------------------------------
// 4. Multi-bot parallel — two supervisors using independent adapters
//    run side by side, each draining its own inbox into its own turns
//    file. Verifies no shared-state crosstalk.
// ---------------------------------------------------------------------

#[tokio::test]
async fn multi_bot_parallel() {
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();

    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(Default::default())));
    let mailbox = DefaultMailboxResolver::with_projects_root(projects_root.clone());
    let mut handles = HandleMap::new();
    handles.insert("lead", "dev-foo", "lead");
    handles.insert("ops", "dev-bar", "ops");

    // Two bots in two different slugs.
    let adapter_lead = Arc::new(StubAdapter::with_canned_reply("LEAD-OK"));
    let adapter_ops = Arc::new(StubAdapter::with_canned_reply("OPS-OK"));
    let sup_lead = Arc::new(BotSupervisor::new(
        reg("dev-foo", "lead"),
        projects_root.clone(),
        adapter_lead.clone(),
    ));
    let sup_ops = Arc::new(BotSupervisor::new(
        reg("dev-bar", "ops"),
        projects_root.clone(),
        adapter_ops.clone(),
    ));
    sup_lead.ensure_started().await.unwrap();
    sup_ops.ensure_started().await.unwrap();

    // Three messages: 2 for lead, 1 for ops.
    let mut seq = 0_u64;
    for content in ["@lead one", "@lead two", "@ops three"] {
        seq += 1;
        let outcome = process_inbound(&im_msg(content), &sec, &handles, &mailbox, 0, seq)
            .await
            .unwrap();
        assert!(matches!(outcome, InboundOutcome::DroppedToBot { .. }));
    }

    // Drain each bot's mailbox in parallel.
    let (n_lead, n_ops) = tokio::join!(
        drain_inbox_into_supervisor(&projects_root, "dev-foo", "lead", &sup_lead),
        drain_inbox_into_supervisor(&projects_root, "dev-bar", "ops", &sup_ops),
    );
    assert_eq!(n_lead, 2);
    assert_eq!(n_ops, 1);
    assert_eq!(adapter_lead.submits.load(Ordering::SeqCst), 2);
    assert_eq!(adapter_ops.submits.load(Ordering::SeqCst), 1);

    // Each bot's turns.jsonl has its own assistant rows; outbound
    // forwards to bot-specific MockChannels.
    let lead_turns = projects_root.join("dev-foo/.ccteam/chat/lead/turns.jsonl");
    let ops_turns = projects_root.join("dev-bar/.ccteam/chat/ops/turns.jsonl");
    let (lead_rows, _) = read_new_rows(&lead_turns, &TailCursor::default()).unwrap();
    let (ops_rows, _) = read_new_rows(&ops_turns, &TailCursor::default()).unwrap();
    assert_eq!(lead_rows.len(), 2);
    assert_eq!(ops_rows.len(), 1);
    assert!(lead_rows.iter().all(|r| r.content == "LEAD-OK"));
    assert_eq!(ops_rows[0].content, "OPS-OK");

    // Separate channels — no crosstalk.
    let ch_lead = MockChannel::new();
    let ch_ops = MockChannel::new();
    let sent_lead = forward_new_rows(&lead_rows, &ch_lead, "alice", &[]).await;
    let sent_ops = forward_new_rows(&ops_rows, &ch_ops, "bob", &[]).await;
    assert_eq!(sent_lead, 2);
    assert_eq!(sent_ops, 1);
    assert_eq!(ch_lead.outbox().await.len(), 2);
    assert_eq!(ch_ops.outbox().await.len(), 1);
}

// ---------------------------------------------------------------------
// 5. End-to-end echo verifies the MockChannel listen() → mpsc path
//    used by the production daemon is type-compatible with the rest
//    of the pipeline (channel API surface gate).
// ---------------------------------------------------------------------

#[tokio::test]
async fn channel_listen_to_inbound_pipeline() {
    let tmp = TempDir::new().unwrap();
    let projects_root = tmp.path().to_path_buf();
    let slug = "dev-foo";
    let role = "lead";

    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(Default::default())));
    let mailbox = DefaultMailboxResolver::with_projects_root(projects_root.clone());
    let mut handles = HandleMap::new();
    handles.insert("lead", slug, role);

    // Push two messages into the MockChannel's inbox before listen.
    let ch = MockChannel::new();
    for s in ["@lead first", "@lead second"] {
        ch.push(ChannelMessage {
            id: format!("id-{s}"),
            sender: "alice".into(),
            reply_target: "alice".into(),
            content: s.into(),
            channel: "telegram".into(),
            timestamp: 0,
            thread_ts: None,
        })
        .await;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(8);
    ch.listen(tx).await.unwrap();

    // Pump receiver through the inbound pipeline.
    let mut count = 0_u64;
    while let Ok(msg) = rx.try_recv() {
        count += 1;
        let outcome = process_inbound(&msg, &sec, &handles, &mailbox, 0, count)
            .await
            .unwrap();
        assert!(matches!(outcome, InboundOutcome::DroppedToBot { .. }));
    }
    assert_eq!(count, 2);

    let adapter = Arc::new(StubAdapter::new());
    let sup = BotSupervisor::new(reg(slug, role), projects_root.clone(), adapter.clone());
    sup.ensure_started().await.unwrap();
    let drained = drain_inbox_into_supervisor(&projects_root, slug, role, &sup).await;
    assert_eq!(drained, 2);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 2);
}
