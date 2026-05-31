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
}

impl MockChannel {
    /// Build with the platform-name string `"mock"`.
    pub fn new() -> Self {
        Self {
            inbox: Arc::default(),
            outbox: Arc::default(),
            name: "mock".to_string(),
        }
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
