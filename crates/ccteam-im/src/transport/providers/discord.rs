//! Discord channel — REST `messages` polling + `messages` POST.
//!
//! Slim port. No gateway WebSocket; polling matches the V0.6
//! "no public URL" ops shape.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::transport::{Channel, ChannelMessage, SendMessage};

const DISCORD_API: &str = "https://discord.com/api/v10";
const POLL_INTERVAL_SECS: u64 = 4;

/// Discord channel.
pub struct DiscordChannel {
    bot_token: String,
    poll_channels: Vec<String>,
    authorized_user_ids: Vec<String>,
    http: reqwest::Client,
    /// channel_id → last message_id forwarded.
    last_id: Arc<Mutex<HashMap<String, String>>>,
    name: String,
}

impl DiscordChannel {
    /// Build with bot token, channels to poll, and the authorised
    /// user-id allowlist (empty = open).
    pub fn new(
        bot_token: String,
        poll_channels: Vec<String>,
        authorized_user_ids: Vec<String>,
    ) -> Self {
        Self {
            bot_token,
            poll_channels,
            authorized_user_ids,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            last_id: Arc::new(Mutex::new(HashMap::new())),
            name: "discord".to_string(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bot {}", self.bot_token)
    }

    fn user_allowed(&self, uid: &str) -> bool {
        if self.authorized_user_ids.is_empty() {
            return true;
        }
        self.authorized_user_ids.iter().any(|x| x == uid)
    }
}

#[derive(Debug, Deserialize)]
struct DiscordMessage {
    id: String,
    #[serde(default)]
    content: String,
    author: DiscordAuthor,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiscordAuthor {
    id: String,
    #[serde(default)]
    bot: bool,
    #[serde(default)]
    username: Option<String>,
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let url = format!("{DISCORD_API}/channels/{}/messages", message.recipient);
        let body = serde_json::json!({ "content": message.content });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!(
                "discord POST messages {} → {}: {}",
                message.recipient,
                status,
                text
            );
        }
        let id = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("id").and_then(|s| s.as_str()).map(|s| s.to_string()));
        Ok(id)
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        if self.poll_channels.is_empty() {
            tracing::info!("discord: no poll_channels configured; listener idle");
            return Ok(());
        }
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            for chan in &self.poll_channels {
                let after = {
                    let map = self.last_id.lock().await;
                    map.get(chan).cloned()
                };
                let url = format!("{DISCORD_API}/channels/{chan}/messages");
                let mut q: Vec<(&str, String)> = vec![("limit", "30".into())];
                if let Some(a) = after.clone() {
                    q.push(("after", a));
                }
                let resp = self
                    .http
                    .get(&url)
                    .header("Authorization", self.auth_header())
                    .query(&q)
                    .send()
                    .await;
                let msgs: Vec<DiscordMessage> = match resp {
                    Ok(r) => match r.json().await {
                        Ok(v) => v,
                        Err(err) => {
                            tracing::warn!(error = %err, "discord parse failed");
                            continue;
                        }
                    },
                    Err(err) => {
                        tracing::warn!(error = %err, "discord http failed");
                        continue;
                    }
                };
                // API returns newest-first; chronological forward.
                let mut chronological = msgs;
                chronological.reverse();
                for m in chronological {
                    if m.author.bot {
                        continue;
                    }
                    if !self.user_allowed(&m.author.id) {
                        continue;
                    }
                    let payload = ChannelMessage {
                        id: format!("discord-{}", m.id),
                        sender: m
                            .author
                            .username
                            .clone()
                            .unwrap_or_else(|| m.author.id.clone()),
                        reply_target: chan.clone(),
                        content: m.content,
                        channel: "discord".into(),
                        timestamp: m
                            .timestamp
                            .as_deref()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|d| d.timestamp().max(0) as u64)
                            .unwrap_or(0),
                        thread_ts: None,
                        attachments: Vec::new(),
                    };
                    {
                        let mut map = self.last_id.lock().await;
                        map.insert(chan.clone(), m.id.clone());
                    }
                    if tx.send(payload).await.is_err() {
                        return Ok(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{DISCORD_API}/users/@me");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await;
        match resp {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_header_format() {
        let ch = DiscordChannel::new("abc".into(), vec![], vec![]);
        assert_eq!(ch.auth_header(), "Bot abc");
    }

    #[test]
    fn user_allowed_open_when_empty() {
        let ch = DiscordChannel::new("t".into(), vec![], vec![]);
        assert!(ch.user_allowed("anyone"));
    }

    #[test]
    fn user_allowed_enforces_list() {
        let ch = DiscordChannel::new("t".into(), vec![], vec!["111".into()]);
        assert!(ch.user_allowed("111"));
        assert!(!ch.user_allowed("222"));
    }
}
