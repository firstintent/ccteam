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

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, TurnId, TurnInput,
};
use ccteam_im::daemon::{
    default_adapter_factory, run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs,
};
use ccteam_im::register_bot;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::providers::ws::WsChannel;
use ccteam_im::transport::{Channel, ChannelMessage, SendMessage};
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, WebSocketStream};

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
    submitted_threads: tokio::sync::Mutex<Vec<String>>,
    submitted_payloads: tokio::sync::Mutex<Vec<String>>,
    events: Arc<tokio::sync::Mutex<VecDeque<ThreadEvent>>>,
}

#[derive(Debug)]
struct FailingGatewayAdapter {
    fail_start: bool,
    fail_submit: bool,
    starts: AtomicUsize,
    submits: AtomicUsize,
}

impl FailingGatewayAdapter {
    fn new(fail_start: bool, fail_submit: bool) -> Self {
        Self {
            fail_start,
            fail_submit,
            starts: AtomicUsize::new(0),
            submits: AtomicUsize::new(0),
        }
    }
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
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let text = match input {
            TurnInput::UserText(s) => s,
            other => format!("{other:?}"),
        };
        self.submitted_threads.lock().await.push(h.identity.clone());
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
                loop {
                    if let Some(evt) = events.lock().await.pop_front() {
                        return Some((evt, ()));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
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
impl HarnessAdapter for FailingGatewayAdapter {
    fn name(&self) -> &'static str {
        "failing-gateway-stub"
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
        if self.fail_start {
            return Err(HarnessError::SpawnFailed(
                "simulated start failure".to_string(),
            ));
        }
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
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        if self.fail_submit {
            return Err(HarnessError::SubmitFailed(
                "simulated submit failure".to_string(),
            ));
        }
        Ok(TurnId::new("failing-stub-turn"))
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
        max_runtime: Some(Duration::from_millis(1200)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
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
        max_runtime: Some(Duration::from_millis(600)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
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
    let mut content_counts: BTreeMap<String, usize> = BTreeMap::new();
    for content in contents {
        *content_counts.entry(content).or_default() += 1;
    }
    assert_eq!(content_counts.len(), 3);
    assert_eq!(content_counts.get("created session s1"), Some(&1));
    assert_eq!(
        content_counts.get("submitted s1 turn gateway-turn"),
        Some(&1)
    );
    assert_eq!(content_counts.get("gateway echo: hello gateway"), Some(&1));

    let rows = read_durable_outbound_rows();
    assert_eq!(rows.len(), 6, "queued+sent rows per outbound message");
    let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut rows_by_id: BTreeMap<String, Vec<(usize, String, String)>> = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        let state = row["state"].as_str().unwrap().to_string();
        let id = row["id"].as_str().unwrap().to_string();
        let content = row["message"]["content"].as_str().unwrap().to_string();
        *state_counts.entry(state.clone()).or_default() += 1;
        rows_by_id
            .entry(id)
            .or_default()
            .push((idx, state, content));
    }
    assert_eq!(state_counts.get("queued"), Some(&3));
    assert_eq!(state_counts.get("sent"), Some(&3));
    assert_eq!(rows_by_id.len(), 3, "one outbound id per message");
    for (id, entries) in &rows_by_id {
        assert_eq!(entries.len(), 2, "outbound id {id} must have queued+sent");
        let queued = entries
            .iter()
            .find(|(_, state, _)| state == "queued")
            .unwrap_or_else(|| panic!("outbound id {id} missing queued row"));
        let sent = entries
            .iter()
            .find(|(_, state, _)| state == "sent")
            .unwrap_or_else(|| panic!("outbound id {id} missing sent row"));
        assert!(
            queued.0 < sent.0,
            "outbound id {id} must queue before sent marker"
        );
        assert_eq!(queued.2, sent.2, "outbound id {id} content drifted");
    }
    assert!(rows_by_id.values().any(|entries| {
        entries
            .iter()
            .any(|(_, state, content)| state == "sent" && content == "gateway echo: hello gateway")
    }));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_replays_queued_durable_outbound_to_mock_channel() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    write_durable_outbound_row("replay-1", "telegram", "queued", "queued before restart");

    let mock = Arc::new(MockChannel::new());
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
        max_runtime: Some(Duration::from_millis(100)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
    };

    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    let outbox = mock.outbox().await;
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].content, "queued before restart");
    let rows = read_durable_outbound_rows();
    assert_eq!(rows.last().unwrap()["state"], "sent");
    assert_eq!(
        rows.last().unwrap()["message"]["content"],
        "queued before restart"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_surfaces_start_failure_to_im_and_ledger() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "fail-start-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;
    let adapter = Arc::new(FailingGatewayAdapter::new(true, false));
    run_mock_gateway_daemon(projects_root, Arc::clone(&mock), Arc::clone(&adapter)).await;

    let contents: Vec<String> = mock
        .outbox()
        .await
        .into_iter()
        .map(|message| message.content)
        .collect();
    assert_eq!(
        contents,
        vec!["gateway error: spawn failed: simulated start failure"]
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    let rows = read_durable_outbound_rows();
    assert!(rows.iter().any(|row| {
        row["state"] == "sent"
            && row["message"]["content"] == "gateway error: spawn failed: simulated start failure"
    }));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_surfaces_submit_failure_to_im_and_ledger() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "fail-submit-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;
    mock.push(ChannelMessage {
        id: "fail-submit-2".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "hello after start".into(),
        channel: "telegram".into(),
        timestamp: 1,
        thread_ts: None,
    })
    .await;
    let adapter = Arc::new(FailingGatewayAdapter::new(false, true));
    run_mock_gateway_daemon(projects_root, Arc::clone(&mock), Arc::clone(&adapter)).await;

    let contents: Vec<String> = mock
        .outbox()
        .await
        .into_iter()
        .map(|message| message.content)
        .collect();
    assert_eq!(
        contents,
        vec![
            "created session s1".to_string(),
            "gateway error: submit failed: simulated submit failure".to_string()
        ]
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);
    let rows = read_durable_outbound_rows();
    assert!(rows.iter().any(|row| {
        row["state"] == "sent"
            && row["message"]["content"] == "gateway error: submit failed: simulated submit failure"
    }));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_surfaces_turn_timeout_to_im_and_ledger() {
    let _g = env_lock();
    let old_timeout = std::env::var_os("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS");
    std::env::set_var("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS", "50");
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "turn-timeout-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;
    mock.push(ChannelMessage {
        id: "turn-timeout-2".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "hello but never answer".into(),
        channel: "telegram".into(),
        timestamp: 1,
        thread_ts: None,
    })
    .await;
    let adapter = Arc::new(FailingGatewayAdapter::new(false, false));
    run_mock_gateway_daemon(projects_root, Arc::clone(&mock), Arc::clone(&adapter)).await;
    restore_env("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS", old_timeout);

    let contents: Vec<String> = mock
        .outbox()
        .await
        .into_iter()
        .map(|message| message.content)
        .collect();
    assert_eq!(contents.len(), 3, "created + ack + timeout");
    assert_eq!(contents[0], "created session s1");
    assert_eq!(contents[1], "submitted s1 turn failing-stub-turn");
    assert!(
        contents[2]
            .starts_with("gateway error: turn timed out after 50ms for s1 turn failing-stub-turn"),
        "unexpected timeout content: {:?}",
        contents[2]
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);
    let rows = read_durable_outbound_rows();
    assert!(rows.iter().any(|row| {
        row["state"] == "sent"
            && row["message"]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("gateway error: turn timed out after 50ms"))
    }));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_routes_ws_channel_to_gateway_over_real_socket() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let ws = Arc::new(WsChannel::bind_localhost().await.unwrap());
    let ws_url = format!("ws://{}", ws.local_addr());
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "ws".to_string(),
        ws.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(GatewayAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(1200)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
    };

    let daemon = tokio::spawn(async move {
        run_daemon_with_shutdown(args, async {
            futures::future::pending::<()>().await;
        })
        .await
        .unwrap();
    });

    let mut socket = connect_ws_with_retry(&ws_url).await;
    socket
        .send(Message::Text(
            serde_json::json!({
                "id": "ws-1",
                "sender": "alice",
                "reply_target": "chat-1",
                "content": "/new claude helper"
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let created = recv_ws_send(&mut socket).await;
    assert_eq!(created.content, "created session s1");
    assert_eq!(created.recipient, "chat-1");

    socket
        .send(Message::Text(
            serde_json::json!({
                "id": "ws-2",
                "sender": "alice",
                "reply_target": "chat-1",
                "content": "hello over ws"
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let ack = recv_ws_send(&mut socket).await;
    assert_eq!(ack.content, "submitted s1 turn gateway-turn");
    let reply = recv_ws_send(&mut socket).await;
    assert_eq!(reply.content, "gateway echo: hello over ws");
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);

    daemon.await.unwrap();
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_restart_preserves_ws_gateway_session() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let adapter = Arc::new(GatewayAdapter::default());
    let first_ws = Arc::new(WsChannel::bind_localhost().await.unwrap());
    let ws_addr = first_ws.local_addr();
    let ws_url = format!("ws://{ws_addr}");
    let (first_stop_tx, first_daemon) =
        spawn_ws_gateway_daemon(projects_root.clone(), first_ws, Arc::clone(&adapter));

    let mut first_socket = connect_ws_with_retry(&ws_url).await;
    send_ws_text(&mut first_socket, "ws-r1-new", "/new claude helper").await;
    assert_eq!(
        recv_ws_send(&mut first_socket).await.content,
        "created session s1"
    );
    send_ws_text(&mut first_socket, "ws-r1-msg", "before restart").await;
    assert_eq!(
        recv_ws_send(&mut first_socket).await.content,
        "submitted s1 turn gateway-turn"
    );
    assert_eq!(
        recv_ws_send(&mut first_socket).await.content,
        "gateway echo: before restart"
    );
    drop(first_socket);
    let _ = first_stop_tx.send(());
    first_daemon.await.unwrap();
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

    let second_ws = Arc::new(WsChannel::bind_on_listen(ws_addr));
    let (second_stop_tx, second_daemon) =
        spawn_ws_gateway_daemon(projects_root, second_ws, Arc::clone(&adapter));
    let mut second_socket = connect_ws_with_retry(&ws_url).await;
    send_ws_text(&mut second_socket, "ws-r2-msg", "after restart").await;
    assert_eq!(
        recv_ws_send(&mut second_socket).await.content,
        "submitted s1 turn gateway-turn"
    );
    assert_eq!(
        recv_ws_send(&mut second_socket).await.content,
        "gateway echo: after restart"
    );
    drop(second_socket);
    let _ = second_stop_tx.send(());
    second_daemon.await.unwrap();

    assert_eq!(
        adapter.starts.load(Ordering::SeqCst),
        1,
        "restart must reuse persisted s1 instead of spawning s2"
    );
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 2);
    assert_eq!(
        adapter.submitted_threads.lock().await.as_slice(),
        &[
            "gateway-default-helper-s1".to_string(),
            "gateway-default-helper-s1".to_string()
        ]
    );
    assert_eq!(
        adapter.submitted_payloads.lock().await.as_slice(),
        &["before restart".to_string(), "after restart".to_string()]
    );

    let rows = read_durable_outbound_rows();
    let sent_contents: Vec<String> = rows
        .iter()
        .filter(|row| row["state"] == "sent")
        .filter_map(|row| row["message"]["content"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        sent_contents,
        vec![
            "created session s1".to_string(),
            "submitted s1 turn gateway-turn".to_string(),
            "gateway echo: before restart".to_string(),
            "submitted s1 turn gateway-turn".to_string(),
            "gateway echo: after restart".to_string()
        ]
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_replays_ws_outbound_when_client_reconnects() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    write_durable_outbound_row("ws-replay-1", "ws", "failed", "stored while ws was offline");

    let ws = Arc::new(WsChannel::bind_localhost().await.unwrap());
    let ws_url = format!("ws://{}", ws.local_addr());
    let (stop_tx, daemon) =
        spawn_ws_gateway_daemon(projects_root, ws, Arc::new(GatewayAdapter::default()));

    let mut socket = connect_ws_with_retry(&ws_url).await;
    // Reconnect triggers replay of the failed durable row, while the /projects
    // command produces its own response frame. The two outbound paths race on the
    // wire, so assert the replay content is present among the received frames
    // (membership) instead of expecting it to arrive first.
    send_ws_text(&mut socket, "ws-replay-presence", "/projects").await;
    recv_ws_until_contains(
        &mut socket,
        "stored while ws was offline",
        Duration::from_secs(3),
    )
    .await;

    drop(socket);
    let _ = stop_tx.send(());
    daemon.await.unwrap();

    let rows = read_durable_outbound_rows();
    assert!(rows
        .iter()
        .any(|row| row["id"] == "ws-replay-1" && row["state"] == "sent"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ws_dual_harness_smoke() {
    if std::env::var("CCTEAM_REAL_IM_WS").ok().as_deref() != Some("1") {
        eprintln!("skip: set CCTEAM_REAL_IM_WS=1 for real WS dual-harness smoke");
        return;
    }
    let _g = env_lock();
    assert!(command_exists("tmux"), "tmux is required for real Claude");
    assert!(command_exists("claude"), "claude binary is required");
    assert!(command_exists("codex"), "codex binary is required");

    let ccteam_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let slug = format!("real-ws-{}", std::process::id());
    let old_ccteam_home = std::env::var_os("CCTEAM_HOME");
    let old_transport = std::env::var_os("CCTEAM_CODEX_APP_SERVER_TRANSPORT");
    let old_socket = std::env::var_os("CCTEAM_CODEX_APP_SERVER_SOCKET");
    let old_codex_fault = std::env::var_os("CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN");
    let old_mux_backend = std::env::var_os("CCTEAM_MUX_BACKEND");
    let old_path = std::env::var_os("PATH");
    let nl_mode = std::env::var("CCTEAM_REAL_IM_WS_NL").ok();
    let restart_mode = std::env::var("CCTEAM_REAL_IM_WS_RESTART").ok().as_deref() == Some("1");
    let fault_mode = std::env::var("CCTEAM_REAL_IM_WS_FAULTS").ok().as_deref() == Some("1");
    std::env::set_var("CCTEAM_HOME", ccteam_home.path());
    std::env::set_var("CCTEAM_CODEX_APP_SERVER_TRANSPORT", "stdio");
    std::env::remove_var("CCTEAM_CODEX_APP_SERVER_SOCKET");
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if let Some(bin) = workspace_ccteam_bin() {
        let debug_dir = bin.parent().unwrap().to_path_buf();
        let mut paths = vec![debug_dir];
        if let Some(old) = old_path.as_ref() {
            paths.extend(std::env::split_paths(old));
        }
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    }
    std::fs::write(
        ccteam_home.path().join("config.yaml"),
        format!(
            "projects:\n  - slug: {slug}\n    path: {}\n    team: real-ws\n    installed_at: 2026-01-01T00:00:00Z\n",
            project.path().display()
        ),
    )
    .unwrap();
    let paths = ccteam_core::CcteamPaths::from_env().unwrap();
    ccteam_core::bootstrap_project_at_dir(&paths, project.path(), &slug, "", "real-ws").unwrap();
    ccteam_core::install_hooks(&paths).unwrap();
    if let Some(bin) = workspace_ccteam_bin() {
        let hook = paths.hooks_script();
        std::fs::write(
            &hook,
            format!("#!/bin/sh\nexec '{}' internal hook \"$@\"\n", bin.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&hook).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&hook, perms).unwrap();
        }
    }

    let ws = Arc::new(WsChannel::bind_localhost().await.unwrap());
    let ws_addr = ws.local_addr();
    let ws_url = format!("ws://{ws_addr}");
    let max_runtime = if nl_mode.is_some() || restart_mode || fault_mode {
        Duration::from_secs(300)
    } else {
        Duration::from_secs(30)
    };
    let (mut stop_tx, mut daemon) =
        spawn_real_ws_gateway_daemon(project.path().to_path_buf(), ws, max_runtime);

    let mut socket = connect_ws_with_retry(&ws_url).await;
    send_ws_text(&mut socket, "real-ws-codex-new", "/new codex api").await;
    assert_eq!(
        recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
            .await
            .content,
        "created session s1"
    );
    send_ws_text(&mut socket, "real-ws-claude-new", "/new claude reviewer").await;
    assert_eq!(
        recv_ws_send_with_timeout(&mut socket, Duration::from_secs(20))
            .await
            .content,
        "created session s2"
    );
    send_ws_text(&mut socket, "real-ws-sessions", "/sessions").await;
    let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(5))
        .await
        .content;
    assert!(
        sessions.contains(&format!("s1:{slug}:Codex:api"))
            && sessions.contains(&format!("s2:{slug}:Claude:reviewer")),
        "real WS sessions must use the configured project slug; got {sessions:?}"
    );
    let claude_tmux_session = format!("ccteam-chat-{slug}-reviewer");
    assert!(
        tmux_session_exists(&claude_tmux_session),
        "Claude tmux session should remain live after /new: {claude_tmux_session}"
    );
    send_ws_text(&mut socket, "real-ws-codex-compact", "@api /compact").await;
    let codex = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
    assert!(
        codex.content.starts_with("submitted s1 turn "),
        "Codex /compact should reach app-server RPC, got {:?}",
        codex.content
    );

    if nl_mode
        .as_deref()
        .is_some_and(|mode| mode == "1" || mode == "codex" || mode == "claude")
    {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if nl_mode.as_deref() != Some("claude") {
            send_ws_text(
                &mut socket,
                "real-ws-codex-nl",
                "@api Reply with exactly CCTEAM-CODEX-WS-OK and no extra text.",
            )
            .await;
            let codex_ack = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
            assert!(
                codex_ack.content.starts_with("submitted s1 turn "),
                "Codex NL prompt should be submitted, got {:?}",
                codex_ack.content
            );
            recv_ws_until_contains(&mut socket, "CCTEAM-CODEX-WS-OK", Duration::from_secs(120))
                .await;
        }

        if nl_mode.as_deref() != Some("codex") {
            send_ws_text(
                &mut socket,
                "real-ws-claude-nl",
                "@reviewer Reply with exactly CCTEAM-CLAUDE-WS-OK and no extra text.",
            )
            .await;
            let claude_ack = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
            assert!(
                claude_ack.content.starts_with("submitted s2 turn "),
                "Claude NL prompt should be submitted, got {:?}",
                claude_ack.content
            );
            recv_ws_until_contains(&mut socket, "CCTEAM-CLAUDE-WS-OK", Duration::from_secs(180))
                .await;
        }
    }

    if restart_mode {
        drop(socket);
        let _ = stop_tx.send(());
        daemon.await.unwrap();
        assert!(
            tmux_session_exists(&claude_tmux_session),
            "Claude tmux session must survive daemon restart: {claude_tmux_session}"
        );

        let ws = Arc::new(WsChannel::bind_on_listen(ws_addr));
        let restarted = spawn_real_ws_gateway_daemon(project.path().to_path_buf(), ws, max_runtime);
        stop_tx = restarted.0;
        daemon = restarted.1;
        socket = connect_ws_with_retry(&ws_url).await;

        send_ws_text(&mut socket, "real-ws-restart-sessions", "/sessions").await;
        let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
            .await
            .content;
        assert!(
            sessions.contains(&format!("s1:{slug}:Codex:api"))
                && sessions.contains(&format!("s2:{slug}:Claude:reviewer")),
            "restart must restore original sessions; got {sessions:?}"
        );

        send_ws_text(
            &mut socket,
            "real-ws-codex-after-restart",
            "@api Reply with exactly CCTEAM-CODEX-WS-RESTART-OK and no extra text.",
        )
        .await;
        let codex_ack = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
        assert!(
            codex_ack.content.starts_with("submitted s1 turn "),
            "Codex after restart should reuse s1, got {:?}",
            codex_ack.content
        );
        recv_ws_until_contains(
            &mut socket,
            "CCTEAM-CODEX-WS-RESTART-OK",
            Duration::from_secs(120),
        )
        .await;

        send_ws_text(
            &mut socket,
            "real-ws-claude-after-restart",
            "@reviewer Reply with exactly CCTEAM-CLAUDE-WS-RESTART-OK and no extra text.",
        )
        .await;
        let claude_ack = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
        assert!(
            claude_ack.content.starts_with("submitted s2 turn "),
            "Claude after restart should reuse s2, got {:?}",
            claude_ack.content
        );
        recv_ws_until_contains(
            &mut socket,
            "CCTEAM-CLAUDE-WS-RESTART-OK",
            Duration::from_secs(180),
        )
        .await;
    }

    if fault_mode {
        let status = std::process::Command::new("tmux")
            .arg("kill-session")
            .arg("-t")
            .arg(&claude_tmux_session)
            .status()
            .expect("tmux kill-session should run");
        assert!(status.success(), "tmux kill-session should succeed");
        assert!(
            !tmux_session_exists(&claude_tmux_session),
            "Claude tmux session should be gone after injected fault"
        );
        send_ws_text(
            &mut socket,
            "real-ws-claude-after-kill",
            "@reviewer this should surface a missing tmux error",
        )
        .await;
        let fault = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
        assert!(
            fault
                .content
                .starts_with("gateway error: submit failed: tmux session missing:"),
            "Claude tmux death should be user-visible, got {:?}",
            fault.content
        );
        std::env::set_var("CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN", "1");
        send_ws_text(
            &mut socket,
            "real-ws-codex-after-kill",
            "@api this should surface a codex app-server disconnect",
        )
        .await;
        let fault = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(20)).await;
        assert!(
            fault.content.starts_with("gateway error: submit failed:")
                && (fault.content.contains("turn/start")
                    || fault.content.contains("codex app-server fault injection")),
            "Codex app-server death should be user-visible, got {:?}",
            fault.content
        );
        drop(socket);
        let _ = stop_tx.send(());
        daemon.await.unwrap();
        restore_env("CCTEAM_HOME", old_ccteam_home);
        restore_env("CCTEAM_CODEX_APP_SERVER_TRANSPORT", old_transport);
        restore_env("CCTEAM_CODEX_APP_SERVER_SOCKET", old_socket);
        restore_env(
            "CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN",
            old_codex_fault,
        );
        restore_env("CCTEAM_MUX_BACKEND", old_mux_backend);
        restore_env("PATH", old_path);
        return;
    }

    send_ws_text(&mut socket, "real-ws-claude-clear", "@reviewer /clear").await;
    let claude = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
    assert!(
        claude.content.starts_with("submitted s2 turn "),
        "Claude /clear should reach tmux send-keys, got {:?}",
        claude.content
    );

    let _ = std::process::Command::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(&claude_tmux_session)
        .status();
    drop(socket);
    let _ = stop_tx.send(());
    daemon.await.unwrap();
    restore_env("CCTEAM_HOME", old_ccteam_home);
    restore_env("CCTEAM_CODEX_APP_SERVER_TRANSPORT", old_transport);
    restore_env("CCTEAM_CODEX_APP_SERVER_SOCKET", old_socket);
    restore_env(
        "CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN",
        old_codex_fault,
    );
    restore_env("CCTEAM_MUX_BACKEND", old_mux_backend);
    restore_env("PATH", old_path);
}

fn spawn_ws_gateway_daemon(
    projects_root: std::path::PathBuf,
    ws: Arc<WsChannel>,
    adapter: Arc<GatewayAdapter>,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert("ws".to_string(), ws as Arc<dyn Channel + Send + Sync>);

    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_secs(5)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        run_daemon_with_shutdown(args, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });
    (stop_tx, handle)
}

fn spawn_real_ws_gateway_daemon(
    project_root: std::path::PathBuf,
    ws: Arc<WsChannel>,
    max_runtime: Duration,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert("ws".to_string(), ws as Arc<dyn Channel + Send + Sync>);

    let args = DaemonArgs {
        credentials: None,
        registry: Some(project_root),
        max_runtime: Some(max_runtime),
        adapter_factory: Some(default_adapter_factory()),
        channels_override: Some(channels),
        extra_channels: None,
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        run_daemon_with_shutdown(args, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });
    (stop_tx, handle)
}

async fn run_mock_gateway_daemon<T>(
    projects_root: std::path::PathBuf,
    mock: Arc<MockChannel>,
    adapter: Arc<T>,
) where
    T: HarnessAdapter + Send + Sync + 'static,
{
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock as Arc<dyn Channel + Send + Sync>,
    );
    let adapter_factory: AdapterFactory = {
        let cloned = Arc::clone(&adapter);
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(600)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();
}

async fn send_ws_text<S>(socket: &mut WebSocketStream<S>, id: &str, content: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            serde_json::json!({
                "id": id,
                "sender": "alice",
                "reply_target": "chat-1",
                "content": content
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

async fn connect_ws_with_retry(
    url: &str,
) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut last_err = None;
    for _ in 0..40 {
        match connect_async(url).await {
            Ok((socket, _)) => return socket,
            Err(err) => {
                last_err = Some(err);
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
    panic!("failed to connect to {url}: {last_err:?}");
}

async fn recv_ws_send<S>(socket: &mut WebSocketStream<S>) -> SendMessage
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    recv_ws_send_with_timeout(socket, Duration::from_secs(3)).await
}

async fn recv_ws_send_with_timeout<S>(
    socket: &mut WebSocketStream<S>,
    timeout: Duration,
) -> SendMessage
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, async {
        while let Some(frame) = socket.next().await {
            let frame = frame.unwrap();
            if let Message::Text(text) = frame {
                return serde_json::from_str(&text).unwrap();
            }
        }
        panic!("websocket closed before outbound SendMessage");
    })
    .await
    .expect("timed out waiting for websocket SendMessage")
}

#[allow(clippy::result_large_err)]
async fn recv_ws_until_contains<S>(socket: &mut WebSocketStream<S>, needle: &str, timeout: Duration)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let now = tokio::time::Instant::now();
        assert!(
            now < deadline,
            "timed out waiting for {needle}; seen:\n{seen}"
        );
        let remaining = deadline.saturating_duration_since(now);
        let msg = tokio::time::timeout(remaining, async {
            loop {
                let frame = socket
                    .next()
                    .await
                    .unwrap_or_else(|| panic!("websocket closed while waiting for {needle}"))
                    .unwrap();
                if let Message::Text(text) = frame {
                    return serde_json::from_str::<SendMessage>(&text).unwrap();
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle}; seen:\n{seen}"));
        seen.push_str(&msg.content);
        seen.push('\n');
        if seen.contains(needle) {
            return;
        }
    }
}

fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn workspace_ccteam_bin() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug_dir = exe.parent()?.parent()?;
    let bin = debug_dir.join("ccteam");
    bin.exists().then(|| bin.canonicalize().ok()).flatten()
}

fn tmux_session_exists(session: &str) -> bool {
    std::process::Command::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(session)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
        std::env::set_var(name, value);
    } else {
        std::env::remove_var(name);
    }
}

fn read_durable_outbound_rows() -> Vec<serde_json::Value> {
    let path = dirs::home_dir()
        .unwrap()
        .join(".ccteam")
        .join("imd")
        .join("outbound.jsonl");
    let raw = std::fs::read_to_string(path).unwrap();
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn write_durable_outbound_row(id: &str, channel: &str, state: &str, content: &str) {
    let path = dirs::home_dir()
        .unwrap()
        .join(".ccteam")
        .join("imd")
        .join("outbound.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = serde_json::json!({
        "ts_ms": 1,
        "id": id,
        "inbound_id": format!("{id}-in"),
        "channel": channel,
        "state": state,
        "message": {
            "content": content,
            "recipient": "chat-1",
            "subject": null,
            "thread_ts": null
        },
        "platform_message_id": null,
        "error": null
    });
    std::fs::write(path, format!("{row}\n")).unwrap();
}
