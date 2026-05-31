//! V0.6.1 F132 — daemon-level inbound wiring integration test.
//!
//! Before F132 the daemon spawned no `Channel::listen` task and had
//! no inbox-drain pass — Telegram messages sitting in `getUpdates`
//! were never forwarded to the bot's tmux pane. The user-chat ship
//! day discovered this on the NAS (5 unread messages, silent bot).
//!
//! This integration test stitches together the production wiring
//! without a real network round-trip:
//!
//!   `MockChannel` → daemon listener task → mpsc consumer →
//!   `process_inbound_admin_aware` → mailbox `.md` file →
//!   `drain_inboxes` → `BotSupervisor::handle_inbound` →
//!   stub `HarnessAdapter::submit_turn`.
//!
//! Two assertions guard the regression:
//! 1. The mailbox `.md` file appears under
//!    `<projects_root>/<slug>/.ccteam/chat/<role>/inbox/`.
//! 2. The stub adapter's `submit_turn` counter advances (proves the
//!    supervisor inbox drain reached the harness layer).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, TurnId, TurnInput,
};
use ccteam_im::daemon::{run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs};
use ccteam_im::register_bot;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::{Channel, ChannelMessage};
use futures::stream::BoxStream;
use tempfile::TempDir;

// ----- env isolation helpers (mirrors tests/daemon_test.rs) ---------

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

// ----- stub adapter — records submit_turn -----------------------------

#[derive(Debug, Default)]
struct StubAdapter {
    starts: AtomicUsize,
    submits: AtomicUsize,
    closes: AtomicUsize,
    submitted_payloads: tokio::sync::Mutex<Vec<String>>,
}

#[derive(Debug, Default)]
struct GatewayAdapter {
    starts: AtomicUsize,
    submits: AtomicUsize,
    submitted_payloads: tokio::sync::Mutex<Vec<String>>,
    events: Arc<tokio::sync::Mutex<VecDeque<ThreadEvent>>>,
}

#[async_trait]
impl HarnessAdapter for GatewayAdapter {
    fn name(&self) -> &'static str {
        "gateway-stub"
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
            identity: format!("gateway-{}-{}-{}", ctx.slug, spec.role, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }

    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let text = match input {
            TurnInput::UserText(s) => s,
            other => format!("{other:?}"),
        };
        self.submitted_payloads.lock().await.push(text.clone());
        self.events
            .lock()
            .await
            .push_back(ThreadEvent::ItemCompleted {
                item: ThreadItem {
                    id: "gateway-msg-1".to_string(),
                    details: ThreadItemDetails::AgentMessage(format!("gateway echo: {text}")),
                },
            });
        Ok(TurnId::new("gateway-turn"))
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let events = Arc::clone(&self.events);
        Box::pin(futures::stream::unfold((), move |_| {
            let events = Arc::clone(&events);
            async move {
                let evt = events.lock().await.pop_front()?;
                Some((evt, ()))
            }
        }))
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

#[async_trait]
impl HarnessAdapter for StubAdapter {
    fn name(&self) -> &'static str {
        "f132-stub"
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
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        if let TurnInput::UserText(s) = input {
            self.submitted_payloads.lock().await.push(s);
        }
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

/// Smoke-test the full F132 daemon path with a MockChannel pre-seeded
/// with one `@<role>` message. Mailbox file must appear during the
/// daemon's lifetime and submit_turn must fire before max_runtime
/// expires.
///
/// Held lint: `await_holding_lock` doesn't apply on the
/// `current_thread` runtime we use here (the task cannot migrate).
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_wires_mock_channel_to_supervisor_inbox() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    // Register one bot ("lead" role on slug "dev-foo") so list_bots()
    // returns it and the router can resolve @lead.
    register_bot("dev-foo", "lead", AgentVendor::Claude, "telegram", "chat-1").unwrap();

    // Seed the MockChannel with one @lead message. We tag it as a
    // "telegram" inbound so the three-layer ACL (which is keyed on
    // platform) treats this as an open allowlist — the daemon's
    // platform-name routing is independent of the underlying transport
    // impl (MockChannel impersonates a Telegram channel in the test).
    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "msg-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "@lead please look at this".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;

    // Inject the MockChannel through the daemon args. Key matches
    // ChannelMessage::channel so the consumer can route admin-reply
    // sends back through the same transport.
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    // Stub adapter the supervisor uses for every spawn.
    let adapter = Arc::new(StubAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        // Short tick so the supervisor spawns + inbox drains within
        // the test budget. Two ticks are enough: first tick spawns
        // the supervisor (no handle yet → `Spawn`), second tick (or
        // first if the mailbox file is already there) drains the inbox.
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(1200)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };

    run_daemon_with_shutdown(args, async {
        // shutdown is also bounded by max_runtime; this future never
        // fires (test runtime ends via max_runtime).
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    // Supervisor was started at least once.
    assert!(
        adapter.starts.load(Ordering::SeqCst) >= 1,
        "stub adapter start_thread should fire at least once (got {})",
        adapter.starts.load(Ordering::SeqCst)
    );

    // submit_turn fired with the stripped payload (router strips
    // `@lead ` → `please look at this`).
    assert_eq!(
        adapter.submits.load(Ordering::SeqCst),
        1,
        "submit_turn must run exactly once for the one inbound message"
    );
    let submitted = adapter.submitted_payloads.lock().await.clone();
    assert_eq!(submitted, vec!["please look at this".to_string()]);

    // After drain the inbox dir is empty (one-shot semantics).
    let inbox = projects_root
        .join("dev-foo")
        .join(".ccteam")
        .join("chat")
        .join("lead")
        .join("inbox");
    if inbox.exists() {
        let remaining: Vec<_> = std::fs::read_dir(&inbox)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
            .collect();
        assert!(
            remaining.is_empty(),
            "drain pass should remove dispatched envelopes (got {} leftover)",
            remaining.len()
        );
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_routes_gateway_inbound_to_submit_turn_and_outbound() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "gw-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;
    mock.push(ChannelMessage {
        id: "gw-2".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "hello gateway".into(),
        channel: "telegram".into(),
        timestamp: 1,
        thread_ts: None,
    })
    .await;

    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(GatewayAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
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

    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);
    assert_eq!(
        adapter.submitted_payloads.lock().await.as_slice(),
        &["hello gateway".to_string()]
    );

    let outbox = mock.outbox().await;
    let contents: Vec<String> = outbox.into_iter().map(|m| m.content).collect();
    assert_eq!(
        contents,
        vec![
            "created session s1".to_string(),
            "gateway echo: hello gateway".to_string()
        ]
    );
}
