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
//!   `Gateway::handle_message` → stub `HarnessAdapter::submit_turn`.
//!
//! The stub adapter's `submit_turn` counter advancing proves an
//! inbound IM message reached the harness layer.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, ThreadStatus,
    TurnId, TurnInput,
};
use ccteam_im::daemon::{
    default_adapter_factory, run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs,
};
use ccteam_im::gateway::{GatewayEvent, GatewayEventKind};
use ccteam_im::register_bot;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::providers::ws::WsChannel;
use ccteam_im::transport::{
    AttachmentKind, Channel, ChannelAttachment, ChannelMessage, OutboundFile, OutboundFileKind,
    SendMessage,
};
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
    // CCTEAM_HOME wins over HOME in the root resolvers; a shell that
    // exports it would redirect every "isolated" write back into the
    // REAL ~/.ccteam. Pin both.
    std::env::set_var("CCTEAM_HOME", tmp.path().join(".ccteam"));
    tmp
}

fn seed_role(project_dir: &std::path::Path, role: &str) {
    let agents_dir = project_dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    let model = if role == "reviewer" {
        "model: sonnet\n"
    } else {
        ""
    };
    std::fs::write(
        agents_dir.join(format!("{role}.md")),
        format!(
            "---\nname: {role}\n{model}---\n\
             You are a ccteam real-machine smoke-test role.\n\
             When the user asks you to reply with exactly a token, output only that token.\n\
             Do not inspect files or add explanation for smoke-test token prompts.\n"
        ),
    )
    .unwrap();
}

fn created_session_receipt(sid: &str) -> String {
    format!("created session {sid}\n↓ 查看状态 → /status")
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
    /// How many times the gateway asked this session to rebuild its ccteam
    /// tool face — the first-activation priming must be exactly once per
    /// session, not once per turn.
    tool_face_rebuilds: AtomicUsize,
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

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: ccteam_harness::TurnRouting,
    ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(ccteam_harness::TurnSubmission::started)
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ccteam_harness::ToolSurfaceRebuild, HarnessError> {
        self.tool_face_rebuilds.fetch_add(1, Ordering::SeqCst);
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "stub: only a respawn reapplies the curated config; send /new".into(),
        })
    }

    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        // Scripted test stream: one-shot. Re-attaching would replay
        // the script, which is exactly what `Rebuildable` forbids.
        ccteam_harness::EventAttachment::OneShot
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

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: ccteam_harness::TurnRouting,
    ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(ccteam_harness::TurnSubmission::started)
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ccteam_harness::ToolSurfaceRebuild, HarnessError> {
        // Test double: no tool face to rebuild.
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }

    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        // Scripted test stream: one-shot. Re-attaching would replay
        // the script, which is exactly what `Rebuildable` forbids.
        ccteam_harness::EventAttachment::OneShot
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
    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: ccteam_harness::TurnRouting,
    ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(ccteam_harness::TurnSubmission::started)
    }
    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ccteam_harness::ToolSurfaceRebuild, HarnessError> {
        // Test double: no tool face to rebuild.
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }

    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        // Scripted test stream: one-shot. Re-attaching would replay
        // the script, which is exactly what `Rebuildable` forbids.
        ccteam_harness::EventAttachment::OneShot
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
        attachments: Vec::new(),
        selection: None,
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        max_runtime: Some(Duration::from_millis(1200)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
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
        attachments: Vec::new(),
        selection: None,
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
        attachments: Vec::new(),
        selection: None,
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(600)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
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

    // v0.8.23 review §3.2-5 — a focused IM answer now carries the compact
    // "→ slug/sid (role)" context echo suffix; the session's independent
    // v0.8.22 P1 auto-title (from its first user message) can ALSO land on
    // the same meta.json and append an OPTIONAL `「title」` tag after it, so
    // match the deterministic prefix rather than betting on whether that
    // race won.
    let echo = "gateway echo: hello gateway\n\n→ default/s1 (helper)";

    let outbox = mock.outbox().await;
    let contents: Vec<String> = outbox.into_iter().map(|m| m.content).collect();
    let mut content_counts: BTreeMap<String, usize> = BTreeMap::new();
    for content in contents {
        *content_counts.entry(content).or_default() += 1;
    }
    // V0.8.4 P1 (F1): the "submitted … turn …" ack is folded away on the
    // async-pump path — a turn delivers only `created session` (the /new
    // command reply) + the answer. GatewayAdapter emits no tool events, so
    // there is no progress seed either.
    assert_eq!(content_counts.len(), 2, "got {content_counts:?}");
    assert_eq!(content_counts.get(&created_session_receipt("s1")), Some(&1));
    let echo_matches = content_counts
        .keys()
        .filter(|c| c.starts_with(echo))
        .count();
    assert_eq!(echo_matches, 1, "got {content_counts:?}");
    assert_eq!(
        content_counts.get("submitted s1 turn gateway-turn"),
        None,
        "machine-ish ack must be folded away"
    );

    let rows = read_durable_outbound_rows();
    assert_eq!(
        rows.len(),
        4,
        "queued+sent rows per outbound message (no ack)"
    );
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
    assert_eq!(state_counts.get("queued"), Some(&2));
    assert_eq!(state_counts.get("sent"), Some(&2));
    assert_eq!(rows_by_id.len(), 2, "one outbound id per message");
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
            .any(|(_, state, content)| state == "sent" && content.starts_with(echo))
    }));
}

/// V0.8.4 P0 — a gateway reply that overflows the channel's
/// `max_message_len` is split into ordered durable sub-messages. Built on
/// `daemon_routes_gateway_inbound_to_submit_turn_and_outbound`, but with a
/// `MockChannel` that declares a 40-unit ceiling so the async echo splits
/// (the short "created session"/"submitted" acks stay single).
///
/// Ledger assertions are **multiset + pairing** (every id carries its own
/// Queued+Sent, paired by id, ordered only *within* an id) — never
/// positional across logical messages, since the sync ack and the async
/// echo race (PRD §4.2 / the v8.2 flake).
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_splits_long_outbound_into_ordered_parts() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let long_input = "please echo this fairly long sentence back to me intact";
    let mock = Arc::new(MockChannel::new().with_max_message_len(40));
    mock.push(ChannelMessage {
        id: "gw-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    })
    .await;
    mock.push(ChannelMessage {
        id: "gw-2".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: long_input.into(),
        channel: "telegram".into(),
        timestamp: 1,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(900)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    // v0.8.23 review §3.2-5 — a focused IM answer now carries the compact
    // "→ slug/sid (role)" context echo suffix (folded into the split budget
    // just like the rest of the reply text).
    let echo = format!("gateway echo: {long_input}\n\n→ default/s1 (helper)");

    // ---- ledger: multiset + pairing (no cross-message ordering) ----
    let rows = read_durable_outbound_rows();
    let mut rows_by_id: BTreeMap<String, Vec<(usize, String, String)>> = BTreeMap::new();
    let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        let id = row["id"].as_str().unwrap().to_string();
        let state = row["state"].as_str().unwrap().to_string();
        let content = row["message"]["content"].as_str().unwrap().to_string();
        *state_counts.entry(state.clone()).or_default() += 1;
        rows_by_id
            .entry(id)
            .or_default()
            .push((idx, state, content));
    }
    // Happy path: every Queued has a paired Sent, no Failed.
    assert_eq!(state_counts.get("failed"), None, "no failures expected");
    assert_eq!(
        state_counts.get("queued"),
        state_counts.get("sent"),
        "every queued part must reach sent"
    );
    for (id, entries) in &rows_by_id {
        let queued = entries
            .iter()
            .find(|(_, s, _)| s == "queued")
            .unwrap_or_else(|| panic!("id {id} missing queued row"));
        let sent = entries
            .iter()
            .find(|(_, s, _)| s == "sent")
            .unwrap_or_else(|| panic!("id {id} missing sent row"));
        assert!(queued.0 < sent.0, "id {id}: queued must precede sent");
        assert_eq!(queued.2, sent.2, "id {id}: content drifted queued→sent");
    }

    // ---- the echo specifically split into ≥2 ordered parts ---------
    // The async echo flows through the event pump ⇒ inbound_id starts
    // "gateway-event-"; its split parts are `…-0-<part>`.
    let mut echo_parts: Vec<(usize, String)> = rows
        .iter()
        .filter_map(|row| {
            if row["state"].as_str()? != "sent" {
                return None;
            }
            if !row["inbound_id"].as_str()?.starts_with("gateway-event-") {
                return None;
            }
            let part_idx: usize = row["id"].as_str()?.rsplit('-').next()?.parse().ok()?;
            Some((part_idx, row["message"]["content"].as_str()?.to_string()))
        })
        .collect();
    echo_parts.sort_by_key(|(idx, _)| *idx);
    assert!(
        echo_parts.len() >= 2,
        "echo must split into ordered parts (got {})",
        echo_parts.len()
    );
    for (_, content) in &echo_parts {
        assert!(
            content.chars().map(char::len_utf16).sum::<usize>() <= 40,
            "split part exceeds 40-unit budget: {content:?}"
        );
    }
    // v0.8.22 P1's auto-title (from the session's first user message) can
    // independently land on this same session's meta.json, appending an
    // OPTIONAL `「title」` tag after the context echo — orthogonal to this
    // test's splitting behavior, so match the deterministic prefix rather
    // than betting on whether that race won. `reconstructed` itself (not a
    // hand-computed guess) is then the source of truth for the outbox
    // cross-check below.
    let reconstructed: String = echo_parts.iter().map(|(_, c)| c.as_str()).collect();
    assert!(
        reconstructed.starts_with(&echo),
        "ordered parts must concatenate to the echo (+ optional title tag): {reconstructed:?}"
    );

    // ---- the mock outbox carries the same ordered parts ------------
    let outbox = mock.outbox().await;
    let echo_outbox: Vec<String> = outbox
        .into_iter()
        .map(|m| m.content)
        .filter(|c| !c.is_empty() && c != &reconstructed && reconstructed.contains(c.as_str()))
        .collect();
    assert_eq!(
        echo_outbox.concat(),
        reconstructed,
        "outbox echo parts must reconstruct"
    );
}

/// V0.8.4 P0 — when a split part fails to send, the daemon surfaces one
/// `⚠️` notice back to the chat (no silent partial delivery) and records
/// a `Failed` ledger row. The failing channel keys on content
/// (`"intact"`, which lands in the echo's final part) so the test is
/// immune to the sync-ack / async-echo send ordering.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_split_failure_surfaces_notice() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let long_input = "please echo this fairly long sentence back to me intact";
    let mock = Arc::new(
        MockChannel::new()
            .with_max_message_len(40)
            .failing_on_content(&["intact"]),
    );
    mock.push(ChannelMessage {
        id: "gw-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    })
    .await;
    mock.push(ChannelMessage {
        id: "gw-2".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: long_input.into(),
        channel: "telegram".into(),
        timestamp: 1,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(900)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    // The final echo part (carrying "intact") failed → a ⚠️ notice was
    // delivered to the same chat.
    let outbox = mock.outbox().await;
    assert!(
        outbox.iter().any(|m| m.content.starts_with("⚠️")),
        "expected a split-failure notice, got {:?}",
        outbox
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
    // And the ledger recorded the failed part.
    let rows = read_durable_outbound_rows();
    assert!(
        rows.iter().any(|r| r["state"] == "failed"),
        "expected a failed ledger row for the undelivered part"
    );
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(100)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
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
async fn daemon_replays_queued_durable_outbound_idempotently_once() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    write_durable_outbound_row(
        "replay-idem-1",
        "telegram",
        "queued",
        "queued exactly once across restarts",
    );

    let first_mock = Arc::new(MockChannel::new());
    let mut first_channels: ChannelMap = std::collections::HashMap::new();
    first_channels.insert(
        "telegram".to_string(),
        first_mock.clone() as Arc<dyn Channel + Send + Sync>,
    );
    let adapter = Arc::new(GatewayAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    run_daemon_with_shutdown(
        DaemonArgs {
            credentials: None,
            registry: Some(projects_root.clone()),
            max_runtime: Some(Duration::from_millis(100)),
            adapter_factory: Some(adapter_factory),
            channels_override: Some(first_channels),
            extra_channels: None,
            ..Default::default()
        },
        async {
            futures::future::pending::<()>().await;
        },
    )
    .await
    .unwrap();

    let first_outbox = first_mock.outbox().await;
    assert_eq!(first_outbox.len(), 1);
    assert_eq!(
        first_outbox[0].content,
        "queued exactly once across restarts"
    );

    let second_mock = Arc::new(MockChannel::new());
    let mut second_channels: ChannelMap = std::collections::HashMap::new();
    second_channels.insert(
        "telegram".to_string(),
        second_mock.clone() as Arc<dyn Channel + Send + Sync>,
    );
    let second_adapter = Arc::new(GatewayAdapter::default());
    let second_factory: AdapterFactory = {
        let cloned = second_adapter.clone();
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    run_daemon_with_shutdown(
        DaemonArgs {
            credentials: None,
            registry: Some(projects_root),
            max_runtime: Some(Duration::from_millis(100)),
            adapter_factory: Some(second_factory),
            channels_override: Some(second_channels),
            extra_channels: None,
            ..Default::default()
        },
        async {
            futures::future::pending::<()>().await;
        },
    )
    .await
    .unwrap();

    assert!(
        second_mock.outbox().await.is_empty(),
        "a durable row whose latest state is sent must not replay again"
    );
    let rows = read_durable_outbound_rows();
    let sent_count = rows
        .iter()
        .filter(|row| row["id"] == "replay-idem-1" && row["state"] == "sent")
        .count();
    assert_eq!(sent_count, 1, "replay must append exactly one sent row");
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
        attachments: Vec::new(),
        selection: None,
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
    let expected =
        "会话启动失败: simulated start failure。下一步: 请检查项目和角色后重试 /new；如果仍失败，重启 ccteam start 后再试。";
    assert_eq!(contents, vec![expected.to_string()]);
    assert!(!contents[0].contains("gateway error"));
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    let rows = read_durable_outbound_rows();
    assert!(rows
        .iter()
        .any(|row| { row["state"] == "sent" && row["message"]["content"] == expected }));
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
        attachments: Vec::new(),
        selection: None,
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
        attachments: Vec::new(),
        selection: None,
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
    let expected =
        "发送失败: simulated submit failure。下一步: 请重试；如果仍失败，发送 /sessions 确认会话还在，或重新 /new。";
    assert_eq!(
        contents,
        vec![created_session_receipt("s1"), expected.to_string()]
    );
    assert!(!contents[1].contains("gateway error"));
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);
    let rows = read_durable_outbound_rows();
    assert!(rows
        .iter()
        .any(|row| { row["state"] == "sent" && row["message"]["content"] == expected }));
}

/// A child session is driven by its PARENT agent, so nobody is there to type
/// `/mcp` into it — the tool face is therefore checked automatically, at the
/// moment the session first does work under this daemon. EXACTLY once per
/// session: the probe is a per-turn cost otherwise, and for the vendors that
/// once answered it in place it also tore down a healthy MCP client per
/// message. Held after the in-place rebuild was withdrawn: "once per session"
/// is the property, whatever the adapter answers.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_activation_probes_the_tool_face_once_per_session() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    for (id, content, ts) in [
        ("prime-1", "/new claude helper", 0),
        ("prime-2", "first task", 1),
        ("prime-3", "second task", 2),
    ] {
        mock.push(ChannelMessage {
            id: id.into(),
            sender: "alice".into(),
            reply_target: "chat-1".into(),
            content: content.into(),
            channel: "telegram".into(),
            timestamp: ts,
            thread_ts: None,
            attachments: Vec::new(),
            selection: None,
        })
        .await;
    }
    let adapter = Arc::new(GatewayAdapter::default());
    run_mock_gateway_daemon(projects_root, Arc::clone(&mock), Arc::clone(&adapter)).await;

    assert_eq!(
        adapter.submits.load(Ordering::SeqCst),
        2,
        "both tasks must reach the vendor"
    );
    assert_eq!(
        adapter.tool_face_rebuilds.load(Ordering::SeqCst),
        1,
        "the tool face is probed on first activation and never again"
    );
}

/// The 2026-08-09 attachment double: a `Rebuildable` adapter whose FIRST
/// `events()` stream ends while the session stays perfectly alive (a shared
/// connection swapped out from under it, a satellite link reconnecting), and
/// whose SECOND stream is the live one.
struct DetachingAdapter {
    attaches: AtomicUsize,
    /// Emit an answer on the first attachment before ending it (so the test
    /// can also assert nothing is replayed), or end it silently mid-turn.
    answer_before_detach: bool,
}

impl DetachingAdapter {
    fn new(answer_before_detach: bool) -> Self {
        Self {
            attaches: AtomicUsize::new(0),
            answer_before_detach,
        }
    }
}

#[async_trait]
impl HarnessAdapter for DetachingAdapter {
    fn name(&self) -> &'static str {
        "detaching-stub"
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
            identity: format!("detaching-{}-{}-{}", ctx.slug, spec.role, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("detaching-turn"))
    }
    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: ccteam_harness::TurnRouting,
    ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(ccteam_harness::TurnSubmission::started)
    }
    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ccteam_harness::ToolSurfaceRebuild, HarnessError> {
        // Test double: no tool face to rebuild.
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }

    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        // Subscription-based, like every long-lived stdio/app-server vendor.
        ccteam_harness::EventAttachment::Rebuildable
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let attach = self.attaches.fetch_add(1, Ordering::SeqCst);
        let answer_first = self.answer_before_detach;
        Box::pin(futures::stream::unfold(
            (attach, 0u32),
            move |(attach, step)| async move {
                match (attach, step) {
                    // First attachment: live long enough for the session's
                    // turn to actually be in flight, (optionally) answer, then
                    // the transport disappears — the stream ENDS while the
                    // session is still very much alive.
                    (0, 0) => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if !answer_first {
                            return None;
                        }
                        let evt = ThreadEvent::ItemCompleted {
                            item: ThreadItem {
                                id: "a-0".into(),
                                details: ThreadItemDetails::AgentMessage(
                                    "answer before the drop".into(),
                                ),
                            },
                        };
                        Some((evt, (attach, step + 1)))
                    }
                    (0, _) => None,
                    // Every rebuild after it is healthy: one answer, then quiet.
                    (_, 0) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        let evt = ThreadEvent::ItemCompleted {
                            item: ThreadItem {
                                id: "a-1".into(),
                                details: ThreadItemDetails::AgentMessage(
                                    "answer after the rebuild".into(),
                                ),
                            },
                        };
                        Some((evt, (attach, step + 1)))
                    }
                    _ => {
                        futures::future::pending::<()>().await;
                        None
                    }
                }
            },
        ))
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "test double".into(),
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
        Err(HarnessError::NotImplemented {
            reason: "test double".into(),
        })
    }
    async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

/// 2026-08-09 — an ended `events()` stream is an ATTACHMENT fact, not a session
/// fact. The pump must rebuild it against the current transport (and record
/// the blind window), or a session whose connection was swapped goes silently
/// unobservable while it keeps working.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pump_reattaches_after_the_inbound_stream_ends() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "reattach-1".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "/new claude helper".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    })
    .await;
    let adapter = Arc::new(DetachingAdapter::new(true));
    run_mock_gateway_daemon_for(
        projects_root,
        Arc::clone(&mock),
        Arc::clone(&adapter),
        Duration::from_millis(1200),
    )
    .await;

    assert!(
        adapter.attaches.load(Ordering::SeqCst) >= 2,
        "the pump must re-acquire the event stream, got {} attach(es)",
        adapter.attaches.load(Ordering::SeqCst)
    );
    let contents: Vec<String> = mock.outbox().await.into_iter().map(|m| m.content).collect();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("answer before the drop")),
        "the first attachment's answer must still land: {contents:?}"
    );
    assert!(
        contents
            .iter()
            .any(|c| c.contains("answer after the rebuild")),
        "the rebuilt attachment must deliver: {contents:?}"
    );

    // …and the blind window is on the record, both ends of it.
    let paths = ccteam_core::CcteamPaths {
        root: ccteam_im::default_ccteam_root_public(),
        projects_root: home.path().join("projects"),
    };
    let body = std::fs::read_to_string(paths.progress_jsonl("default")).unwrap_or_default();
    let rows: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let detached = rows
        .iter()
        .find(|row| row["event"] == "session_stream_detached")
        .expect("a detach must be recorded");
    assert_eq!(detached["sid"], "s1");
    assert_eq!(detached["slug"], "default");
    let reattached = rows
        .iter()
        .find(|row| row["event"] == "session_stream_reattached")
        .expect("a proven rebuild must be recorded");
    assert_eq!(reattached["sid"], "s1");
    assert!(reattached["gap_ms"].is_number());
}

/// The other half of the same invariant: a turn that was in flight when the
/// transport died cannot be observed to completion, so it must be CLOSED
/// honestly — 永久假装在工作 is the failure this replaces.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_stream_closes_the_turn_it_swallowed() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    for (id, content, ts) in [
        ("detach-close-1", "/new claude helper", 0),
        ("detach-close-2", "do the thing", 1),
    ] {
        mock.push(ChannelMessage {
            id: id.into(),
            sender: "alice".into(),
            reply_target: "chat-1".into(),
            content: content.into(),
            channel: "telegram".into(),
            timestamp: ts,
            thread_ts: None,
            attachments: Vec::new(),
            selection: None,
        })
        .await;
    }
    let adapter = Arc::new(DetachingAdapter::new(false));
    run_mock_gateway_daemon_for(
        projects_root,
        Arc::clone(&mock),
        Arc::clone(&adapter),
        Duration::from_millis(1200),
    )
    .await;

    let contents: Vec<String> = mock.outbox().await.into_iter().map(|m| m.content).collect();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("can no longer be observed")),
        "the swallowed turn must be reported, not left working: {contents:?}"
    );
    // Exactly once — the re-attach loop keeps retrying, but the report does not
    // repeat per attempt.
    assert_eq!(
        contents
            .iter()
            .filter(|c| c.contains("can no longer be observed"))
            .count(),
        1,
        "one report per detachment: {contents:?}"
    );
    // The ledger closes the busy window too, or file-backed readers keep
    // calling the session `working`.
    let paths = ccteam_core::CcteamPaths {
        root: ccteam_im::default_ccteam_root_public(),
        projects_root: home.path().join("projects"),
    };
    let body = std::fs::read_to_string(paths.progress_jsonl("default")).unwrap_or_default();
    assert!(
        body.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|row| row["event"] == "chat_turn_completed" && row["sid"] == "s1"),
        "the open turn must reach a terminal ledger row: {body}"
    );
}

/// Emits a steady stream of NON-visible activity (`ItemUpdated`) for well past
/// the idle window, then a final answer — a "long but actively working" turn.
/// The idle watchdog must NOT interrupt it (each event resets the idle clock).
struct StreamingGatewayAdapter {
    esc_calls: AtomicUsize,
}

#[async_trait]
impl HarnessAdapter for StreamingGatewayAdapter {
    fn name(&self) -> &'static str {
        "streaming-stub"
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
            identity: format!("streaming-{}-{}-{}", ctx.slug, spec.role, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("streaming-turn"))
    }
    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: ccteam_harness::TurnRouting,
    ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(ccteam_harness::TurnSubmission::started)
    }
    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ccteam_harness::ToolSurfaceRebuild, HarnessError> {
        // Test double: no tool face to rebuild.
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }

    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        // Scripted test stream: one-shot. Re-attaching would replay
        // the script, which is exactly what `Rebuildable` forbids.
        ccteam_harness::EventAttachment::OneShot
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // 15 reasoning ticks @ 20ms = 300ms of activity (past the 200ms idle
        // window — an old "no answer in window" watchdog would have killed it),
        // then the answer at ~320ms (well inside the 600ms test daemon window).
        // Activity resets idle every 20ms, so a correct watchdog never fires.
        Box::pin(futures::stream::unfold(0u32, |i| async move {
            if i < 15 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let evt = ThreadEvent::ItemUpdated {
                    item: ThreadItem {
                        id: format!("upd-{i}"),
                        details: ThreadItemDetails::Reasoning("working".into()),
                    },
                };
                Some((evt, i + 1))
            } else if i == 15 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let evt = ThreadEvent::ItemCompleted {
                    item: ThreadItem {
                        id: "ans".into(),
                        details: ThreadItemDetails::AgentMessage("done working".into()),
                    },
                };
                Some((evt, i + 1))
            } else {
                None
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
    async fn handle_directive(
        &self,
        _h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        if d.name == "esc" {
            self.esc_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(DirectiveOutcome::Rejected {
            reason: "test double".into(),
        })
    }
    async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

/// v0.8.15: a long-but-active turn (streaming activity past the idle window)
/// must NOT be interrupted — the watchdog is idle-based, not "no answer yet".
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watchdog_does_not_interrupt_a_streaming_turn() {
    let _g = env_lock();
    let old_timeout = std::env::var_os("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS");
    // 200ms idle window vs 20ms event spacing → activity keeps resetting idle.
    std::env::set_var("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS", "200");
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    for (id, content, ts) in [
        ("stream-1", "/new claude helper", 0),
        ("stream-2", "do a long task", 1),
    ] {
        mock.push(ChannelMessage {
            id: id.into(),
            sender: "alice".into(),
            reply_target: "chat-1".into(),
            content: content.into(),
            channel: "telegram".into(),
            timestamp: ts,
            thread_ts: None,
            attachments: Vec::new(),
            selection: None,
        })
        .await;
    }
    let adapter = Arc::new(StreamingGatewayAdapter {
        esc_calls: AtomicUsize::new(0),
    });
    run_mock_gateway_daemon(projects_root, Arc::clone(&mock), Arc::clone(&adapter)).await;
    restore_env("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS", old_timeout);

    // The watchdog never esc-interrupted the active turn.
    assert_eq!(
        adapter.esc_calls.load(Ordering::SeqCst),
        0,
        "watchdog must NOT interrupt a turn that is streaming activity"
    );
    let contents: Vec<String> = mock.outbox().await.into_iter().map(|m| m.content).collect();
    assert!(
        !contents.iter().any(|c| c.contains("went silent")),
        "no stall message for an active turn: {contents:?}"
    );
    assert!(
        contents.iter().any(|c| c.contains("done working")),
        "the answer must be delivered: {contents:?}"
    );
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
        attachments: Vec::new(),
        selection: None,
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
        attachments: Vec::new(),
        selection: None,
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
    // V0.8.4 P1 (F1): no folded "submitted … turn …" ack → created + timeout.
    assert_eq!(contents.len(), 2, "created + timeout (ack folded away)");
    assert_eq!(contents[0], created_session_receipt("s1"));
    // v0.8.9: the watchdog INTERRUPTS the stalled turn (handle_directive `esc`
    // → Ok here) and notifies. v0.8.15: idle-based — this stub emits NO events
    // (stream::empty), so it idles out and the message reads "went silent".
    assert!(
        contents[1].starts_with("⏱️ turn failing-stub-turn went silent for 50ms"),
        "unexpected timeout content: {:?}",
        contents[1]
    );
    // v0.8.18 (owner request): WARN-ONLY — the watchdog must NOT interrupt the
    // turn (a long silent command like a benchmark is real work); it only flags
    // the silence + tells the user it did not touch the turn.
    assert!(
        !contents[1].contains("interrupted it") && contents[1].contains("does NOT interrupt"),
        "watchdog must be heads-up only (no interrupt): {:?}",
        contents[1]
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submits.load(Ordering::SeqCst), 1);
    let rows = read_durable_outbound_rows();
    assert!(rows.iter().any(|row| {
        row["state"] == "sent"
            && row["message"]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("⏱️ turn failing-stub-turn went silent for 50ms"))
    }));

    let paths = ccteam_core::CcteamPaths {
        root: ccteam_im::default_ccteam_root_public(),
        projects_root: home.path().join("projects"),
    };
    let progress = paths.progress_jsonl("default");
    let body = std::fs::read_to_string(&progress).unwrap_or_else(|err| {
        panic!(
            "watchdog must append chat_turn_timeout to {}: {err}",
            progress.display()
        )
    });
    let timeout_event = body
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row| row["event"] == "chat_turn_timeout")
        .expect("expected chat_turn_timeout in progress.jsonl");
    assert_eq!(timeout_event["role"], "helper");
    assert_eq!(timeout_event["slug"], "default");
    assert_eq!(timeout_event["turn_id"], "failing-stub-turn");
    assert_eq!(timeout_event["stuck"], true);
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(1200)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
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
    assert_eq!(created.content, created_session_receipt("s1"));
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
    // V0.8.4 P1 (F1): no "submitted … turn …" ack — the answer is the
    // next (and only) send.
    let reply = recv_ws_send(&mut socket).await;
    // v0.8.23 review §3.2-5 — a focused IM answer now carries the compact
    // "→ slug/sid (role)" context echo suffix ("ws" is an IM text surface,
    // not "web", so the echo applies). The session's independent v0.8.22 P1
    // auto-title can ALSO land on the same meta.json and append an OPTIONAL
    // `「title」` tag after it, so match the deterministic prefix.
    assert!(
        reply
            .content
            .starts_with("gateway echo: hello over ws\n\n→ default/s1 (helper)"),
        "got: {:?}",
        reply.content
    );
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
        created_session_receipt("s1")
    );
    send_ws_text(&mut first_socket, "ws-r1-msg", "before restart").await;
    // V0.8.4 P1 (F1): ack folded away — the answer is the only send. Carries
    // the v0.8.23 review §3.2-5 context echo suffix; the session's
    // independent v0.8.22 P1 auto-title can ALSO land on the same
    // meta.json and append an OPTIONAL `「title」` tag after it, so match
    // the deterministic prefix rather than betting on whether that race won.
    let first_reply = recv_ws_send(&mut first_socket).await.content;
    assert!(
        first_reply.starts_with("gateway echo: before restart\n\n→ default/s1 (helper)"),
        "got: {first_reply:?}"
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
    // V0.8.4 P1 (F1): ack folded away — the answer is the only send. Carries
    // the v0.8.23 review §3.2-5 context echo suffix (role survives restart);
    // see the note above the first assertion re: the optional title tag.
    let second_reply = recv_ws_send(&mut second_socket).await.content;
    assert!(
        second_reply.starts_with("gateway echo: after restart\n\n→ default/s1 (helper)"),
        "got: {second_reply:?}"
    );
    drop(second_socket);
    let _ = second_stop_tx.send(());
    second_daemon.await.unwrap();

    assert_eq!(
        adapter.starts.load(Ordering::SeqCst),
        2,
        "each daemon starts/resumes the persisted s1 exactly once"
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
    // Cross-check against the ACTUAL delivered content observed above
    // (already prefix-verified) rather than a hand-computed guess, so the
    // optional auto-title tag can't desync this from the two assertions
    // above it.
    assert_eq!(
        sent_contents,
        vec![created_session_receipt("s1"), first_reply, second_reply,]
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
    let ccteam_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let slug = format!("real-ws-{}", std::process::id());
    let old_ccteam_home = std::env::var_os("CCTEAM_HOME");
    let old_socket = std::env::var_os("CCTEAM_CODEX_APP_SERVER_SOCKET");
    let old_codex_fault = std::env::var_os("CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN");
    let old_mux_backend = std::env::var_os("CCTEAM_MUX_BACKEND");
    let old_path = std::env::var_os("PATH");
    let nl_mode = std::env::var("CCTEAM_REAL_IM_WS_NL").ok();
    let codex_nl_mode = nl_mode
        .as_deref()
        .is_some_and(|mode| mode == "1" || mode == "codex");
    let codex_mode =
        std::env::var("CCTEAM_REAL_IM_WS_CODEX").ok().as_deref() == Some("1") || codex_nl_mode;
    if codex_mode {
        assert!(command_exists("codex"), "codex binary is required");
    }
    let restart_mode = std::env::var("CCTEAM_REAL_IM_WS_RESTART").ok().as_deref() == Some("1");
    let fault_mode = std::env::var("CCTEAM_REAL_IM_WS_FAULTS").ok().as_deref() == Some("1");
    let host_fault_mode = std::env::var("CCTEAM_REAL_IM_WS_HOST_FAULTS")
        .ok()
        .as_deref()
        == Some("1");
    let host_fault_stop = real_ws_host_fault_stop_duration();
    std::env::set_var("CCTEAM_HOME", ccteam_home.path());
    // F10: stdio is the default transport — unset the socket override so
    // the adapter spawns `codex app-server --listen stdio://` itself.
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
    seed_role(project.path(), "api");
    seed_role(project.path(), "reviewer");
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

    let mut ws = Arc::new(WsChannel::bind_localhost().await.unwrap());
    let ws_addr = ws.local_addr();
    let ws_url = format!("ws://{ws_addr}");
    let mut max_runtime = if nl_mode.is_some() || restart_mode || fault_mode || host_fault_mode {
        Duration::from_secs(300)
    } else {
        Duration::from_secs(30)
    };
    if host_fault_mode {
        max_runtime += host_fault_stop;
    }
    let (mut stop_tx, mut daemon) =
        spawn_real_ws_gateway_daemon(project.path().to_path_buf(), Arc::clone(&ws), max_runtime);

    let mut socket = connect_ws_with_retry(&ws_url).await;
    let claude_sid = if codex_mode {
        send_ws_text(&mut socket, "real-ws-codex-new", "/new codex api").await;
        assert_eq!(
            recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
                .await
                .content,
            created_session_receipt("s1")
        );
        "s2"
    } else {
        "s1"
    };
    send_ws_text(&mut socket, "real-ws-claude-new", "/new claude reviewer").await;
    assert_eq!(
        recv_ws_send_with_timeout(&mut socket, Duration::from_secs(20))
            .await
            .content,
        created_session_receipt(claude_sid)
    );
    send_ws_text(&mut socket, "real-ws-sessions", "/sessions").await;
    let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(5))
        .await
        .content;
    let has_claude = sessions.contains(&format!("{claude_sid}:{slug}:Claude:reviewer"));
    let has_codex = !codex_mode || sessions.contains(&format!("s1:{slug}:Codex:api"));
    assert!(
        has_claude && has_codex,
        "real WS sessions must use the configured project slug; got {sessions:?}"
    );
    let claude_tmux_session = format!("ccteam-chat-{slug}-{claude_sid}");
    assert!(
        tmux_session_exists(&claude_tmux_session),
        "Claude tmux session should remain live after /new: {claude_tmux_session}"
    );
    if codex_mode {
        send_ws_text(&mut socket, "real-ws-codex-status", "@api /status").await;
        // Keep this as an immediate Codex-session routing probe. `/compact` starts
        // a Codex turn and may legitimately produce no short-window event while
        // the daemon event-sink path has folded away submit acks.
        let codex = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
        assert!(
            !codex.content.starts_with("gateway error"),
            "Codex /status should reach the Codex session, got {:?}",
            codex.content
        );
    }

    if nl_mode
        .as_deref()
        .is_some_and(|mode| mode == "1" || mode == "codex" || mode == "claude")
    {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if codex_nl_mode {
            send_ws_text(
                &mut socket,
                "real-ws-codex-nl",
                "@api Reply with exactly CCTEAM-CODEX-WS-OK and no extra text.",
            )
            .await;
            let codex_ack = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
            assert!(
                !codex_ack.content.starts_with("gateway error"),
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
                !claude_ack.content.starts_with("gateway error"),
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

        ws = Arc::new(WsChannel::bind_on_listen(ws_addr));
        let restarted = spawn_real_ws_gateway_daemon(
            project.path().to_path_buf(),
            Arc::clone(&ws),
            max_runtime,
        );
        stop_tx = restarted.0;
        daemon = restarted.1;
        socket = connect_ws_with_retry(&ws_url).await;

        send_ws_text(&mut socket, "real-ws-restart-sessions", "/sessions").await;
        let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
            .await
            .content;
        let has_claude = sessions.contains(&format!("{claude_sid}:{slug}:Claude:reviewer"));
        let has_codex = !codex_mode || sessions.contains(&format!("s1:{slug}:Codex:api"));
        assert!(
            has_claude && has_codex,
            "restart must restore original sessions; got {sessions:?}"
        );

        if codex_mode {
            if codex_nl_mode {
                send_ws_text(
                    &mut socket,
                    "real-ws-codex-after-restart",
                    "@api Reply with exactly CCTEAM-CODEX-WS-RESTART-OK and no extra text.",
                )
                .await;
                let codex_ack =
                    recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
                assert!(
                    !codex_ack.content.starts_with("gateway error"),
                    "Codex after restart should reuse s1, got {:?}",
                    codex_ack.content
                );
                recv_ws_until_contains(
                    &mut socket,
                    "CCTEAM-CODEX-WS-RESTART-OK",
                    Duration::from_secs(120),
                )
                .await;
            } else {
                send_ws_text(
                    &mut socket,
                    "real-ws-codex-after-restart-status",
                    "@api /status",
                )
                .await;
                let codex_status =
                    recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
                assert!(
                    !codex_status.content.starts_with("gateway error"),
                    "Codex status after restart should reuse s1, got {:?}",
                    codex_status.content
                );
            }
        }

        send_ws_text(
            &mut socket,
            "real-ws-claude-after-restart",
            "@reviewer Reply with exactly CCTEAM-CLAUDE-WS-RESTART-OK and no extra text.",
        )
        .await;
        recv_ws_until_contains(
            &mut socket,
            "CCTEAM-CLAUDE-WS-RESTART-OK",
            Duration::from_secs(180),
        )
        .await;
    }

    if host_fault_mode {
        send_ws_text(
            &mut socket,
            "real-ws-claude-sigstop",
            "@reviewer Reply with exactly CCTEAM-CLAUDE-WS-SIGSTOP-OK and no extra text.",
        )
        .await;
        let sigstop_ack = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10)).await;
        assert!(
            !sigstop_ack.content.starts_with("gateway error"),
            "Claude SIGSTOP prompt should be submitted, got {:?}",
            sigstop_ack.content
        );
        wait_turns_file_contains(
            project.path(),
            claude_sid,
            "CCTEAM-CLAUDE-WS-SIGSTOP-OK",
            Duration::from_secs(10),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        sigstop_self_for(host_fault_stop);
        recv_ws_until_contains_once(
            &mut socket,
            "CCTEAM-CLAUDE-WS-SIGSTOP-OK",
            Duration::from_secs(30),
            Duration::from_secs(3),
        )
        .await;

        send_ws_text(&mut socket, "real-ws-sigstop-sessions", "/sessions").await;
        let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
            .await
            .content;
        assert!(
            sessions.contains(&format!("{claude_sid}:{slug}:Claude:reviewer")),
            "SIGSTOP resume must preserve the original Claude sid; got {sessions:?}"
        );

        let _ = socket.close(None).await;
        drop(socket);
        tokio::time::sleep(Duration::from_secs(1)).await;
        ws.send(&SendMessage::new("CCTEAM-WS-NETDROP-OK", "chat-1"))
            .await
            .expect("inject WS netdrop backlog message");
        tokio::time::sleep(Duration::from_secs(1)).await;
        socket = connect_ws_with_retry(&ws_url).await;
        send_ws_text(&mut socket, "real-ws-netdrop-resync", "/sessions").await;
        recv_ws_until_contains_once(
            &mut socket,
            "CCTEAM-WS-NETDROP-OK",
            Duration::from_secs(10),
            Duration::from_secs(3),
        )
        .await;

        send_ws_text(&mut socket, "real-ws-netdrop-sessions", "/sessions").await;
        let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
            .await
            .content;
        assert!(
            sessions.contains(&format!("{claude_sid}:{slug}:Claude:reviewer")),
            "WS reconnect must preserve the original Claude sid; got {sessions:?}"
        );

        drop(socket);
        let _ = stop_tx.send(());
        daemon.await.unwrap();
        assert!(
            tmux_session_exists(&claude_tmux_session),
            "Claude tmux session must survive local host-fault daemon restart: {claude_tmux_session}"
        );

        let ws = Arc::new(WsChannel::bind_on_listen(ws_addr));
        let restarted = spawn_real_ws_gateway_daemon(
            project.path().to_path_buf(),
            Arc::clone(&ws),
            max_runtime,
        );
        stop_tx = restarted.0;
        daemon = restarted.1;
        socket = connect_ws_with_retry(&ws_url).await;
        send_ws_text(
            &mut socket,
            "real-ws-host-fault-restart-sessions",
            "/sessions",
        )
        .await;
        let sessions = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(10))
            .await
            .content;
        assert!(
            sessions.contains(&format!("{claude_sid}:{slug}:Claude:reviewer")),
            "daemon restart after local host faults must restore the original sid; got {sessions:?}"
        );
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
            fault.content.starts_with("发送失败: tmux session missing:"),
            "Claude tmux death should be user-visible, got {:?}",
            fault.content
        );
        if codex_mode {
            std::env::set_var("CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN", "1");
            send_ws_text(
                &mut socket,
                "real-ws-codex-after-kill",
                "@api this should surface a codex app-server disconnect",
            )
            .await;
            let fault = recv_ws_send_with_timeout(&mut socket, Duration::from_secs(20)).await;
            assert!(
                fault.content.starts_with("发送失败:")
                    && (fault.content.contains("turn/start")
                        || fault.content.contains("codex app-server fault injection")),
                "Codex app-server death should be user-visible, got {:?}",
                fault.content
            );
        }
        drop(socket);
        let _ = stop_tx.send(());
        daemon.await.unwrap();
        restore_env("CCTEAM_HOME", old_ccteam_home);
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
        !claude.content.starts_with("gateway error"),
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_secs(5)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
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
        ..Default::default()
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
    run_mock_gateway_daemon_for(projects_root, mock, adapter, Duration::from_millis(600)).await
}

/// Same harness with an explicit runtime, for cases that need more than the
/// default 600ms window (a re-attach backoff, say).
async fn run_mock_gateway_daemon_for<T>(
    projects_root: std::path::PathBuf,
    mock: Arc<MockChannel>,
    adapter: Arc<T>,
    max_runtime: Duration,
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(max_runtime),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
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

#[allow(clippy::result_large_err)]
async fn recv_ws_until_contains_once<S>(
    socket: &mut WebSocketStream<S>,
    needle: &str,
    timeout: Duration,
    quiet_window: Duration,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut seen = String::new();
    let mut matches = 0usize;
    let deadline = tokio::time::Instant::now() + timeout;
    while matches == 0 {
        let now = tokio::time::Instant::now();
        assert!(
            now < deadline,
            "timed out waiting for exactly one {needle}; seen:\n{seen}"
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
        .unwrap_or_else(|_| panic!("timed out waiting for exactly one {needle}; seen:\n{seen}"));
        matches += msg.content.matches(needle).count();
        seen.push_str(&msg.content);
        seen.push('\n');
    }
    assert_eq!(
        matches, 1,
        "expected exactly one {needle} before quiet window; seen:\n{seen}"
    );

    let quiet_deadline = tokio::time::Instant::now() + quiet_window;
    loop {
        let now = tokio::time::Instant::now();
        if now >= quiet_deadline {
            break;
        }
        match tokio::time::timeout(quiet_deadline.saturating_duration_since(now), socket.next())
            .await
        {
            Ok(Some(Ok(Message::Text(text)))) => {
                let msg: SendMessage = serde_json::from_str(&text).unwrap();
                matches += msg.content.matches(needle).count();
                seen.push_str(&msg.content);
                seen.push('\n');
                assert_eq!(
                    matches, 1,
                    "expected exactly one {needle} after quiet window; seen:\n{seen}"
                );
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => panic!("websocket error while checking duplicates: {err}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

fn real_ws_host_fault_stop_duration() -> Duration {
    let secs = std::env::var("CCTEAM_REAL_IM_WS_HOST_FAULT_STOP_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(12)
        .max(1);
    Duration::from_secs(secs)
}

fn sigstop_self_for(duration: Duration) {
    let pid = std::process::id();
    let secs = duration.as_secs().max(1);
    let mut helper = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep {secs}; kill -CONT {pid}"))
        .spawn()
        .expect("spawn SIGCONT helper");
    let status = std::process::Command::new("kill")
        .arg("-STOP")
        .arg(pid.to_string())
        .status()
        .expect("send SIGSTOP to self");
    assert!(status.success(), "kill -STOP self should succeed");
    let _ = helper.wait();
}

async fn wait_turns_file_contains(
    project: &std::path::Path,
    sid: &str,
    needle: &str,
    timeout: Duration,
) {
    let path = project
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("turns.jsonl");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if body.contains(needle) {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {needle} in {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
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

/// V0.8.4 P2a — an inbound message carrying an image attachment reaches
/// the agent as a turn wrapped in `<channel … image_path="…">` so the
/// Read convention (taught by the MCP server instructions) can fire.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_routes_inbound_image_attachment_into_turn_text() {
    let _g = env_lock();
    // Progress off keeps this focused on the inbound turn text.
    std::env::set_var("CCTEAM_IM_PROGRESS", "off");
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
        attachments: Vec::new(),
        selection: None,
    })
    .await;
    mock.push(ChannelMessage {
        id: "tg-77".into(),
        sender: "alice".into(),
        reply_target: "chat-1".into(),
        content: "这是报错".into(),
        channel: "telegram".into(),
        timestamp: 1,
        thread_ts: None,
        attachments: vec![ChannelAttachment {
            kind: AttachmentKind::Image,
            file_name: "tg-77-shot.png".into(),
            local_path: "/tmp/ccteam-inbound/tg-77-shot.png".into(),
            mime: Some("image/png".into()),
            size: Some(1234),
        }],
        selection: None,
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
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(700)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    let payloads = adapter.submitted_payloads.lock().await.clone();
    let turn = payloads
        .iter()
        .find(|p| p.contains("这是报错"))
        .unwrap_or_else(|| panic!("image turn not submitted: {payloads:?}"));
    assert!(turn.contains("<channel "), "no channel wrapper: {turn}");
    assert!(
        turn.contains("image_path=\"/tmp/ccteam-inbound/tg-77-shot.png\""),
        "image path not named in turn: {turn}"
    );
    std::env::remove_var("CCTEAM_IM_PROGRESS");
}

/// V0.8.4 P2b — a `GatewayEvent` carrying an `OutboundFile` (as
/// `chat_send_file` enqueues) flows through the gateway-event consumer to
/// the channel as a `SendMessage` with attachments (Telegram would then
/// `sendPhoto`). Proves the shared-sink delivery seam end to end; the
/// `mcp.sock`/env addressing is covered by ccteam-cli unit tests.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_delivers_gateway_event_attachment_to_channel() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    // Shared gateway-event channel (as `ccteam start` builds it); enqueue
    // a file-bearing event before the daemon's consumer drains it.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
    tx.send(GatewayEvent {
        id: "csf-1".into(),
        channel: "telegram".into(),
        chat_id: "chat-77".into(),
        thread_ts: None,
        content: String::new(),
        kind: GatewayEventKind::Answer,
        attachments: vec![OutboundFile {
            id: String::new(),
            size: 0,
            path: "/tmp/ccteam-out/chart.png".into(),
            caption: Some("the chart".into()),
            kind: OutboundFileKind::Photo,
        }],
        options: Vec::new(),
        sid: None,
        slug: None,
    })
    .unwrap();

    let adapter = Arc::new(GatewayAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(400)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        gateway_event_tx: Some(tx.clone()),
        gateway_event_rx: Some(rx),
        pending: None,
        gateway: None,
        ..Default::default()
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    let outbox = mock.outbox().await;
    let with_file = outbox
        .iter()
        .find(|m| !m.attachments.is_empty())
        .unwrap_or_else(|| panic!("no attachment SendMessage delivered: {outbox:?}"));
    assert_eq!(with_file.recipient, "chat-77");
    assert_eq!(with_file.attachments.len(), 1);
    assert_eq!(with_file.attachments[0].path, "/tmp/ccteam-out/chart.png");
    assert_eq!(with_file.attachments[0].kind, OutboundFileKind::Photo);
    assert_eq!(
        with_file.attachments[0].caption.as_deref(),
        Some("the chart")
    );
}

fn read_durable_outbound_rows() -> Vec<serde_json::Value> {
    let path = dirs::home_dir()
        .unwrap()
        .join(".ccteam")
        .join("state")
        .join("im")
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
        .join("state")
        .join("im")
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
