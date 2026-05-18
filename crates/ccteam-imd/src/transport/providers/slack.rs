//! Slack channel — HTTP `chat.postMessage` for outbound, polling
//! `conversations.history` for inbound.
//!
//! No Socket Mode in V0.6 (avoids `tokio-tungstenite`); polling is
//! enough for the host-probe scope. Switch to Socket Mode is a V0.7
//! decision per `docs/v0-6-0/wave-2-decisions.md` §5.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::transport::{Channel, ChannelMessage, SendMessage};

const SLACK_API: &str = "https://slack.com/api";
const POLL_INTERVAL_SECS: u64 = 4;

/// Slack channel.
pub struct SlackChannel {
    bot_token: String,
    poll_channels: Vec<String>,
    http: reqwest::Client,
    /// channel_id → last `ts` we forwarded; used to skip already-seen
    /// messages on the next poll tick.
    last_ts: Arc<Mutex<HashMap<String, String>>>,
    name: String,
}

impl SlackChannel {
    /// Build with the `xoxb-...` bot token and list of channel IDs
    /// to poll.
    pub fn new(bot_token: String, poll_channels: Vec<String>) -> Self {
        Self {
            bot_token,
            poll_channels,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            last_ts: Arc::new(Mutex::new(HashMap::new())),
            name: "slack".to_string(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.bot_token)
    }
}

#[derive(Debug, Deserialize)]
struct HistoryResp {
    ok: bool,
    #[serde(default)]
    messages: Vec<SlackMessage>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackMessage {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    text: Option<String>,
    ts: String,
    #[serde(default, rename = "thread_ts")]
    thread_ts: Option<String>,
    #[serde(default, rename = "bot_id")]
    bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostMessageResp {
    ok: bool,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let url = format!("{SLACK_API}/chat.postMessage");
        let body = serde_json::json!({
            "channel": message.recipient,
            "text": message.content,
            "thread_ts": message.thread_ts,
        });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await?;
        let parsed: PostMessageResp = resp.json().await?;
        if !parsed.ok {
            anyhow::bail!(
                "slack chat.postMessage {} failed: {}",
                message.recipient,
                parsed.error.unwrap_or_default()
            );
        }
        Ok(parsed.ts)
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        if self.poll_channels.is_empty() {
            tracing::info!("slack: no poll_channels configured; listener idle");
            return Ok(());
        }
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            for chan in &self.poll_channels {
                let oldest = {
                    let map = self.last_ts.lock().await;
                    map.get(chan).cloned()
                };
                let url = format!("{SLACK_API}/conversations.history");
                let mut q = vec![
                    ("channel", chan.clone()),
                    ("limit", "30".into()),
                ];
                if let Some(o) = oldest.clone() {
                    q.push(("oldest", o));
                }
                let resp = self
                    .http
                    .get(&url)
                    .header("Authorization", self.auth_header())
                    .query(&q)
                    .send()
                    .await;
                let parsed: HistoryResp = match resp {
                    Ok(r) => match r.json().await {
                        Ok(b) => b,
                        Err(err) => {
                            tracing::warn!(error = %err, "slack history parse failed");
                            continue;
                        }
                    },
                    Err(err) => {
                        tracing::warn!(error = %err, "slack history request failed");
                        continue;
                    }
                };
                if !parsed.ok {
                    tracing::warn!(error = ?parsed.error, "slack history ok=false");
                    continue;
                }
                // Slack returns newest-first; reverse for chrono order.
                let mut msgs = parsed.messages;
                msgs.reverse();
                for m in msgs {
                    if Some(&m.ts) == oldest.as_ref() {
                        continue;
                    }
                    if m.bot_id.is_some() {
                        // Skip bot echoes (including our own posts).
                        continue;
                    }
                    let payload = ChannelMessage {
                        id: format!("slack-{}", m.ts),
                        sender: m.user.unwrap_or_else(|| "anonymous".to_string()),
                        reply_target: chan.clone(),
                        content: m.text.unwrap_or_default(),
                        channel: "slack".into(),
                        timestamp: m
                            .ts
                            .split('.')
                            .next()
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0),
                        thread_ts: m.thread_ts.clone(),
                    };
                    {
                        let mut map = self.last_ts.lock().await;
                        map.insert(chan.clone(), m.ts.clone());
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
        let url = format!("{SLACK_API}/auth.test");
        let resp = self
            .http
            .post(&url)
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
        let ch = SlackChannel::new("xoxb-abc".into(), vec![]);
        assert_eq!(ch.auth_header(), "Bearer xoxb-abc");
    }

    #[tokio::test]
    async fn listen_returns_when_no_channels() {
        let ch = SlackChannel::new("xoxb-x".into(), vec![]);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        // Should return Ok(()) immediately because poll_channels is empty.
        ch.listen(tx).await.unwrap();
    }
}
