//! Telegram channel — `getUpdates` long-polling + `sendMessage`.
//!
//! Slim port of `references/openhuman/src/openhuman/channels/providers/telegram/`
//! with openhuman's `event_bus` / `security::pairing` / `config`
//! dependencies elided (ccteam-im has its own ACL + sanitize layers).
//!
//! V0.6.0 host probe is **mock-only** (no real TG token paste yet).
//! The provider compiles and the request shapes are correct against
//! the Bot API documented at <https://core.telegram.org/bots/api>;
//! end-to-end verification ships post-token-paste.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::latency::now_unix_ms;
use crate::transport::{Channel, ChannelMessage, SendMessage};

/// `getUpdates` long-poll seconds.
const POLL_TIMEOUT_SECS: u64 = 25;

/// Conservative per-message ceiling in **UTF-16 code units**. Telegram's
/// hard `sendMessage` limit is 4096 UTF-16 units; we reserve headroom for
/// re-opened code fences and reply metadata so a split part never trips a
/// 400. This is the *only* place the Telegram length constant lives — the
/// gateway/daemon read it polymorphically via
/// [`Channel::max_message_len`], keeping the split path channel-neutral.
const MAX_MESSAGE_UTF16: usize = 3900;

/// Telegram channel.
pub struct TelegramChannel {
    bot_token: String,
    allowed_chat_ids: Vec<String>,
    http: reqwest::Client,
    last_offset: Arc<Mutex<i64>>,
    name: String,
}

impl TelegramChannel {
    /// Build with the @BotFather token and allowed chat IDs (empty =
    /// open). The chat-ID allowlist is enforced inside [`Channel::listen`]
    /// before pushing to the daemon mpsc.
    pub fn new(bot_token: String, allowed_chat_ids: Vec<String>) -> Self {
        Self {
            bot_token,
            allowed_chat_ids,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 10))
                .build()
                .expect("reqwest client"),
            last_offset: Arc::new(Mutex::new(0)),
            name: "telegram".to_string(),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }

    /// Whether a chat is permitted (open mode when allowlist empty).
    fn chat_allowed(&self, chat_id: &str) -> bool {
        if self.allowed_chat_ids.is_empty() {
            return true;
        }
        self.allowed_chat_ids.iter().any(|id| id == chat_id)
    }
}

#[derive(Debug, Deserialize)]
struct GetUpdatesResp {
    ok: bool,
    #[serde(default)]
    result: Vec<TgUpdate>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    date: i64,
    chat: TgChat,
    #[serde(default)]
    from: Option<TgUser>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
    #[serde(default)]
    username: Option<String>,
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let url = self.api_url("sendMessage");
        let body = serde_json::json!({
            "chat_id": message.recipient,
            "text": message.content,
            // Telegram supports reply_to_message_id for in-thread replies.
            "reply_to_message_id": message.thread_ts.as_ref().and_then(|s| s.parse::<i64>().ok()),
        });
        let t0 = Instant::now();
        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let send_http_ms = t0.elapsed().as_millis() as u64;
        if !status.is_success() {
            tracing::warn!(
                event = "latency",
                stage = "tg.egress",
                recipient = %message.recipient,
                status = %status,
                send_http_ms,
                content_len = message.content.len(),
                "latency tg.egress (failed)"
            );
            anyhow::bail!(
                "telegram sendMessage {} → {}: {}",
                message.recipient,
                status,
                text
            );
        }
        // Best-effort: pluck message_id without a full type.
        let id = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("message_id"))
                    .and_then(|n| n.as_i64())
            })
            .map(|n| n.to_string());
        tracing::info!(
            event = "latency",
            stage = "tg.egress",
            recipient = %message.recipient,
            tg_msg_id = id.as_deref().unwrap_or(""),
            send_http_ms,
            content_len = message.content.len(),
            "latency tg.egress"
        );
        Ok(id)
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            let offset = { *self.last_offset.lock().await };
            let url = self.api_url("getUpdates");
            let req = self
                .http
                .get(&url)
                .query(&[
                    ("timeout", POLL_TIMEOUT_SECS.to_string()),
                    ("offset", offset.to_string()),
                ])
                .send()
                .await;
            let resp = match req {
                Ok(r) => r,
                Err(err) => {
                    tracing::warn!(error = %err, "telegram getUpdates failed, backing off 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let body: GetUpdatesResp = match resp.json().await {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!(error = %err, "telegram parse getUpdates failed");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            if !body.ok {
                tracing::warn!("telegram getUpdates ok=false");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            for upd in body.result {
                {
                    let mut last = self.last_offset.lock().await;
                    *last = (*last).max(upd.update_id + 1);
                }
                if let Some(m) = upd.message {
                    let chat_id = m.chat.id.to_string();
                    if !self.chat_allowed(&chat_id) {
                        tracing::debug!(chat = %chat_id, "drop msg from non-allowed chat");
                        continue;
                    }
                    let sender = m
                        .from
                        .as_ref()
                        .and_then(|u| u.username.clone())
                        .unwrap_or_else(|| {
                            m.from
                                .as_ref()
                                .map(|u| u.id.to_string())
                                .unwrap_or_else(|| "anonymous".to_string())
                        });
                    let cid = format!("tg-{}", m.message_id);
                    let recv_ms = now_unix_ms();
                    let tg_date_ms = (m.date.max(0) as u128).saturating_mul(1000);
                    let tg_age_ms = recv_ms.saturating_sub(tg_date_ms);
                    let content_len = m.text.as_ref().map(|s| s.len()).unwrap_or(0);
                    tracing::info!(
                        event = "latency",
                        stage = "tg.ingress",
                        cid = %cid,
                        chat_id = %chat_id,
                        sender = %sender,
                        recv_ms = recv_ms as u64,
                        tg_age_ms = tg_age_ms as u64,
                        content_len,
                        "latency tg.ingress"
                    );
                    let payload = ChannelMessage {
                        id: cid,
                        sender,
                        reply_target: chat_id.clone(),
                        content: m.text.unwrap_or_default(),
                        channel: "telegram".into(),
                        timestamp: m.date.max(0) as u64,
                        thread_ts: None,
                    };
                    if tx.send(payload).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let url = self.api_url("getMe");
        match self.http.get(&url).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    fn max_message_len(&self) -> Option<usize> {
        Some(MAX_MESSAGE_UTF16)
    }

    async fn edit_message(
        &self,
        recipient: &str,
        message_id: &str,
        content: &str,
    ) -> anyhow::Result<Option<String>> {
        let url = self.api_url("editMessageText");
        let body = serde_json::json!({
            "chat_id": recipient,
            "message_id": message_id.parse::<i64>().ok(),
            "text": content,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("telegram editMessageText {recipient}#{message_id} → {status}: {text}");
        }
        // editMessageText returns the (same) edited Message; the id is
        // stable, so echo it back for the daemon's status bookkeeping.
        Ok(Some(message_id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_template() {
        let ch = TelegramChannel::new("ABC".into(), vec![]);
        let url = ch.api_url("getMe");
        assert_eq!(url, "https://api.telegram.org/botABC/getMe");
    }

    #[test]
    fn chat_allowed_open_when_empty() {
        let ch = TelegramChannel::new("t".into(), vec![]);
        assert!(ch.chat_allowed("12345"));
    }

    #[test]
    fn chat_allowed_enforces_list() {
        let ch = TelegramChannel::new("t".into(), vec!["12345".into()]);
        assert!(ch.chat_allowed("12345"));
        assert!(!ch.chat_allowed("99999"));
    }
}
