//! In-memory Channel for tests + the V0.6 host-probe mock-only path.
//!
//! `MockChannel` lets a test (or a hand-driven `ccteam-im run
//! --platform mock`) inject inbound messages via [`MockChannel::push`]
//! and inspect outbound sends via [`MockChannel::outbox`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::transport::{Channel, ChannelMessage, CommandSpec, SendMessage};

/// One recorded reaction call (v0.8.19): `(op, chat_id, message_id, handle)`
/// where `op` is `"add"`/`"remove"` and `handle` is what was passed to remove
/// (always `None` on add). Factored out so the [`MockChannel`] field stays a
/// simple type.
type RecordedReaction = (String, String, String, Option<String>);

/// Test-only Channel. Cheap to clone (`Arc` inside) so a test can
/// hand one copy to the daemon and keep another for assertions.
#[derive(Debug, Clone, Default)]
pub struct MockChannel {
    inbox: Arc<Mutex<Vec<ChannelMessage>>>,
    outbox: Arc<Mutex<Vec<SendMessage>>>,
    /// Edits applied via [`Channel::edit_message`], as `(message_id,
    /// content)` — lets a test prove a status message is *edited* in
    /// place rather than re-sent (V0.8.4 P1).
    edits: Arc<Mutex<Vec<(String, String)>>>,
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
    /// Command menus registered via [`Channel::register_commands`] (v0.8.5
    /// P1). Records each call's specs so a test can prove the daemon wires
    /// the menu at startup. A real menu-less channel keeps the trait default
    /// (no-op) — this recorder is purely an observation hook, not behavior.
    registered_commands: Arc<Mutex<Vec<Vec<CommandSpec>>>>,
    /// v0.8.19 — when set, makes [`Channel::add_reaction`] STATEFUL (return
    /// `Some(handle)`) so a test can prove the daemon egress round-trips a
    /// reaction handle (the Lark shape). `None` (the default) keeps the trait's
    /// stateless `Ok(None)` (the Telegram shape). Recorded reaction calls go to
    /// `reactions`.
    reaction_handle: Option<String>,
    /// v0.8.19 — every `add_reaction`/`remove_reaction` call recorded as
    /// `(op, chat_id, message_id, handle)` where `op` is `"add"`/`"remove"`
    /// and `handle` is the handle passed to remove (always `None` on add). Lets
    /// a test assert the daemon egress add→remove handle round-trip.
    reactions: Arc<Mutex<Vec<RecordedReaction>>>,
}

impl MockChannel {
    /// Build with the platform-name string `"mock"`.
    pub fn new() -> Self {
        Self {
            inbox: Arc::default(),
            outbox: Arc::default(),
            edits: Arc::default(),
            name: "mock".to_string(),
            max_len: None,
            fail_if_contains: Arc::default(),
            registered_commands: Arc::default(),
            reaction_handle: None,
            reactions: Arc::default(),
        }
    }

    /// Make [`Channel::add_reaction`] return `Some(handle)` (the stateful Lark
    /// shape) so a test can assert the daemon egress stores + replays it on
    /// remove. Builder-style; default keeps the stateless `Ok(None)`.
    pub fn with_reaction_handle(mut self, handle: impl Into<String>) -> Self {
        self.reaction_handle = Some(handle.into());
        self
    }

    /// Snapshot of every reaction call so far, as `(op, chat_id, message_id,
    /// handle)` (`op` = `"add"`/`"remove"`; `handle` is what was passed to
    /// remove). v0.8.19.
    pub async fn reactions(&self) -> Vec<RecordedReaction> {
        self.reactions.lock().await.clone()
    }

    /// Override the platform-name string (default `"mock"`). Builder-style;
    /// used by the v0.8.5 P1 menu test to inject two distinct channels into
    /// one `channels_override` map.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
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

    /// Snapshot of every `edit_message` call so far, as `(message_id,
    /// content)`.
    pub async fn edits(&self) -> Vec<(String, String)> {
        self.edits.lock().await.clone()
    }

    /// Snapshot of every [`Channel::register_commands`] call's specs
    /// (v0.8.5 P1). One entry per call; empty until the daemon wires the
    /// menu at startup.
    pub async fn registered_commands(&self) -> Vec<Vec<CommandSpec>> {
        self.registered_commands.lock().await.clone()
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

    async fn edit_message(
        &self,
        _recipient: &str,
        message_id: &str,
        content: &str,
    ) -> anyhow::Result<Option<String>> {
        self.edits
            .lock()
            .await
            .push((message_id.to_string(), content.to_string()));
        Ok(Some(message_id.to_string()))
    }

    async fn register_commands(&self, cmds: &[CommandSpec]) -> anyhow::Result<()> {
        // Menu-less channel: record the call (so a test can assert the
        // daemon wired the menu) but take no action — the trait default's
        // no-op semantics are preserved.
        self.registered_commands.lock().await.push(cmds.to_vec());
        Ok(())
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<String>> {
        // Record the call + return the configured handle (None by default =
        // stateless Telegram shape; Some = stateful Lark shape).
        self.reactions.lock().await.push((
            "add".to_string(),
            chat_id.to_string(),
            message_id.to_string(),
            None,
        ));
        Ok(self.reaction_handle.clone())
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        handle: Option<&str>,
    ) -> anyhow::Result<()> {
        self.reactions.lock().await.push((
            "remove".to_string(),
            chat_id.to_string(),
            message_id.to_string(),
            handle.map(str::to_string),
        ));
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
            attachments: Vec::new(),
            selection: None,
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

    /// v0.8.19 — a channel that doesn't override the reaction methods (web /
    /// discord / slack / mock) gets the trait's no-op defaults: `add_reaction`
    /// returns `Ok(None)` (no handle), `remove_reaction` returns `Ok(())`.
    /// Reactions are an IM-only affordance, so this keeps non-IM channels
    /// change-free.
    #[tokio::test]
    async fn reaction_methods_default_to_noop() {
        let ch = MockChannel::new();
        let handle = ch.add_reaction("chat-1", "m-1").await.unwrap();
        assert!(handle.is_none(), "default add_reaction returns no handle");
        // remove with the (None) handle is a no-op that succeeds.
        ch.remove_reaction("chat-1", "m-1", handle.as_deref())
            .await
            .unwrap();
        // And it never touched the outbox (a reaction is not a message).
        assert!(ch.outbox().await.is_empty());
    }
}
