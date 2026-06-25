//! CLI-owned bridge between browser chat WS and the IM gateway.
//!
//! This module is the only place that translates between
//! `ccteam-web`'s neutral wire structs and `ccteam-im`'s Channel trait.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc, Mutex};

use ccteam_im::transport::{Channel, ChannelMessage, ChoiceReply, SendMessage};
use ccteam_web::chat_protocol::{WebChannelMessage, WebMessageOption, WebSendMessage};
use ccteam_web::{ChatConns, CHAT_BACKLOG_CAP};

pub(crate) struct WebChatBridge {
    pub inbound_tx: mpsc::Sender<WebChannelMessage>,
    pub outbound_tx: broadcast::Sender<WebSendMessage>,
    pub backlog: Arc<Mutex<Vec<WebSendMessage>>>,
    /// Shared with `AppState` so the send path and the WS edge agree on
    /// which recipients currently have a live socket.
    pub conns: ChatConns,
    pub channel: Arc<dyn Channel + Send + Sync>,
}

pub(crate) fn build() -> WebChatBridge {
    let (inbound_tx, inbound_rx) = mpsc::channel(64);
    let (outbound_tx, _) = broadcast::channel(256);
    let backlog = Arc::new(Mutex::new(Vec::new()));
    let conns: ChatConns = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let channel = Arc::new(WebChatChannel {
        inbound_rx: Mutex::new(Some(inbound_rx)),
        outbound_tx: outbound_tx.clone(),
        backlog: Arc::clone(&backlog),
        conns: Arc::clone(&conns),
    });
    WebChatBridge {
        inbound_tx,
        outbound_tx,
        backlog,
        conns,
        channel,
    }
}

struct WebChatChannel {
    inbound_rx: Mutex<Option<mpsc::Receiver<WebChannelMessage>>>,
    outbound_tx: broadcast::Sender<WebSendMessage>,
    backlog: Arc<Mutex<Vec<WebSendMessage>>>,
    conns: ChatConns,
}

#[async_trait]
impl Channel for WebChatChannel {
    fn name(&self) -> &str {
        "web"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let message = WebSendMessage {
            content: message.content.clone(),
            recipient: message.recipient.clone(),
            subject: message.subject.clone(),
            thread_ts: message.thread_ts.clone(),
            // v0.8.5 D3: carry choice options through to the browser chips.
            options: message
                .options
                .iter()
                .map(|o| WebMessageOption {
                    data: o.data.clone(),
                    label: o.label.clone(),
                })
                .collect(),
        };
        // P1-1/P1-3: only park the message when the recipient has no live
        // socket. A connected socket receives it via the broadcast below,
        // so backlogging it would re-deliver a stale copy on the next
        // connect. With the registry gating inserts, the backlog only
        // grows while offline; the cap bounds even that.
        let live = self
            .conns
            .lock()
            .await
            .get(&message.recipient)
            .copied()
            .unwrap_or(0);
        if live == 0 {
            let mut backlog = self.backlog.lock().await;
            backlog.push(message.clone());
            let overflow = backlog.len().saturating_sub(CHAT_BACKLOG_CAP);
            if overflow > 0 {
                backlog.drain(0..overflow);
            }
        }
        let _ = self.outbound_tx.send(message);
        Ok(None)
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let mut rx = {
            let mut guard = self.inbound_rx.lock().await;
            guard
                .take()
                .ok_or_else(|| anyhow::anyhow!("web chat channel listener already running"))?
        };
        while let Some(message) = rx.recv().await {
            if tx.send(to_im_message(message)).await.is_err() {
                break;
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }
}

fn to_im_message(message: WebChannelMessage) -> ChannelMessage {
    ChannelMessage {
        id: message.id,
        sender: message.sender,
        reply_target: message.reply_target,
        content: message.content,
        channel: message.channel,
        timestamp: message.timestamp,
        thread_ts: message.thread_ts,
        // Web inbound attachments arrive text-encoded as a `/attach`
        // pseudo-command (chat_ws `frame_to_messages`), never as structured
        // attachments — so there are none to carry here.
        attachments: Vec::new(),
        // v0.8.5 D3: a chip click carries its opaque "{token}:{idx}".
        selection: message.selection.map(|data| ChoiceReply { data }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::net::SocketAddr;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ccteam_core::CcteamPaths;
    use ccteam_harness::{
        AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
        HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails,
        ThreadStatus, TurnId, TurnInput,
    };
    use ccteam_web::chat_protocol::{ServerChatFrame, SUBPROTOCOL};
    use ccteam_web::{router_with_state, AppState, AuthState};
    use futures::stream::BoxStream;
    use futures::{SinkExt, StreamExt};
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{connect_async, WebSocketStream};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvRestore {
        home: Option<std::ffi::OsString>,
        ccteam_home: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        fn install(home: &Path, ccteam_home: &Path) -> Self {
            let restore = Self {
                home: std::env::var_os("HOME"),
                ccteam_home: std::env::var_os("CCTEAM_HOME"),
            };
            std::env::set_var("HOME", home);
            std::env::set_var("CCTEAM_HOME", ccteam_home);
            restore
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            restore_env("HOME", self.home.take());
            restore_env("CCTEAM_HOME", self.ccteam_home.take());
        }
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    fn fake_paths(root: &Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    #[test]
    fn to_im_message_maps_chip_selection() {
        // v0.8.5 D3: a web chip click → ChannelMessage.selection.
        let clicked = to_im_message(WebChannelMessage {
            id: "w1".into(),
            sender: "alice".into(),
            reply_target: "chat-1".into(),
            content: String::new(),
            channel: "web".into(),
            timestamp: 1,
            thread_ts: None,
            selection: Some("tok:2".into()),
        });
        assert_eq!(
            clicked.selection,
            Some(ChoiceReply {
                data: "tok:2".into()
            })
        );
        assert!(clicked.attachments.is_empty());
        // Ordinary text carries no selection.
        let plain = to_im_message(WebChannelMessage {
            id: "w2".into(),
            sender: "alice".into(),
            reply_target: "chat-1".into(),
            content: "hi".into(),
            channel: "web".into(),
            timestamp: 2,
            thread_ts: None,
            selection: None,
        });
        assert_eq!(plain.selection, None);
        assert_eq!(plain.content, "hi");
    }

    #[derive(Default)]
    struct RecordingState {
        starts: AtomicUsize,
        submits: AtomicUsize,
        resumes: AtomicUsize,
        started_vendors: Mutex<Vec<AgentVendor>>,
        submitted_payloads: Mutex<Vec<String>>,
        event_queues: Arc<Mutex<BTreeMap<String, VecDeque<ThreadEvent>>>>,
    }

    #[derive(Clone)]
    struct RecordingAdapter {
        vendor: AgentVendor,
        state: Arc<RecordingState>,
    }

    impl RecordingAdapter {
        async fn thread(&self, identity: String) -> ThreadHandle {
            self.ensure_queue(&identity).await;
            ThreadHandle {
                vendor: self.vendor,
                mode: ExecutionMode::Chat,
                identity,
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({}),
            }
        }

        async fn ensure_queue(&self, identity: &str) {
            let mut guard = self.state.event_queues.lock().await;
            guard
                .entry(identity.to_string())
                .or_insert_with(VecDeque::new);
        }
    }

    #[async_trait]
    impl HarnessAdapter for RecordingAdapter {
        fn name(&self) -> &'static str {
            "web-chat-recording"
        }

        fn vendor(&self) -> AgentVendor {
            self.vendor
        }

        async fn start_thread(
            &self,
            spec: &AgentSpecBrief,
            ctx: &SpawnCtx,
        ) -> Result<ThreadHandle, HarnessError> {
            self.state.starts.fetch_add(1, Ordering::SeqCst);
            self.state.started_vendors.lock().await.push(self.vendor);
            self.thread(format!(
                "fake-{:?}-{}-{}-{}",
                self.vendor, ctx.slug, spec.role, ctx.sid
            ))
            .await
            .pipe(Ok)
        }

        async fn submit_turn(
            &self,
            h: &ThreadHandle,
            input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            self.state.submits.fetch_add(1, Ordering::SeqCst);
            let text = match input {
                TurnInput::UserText(text) => text,
                other => format!("{other:?}"),
            };
            self.state
                .submitted_payloads
                .lock()
                .await
                .push(text.clone());
            self.state
                .event_queues
                .lock()
                .await
                .entry(h.identity.clone())
                .or_insert_with(VecDeque::new)
                .push_back(ThreadEvent::ItemCompleted {
                    item: ThreadItem {
                        id: format!("event-{}", self.state.submits.load(Ordering::SeqCst)),
                        details: ThreadItemDetails::AgentMessage(format!(
                            "{:?} echo: {text}",
                            self.vendor
                        )),
                    },
                });
            Ok(TurnId::new(format!(
                "{:?}-turn-{}",
                self.vendor,
                self.state.submits.load(Ordering::SeqCst)
            )))
        }

        fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
            let identity = h.identity.clone();
            let queues = Arc::clone(&self.state.event_queues);
            Box::pin(futures::stream::unfold((), move |_| {
                let identity = identity.clone();
                let queues = Arc::clone(&queues);
                async move {
                    loop {
                        if let Some(event) = queues
                            .lock()
                            .await
                            .entry(identity.clone())
                            .or_insert_with(VecDeque::new)
                            .pop_front()
                        {
                            return Some((event, ()));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }))
        }

        async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
            self.state.resumes.fetch_add(1, Ordering::SeqCst);
            Ok(self.thread(persistent_id.to_string()).await)
        }

        async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
            Ok(())
        }

        async fn handle_directive(
            &self,
            h: &ThreadHandle,
            d: Directive,
        ) -> Result<DirectiveOutcome, HarnessError> {
            // Echo the directive the same way `submit_turn` echoes text, so
            // the web e2e can assert routing through `handle_directive`
            // (`<Vendor> echo: directive:<name>`), then let the pump deliver.
            self.state
                .event_queues
                .lock()
                .await
                .entry(h.identity.clone())
                .or_insert_with(VecDeque::new)
                .push_back(ThreadEvent::ItemCompleted {
                    item: ThreadItem {
                        id: format!("directive-{}", d.name),
                        details: ThreadItemDetails::AgentMessage(format!(
                            "{:?} echo: directive:{}",
                            self.vendor, d.name
                        )),
                    },
                });
            Ok(DirectiveOutcome::Turn(TurnId::new(format!(
                "{:?}-directive-{}",
                self.vendor, d.name
            ))))
        }

        async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
            Ok(ThreadStatus::default())
        }
    }

    trait Pipe: Sized {
        fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
            f(self)
        }
    }
    impl<T> Pipe for T {}

    fn adapter_factory(state: Arc<RecordingState>) -> ccteam_im::daemon::AdapterFactory {
        Arc::new(move |vendor, _protocol| {
            Arc::new(RecordingAdapter {
                vendor,
                state: Arc::clone(&state),
            }) as Arc<dyn HarnessAdapter + Send + Sync>
        })
    }

    struct Stack {
        addr: SocketAddr,
        web_stop: tokio::sync::oneshot::Sender<()>,
        daemon_stop: tokio::sync::oneshot::Sender<()>,
        web_handle: tokio::task::JoinHandle<()>,
        daemon_handle: tokio::task::JoinHandle<()>,
    }

    async fn spawn_stack(paths: CcteamPaths, adapter_state: Arc<RecordingState>) -> Stack {
        let bridge = build();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router_with_state(
            AppState::with_auth(paths.clone(), AuthState::disabled()).with_chat_bridge(
                bridge.inbound_tx.clone(),
                bridge.outbound_tx.clone(),
                bridge.backlog.clone(),
                bridge.conns.clone(),
            ),
        );
        let (web_stop, web_stop_rx) = tokio::sync::oneshot::channel::<()>();
        let web_handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = web_stop_rx.await;
                })
                .await
                .unwrap();
        });

        let mut channels = ccteam_im::daemon::ChannelMap::new();
        channels.insert("web".to_string(), bridge.channel.clone());
        let args = ccteam_im::DaemonArgs {
            credentials: None,
            registry: Some(paths.projects_root.clone()),
            max_runtime: None,
            adapter_factory: Some(adapter_factory(adapter_state)),
            channels_override: None,
            extra_channels: Some(channels),
            ..Default::default()
        };
        let (daemon_stop, daemon_stop_rx) = tokio::sync::oneshot::channel::<()>();
        let daemon_handle = tokio::spawn(async move {
            ccteam_im::run_daemon_with_shutdown(args, async {
                let _ = daemon_stop_rx.await;
            })
            .await
            .unwrap();
        });
        tokio::task::yield_now().await;
        Stack {
            addr,
            web_stop,
            daemon_stop,
            web_handle,
            daemon_handle,
        }
    }

    async fn stop_stack(stack: Stack) {
        let _ = stack.web_stop.send(());
        let _ = stack.daemon_stop.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), stack.web_handle)
            .await
            .unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), stack.daemon_handle)
            .await
            .unwrap();
    }

    async fn connect_chat(
        addr: SocketAddr,
    ) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        connect_chat_as(addr, "chat-1", "alice").await
    }

    async fn connect_chat_as(
        addr: SocketAddr,
        chat_id: &str,
        user_id: &str,
    ) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        let url = format!("ws://{addr}/ws/chat?chat_id={chat_id}&user_id={user_id}");
        let mut req = url.into_client_request().unwrap();
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        let (mut socket, response) = connect_async(req).await.unwrap();
        assert_eq!(
            response
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|h| h.to_str().ok()),
            Some(SUBPROTOCOL)
        );
        assert!(matches!(
            recv_frame(&mut socket, "initial sessions").await,
            ServerChatFrame::Sessions { .. }
        ));
        socket
    }

    async fn send_text<S>(socket: &mut WebSocketStream<S>, id: &str, content: &str)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket
            .send(Message::Text(
                serde_json::json!({"type":"text","id":id,"content":content}).to_string(),
            ))
            .await
            .unwrap();
    }

    async fn recv_frame<S>(socket: &mut WebSocketStream<S>, label: &str) -> ServerChatFrame
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(3), socket.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for web-chat frame: {label}"))
            .expect("socket closed")
            .expect("web-chat socket error");
        let Message::Text(text) = frame else {
            panic!("expected text frame, got {frame:?}");
        };
        serde_json::from_str(&text).unwrap()
    }

    async fn recv_reply_contains<S>(socket: &mut WebSocketStream<S>, needle: &str)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for reply containing {needle:?}"
            );
            if let ServerChatFrame::Reply { content } = recv_frame(socket, needle).await {
                if content.contains(needle) {
                    return;
                }
            }
        }
    }

    async fn recv_replies_containing_all<S>(socket: &mut WebSocketStream<S>, needles: &[&str])
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut seen = vec![false; needles.len()];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.iter().any(|hit| !*hit) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for replies containing {needles:?}"
            );
            if let ServerChatFrame::Reply { content } = recv_frame(socket, "reply set").await {
                for (idx, needle) in needles.iter().enumerate() {
                    if content.contains(needle) {
                        seen[idx] = true;
                    }
                }
            }
        }
    }

    async fn recv_sessions<S>(
        socket: &mut WebSocketStream<S>,
    ) -> Vec<ccteam_web::chat_protocol::SessionItem>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for sessions frame"
            );
            if let ServerChatFrame::Sessions { items } = recv_frame(socket, "sessions").await {
                return items;
            }
        }
    }

    fn write_queued_web_outbound(ccteam_home: &Path, content: &str) {
        let path = ccteam_home.join("state").join("im").join("outbound.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let row = serde_json::json!({
            "ts_ms": 1_u64,
            "id": "web-replay-1",
            "inbound_id": "web-replay",
            "channel": "web",
            "state": "queued",
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

    #[tokio::test]
    async fn web_chat_bridge_forwards_inbound_and_outbound_shapes() {
        let bridge = build();
        let (im_tx, mut im_rx) = mpsc::channel(4);
        let channel = bridge.channel.clone();
        let listener = tokio::spawn(async move { channel.listen(im_tx).await });

        bridge
            .inbound_tx
            .send(WebChannelMessage {
                id: "web-1".into(),
                sender: "alice".into(),
                reply_target: "chat-1".into(),
                content: "/projects".into(),
                channel: "web".into(),
                timestamp: 42,
                thread_ts: Some("thread-1".into()),
                selection: None,
            })
            .await
            .unwrap();

        let inbound = tokio::time::timeout(std::time::Duration::from_secs(2), im_rx.recv())
            .await
            .expect("timed out waiting for IM inbound")
            .expect("IM inbound channel closed");
        assert_eq!(inbound.id, "web-1");
        assert_eq!(inbound.channel, "web");
        assert_eq!(inbound.reply_target, "chat-1");
        assert_eq!(inbound.content, "/projects");
        assert_eq!(inbound.thread_ts.as_deref(), Some("thread-1"));

        let mut outbound = bridge.outbound_tx.subscribe();
        bridge
            .channel
            .send(&SendMessage::new("default", "chat-1").in_thread(Some("thread-1".into())))
            .await
            .unwrap();
        assert_eq!(bridge.backlog.lock().await.len(), 1);
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), outbound.recv())
            .await
            .expect("timed out waiting for web outbound")
            .expect("web outbound channel closed");
        assert_eq!(reply.content, "default");
        assert_eq!(reply.recipient, "chat-1");
        assert_eq!(reply.thread_ts.as_deref(), Some("thread-1"));

        drop(bridge.inbound_tx);
        listener.abort();
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn web_chat_ws_routes_through_gateway_and_survives_restart() {
        let _guard = env_lock();
        let home = TempDir::new().unwrap();
        let ccteam_home = home.path().join(".ccteam");
        let _restore = EnvRestore::install(home.path(), &ccteam_home);
        let paths = fake_paths(home.path());
        std::fs::create_dir_all(&paths.projects_root).unwrap();

        let adapter_state = Arc::new(RecordingState::default());
        let first = spawn_stack(paths.clone(), Arc::clone(&adapter_state)).await;
        let mut socket = connect_chat(first.addr).await;

        send_text(&mut socket, "new-claude", "/new claude reviewer").await;
        recv_reply_contains(&mut socket, "created session s1").await;
        send_text(&mut socket, "new-codex", "/new codex api").await;
        recv_reply_contains(&mut socket, "created session s2").await;
        send_text(&mut socket, "sessions", "/sessions").await;
        let sessions = recv_sessions(&mut socket).await;
        assert!(sessions.iter().any(|item| {
            item.session.as_deref() == Some("s1") && item.vendor.as_deref() == Some("claude")
        }));
        assert!(sessions.iter().any(|item| {
            item.session.as_deref() == Some("s2") && item.vendor.as_deref() == Some("codex")
        }));

        // V0.8.4 P1 (F1): the "submitted … turn …" ack is folded away — only
        // the answer is delivered.
        send_text(&mut socket, "codex-compact", "@api /compact").await;
        recv_replies_containing_all(&mut socket, &["Codex echo: directive:compact"]).await;
        send_text(&mut socket, "claude-review", "@reviewer /review").await;
        recv_replies_containing_all(&mut socket, &["Claude echo: directive:review"]).await;
        drop(socket);
        stop_stack(first).await;

        write_queued_web_outbound(&ccteam_home, "stored while web was offline");
        let second = spawn_stack(paths, Arc::clone(&adapter_state)).await;
        let mut socket = connect_chat(second.addr).await;
        recv_reply_contains(&mut socket, "stored while web was offline").await;
        send_text(&mut socket, "sessions-after-restart", "/sessions").await;
        let sessions = recv_sessions(&mut socket).await;
        assert_eq!(sessions.len(), 2);
        send_text(&mut socket, "after-restart", "@api after restart").await;
        recv_replies_containing_all(&mut socket, &["Codex echo: after restart"]).await;
        drop(socket);
        stop_stack(second).await;

        assert_eq!(adapter_state.starts.load(Ordering::SeqCst), 2);
        assert!(
            adapter_state.resumes.load(Ordering::SeqCst) >= 2,
            "restart should resume persisted Claude and Codex sessions"
        );
        let vendors = adapter_state.started_vendors.lock().await.clone();
        assert!(vendors.contains(&AgentVendor::Claude));
        assert!(vendors.contains(&AgentVendor::Codex));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn web_chat_newproject_scaffolds_registers_and_cd_works() {
        let _guard = env_lock();
        let home = TempDir::new().unwrap();
        let ccteam_home = home.path().join(".ccteam");
        let _restore = EnvRestore::install(home.path(), &ccteam_home);
        let paths = fake_paths(home.path());
        std::fs::create_dir_all(&paths.projects_root).unwrap();

        let adapter_state = Arc::new(RecordingState::default());
        let stack = spawn_stack(paths.clone(), Arc::clone(&adapter_state)).await;
        let mut socket = connect_chat(stack.addr).await;

        // `/newproject <slug> <path>` scaffolds at the path and registers it.
        let proj_dir = home.path().join("code").join("demo");
        send_text(
            &mut socket,
            "newproject",
            &format!("/newproject demo {}", proj_dir.display()),
        )
        .await;
        recv_reply_contains(&mut socket, "created project demo").await;

        assert!(proj_dir.join(".ccteam").join("state.json").exists());
        let config = std::fs::read_to_string(ccteam_home.join("config.yaml")).unwrap();
        assert!(config.contains("demo"), "config.yaml: {config}");
        assert!(config.contains(&proj_dir.display().to_string()));

        // Immediately addressable by /cd in the running daemon.
        send_text(&mut socket, "cd-demo", "/cd demo").await;
        recv_reply_contains(&mut socket, "project set to demo").await;

        drop(socket);
        stop_stack(stack).await;
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn web_chat_sessions_share_the_web_console_pool() {
        // v0.8.18 柱2 档0 — the web console is ONE shared operator pool (until
        // 档1 per-user tokens): a session created from one web chat is visible +
        // addressable from another web chat (the single-user "create on web,
        // drive from phone" flow). IM-created sessions instead stay PRIVATE to
        // their chat — covered by the gateway own-only tests.
        let _guard = env_lock();
        let home = TempDir::new().unwrap();
        let ccteam_home = home.path().join(".ccteam");
        let _restore = EnvRestore::install(home.path(), &ccteam_home);
        let paths = fake_paths(home.path());
        std::fs::create_dir_all(&paths.projects_root).unwrap();

        let adapter_state = Arc::new(RecordingState::default());
        let stack = spawn_stack(paths.clone(), Arc::clone(&adapter_state)).await;

        // chat-1 (web) creates a session.
        let mut s1 = connect_chat_as(stack.addr, "chat-1", "alice").await;
        send_text(&mut s1, "new", "/new claude reviewer").await;
        recv_reply_contains(&mut s1, "created session s1").await;

        // chat-2 (a different web chat) SEES it via the shared user pool and can
        // /use it.
        let mut s2 = connect_chat_as(stack.addr, "chat-2", "bob").await;
        send_text(&mut s2, "sessions", "/sessions").await;
        let listed = recv_sessions(&mut s2).await;
        assert!(
            listed.iter().any(|s| s.session.as_deref() == Some("s1")),
            "web chat-2 should see the shared web-pool session: {listed:?}"
        );
        send_text(&mut s2, "use", "/use s1").await;
        recv_reply_contains(&mut s2, "using session s1").await;

        drop(s1);
        drop(s2);
        stop_stack(stack).await;
    }
}
