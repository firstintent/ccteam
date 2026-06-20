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

use anyhow::Context as _;

use crate::latency::now_unix_ms;
use crate::transport::{
    inbound_staging_dir, sanitize_attachment_name, AttachmentKind, Channel, ChannelAttachment,
    ChannelMessage, ChoiceReply, CommandSpec, OutboundFile, OutboundFileKind, SendMessage,
};

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

    /// Resolve a `file_id` to its server-side `file_path` via `getFile`.
    async fn get_file_path(&self, file_id: &str) -> anyhow::Result<String> {
        let url = self.api_url("getFile");
        let resp = self
            .http
            .get(&url)
            .query(&[("file_id", file_id)])
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("telegram getFile {status}: {text}");
        }
        serde_json::from_str::<serde_json::Value>(&text)?
            .get("result")
            .and_then(|r| r.get("file_path"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("telegram getFile: missing file_path"))
    }

    /// Download a resolved `file_path` from the Bot file endpoint.
    async fn download_file_bytes(&self, file_path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.bot_token, file_path
        );
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("telegram file download {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Download + stage one attachment. Returns `Ok(None)` if it exceeds
    /// the 20 MB Bot-API ceiling (rejected, not an error). The staged file
    /// lives at `<staging>/<cid>-<sanitized_name>`.
    async fn stage_attachment(
        &self,
        cid: &str,
        pending: &PendingDownload,
    ) -> anyhow::Result<Option<ChannelAttachment>> {
        if pending
            .size
            .map(|s| s > MAX_ATTACHMENT_BYTES)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let file_path = self.get_file_path(&pending.file_id).await?;
        let bytes = self.download_file_bytes(&file_path).await?;
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Ok(None);
        }
        let safe_name = sanitize_attachment_name(&pending.file_name);
        let dir = inbound_staging_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let dest = dir.join(format!("{cid}-{safe_name}"));
        tokio::fs::write(&dest, &bytes).await?;
        Ok(Some(ChannelAttachment {
            kind: pending.kind,
            file_name: safe_name,
            local_path: dest.to_string_lossy().into_owned(),
            mime: pending.mime.clone(),
            size: Some(bytes.len() as u64),
        }))
    }

    /// Send each outbound file as `sendPhoto`/`sendDocument`; the caption
    /// rides the first attachment (preferring its own caption, else the
    /// message text). Returns the first attachment's message id.
    /// Handle an inline-keyboard button click (v0.8.5 D3): answer the
    /// callback (clears the client spinner) + forward a `ChannelMessage`
    /// carrying the opaque selection so the gateway can resolve it.
    async fn handle_callback_query(
        &self,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        cb: TgCallbackQuery,
    ) {
        let _ = self
            .http
            .post(self.api_url("answerCallbackQuery"))
            .json(&serde_json::json!({ "callback_query_id": cb.id }))
            .send()
            .await;
        let Some(data) = cb.data else {
            return;
        };
        let Some(msg) = cb.message else {
            return;
        };
        let chat_id = msg.chat.id.to_string();
        if !self.chat_allowed(&chat_id) {
            return;
        }
        let sender = cb
            .from
            .as_ref()
            .and_then(|u| u.username.clone())
            .or_else(|| cb.from.as_ref().map(|u| u.id.to_string()))
            .unwrap_or_else(|| "anonymous".to_string());
        let payload = ChannelMessage {
            id: format!("tg-cb-{}", msg.message_id),
            sender,
            reply_target: chat_id,
            content: String::new(),
            channel: "telegram".into(),
            timestamp: (now_unix_ms() / 1000) as u64,
            thread_ts: None,
            attachments: Vec::new(),
            selection: Some(ChoiceReply { data }),
        };
        let _ = tx.send(payload).await;
    }

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
            let id = self
                .send_one_attachment(
                    &message.recipient,
                    att,
                    caption.as_deref(),
                    message.thread_ts.as_deref(),
                )
                .await?;
            if first_id.is_none() {
                first_id = id;
            }
        }
        Ok(first_id)
    }

    async fn send_one_attachment(
        &self,
        recipient: &str,
        att: &OutboundFile,
        caption: Option<&str>,
        reply_to: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let (method, field) = match att.kind {
            OutboundFileKind::Photo => ("sendPhoto", "photo"),
            OutboundFileKind::Document => ("sendDocument", "document"),
        };
        let bytes = tokio::fs::read(&att.path)
            .await
            .with_context(|| format!("read outbound file {}", att.path))?;
        let file_name = std::path::Path::new(&att.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", recipient.to_string())
            .part(
                field,
                reqwest::multipart::Part::bytes(bytes).file_name(file_name),
            );
        if let Some(cap) = caption {
            // V0.8.4 P2b (F7): caption ceiling (1024) ≠ message ceiling
            // (4096), and attachment messages skip the splitter — truncate
            // here so an over-long caption can't trip a 400.
            form = form.text("caption", truncate_caption(cap));
        }
        if let Some(rt) = reply_to.and_then(|s| s.parse::<i64>().ok()) {
            form = form.text("reply_to_message_id", rt.to_string());
        }
        let url = self.api_url(method);
        let resp = self.http.post(&url).multipart(form).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("telegram {method} {recipient} → {status}: {text}");
        }
        let id = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("message_id"))
                    .and_then(|n| n.as_i64())
            })
            .map(|n| n.to_string());
        Ok(id)
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
    // v0.8.5 D3 — inline-keyboard button clicks.
    #[serde(default)]
    callback_query: Option<TgCallbackQuery>,
}

/// An inline-keyboard button click (v0.8.5 D3).
#[derive(Debug, Deserialize)]
struct TgCallbackQuery {
    id: String,
    #[serde(default)]
    from: Option<TgUser>,
    #[serde(default)]
    message: Option<TgMessage>,
    #[serde(default)]
    data: Option<String>,
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
    // V0.8.4 P2a — inbound media.
    #[serde(default)]
    photo: Vec<TgPhotoSize>,
    #[serde(default)]
    document: Option<TgDocument>,
    #[serde(default)]
    caption: Option<String>,
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

/// One size of an inbound photo (Telegram sends an ascending-size array;
/// the last entry is the largest).
#[derive(Debug, Deserialize)]
struct TgPhotoSize {
    file_id: String,
    #[serde(default)]
    file_size: Option<u64>,
}

/// An inbound document (any non-photo file).
#[derive(Debug, Deserialize)]
struct TgDocument {
    file_id: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
}

/// Bot-API download ceiling: 20 MB (`getFile`).
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

/// What `pick_attachment` decides to fetch for one inbound message.
#[derive(Debug, PartialEq)]
struct PendingDownload {
    file_id: String,
    kind: AttachmentKind,
    file_name: String,
    mime: Option<String>,
    size: Option<u64>,
}

/// Pure: choose the single attachment to download for a message —
/// a document (preferred, has a real name) or the largest photo size.
/// Returns `None` for a text-only message.
fn pick_attachment(m: &TgMessage) -> Option<PendingDownload> {
    if let Some(doc) = &m.document {
        let is_image = doc
            .mime_type
            .as_deref()
            .map(|t| t.starts_with("image/"))
            .unwrap_or(false);
        return Some(PendingDownload {
            file_id: doc.file_id.clone(),
            kind: if is_image {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            file_name: doc.file_name.clone().unwrap_or_else(|| "file".to_string()),
            mime: doc.mime_type.clone(),
            size: doc.file_size,
        });
    }
    m.photo.last().map(|largest| PendingDownload {
        file_id: largest.file_id.clone(),
        kind: AttachmentKind::Image,
        file_name: "photo.jpg".to_string(),
        mime: Some("image/jpeg".to_string()),
        size: largest.file_size,
    })
}

/// Telegram caption ceiling, in UTF-16 code units (separate from the
/// 4096 message ceiling). Attachment messages skip the outbound
/// splitter, so an over-long caption is truncated here (V0.8.4 P2b / F7).
const MAX_CAPTION_UTF16: usize = 1024;

/// Truncate `s` to at most [`MAX_CAPTION_UTF16`] UTF-16 code units (never
/// splitting a char), so an over-long attachment caption can't trip a 400.
fn truncate_caption(s: &str) -> String {
    let mut units = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.len_utf16();
        if units + w > MAX_CAPTION_UTF16 {
            break;
        }
        units += w;
        out.push(ch);
    }
    out
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        // V0.8.4 P2b — files go via sendPhoto/sendDocument (multipart);
        // the caption rides the first attachment.
        if !message.attachments.is_empty() {
            return self.send_with_attachments(message).await;
        }
        let url = self.api_url("sendMessage");
        let mut body = serde_json::json!({
            "chat_id": message.recipient,
            "text": message.content,
            // Telegram supports reply_to_message_id for in-thread replies.
            "reply_to_message_id": message.thread_ts.as_ref().and_then(|s| s.parse::<i64>().ok()),
        });
        // v0.8.5 D3 — render choice options as an inline keyboard (one button
        // per row); the opaque `data` ("{token}:{idx}") rides `callback_data`
        // (≤64B). The numbered-text fallback already lives in `content`, so
        // button-less clients still work.
        if !message.options.is_empty() {
            let rows: Vec<Vec<serde_json::Value>> = message
                .options
                .iter()
                .map(|o| vec![serde_json::json!({ "text": o.label, "callback_data": o.data })])
                .collect();
            body["reply_markup"] = serde_json::json!({ "inline_keyboard": rows });
        }
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
                if let Some(cb) = upd.callback_query {
                    self.handle_callback_query(&tx, cb).await;
                    continue;
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
                    // V0.8.4 P2a — caption is the text for media messages.
                    let content = m
                        .text
                        .clone()
                        .or_else(|| m.caption.clone())
                        .unwrap_or_default();
                    let mut attachments = Vec::new();
                    let mut rejected_notice: Option<String> = None;
                    if let Some(pending) = pick_attachment(&m) {
                        match self.stage_attachment(&cid, &pending).await {
                            Ok(Some(att)) => attachments.push(att),
                            Ok(None) => {
                                rejected_notice = Some(format!(
                                    "⚠️ 附件 {} 超过 20MB 上限,已拒收",
                                    pending.file_name
                                ));
                            }
                            Err(err) => {
                                tracing::warn!(cid = %cid, error = %err, "telegram: attachment download failed");
                                rejected_notice =
                                    Some(format!("⚠️ 附件 {} 下载失败", pending.file_name));
                            }
                        }
                    }
                    // V0.8.4 P2a (F3): deliver a rejection DIRECTLY to the
                    // chat (like the P0 split-failure notice), not as a
                    // turn-text note the agent must relay. With no
                    // accompanying text there is no turn to submit.
                    if let Some(notice) = rejected_notice {
                        if let Err(err) =
                            self.send(&SendMessage::new(notice, chat_id.clone())).await
                        {
                            tracing::warn!(cid = %cid, error = %err, "telegram: rejection notice send failed");
                        }
                        if content.is_empty() {
                            continue;
                        }
                    }
                    let content_len = content.len();
                    tracing::info!(
                        event = "latency",
                        stage = "tg.ingress",
                        cid = %cid,
                        chat_id = %chat_id,
                        sender = %sender,
                        recv_ms = recv_ms as u64,
                        tg_age_ms = tg_age_ms as u64,
                        content_len,
                        attachments = attachments.len(),
                        "latency tg.ingress"
                    );
                    let payload = ChannelMessage {
                        id: cid,
                        sender,
                        reply_target: chat_id.clone(),
                        content,
                        channel: "telegram".into(),
                        timestamp: m.date.max(0) as u64,
                        thread_ts: None,
                        attachments,
                        selection: None,
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

    /// v0.8.5 P1 — publish the gateway's command menu via `setMyCommands`.
    /// Telegram wants **bare** command names (no leading `/`), so the body
    /// builder strips it. An empty spec list clears the menu (Telegram's
    /// documented behaviour for an empty `commands` array). Best-effort:
    /// the daemon logs a warn on failure rather than aborting startup.
    async fn register_commands(&self, cmds: &[CommandSpec]) -> anyhow::Result<()> {
        // Publish to BOTH the default scope (groups / fallback) AND the
        // `all_private_chats` scope. ccteam is driven from DMs, and Telegram
        // resolves a private chat's menu from the MOST SPECIFIC scope. A stale
        // `all_private_chats` menu (e.g. left by a prior bot-setup flow's
        // `start`/`help`/`status`) therefore shadows a default-scope menu in
        // every DM, forever — which is why a default-only write was invisible
        // and "fixed several times" never stuck. Writing `all_private_chats`
        // explicitly is the only way the DM menu shows ccteam's commands; the
        // daemon re-asserts it on every start, so any later clobber self-heals.
        let scopes = [
            None,
            Some(serde_json::json!({ "type": "all_private_chats" })),
        ];
        let url = self.api_url("setMyCommands");
        for scope in scopes {
            let body = set_my_commands_body(cmds, scope.as_ref());
            let resp = self.http.post(&url).json(&body).send().await?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("telegram setMyCommands → {status}: {text}");
            }
        }
        Ok(())
    }
}

/// Build the `setMyCommands` request body (v0.8.5 P1). Pure + isolated so
/// the bare-name stripping + JSON shape are unit-testable without a live
/// Bot API. Telegram requires bare command names (`new`, not `/new`). An
/// optional `scope` (e.g. `{"type":"all_private_chats"}`) targets a single
/// command scope; `None` writes the default scope.
fn set_my_commands_body(
    cmds: &[CommandSpec],
    scope: Option<&serde_json::Value>,
) -> serde_json::Value {
    let commands: Vec<serde_json::Value> = cmds
        .iter()
        .map(|c| {
            serde_json::json!({
                "command": c.name.trim_start_matches('/'),
                "description": c.description,
            })
        })
        .collect();
    let mut body = serde_json::json!({ "commands": commands });
    if let Some(scope) = scope {
        body["scope"] = scope.clone();
    }
    body
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
    fn set_my_commands_body_strips_leading_slash() {
        // v0.8.5 P1 — Telegram wants BARE command names.
        let specs = vec![
            CommandSpec {
                name: "/new".into(),
                description: "start a new session".into(),
            },
            CommandSpec {
                name: "/sessions".into(),
                description: "list sessions + status".into(),
            },
        ];
        let body = set_my_commands_body(&specs, None);
        let commands = body["commands"].as_array().expect("commands array");
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0]["command"], "new");
        assert_eq!(commands[0]["description"], "start a new session");
        assert_eq!(commands[1]["command"], "sessions");
        // No leading slash leaks through anywhere.
        for c in commands {
            assert!(
                !c["command"].as_str().unwrap().starts_with('/'),
                "telegram command names must be bare"
            );
        }
        // No scope key on the default-scope write.
        assert!(body.get("scope").is_none());
    }

    #[test]
    fn set_my_commands_body_empty_clears_menu() {
        let body = set_my_commands_body(&[], None);
        assert!(body["commands"].as_array().unwrap().is_empty());
    }

    #[test]
    fn set_my_commands_body_carries_all_private_chats_scope() {
        // The DM-scope write is what un-shadows ccteam's menu: a stale
        // all_private_chats menu otherwise wins over the default scope in
        // every private chat. The body must carry that scope verbatim.
        let specs = vec![CommandSpec {
            name: "/new".into(),
            description: "start a new session".into(),
        }];
        let scope = serde_json::json!({ "type": "all_private_chats" });
        let body = set_my_commands_body(&specs, Some(&scope));
        assert_eq!(body["scope"]["type"], "all_private_chats");
        assert_eq!(body["commands"][0]["command"], "new");
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

    // ----- P2a attachment parsing (pure, fixture-driven) ------------

    #[test]
    fn pick_attachment_takes_largest_photo() {
        let m: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5},
            "caption": "error screenshot",
            "photo": [
                {"file_id": "small", "file_size": 100},
                {"file_id": "big", "file_size": 9000}
            ]
        }))
        .unwrap();
        let p = pick_attachment(&m).unwrap();
        assert_eq!(p.file_id, "big");
        assert_eq!(p.kind, AttachmentKind::Image);
        assert_eq!(p.size, Some(9000));
    }

    #[test]
    fn pick_attachment_document_is_file_or_image_by_mime() {
        let doc: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5},
            "document": {"file_id": "d1", "file_name": "log.txt", "mime_type": "text/plain", "file_size": 50}
        }))
        .unwrap();
        let p = pick_attachment(&doc).unwrap();
        assert_eq!(p.kind, AttachmentKind::File);
        assert_eq!(p.file_name, "log.txt");

        let img_doc: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5},
            "document": {"file_id": "d2", "file_name": "shot.png", "mime_type": "image/png"}
        }))
        .unwrap();
        assert_eq!(
            pick_attachment(&img_doc).unwrap().kind,
            AttachmentKind::Image
        );
    }

    #[test]
    fn pick_attachment_none_for_text_only() {
        let m: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5}, "text": "hi"
        }))
        .unwrap();
        assert!(pick_attachment(&m).is_none());
    }

    #[test]
    fn truncate_caption_caps_utf16_units() {
        // F7 — short captions pass through untouched.
        assert_eq!(truncate_caption("hi"), "hi");
        // Over-long captions are capped to the 1024-unit ceiling.
        let long = "x".repeat(2000);
        let out = truncate_caption(&long);
        assert_eq!(out.chars().map(char::len_utf16).sum::<usize>(), 1024);
        // Emoji (2 units each) never split mid-char.
        let emoji = "😀".repeat(1000); // 2000 UTF-16 units
        let out = truncate_caption(&emoji);
        assert!(out.chars().map(char::len_utf16).sum::<usize>() <= 1024);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
