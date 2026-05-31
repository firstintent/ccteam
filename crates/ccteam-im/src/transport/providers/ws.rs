//! WebSocket [`Channel`] implementation for local/browser IM e2e tests.
//!
//! Wire protocol:
//!
//! - Client -> server text JSON:
//!   `{"sender":"alice","reply_target":"chat-1","content":"hello"}`
//! - Server -> client text JSON: [`SendMessage`].
//!
//! Plain client text is accepted as `content` with `sender="ws-user"` and
//! `reply_target="ws-chat"` so a manual websocket client can smoke-test the
//! gateway without building a full UI.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::tungstenite::Message;

use crate::transport::{Channel, ChannelMessage, SendMessage};

/// Local WebSocket transport that plugs into the normal daemon Channel path.
#[derive(Debug)]
pub struct WsChannel {
    name: String,
    addr: SocketAddr,
    listener: Arc<Mutex<Option<TcpListener>>>,
    outbound: broadcast::Sender<SendMessage>,
    next_id: AtomicU64,
}

#[derive(Debug, Deserialize)]
struct WsInbound {
    id: Option<String>,
    sender: Option<String>,
    reply_target: Option<String>,
    content: String,
    thread_ts: Option<String>,
}

impl WsChannel {
    /// Bind an ephemeral localhost listener. Tests use this to get a real
    /// socket without racing on port selection.
    pub async fn bind_localhost() -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        Ok(Self::from_listener(listener, addr))
    }

    /// Create a channel that binds `addr` when [`Channel::listen`] starts.
    pub fn bind_on_listen(addr: SocketAddr) -> Self {
        let (outbound, _) = broadcast::channel(64);
        Self {
            name: "ws".to_string(),
            addr,
            listener: Arc::new(Mutex::new(None)),
            outbound,
            next_id: AtomicU64::new(1),
        }
    }

    /// Local address clients should connect to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    fn from_listener(listener: TcpListener, addr: SocketAddr) -> Self {
        let (outbound, _) = broadcast::channel(64);
        Self {
            name: "ws".to_string(),
            addr,
            listener: Arc::new(Mutex::new(Some(listener))),
            outbound,
            next_id: AtomicU64::new(1),
        }
    }

    fn next_message_id(&self, prefix: &str) -> String {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{id}")
    }
}

#[async_trait]
impl Channel for WsChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let id = self.next_message_id("ws-out");
        let _ = self.outbound.send(message.clone());
        Ok(Some(id))
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let listener = {
            let mut guard = self.listener.lock().await;
            match guard.take() {
                Some(listener) => listener,
                None => TcpListener::bind(self.addr).await?,
            }
        };

        loop {
            let (stream, peer) = listener.accept().await?;
            let inbound_tx = tx.clone();
            let outbound_rx = self.outbound.subscribe();
            tokio::spawn(async move {
                if let Err(err) = handle_connection(stream, peer, inbound_tx, outbound_rx).await {
                    tracing::debug!(peer = %peer, error = %err, "ws channel connection closed");
                }
            });
        }
    }

    async fn health_check(&self) -> bool {
        true
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    mut outbound_rx: broadcast::Receiver<SendMessage>,
) -> anyhow::Result<()> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut source) = ws.split();
    let reply_target: Arc<Mutex<Option<String>>> = Arc::default();

    let writer_target = Arc::clone(&reply_target);
    let writer = tokio::spawn(async move {
        while let Ok(message) = outbound_rx.recv().await {
            let target = writer_target.lock().await.clone();
            if target
                .as_deref()
                .is_some_and(|wanted| wanted != message.recipient)
            {
                continue;
            }
            let payload = serde_json::to_string(&message)?;
            if sink.send(Message::Text(payload)).await.is_err() {
                break;
            }
        }
        anyhow::Ok(())
    });

    while let Some(frame) = source.next().await {
        let frame = frame?;
        if frame.is_close() {
            break;
        }
        let Some(message) = parse_frame(frame, peer)? else {
            continue;
        };
        *reply_target.lock().await = Some(message.reply_target.clone());
        if tx.send(message).await.is_err() {
            break;
        }
    }

    writer.abort();
    let _ = writer.await;
    Ok(())
}

fn parse_frame(frame: Message, peer: SocketAddr) -> anyhow::Result<Option<ChannelMessage>> {
    let text = match frame {
        Message::Text(text) => text,
        Message::Binary(bytes) => {
            String::from_utf8(bytes).context("ws binary frame is not utf-8")?
        }
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) | Message::Frame(_) => {
            return Ok(None);
        }
    };
    let inbound = serde_json::from_str::<WsInbound>(&text).unwrap_or_else(|_| WsInbound {
        id: None,
        sender: Some("ws-user".to_string()),
        reply_target: Some("ws-chat".to_string()),
        content: text,
        thread_ts: None,
    });
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sender = inbound.sender.unwrap_or_else(|| peer.to_string());
    let reply_target = inbound
        .reply_target
        .unwrap_or_else(|| "ws-chat".to_string());
    let id = inbound
        .id
        .unwrap_or_else(|| format!("ws-in-{now}-{}", content_hash(&inbound.content)));
    Ok(Some(ChannelMessage {
        id,
        sender,
        reply_target,
        content: inbound.content,
        channel: "ws".to_string(),
        timestamp: now,
        thread_ts: inbound.thread_ts,
    }))
}

fn content_hash(content: &str) -> u64 {
    content.bytes().fold(0_u64, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u64)
    })
}

impl Default for WsChannel {
    fn default() -> Self {
        Self::bind_on_listen(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8765))
    }
}
