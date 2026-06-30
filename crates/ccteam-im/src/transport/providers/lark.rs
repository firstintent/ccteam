//! Lark/Feishu channel — WebSocket long-connection (Path A) + `im/v1/messages`.
//!
//! Ported from `references/openhuman/src/openhuman/channels/providers/lark.rs`
//! (the native WS implementation, NOT a CLI wrapper) with openhuman's
//! `config` / `event_bus` couplings elided — ccteam-im has its own
//! credentials + ACL + sanitize layers, so this provider is a plain
//! `reqwest` + `tokio-tungstenite` client.
//!
//! **Path A only.** The daemon opens an outbound WSS long-connection
//! (`POST /callback/ws/endpoint` -> `wss://…`); there is no public HTTPS
//! endpoint and no webhook/axum receive path. Inbound is text + `post`
//! rich-text + image/file/audio/media (the resource is downloaded via
//! `im/v1/messages/{id}/resources/{key}` and staged to disk like telegram's,
//! then named in the turn text for the agent to `Read`); every other
//! `message_type` is debug-logged and skipped. Outbound replies go via
//! tenant-token `im/v1/messages` — text directly, files by uploading to
//! `im/v1/{images,files}` first and then sending an image/file message.
//!
//! Two independent allowlist layers, by design (mirrors telegram's
//! two-layer model). Both key on the sender **`open_id`** (`ou_…`): the WS
//! loop sets [`ChannelMessage::sender`] to the user's `open_id` (not the
//! chat id), so the daemon ACL compares like-for-like and turns.jsonl /
//! logs record a user, matching telegram/discord/slack. Replies route via
//! [`ChannelMessage::reply_target`] = the `chat_id`.
//! - **provider layer** — the `allowed_users` `open_id` list enforced in
//!   the WS loop via [`LarkChannel::is_user_allowed`]: empty = **deny
//!   all** (fail-closed). An operator who leaves it empty gets a bot
//!   that responds to no one.
//! - **daemon layer** — `AclPolicy.lark_user_ids` (also `open_id`-keyed):
//!   empty = open; a populated list enforces per-user.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use crate::transport::{
    inbound_staging_dir, sanitize_attachment_name, AttachmentKind, Channel, ChannelAttachment,
    ChannelMessage, LarkOpenIdProbe, OutboundFile, OutboundFileKind, SendMessage,
};

const FEISHU_BASE_URL: &str = "https://open.feishu.cn/open-apis";
const FEISHU_WS_BASE_URL: &str = "https://open.feishu.cn";
const LARK_BASE_URL: &str = "https://open.larksuite.com/open-apis";
const LARK_WS_BASE_URL: &str = "https://open.larksuite.com";

/// Conservative per-message ceiling in **UTF-16 code units**. Feishu's
/// text-message hard cap is far larger (~30 kB of bytes), but we mirror
/// telegram's headroom discipline so the outbound splitter is exercised
/// uniformly. This is the *only* place the Lark length constant lives —
/// the daemon reads it polymorphically via [`Channel::max_message_len`]
/// and calls `split_for_channel`; no `"lark"`/number branch leaks up.
const LARK_MAX_TEXT_UTF16: usize = 4000;

/// Inbound/outbound attachment size ceiling, in bytes. Feishu's own caps
/// are 10 MB for `im/v1/images` and 30 MB for `im/v1/files`; we stage
/// inbound downloads up to this ceiling and let the upload endpoints
/// surface their own over-limit error verbatim. Mirrors telegram's
/// `MAX_ATTACHMENT_BYTES` discipline (one constant, here only).
const LARK_MAX_ATTACHMENT_BYTES: u64 = 30 * 1024 * 1024;

// ─────────────────────────────────────────────────────────────────────────────
// Feishu WebSocket long-connection: pbbp2.proto frame codec
//
// Hand-written prost structs (no `.proto`, no `prost-build`, no codegen).
// The non-contiguous tags are deliberate: fields 1-5 then payload at
// tag=8 (6,7 map to real-pbbp2 fields openhuman doesn't model;
// renumbering would break wire-compat with Feishu).
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, prost::Message)]
struct PbHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

/// Feishu WS frame (pbbp2.proto).
/// `method=0` -> CONTROL (ping/pong)  `method=1` -> DATA (events)
#[derive(Clone, PartialEq, prost::Message)]
struct PbFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<PbHeader>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
}

impl PbFrame {
    fn header_value<'a>(&'a self, key: &str) -> &'a str {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
            .unwrap_or("")
    }
}

/// Server-sent client config (parsed from pong payload).
#[derive(Debug, serde::Deserialize, Default, Clone)]
struct WsClientConfig {
    #[serde(rename = "PingInterval")]
    ping_interval: Option<u64>,
}

/// `POST /callback/ws/endpoint` response.
#[derive(Debug, serde::Deserialize)]
struct WsEndpointResp {
    code: i32,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    data: Option<WsEndpoint>,
}

#[derive(Debug, serde::Deserialize)]
struct WsEndpoint {
    #[serde(rename = "URL")]
    url: String,
    #[serde(rename = "ClientConfig")]
    client_config: Option<WsClientConfig>,
}

/// `LarkEvent` envelope (`method=1` / `type=event` payload).
#[derive(Debug, serde::Deserialize)]
struct LarkEvent {
    header: LarkEventHeader,
    event: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct LarkEventHeader {
    event_type: String,
    #[allow(dead_code)]
    #[serde(default)]
    event_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct MsgReceivePayload {
    sender: LarkSender,
    message: LarkMessage,
}

#[derive(Debug, serde::Deserialize)]
struct LarkSender {
    sender_id: LarkSenderId,
    #[serde(default)]
    sender_type: String,
}

#[derive(Debug, serde::Deserialize, Default)]
struct LarkSenderId {
    open_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct LarkMessage {
    /// Stable per-message id; always present on real events. Defaulted so
    /// minimal unit fixtures need not carry it.
    #[serde(default)]
    message_id: String,
    /// Conversation id (`oc_…`); always present on real events. Defaulted
    /// for the same fixture-ergonomics reason.
    #[serde(default)]
    chat_id: String,
    /// `"p2p"` | `"group"`; absent ⇒ treated as non-group (responds).
    #[serde(default)]
    chat_type: String,
    message_type: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    mentions: Vec<serde_json::Value>,
    /// Lark send time in **milliseconds** (string). Absent in some
    /// fixtures; the decode falls back to wall-clock then.
    #[serde(default)]
    create_time: String,
}

/// One inbound Lark message after decode + the group @-mention gate, but
/// *before* the provider/daemon allowlists and dedup. The single source
/// of truth for "how a `im.message.receive_v1` event becomes a
/// [`ChannelMessage`]" — both the live WS loop ([`LarkChannel::listen_ws`])
/// and the tested seam ([`LarkChannel::decode_event`]) flow through it, so
/// the unit tests exercise the exact mapping production runs.
struct DecodedMessage {
    /// Sender user identity (`open_id`, `ou_…`). Becomes
    /// [`ChannelMessage::sender`] — the daemon feeds this to the ACL, so
    /// it must be the *user*, not the chat.
    open_id: String,
    /// Conversation id (`oc_…`). Becomes [`ChannelMessage::reply_target`].
    chat_id: String,
    /// Stable message id; becomes `lark-{message_id}`. Also the path
    /// component for the `im/v1/messages/{id}/resources/{key}` download.
    message_id: String,
    /// Decoded, `@`-placeholder-stripped, trimmed body. Empty is allowed
    /// **only** when `pending` is set (a bare image/file carries no text).
    text: String,
    /// Lark send time in seconds (from `create_time` ms, or wall-clock).
    timestamp: u64,
    /// Set for `image`/`file`/`audio`/`media` messages: the resource the WS
    /// loop must download + stage into a [`ChannelAttachment`]. `None` for
    /// plain text/`post`. Decoding stays pure (no `&self`/network); the
    /// actual download happens in [`LarkChannel::stage_lark_attachment`].
    pending: Option<LarkPending>,
}

/// A Lark message resource (image or file) the WS loop should download.
/// Produced purely from the event by [`pick_lark_attachment`]; the
/// `message_id` it pairs with lives on the [`DecodedMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct LarkPending {
    /// `image_key` (images) or `file_key` (file/audio/media) — both the
    /// `{key}` path component and what selects `type=image` vs `type=file`.
    key: String,
    /// Image vs. generic file (drives [`AttachmentKind`] + the `type` query).
    kind: AttachmentKind,
    /// Best-known name from the event (real `file_name` for files; a
    /// placeholder for images, whose real extension is sniffed post-download).
    file_name: String,
}

/// Decode a single `im.message.receive_v1` event into a [`DecodedMessage`],
/// applying every content/visibility rule the live WS loop applies *except*
/// the allowlists and dedup (those need `&self` / shared state and stay at
/// the call sites). Returns `None` to skip — bot/app sender, missing
/// open_id, unsupported `message_type`, empty body, or a group message the
/// bot wasn't @-mentioned in.
fn decode_message_receive(recv: &MsgReceivePayload) -> Option<DecodedMessage> {
    // Drop the bot's own (and other apps') messages.
    if recv.sender.sender_type == "app" || recv.sender.sender_type == "bot" {
        return None;
    }

    let open_id = recv.sender.sender_id.open_id.as_deref().unwrap_or("");
    if open_id.is_empty() {
        return None;
    }

    let msg = &recv.message;

    // Decode the body by message type. Text/`post` carry a text body;
    // image/file/audio/media carry a downloadable resource and no text.
    let (text, pending) = match msg.message_type.as_str() {
        "text" => {
            let v = serde_json::from_str::<serde_json::Value>(&msg.content).ok()?;
            let t = v
                .get("text")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())?
                .to_string();
            (t, None)
        }
        "post" => (parse_post_content(&msg.content)?, None),
        "image" | "file" | "audio" | "media" => {
            // `?`: a malformed event missing its image_key/file_key is
            // skipped exactly as an unsupported type would be.
            let pending = pick_lark_attachment(&msg.message_type, &msg.content)?;
            (String::new(), Some(pending))
        }
        other => {
            tracing::debug!("Lark: skipping unsupported message type '{other}'");
            return None;
        }
    };

    // Strip the `@_user_N` placeholders Feishu injects, then trim.
    let text = strip_at_placeholders(&text).trim().to_string();
    // A bare image/file carries no text — only reject when there is neither
    // text nor a downloadable attachment.
    if text.is_empty() && pending.is_none() {
        return None;
    }

    // Group chat: only respond when explicitly @-mentioned. Applies to
    // attachments too (a group image with no @-mention is ignored, matching
    // the text rule).
    if msg.chat_type == "group" && !should_respond_in_group(&msg.mentions) {
        return None;
    }

    let timestamp = msg
        .create_time
        .parse::<u64>()
        .ok()
        // Lark timestamps are in milliseconds.
        .map(|ms| ms / 1000)
        .unwrap_or_else(now_secs);

    Some(DecodedMessage {
        open_id: open_id.to_string(),
        chat_id: msg.chat_id.clone(),
        message_id: msg.message_id.clone(),
        text,
        timestamp,
        pending,
    })
}

impl DecodedMessage {
    /// Build the daemon-facing [`ChannelMessage`].
    ///
    /// `sender` is the user `open_id` (so the daemon-layer
    /// `AclPolicy.lark_user_ids` — documented as `open_id`s — compares
    /// like-for-like, and turns.jsonl / handle-prefix / logs record a user,
    /// matching telegram/discord/slack). `reply_target` is the `chat_id`, so
    /// outbound replies still route to the conversation.
    fn into_channel_message(self) -> ChannelMessage {
        ChannelMessage {
            id: format!("lark-{}", self.message_id),
            sender: self.open_id,
            reply_target: self.chat_id,
            content: self.text,
            channel: "lark".to_string(),
            timestamp: self.timestamp,
            thread_ts: None,
            attachments: Vec::new(),
            selection: None,
        }
    }
}

/// Wall-clock seconds since the Unix epoch (saturating).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Heartbeat timeout for the WS connection — must be larger than
/// `ping_interval` (default 120 s). If no binary frame (pong or event)
/// arrives within this window, reconnect.
const WS_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(300);

/// Returns true when the WebSocket frame indicates live traffic that
/// should refresh the heartbeat watchdog.
fn should_refresh_last_recv(msg: &WsMsg) -> bool {
    matches!(msg, WsMsg::Binary(_) | WsMsg::Ping(_) | WsMsg::Pong(_))
}

/// Lark/Feishu channel (WS long-connection, Path A).
pub struct LarkChannel {
    app_id: String,
    app_secret: String,
    allowed_users: Vec<String>,
    /// When true, use Feishu (CN) endpoints; when false, Lark (intl).
    use_feishu: bool,
    /// One reqwest client built once in [`LarkChannel::new`] and reused
    /// at every call site (token fetch, ws-endpoint POST, send).
    http: reqwest::Client,
    /// Cached tenant access token.
    tenant_token: Arc<RwLock<Option<String>>>,
    /// Dedup set: WS `message_id`s seen in the last ~30 min to prevent
    /// double-dispatch.
    ws_seen_ids: Arc<RwLock<HashMap<String, Instant>>>,
    /// Optional setup-helper JSONL path. Unauthorized sender `open_id`s are
    /// appended here so the web Settings page can surface them without asking
    /// users to inspect daemon logs. This does NOT authorize the message.
    open_id_probe_path: Option<PathBuf>,
    name: String,
}

impl LarkChannel {
    /// Build with the app credentials, the provider-level `open_id`
    /// allowlist (empty = deny all; `"*"` = open), and the region flag.
    pub fn new(
        app_id: String,
        app_secret: String,
        allowed_users: Vec<String>,
        use_feishu: bool,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            allowed_users,
            use_feishu,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            tenant_token: Arc::new(RwLock::new(None)),
            ws_seen_ids: Arc::new(RwLock::new(HashMap::new())),
            open_id_probe_path: None,
            name: "lark".to_string(),
        }
    }

    /// v0.8.20 F2 — override the channel-map key (`"lark@<tenant_id>"`) for a
    /// per-tenant bot (see [`super::telegram::TelegramChannel::with_name`]).
    pub fn with_name(mut self, name: String) -> Self {
        self.name = name;
        self
    }

    /// Record unauthorized sender `open_id`s to this JSONL path. The daemon
    /// sets this for real channels; tests and standalone providers may leave
    /// it unset.
    pub fn with_open_id_probe_path(mut self, path: PathBuf) -> Self {
        self.open_id_probe_path = Some(path);
        self
    }

    fn api_base(&self) -> &'static str {
        if self.use_feishu {
            FEISHU_BASE_URL
        } else {
            LARK_BASE_URL
        }
    }

    fn ws_base(&self) -> &'static str {
        if self.use_feishu {
            FEISHU_WS_BASE_URL
        } else {
            LARK_WS_BASE_URL
        }
    }

    fn tenant_access_token_url(&self) -> String {
        format!("{}/auth/v3/tenant_access_token/internal", self.api_base())
    }

    fn send_message_url(&self) -> String {
        format!("{}/im/v1/messages?receive_id_type=chat_id", self.api_base())
    }

    /// `POST` URL to add a reaction to a message
    /// (`im/v1/messages/{message_id}/reactions`).
    fn add_reaction_url(&self, message_id: &str) -> String {
        format!("{}/im/v1/messages/{message_id}/reactions", self.api_base())
    }

    /// `DELETE` URL to remove a reaction by its `reaction_id`
    /// (`im/v1/messages/{message_id}/reactions/{reaction_id}`).
    fn delete_reaction_url(&self, message_id: &str, reaction_id: &str) -> String {
        format!(
            "{}/im/v1/messages/{message_id}/reactions/{reaction_id}",
            self.api_base()
        )
    }

    /// `POST /callback/ws/endpoint` -> (wss_url, client_config).
    async fn get_ws_endpoint(&self) -> anyhow::Result<(String, WsClientConfig)> {
        let resp = self
            .http
            .post(format!("{}/callback/ws/endpoint", self.ws_base()))
            .header("locale", if self.use_feishu { "zh" } else { "en" })
            .json(&serde_json::json!({
                "AppID": self.app_id,
                "AppSecret": self.app_secret,
            }))
            .send()
            .await?
            .json::<WsEndpointResp>()
            .await?;
        if resp.code != 0 {
            anyhow::bail!(
                "Lark WS endpoint failed: code={} msg={}",
                resp.code,
                resp.msg.as_deref().unwrap_or("(none)")
            );
        }
        let ep = resp
            .data
            .ok_or_else(|| anyhow::anyhow!("Lark WS endpoint: empty data"))?;
        Ok((ep.url, ep.client_config.unwrap_or_default()))
    }

    /// WS long-connection event loop. Returns `Ok(())` when the
    /// connection closes; the [`Channel::listen`] wrapper reconnects.
    ///
    /// Ported from openhuman's native frame loop: fetch the endpoint,
    /// `connect_async`, ping/pong heartbeat with a watchdog, ACK every
    /// DATA frame within Feishu's 3 s window, reassemble fragments,
    /// dedup by `message_id`, parse text/`post`, apply the provider-level
    /// allowlist + group @-mention gate, then push a [`ChannelMessage`].
    #[allow(clippy::too_many_lines)]
    async fn listen_ws(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let (wss_url, client_config) = self.get_ws_endpoint().await?;
        let service_id = wss_url
            .split('?')
            .nth(1)
            .and_then(|qs| {
                qs.split('&')
                    .find(|kv| kv.starts_with("service_id="))
                    .and_then(|kv| kv.split('=').nth(1))
                    .and_then(|v| v.parse::<i32>().ok())
            })
            .unwrap_or(0);
        tracing::info!("Lark: connecting to {wss_url}");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&wss_url).await?;
        let (mut write, mut read) = ws_stream.split();
        tracing::info!("Lark: WS connected (service_id={service_id})");

        let mut ping_secs = client_config.ping_interval.unwrap_or(120).max(10);
        let mut hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
        let mut timeout_check = tokio::time::interval(Duration::from_secs(10));
        hb_interval.tick().await; // consume immediate tick

        let mut seq: u64 = 0;
        let mut last_recv = Instant::now();

        // Send an initial ping immediately (like the official SDK) so the
        // server starts responding with pongs and we can calibrate the
        // ping interval.
        seq = seq.wrapping_add(1);
        let initial_ping = PbFrame {
            seq_id: seq,
            log_id: 0,
            service: service_id,
            method: 0,
            headers: vec![PbHeader {
                key: "type".into(),
                value: "ping".into(),
            }],
            payload: None,
        };
        if write
            .send(WsMsg::Binary(initial_ping.encode_to_vec()))
            .await
            .is_err()
        {
            anyhow::bail!("Lark: initial ping failed");
        }
        // message_id -> (fragment_slots, created_at) for multi-part reassembly.
        type FragEntry = (Vec<Option<Vec<u8>>>, Instant);
        let mut frag_cache: HashMap<String, FragEntry> = HashMap::new();

        loop {
            tokio::select! {
                biased;

                _ = hb_interval.tick() => {
                    seq = seq.wrapping_add(1);
                    let ping = PbFrame {
                        seq_id: seq, log_id: 0, service: service_id, method: 0,
                        headers: vec![PbHeader { key: "type".into(), value: "ping".into() }],
                        payload: None,
                    };
                    if write.send(WsMsg::Binary(ping.encode_to_vec())).await.is_err() {
                        tracing::warn!("Lark: ping failed, reconnecting");
                        break;
                    }
                    // GC stale fragments > 5 min.
                    let cutoff = Instant::now()
                        .checked_sub(Duration::from_secs(300))
                        .unwrap_or_else(Instant::now);
                    frag_cache.retain(|_, (_, ts)| *ts > cutoff);
                }

                _ = timeout_check.tick() => {
                    if last_recv.elapsed() > WS_HEARTBEAT_TIMEOUT {
                        tracing::warn!("Lark: heartbeat timeout, reconnecting");
                        break;
                    }
                }

                msg = read.next() => {
                    let raw = match msg {
                        Some(Ok(ws_msg)) => {
                            if should_refresh_last_recv(&ws_msg) {
                                last_recv = Instant::now();
                            }
                            match ws_msg {
                                WsMsg::Binary(b) => b,
                                WsMsg::Ping(d) => { let _ = write.send(WsMsg::Pong(d)).await; continue; }
                                WsMsg::Pong(_) => continue,
                                WsMsg::Close(_) => { tracing::info!("Lark: WS closed — reconnecting"); break; }
                                _ => continue,
                            }
                        }
                        None => { tracing::info!("Lark: WS closed — reconnecting"); break; }
                        Some(Err(e)) => { tracing::error!("Lark: WS read error: {e}"); break; }
                    };

                    let frame = match PbFrame::decode(&raw[..]) {
                        Ok(f) => f,
                        Err(e) => { tracing::error!("Lark: proto decode: {e}"); continue; }
                    };

                    // CONTROL frame.
                    if frame.method == 0 {
                        if frame.header_value("type") == "pong" {
                            if let Some(p) = &frame.payload {
                                if let Ok(cfg) = serde_json::from_slice::<WsClientConfig>(p) {
                                    if let Some(secs) = cfg.ping_interval {
                                        let secs = secs.max(10);
                                        if secs != ping_secs {
                                            ping_secs = secs;
                                            hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
                                            tracing::info!("Lark: ping_interval -> {ping_secs}s");
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // DATA frame.
                    let msg_type = frame.header_value("type").to_string();
                    let msg_id   = frame.header_value("message_id").to_string();
                    let sum      = frame.header_value("sum").parse::<usize>().unwrap_or(1);
                    let seq_num  = frame.header_value("seq").parse::<usize>().unwrap_or(0);

                    // ACK immediately (Feishu requires within 3 s): echo the
                    // frame back with a 200-ok payload and a biz_rt header.
                    {
                        let mut ack = frame.clone();
                        ack.payload = Some(br#"{"code":200,"headers":{},"data":[]}"#.to_vec());
                        ack.headers.push(PbHeader { key: "biz_rt".into(), value: "0".into() });
                        let _ = write.send(WsMsg::Binary(ack.encode_to_vec())).await;
                    }

                    // Fragment reassembly.
                    let sum = if sum == 0 { 1 } else { sum };
                    let payload: Vec<u8> = if sum == 1 || msg_id.is_empty() || seq_num >= sum {
                        frame.payload.clone().unwrap_or_default()
                    } else {
                        let entry = frag_cache.entry(msg_id.clone())
                            .or_insert_with(|| (vec![None; sum], Instant::now()));
                        if entry.0.len() != sum { *entry = (vec![None; sum], Instant::now()); }
                        entry.0[seq_num] = frame.payload.clone();
                        if entry.0.iter().all(|s| s.is_some()) {
                            let full: Vec<u8> = entry.0.iter()
                                .flat_map(|s| s.as_deref().unwrap_or(&[]))
                                .copied().collect();
                            frag_cache.remove(&msg_id);
                            full
                        } else { continue; }
                    };

                    if msg_type != "event" { continue; }

                    // Decode through the single shared seam so this live path
                    // and the unit tests run the *same* mapping (sender_type
                    // filter, text/post extraction, @-placeholder strip, group
                    // @-mention gate). ACL + dedup stay here — they need
                    // `&self` state, not pure event data.
                    let Some(decoded) = self.decode_event(&payload) else { continue };

                    if !self.is_user_allowed(&decoded.open_id) {
                        self.record_open_id_probe(&decoded).await;
                        tracing::warn!("Lark WS: ignoring {} (not in allowed_users)", decoded.open_id);
                        continue;
                    }

                    // Dedup. Scope the write guard so it is dropped BEFORE
                    // the `tx.send(..).await` below (no guard held across an
                    // await point).
                    {
                        let now = Instant::now();
                        let mut seen = self.ws_seen_ids.write().await;
                        seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(30 * 60));
                        if seen.contains_key(&decoded.message_id) {
                            tracing::debug!("Lark WS: dup {}", decoded.message_id);
                            continue;
                        }
                        seen.insert(decoded.message_id.clone(), now);
                    }

                    // Pull the resource descriptor + raw message id out before
                    // the move: the download needs `&self` + the cached token,
                    // so it can't live in the pure decode.
                    let pending = decoded.pending.clone();
                    let raw_message_id = decoded.message_id.clone();
                    let mut channel_msg = decoded.into_channel_message();
                    // v0.8.20 F2 — stamp THIS bot's channel key so a per-tenant
                    // lark bot's inbound routes to its tenant + replies come back
                    // through it (not a colliding shared "lark").
                    channel_msg.channel = self.name.clone();
                    if let Some(p) = pending {
                        match self.stage_lark_attachment(&channel_msg.id, &raw_message_id, &p).await {
                            Ok(Some(att)) => channel_msg.attachments.push(att),
                            Ok(None) => {
                                let mb = LARK_MAX_ATTACHMENT_BYTES / (1024 * 1024);
                                let _ = self.send(&SendMessage::new(
                                    format!("⚠️ 附件 {} 超过 {mb}MB 上限,已拒收", p.file_name),
                                    channel_msg.reply_target.clone(),
                                )).await;
                                if channel_msg.content.is_empty() { continue; }
                            }
                            Err(e) => {
                                tracing::warn!(message_id = %raw_message_id, error = %e, "Lark: attachment download failed");
                                let _ = self.send(&SendMessage::new(
                                    format!("⚠️ 附件 {} 下载失败", p.file_name),
                                    channel_msg.reply_target.clone(),
                                )).await;
                                if channel_msg.content.is_empty() { continue; }
                            }
                        }
                    }
                    tracing::debug!("Lark WS: message in {}", channel_msg.reply_target);
                    if tx.send(channel_msg).await.is_err() { break; }
                }
            }
        }
        Ok(())
    }

    /// Check whether a user `open_id` is allowed (provider layer:
    /// empty = deny all, `"*"` = open).
    fn is_user_allowed(&self, open_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == open_id)
    }

    async fn record_open_id_probe(&self, decoded: &DecodedMessage) {
        let Some(path) = self.open_id_probe_path.as_ref() else {
            return;
        };
        let probe = LarkOpenIdProbe {
            channel: self.name.clone(),
            open_id: decoded.open_id.clone(),
            chat_id: decoded.chat_id.clone(),
            message_id: decoded.message_id.clone(),
            timestamp: decoded.timestamp,
        };
        let line = match serde_json::to_string(&probe) {
            Ok(line) => format!("{line}\n"),
            Err(err) => {
                tracing::warn!(error = %err, "Lark open_id probe encode failed");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::warn!(
                    path = %parent.display(),
                    error = %err,
                    "Lark open_id probe dir create failed"
                );
                return;
            }
        }
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(line.as_bytes()).await {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "Lark open_id probe append failed"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Lark open_id probe open failed"
                );
            }
        }
    }

    /// Get or refresh the tenant access token (cached).
    async fn get_tenant_access_token(&self) -> anyhow::Result<String> {
        // Check cache first — scope the read guard out before any await.
        {
            let cached = self.tenant_token.read().await;
            if let Some(ref token) = *cached {
                return Ok(token.clone());
            }
        }

        let url = self.tenant_access_token_url();
        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret,
        });

        let resp = self.http.post(&url).json(&body).send().await?;
        let data: serde_json::Value = resp.json().await?;

        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = data
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("Lark tenant_access_token failed: {msg}");
        }

        let token = data
            .get("tenant_access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing tenant_access_token in response"))?
            .to_string();

        // Cache it — scope the write guard.
        {
            let mut cached = self.tenant_token.write().await;
            *cached = Some(token.clone());
        }

        Ok(token)
    }

    /// Invalidate the cached token (called on a 401).
    async fn invalidate_token(&self) {
        let mut cached = self.tenant_token.write().await;
        *cached = None;
    }

    /// Issue a tenant-token-authorized JSON request, transparently handling
    /// the `tenant_access_token` ~2 h expiry: on a `401` it invalidates the
    /// cache, re-fetches the token, and retries the request exactly once.
    ///
    /// The single token-retry path for every authorized call — `send` and
    /// `edit_message` both route through it, so a long-running bot whose
    /// outbound traffic is *only* progress edits still self-heals (the edit
    /// path no longer fails permanently once the cached token ages out).
    async fn send_json_with_token_retry(
        &self,
        method: reqwest::Method,
        url: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<reqwest::Response> {
        let build = |token: &str| {
            self.http
                .request(method.clone(), url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json; charset=utf-8")
                .json(body)
        };

        let token = self.get_tenant_access_token().await?;
        let resp = build(&token).send().await?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }

        // Token expired — invalidate, refresh, retry once.
        self.invalidate_token().await;
        let new_token = self.get_tenant_access_token().await?;
        Ok(build(&new_token).send().await?)
    }

    /// Issue a tenant-token-authorized **GET** with the same one-shot
    /// 401-refresh-retry as [`Self::send_json_with_token_retry`]. Used for
    /// the inbound resource download (`im/v1/messages/{id}/resources/{key}`),
    /// which returns raw bytes rather than JSON.
    async fn get_with_token_retry(&self, url: &str) -> anyhow::Result<reqwest::Response> {
        let token = self.get_tenant_access_token().await?;
        let resp = self.http.get(url).bearer_auth(&token).send().await?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        self.invalidate_token().await;
        let new_token = self.get_tenant_access_token().await?;
        Ok(self.http.get(url).bearer_auth(&new_token).send().await?)
    }

    /// Issue a tenant-token-authorized **multipart POST** with the same
    /// one-shot 401-refresh-retry. `make_form` is invoked once per attempt
    /// because `reqwest::multipart::Form` isn't `Clone` (it's consumed on
    /// send), so the retry rebuilds it. Backs the `im/v1/{images,files}`
    /// uploads.
    async fn post_multipart_with_token_retry(
        &self,
        url: &str,
        make_form: impl Fn() -> reqwest::multipart::Form,
    ) -> anyhow::Result<reqwest::Response> {
        let token = self.get_tenant_access_token().await?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .multipart(make_form())
            .send()
            .await?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        self.invalidate_token().await;
        let new_token = self.get_tenant_access_token().await?;
        Ok(self
            .http
            .post(url)
            .bearer_auth(&new_token)
            .multipart(make_form())
            .send()
            .await?)
    }

    /// Download one inbound [`LarkPending`] resource and stage it to disk as
    /// a [`ChannelAttachment`]. Returns `Ok(None)` when the payload exceeds
    /// [`LARK_MAX_ATTACHMENT_BYTES`] (rejected, not an error). Images get a
    /// magic-byte-sniffed extension so the agent's `Read` renders them; files
    /// keep their event-supplied (sanitized) name. The staged file lives at
    /// `<staging>/<cid>-<sanitized_name>`, identical to telegram's layout.
    async fn stage_lark_attachment(
        &self,
        cid: &str,
        message_id: &str,
        pending: &LarkPending,
    ) -> anyhow::Result<Option<ChannelAttachment>> {
        let type_param = match pending.kind {
            AttachmentKind::Image => "image",
            AttachmentKind::File => "file",
        };
        let url = format!(
            "{}/im/v1/messages/{}/resources/{}?type={}",
            self.api_base(),
            message_id,
            pending.key,
            type_param
        );
        let resp = self.get_with_token_retry(&url).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Lark resource download {message_id}/{} → {status}: {body}",
                pending.key
            );
        }
        let bytes = resp.bytes().await?.to_vec();
        if bytes.len() as u64 > LARK_MAX_ATTACHMENT_BYTES {
            return Ok(None);
        }
        // Images carry no usable name/extension on the wire; sniff it so the
        // staged path is `image.png`/`.jpg`/… and `Read` renders it. Files
        // keep the real event name.
        let raw_name = match pending.kind {
            AttachmentKind::Image => format!("image{}", image_ext(&bytes)),
            AttachmentKind::File => pending.file_name.clone(),
        };
        let safe_name = sanitize_attachment_name(&raw_name);
        let dir = inbound_staging_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let dest = dir.join(format!("{cid}-{safe_name}"));
        tokio::fs::write(&dest, &bytes).await?;
        Ok(Some(ChannelAttachment {
            kind: pending.kind,
            file_name: safe_name,
            local_path: dest.to_string_lossy().into_owned(),
            mime: None,
            size: Some(bytes.len() as u64),
        }))
    }

    /// Send one already-built message: `msg_type` + the Feishu-stringified
    /// `content` JSON. The single outbound funnel for text and media — both
    /// route through [`Self::send_json_with_token_retry`] (so a stale tenant
    /// token self-heals) and [`parse_sent_message_id`].
    async fn send_raw(
        &self,
        recipient: &str,
        msg_type: &str,
        content: String,
    ) -> anyhow::Result<Option<String>> {
        let url = self.send_message_url();
        let body = serde_json::json!({
            "receive_id": recipient,
            "msg_type": msg_type,
            "content": content,
        });
        let resp = self
            .send_json_with_token_retry(reqwest::Method::POST, &url, &body)
            .await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lark send ({msg_type}) failed: {err}");
        }
        Ok(parse_sent_message_id(resp).await)
    }

    /// Outbound files (V0.8.4 P2b — `chat_send_file`). Feishu image/file
    /// messages carry no inline caption, so the effective caption (the
    /// attachment's own, else the message text on the first file — matching
    /// telegram's rule) is delivered as its own preceding text message.
    /// Returns the first sent message id.
    async fn send_with_attachments(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let mut first_id = None;
        for (i, att) in message.attachments.iter().enumerate() {
            let caption = att.caption.clone().or_else(|| {
                if i == 0 && !message.content.is_empty() {
                    Some(message.content.clone())
                } else {
                    None
                }
            });
            if let Some(cap) = caption.filter(|c| !c.is_empty()) {
                let content = serde_json::json!({ "text": cap }).to_string();
                let id = self.send_raw(&message.recipient, "text", content).await?;
                first_id = first_id.or(id);
            }
            let id = self.send_one_attachment(&message.recipient, att).await?;
            first_id = first_id.or(id);
        }
        Ok(first_id)
    }

    /// Upload one [`OutboundFile`] then post it as an `image`/`file` message.
    async fn send_one_attachment(
        &self,
        recipient: &str,
        att: &OutboundFile,
    ) -> anyhow::Result<Option<String>> {
        let bytes = tokio::fs::read(&att.path)
            .await
            .with_context(|| format!("read outbound file {}", att.path))?;
        let file_name = std::path::Path::new(&att.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let (msg_type, content) = match att.kind {
            OutboundFileKind::Photo => {
                let image_key = self.upload_image(&bytes).await?;
                ("image", serde_json::json!({ "image_key": image_key }))
            }
            OutboundFileKind::Document => {
                let file_key = self.upload_file(&bytes, &file_name).await?;
                ("file", serde_json::json!({ "file_key": file_key }))
            }
        };
        self.send_raw(recipient, msg_type, content.to_string())
            .await
    }

    /// Upload an image to `im/v1/images` (`image_type=message`) → `image_key`.
    async fn upload_image(&self, bytes: &[u8]) -> anyhow::Result<String> {
        let url = format!("{}/im/v1/images", self.api_base());
        let make_form = || {
            reqwest::multipart::Form::new()
                .text("image_type", "message")
                .part(
                    "image",
                    reqwest::multipart::Part::bytes(bytes.to_vec()).file_name("image"),
                )
        };
        let resp = self
            .post_multipart_with_token_retry(&url, make_form)
            .await?;
        parse_upload_key(resp, "image_key").await
    }

    /// Upload a file to `im/v1/files` (`file_type=stream`) → `file_key`.
    async fn upload_file(&self, bytes: &[u8], file_name: &str) -> anyhow::Result<String> {
        let url = format!("{}/im/v1/files", self.api_base());
        let name = file_name.to_string();
        let make_form = || {
            reqwest::multipart::Form::new()
                .text("file_type", "stream")
                .text("file_name", name.clone())
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(name.clone()),
                )
        };
        let resp = self
            .post_multipart_with_token_retry(&url, make_form)
            .await?;
        parse_upload_key(resp, "file_key").await
    }

    /// Decode one `im.message.receive_v1` event into the [`ChannelMessage`]
    /// the daemon receives, applying the provider-layer allowlist — i.e.
    /// the exact `decode → map → ACL` the live WS loop runs, minus dedup
    /// (which needs the shared `ws_seen_ids` mutable state). Returns `None`
    /// for anything the loop would `continue` past: wrong event type, bot
    /// sender, missing/disallowed `open_id`, unsupported `message_type`,
    /// empty body, or an un-@-mentioned group message.
    ///
    /// This is the tested seam, and it shares [`decode_message_receive`] +
    /// [`DecodedMessage::into_channel_message`] with [`Self::listen_ws`], so
    /// the unit tests cover the same parser production runs (the two can no
    /// longer drift). Takes a parsed `serde_json::Value` for test ergonomics;
    /// the live loop hands it the WS frame's JSON bytes via
    /// [`Self::decode_event`]. `#[cfg(test)]` because the live loop calls the
    /// shared decode directly — this is purely the value-typed test entry.
    #[cfg(test)]
    fn decode_event_value(&self, payload: &serde_json::Value) -> Option<ChannelMessage> {
        let event: LarkEvent = serde_json::from_value(payload.clone()).ok()?;
        if event.header.event_type != "im.message.receive_v1" {
            return None;
        }
        let recv: MsgReceivePayload = serde_json::from_value(event.event).ok()?;
        let decoded = decode_message_receive(&recv)?;
        if !self.is_user_allowed(&decoded.open_id) {
            tracing::warn!(
                "Lark: ignoring message from unauthorized user: {}",
                decoded.open_id
            );
            return None;
        }
        let mut cm = decoded.into_channel_message();
        cm.channel = self.name.clone();
        Some(cm)
    }

    /// Decode a WS frame's event-JSON bytes into a [`DecodedMessage`]
    /// (pre-ACL, pre-dedup). The live loop's entry into the shared parser.
    fn decode_event(&self, payload: &[u8]) -> Option<DecodedMessage> {
        let event: LarkEvent = match serde_json::from_slice(payload) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("Lark: event JSON: {e}");
                return None;
            }
        };
        if event.header.event_type != "im.message.receive_v1" {
            return None;
        }
        let recv: MsgReceivePayload = match serde_json::from_value(event.event) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Lark: payload parse: {e}");
                return None;
            }
        };
        decode_message_receive(&recv)
    }
}

#[async_trait]
impl Channel for LarkChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        // V0.8.4 P2b — files go via `im/v1/{images,files}` upload + an
        // image/file message; the caption rides the first attachment.
        if !message.attachments.is_empty() {
            return self.send_with_attachments(message).await;
        }
        // Feishu quirk: `content` is a STRING containing JSON, not a
        // nested object.
        let content = serde_json::json!({ "text": message.content }).to_string();
        self.send_raw(&message.recipient, "text", content).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Reconnect loop lives INSIDE `listen` because the daemon
        // supervisor never restarts a listener — it just logs when this
        // returns. `tx.is_closed()` is the only graceful-stop signal
        // (matches the trait contract: `Ok(())` solely when the sender
        // is dropped); every other exit reconnects after a 5 s backoff.
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            if let Err(e) = self.listen_ws(tx.clone()).await {
                tracing::warn!(error = %e, "lark: WS loop errored; reconnecting in 5s");
            } else {
                tracing::info!("lark: WS loop ended; reconnecting in 5s");
            }
            if tx.is_closed() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn health_check(&self) -> bool {
        self.get_tenant_access_token().await.is_ok()
    }

    fn max_message_len(&self) -> Option<usize> {
        Some(LARK_MAX_TEXT_UTF16)
    }

    async fn edit_message(
        &self,
        _recipient: &str,
        message_id: &str,
        content: &str,
    ) -> anyhow::Result<Option<String>> {
        // Lark addresses the edit by `message_id` in the URL path, not by
        // recipient, so `recipient` is unused here.
        let url = format!("{}/im/v1/messages/{message_id}", self.api_base());
        // Same Feishu quirk as `send`: the inner value is stringified JSON.
        let inner = serde_json::json!({ "text": content }).to_string();
        let body = serde_json::json!({
            "msg_type": "text",
            "content": inner,
        });
        // Shared token-retry: a stale tenant token (~2 h) is invalidated +
        // refreshed + retried once here too, so repeated progress edits
        // don't fail permanently once the cache ages out.
        let resp = self
            .send_json_with_token_retry(reqwest::Method::PUT, &url, &body)
            .await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lark edit_message {message_id} failed: {err}");
        }
        // The message id is stable; echo it back for the daemon's status
        // bookkeeping.
        Ok(Some(message_id.to_string()))
    }

    /// Add the 👀-equivalent ack reaction. Feishu has no plain "EYES" in its
    /// fixed set, so we use **`OnIt`** — the semantic "on it / seen, working"
    /// reaction (the closest match to the 👀 "received, processing" intent).
    /// STATEFUL: Feishu returns a `reaction_id` we must keep to delete it
    /// later, so we hand it back as the opaque handle. Reuses the same
    /// `tenant_access_token` + one-shot 401-retry as `send`. Any API/parse
    /// failure surfaces here; the daemon egress swallows it (fire-and-forget).
    async fn add_reaction(
        &self,
        _chat_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<String>> {
        // Feishu addresses the message by id in the URL path; the chat id is
        // not needed (mirrors `edit_message`).
        let url = self.add_reaction_url(message_id);
        let body = reaction_create_body(LARK_ACK_EMOJI_TYPE);
        let resp = self
            .send_json_with_token_retry(reqwest::Method::POST, &url, &body)
            .await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lark add_reaction {message_id} failed: {err}");
        }
        // The reaction_id is the handle remove_reaction needs; a missing one is
        // logged but not fatal (the reaction WAS created — we just can't delete
        // it, so it lingers; better than failing the ack).
        let reaction_id = parse_reaction_id(resp).await;
        if reaction_id.is_none() {
            tracing::warn!(
                message_id = %message_id,
                "Lark add_reaction: no reaction_id in response (reaction may linger)"
            );
        }
        Ok(reaction_id)
    }

    /// Remove the ack reaction by its `reaction_id` (the `handle`
    /// [`Self::add_reaction`] returned). A `None` handle is a no-op (Feishu
    /// can only delete a reaction by its id). Reuses the tenant-token auth.
    async fn remove_reaction(
        &self,
        _chat_id: &str,
        message_id: &str,
        handle: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(reaction_id) = handle else {
            // No reaction_id (add failed / response had none) — nothing to do.
            return Ok(());
        };
        let url = self.delete_reaction_url(message_id, reaction_id);
        // DELETE carries no body; reuse the token-retry helper with an empty
        // JSON object (the API ignores it).
        let resp = self
            .send_json_with_token_retry(reqwest::Method::DELETE, &url, &serde_json::json!({}))
            .await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lark remove_reaction {message_id}/{reaction_id} failed: {err}");
        }
        Ok(())
    }
}

/// Feishu emoji_type for the 👀 "received, processing" ack. Feishu's fixed
/// emoji set has no plain `EYES`; `OnIt` is the semantic "on it / seen,
/// working" reaction — the closest match to the 👀 ack intent. (See the
/// Feishu emoji introduction doc; `GLANCE`/`LOOKDOWN` are the only other
/// looking-adjacent codes, but `OnIt` carries the "received, working" meaning.)
const LARK_ACK_EMOJI_TYPE: &str = "OnIt";

// ─────────────────────────────────────────────────────────────────────────────
// WS helper functions (pure — unit-tested in lark_tests.rs)
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the Feishu `im/v1/messages` success body for the new message id.
/// Returns `Ok(None)` (NOT an error) when the id can't be parsed — the
/// send already succeeded. Success shape:
/// `{"code":0,"msg":"success","data":{"message_id":"om_xxx"}}`.
async fn parse_sent_message_id(resp: reqwest::Response) -> Option<String> {
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    v.pointer("/data/message_id")
        .and_then(|m| m.as_str())
        .map(String::from)
}

/// Build the `im/v1/messages/{id}/reactions` create body (pure + isolated so
/// the `reaction_type.emoji_type` shape is unit-testable without a live API).
/// Feishu wants `{"reaction_type":{"emoji_type":"<TYPE>"}}`.
fn reaction_create_body(emoji_type: &str) -> serde_json::Value {
    serde_json::json!({ "reaction_type": { "emoji_type": emoji_type } })
}

/// Parse the `data.reaction_id` from a successful reaction-create response.
/// Returns `Ok(None)`-shaped `None` (NOT an error) when absent — the reaction
/// was already created; we just can't address it for deletion. Success shape:
/// `{"code":0,"data":{"reaction_id":"ZCaCIjUBVU...", ...}}`.
async fn parse_reaction_id(resp: reqwest::Response) -> Option<String> {
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    v.pointer("/data/reaction_id")
        .and_then(|m| m.as_str())
        .map(String::from)
}

/// Consume an `im/v1/{images,files}` upload response and return the resource
/// key (`image_key`/`file_key`). Feishu replies HTTP 200 with a `code` field,
/// so a non-zero `code` is the real error signal (surfaced with its `msg`).
async fn parse_upload_key(resp: reqwest::Response, key: &str) -> anyhow::Result<String> {
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Lark upload ({key}) → {status}: {v}");
    }
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("Lark upload ({key}) failed: {msg}");
    }
    parse_resource_key(&v, key).ok_or_else(|| anyhow::anyhow!("Lark upload: missing {key}"))
}

/// Pure: pluck `data.{key}` (an `image_key`/`file_key`) from an upload body.
fn parse_resource_key(v: &serde_json::Value, key: &str) -> Option<String> {
    v.pointer(&format!("/data/{key}"))
        .and_then(|k| k.as_str())
        .map(String::from)
}

/// Pure: parse a Feishu message `content` (itself a JSON *string*) and pluck
/// a string field. Feishu stringifies message content, so this double-parses.
fn json_content_str(content: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()?
        .get(key)?
        .as_str()
        .map(String::from)
}

/// Pure: map an inbound media message (`message_type` + stringified
/// `content`) to the [`LarkPending`] resource to download, or `None` when the
/// required key is absent. Images key on `image_key`; file/audio/media key on
/// `file_key` (with the optional `file_name`).
fn pick_lark_attachment(message_type: &str, content: &str) -> Option<LarkPending> {
    match message_type {
        "image" => Some(LarkPending {
            key: json_content_str(content, "image_key")?,
            kind: AttachmentKind::Image,
            file_name: "image".to_string(),
        }),
        "file" | "audio" | "media" => Some(LarkPending {
            key: json_content_str(content, "file_key")?,
            kind: AttachmentKind::File,
            file_name: json_content_str(content, "file_name")
                .unwrap_or_else(|| default_media_name(message_type)),
        }),
        _ => None,
    }
}

/// Fallback name for a file/audio/media message that omitted `file_name`.
fn default_media_name(message_type: &str) -> String {
    match message_type {
        "audio" => "audio.opus".to_string(),
        "media" => "video.mp4".to_string(),
        _ => "file".to_string(),
    }
}

/// Pure: sniff a common image format from its magic bytes, returning the
/// dotted extension (defaulting to `.jpg` — Feishu images are usually JPEG) so
/// the staged inbound path is something the agent's `Read` renders.
fn image_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        ".png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ".jpg"
    } else if bytes.starts_with(b"GIF8") {
        ".gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        ".webp"
    } else {
        ".jpg"
    }
}

/// Flatten a Feishu `post` rich-text message to plain text.
///
/// Returns `None` when the content cannot be parsed or yields no usable
/// text, so callers can simply `continue` rather than forwarding a
/// meaningless placeholder string to the agent.
fn parse_post_content(content: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let locale = parsed
        .get("zh_cn")
        .or_else(|| parsed.get("en_us"))
        .or_else(|| {
            parsed
                .as_object()
                .and_then(|m| m.values().find(|v| v.is_object()))
        })?;

    let mut text = String::new();

    if let Some(title) = locale
        .get("title")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
    {
        text.push_str(title);
        text.push_str("\n\n");
    }

    if let Some(paragraphs) = locale.get("content").and_then(|c| c.as_array()) {
        for para in paragraphs {
            if let Some(elements) = para.as_array() {
                for el in elements {
                    match el.get("tag").and_then(|t| t.as_str()).unwrap_or("") {
                        "text" => {
                            if let Some(t) = el.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                        "a" => {
                            text.push_str(
                                el.get("text")
                                    .and_then(|t| t.as_str())
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| el.get("href").and_then(|h| h.as_str()))
                                    .unwrap_or(""),
                            );
                        }
                        "at" => {
                            let n = el
                                .get("user_name")
                                .and_then(|n| n.as_str())
                                .or_else(|| el.get("user_id").and_then(|i| i.as_str()))
                                .unwrap_or("user");
                            text.push('@');
                            text.push_str(n);
                        }
                        _ => {}
                    }
                }
                text.push('\n');
            }
        }
    }

    let result = text.trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Remove `@_user_N` placeholder tokens Feishu injects in group chats.
fn strip_at_placeholders(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '@' {
            let rest: String = chars.clone().map(|(_, c)| c).collect();
            if let Some(after) = rest.strip_prefix("_user_") {
                let digit_count = after.chars().take_while(|c| c.is_ascii_digit()).count();
                if digit_count == 0 {
                    result.push(ch);
                    continue;
                }
                let skip = "_user_".len() + digit_count;
                for _ in 0..skip {
                    chars.next();
                }
                if chars.peek().map(|(_, c)| *c == ' ').unwrap_or(false) {
                    chars.next();
                }
                continue;
            }
        }
        result.push(ch);
    }
    result
}

/// In group chats, only respond when the bot is explicitly @-mentioned.
fn should_respond_in_group(mentions: &[serde_json::Value]) -> bool {
    !mentions.is_empty()
}

#[cfg(test)]
#[path = "lark_tests.rs"]
mod tests;
