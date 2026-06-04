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
//! rich-text; every other `message_type` is debug-logged and skipped
//! (image ingest / outbound files are an explicit out-of-scope
//! follow-up). Outbound replies go via tenant-token `im/v1/messages`.
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
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use crate::transport::{Channel, ChannelMessage, SendMessage};

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
    /// Stable message id; becomes `lark-{message_id}`.
    message_id: String,
    /// Decoded, `@`-placeholder-stripped, trimmed body (never empty).
    text: String,
    /// Lark send time in seconds (from `create_time` ms, or wall-clock).
    timestamp: u64,
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

    // Decode the body by message type. Path A is text/`post`-only.
    let text = match msg.message_type.as_str() {
        "text" => {
            let v = serde_json::from_str::<serde_json::Value>(&msg.content).ok()?;
            v.get("text")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())?
                .to_string()
        }
        "post" => parse_post_content(&msg.content)?,
        other => {
            tracing::debug!("Lark: skipping unsupported message type '{other}'");
            return None;
        }
    };

    // Strip the `@_user_N` placeholders Feishu injects, then trim.
    let text = strip_at_placeholders(&text).trim().to_string();
    if text.is_empty() {
        return None;
    }

    // Group chat: only respond when explicitly @-mentioned.
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
            name: "lark".to_string(),
        }
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

                    let channel_msg = decoded.into_channel_message();
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
        Some(decoded.into_channel_message())
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
        let url = self.send_message_url();

        // Feishu quirk: `content` is a STRING containing JSON, not a
        // nested object.
        let content = serde_json::json!({ "text": message.content }).to_string();
        let body = serde_json::json!({
            "receive_id": message.recipient,
            "msg_type": "text",
            "content": content,
        });

        let resp = self
            .send_json_with_token_retry(reqwest::Method::POST, &url, &body)
            .await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lark send failed: {err}");
        }

        Ok(parse_sent_message_id(resp).await)
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
}

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
