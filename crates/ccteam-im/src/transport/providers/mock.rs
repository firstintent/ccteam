//! In-memory Channel for tests + the V0.6 host-probe mock-only path.
//!
//! `MockChannel` lets a test (or a hand-driven `ccteam-im run
//! --platform mock`) inject inbound messages via [`MockChannel::push`]
//! and inspect outbound sends via [`MockChannel::outbox`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::transport::{Channel, ChannelMessage, SendMessage};

/// Test-only Channel. Cheap to clone (`Arc` inside) so a test can
/// hand one copy to the daemon and keep another for assertions.
#[derive(Debug, Clone, Default)]
pub struct MockChannel {
    inbox: Arc<Mutex<Vec<ChannelMessage>>>,
    outbox: Arc<Mutex<Vec<SendMessage>>>,
    name: String,
    /// Optional per-message UTF-16 ceiling (default `None` = unlimited),
    /// so a test can exercise the daemon's split path without a real
    /// Telegram channel.
    max_len: Option<usize>,
    /// Substrings that, when present in an outbound message's content,
    /// make [`Channel::send`] return `Err` — for exercising the P0
    /// split-failure notice deterministically (content-keyed, so it is
    /// immune to the sync-ack/async-echo send ordering).
    fail_if_contains: Arc<Vec<String>>,
}

impl MockChannel {
    /// Build with the platform-name string `"mock"`.
    pub fn new() -> Self {
        Self {
            inbox: Arc::default(),
            outbox: Arc::default(),
            name: "mock".to_string(),
            max_len: None,
            fail_if_contains: Arc::default(),
        }
    }

    /// Declare a per-message length ceiling (UTF-16 units), making this
    /// channel exercise [`Channel::max_message_len`] + the daemon split
    /// path. Builder-style so existing `MockChannel::new()` callers keep
    /// the unlimited default.
    pub fn with_max_message_len(mut self, max_len: usize) -> Self {
        self.max_len = Some(max_len);
        self
    }

    /// Make [`Channel::send`] fail for any message whose content contains
    /// one of `needles`. Builder-style; used to drive the P0
    /// split-failure notice path in tests.
    pub fn failing_on_content(mut self, needles: &[&str]) -> Self {
        self.fail_if_contains = Arc::new(needles.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Queue an inbound message to be delivered on the next
    /// [`Channel::listen`] tick.
    pub async fn push(&self, msg: ChannelMessage) {
        self.inbox.lock().await.push(msg);
    }

    /// Snapshot of every outbound send so far.
    pub async fn outbox(&self) -> Vec<SendMessage> {
        self.outbox.lock().await.clone()
    }
}

#[async_trait]
impl Channel for MockChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if let Some(needle) = self
            .fail_if_contains
            .iter()
            .find(|n| message.content.contains(n.as_str()))
        {
            anyhow::bail!("mock: simulated send failure (content contains {needle:?})");
        }
        self.outbox.lock().await.push(message.clone());
        Ok(Some(format!("mock-{}", self.outbox.lock().await.len())))
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Drain whatever's queued, then exit (production providers
        // loop forever — the mock returns so tests don't hang).
        let queued = std::mem::take(&mut *self.inbox.lock().await);
        for msg in queued {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
        Ok(())
    }

    fn max_message_len(&self) -> Option<usize> {
        self.max_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_inbox_outbox() {
        let ch = MockChannel::new();
        ch.push(ChannelMessage {
            id: "u1".into(),
            sender: "alice".into(),
            reply_target: "alice".into(),
            content: "hi".into(),
            channel: "mock".into(),
            timestamp: 1,
            thread_ts: None,
        })
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        ch.listen(tx).await.unwrap();
        let got = rx.recv().await.unwrap();
        assert_eq!(got.sender, "alice");

        ch.send(&SendMessage::new("pong", "alice")).await.unwrap();
        let out = ch.outbox().await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content, "pong");
    }
}
