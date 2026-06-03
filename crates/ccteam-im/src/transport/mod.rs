//! Channel trait + per-platform providers.
//!
//! V0.6.0 Wave 2 Option-C implementation
//! (see `docs/versions/v0-6-0/wave-2-decisions.md` §3). The [`Channel`] trait
//! and the three providers (telegram / slack / discord) are vendored
//! from `references/openhuman/src/openhuman/channels/` with shell
//! reductions:
//!
//! - **No event_bus / security / config coupling.** ccteam-im has
//!   its own credentials + ACL + sanitize layers; providers stay
//!   plain reqwest clients.
//! - **No Socket Mode / gateway WebSockets.** Slack + Discord both
//!   use HTTP polling. Telegram uses `getUpdates` long-polling. None
//!   of the V0.6 scope needs a public HTTPS endpoint, which keeps
//!   ops surface to "edit credentials.json, run daemon".
//!
//! See `providers/mock.rs` for the in-memory test channel.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod providers;

/// A message received from or sent to a channel. Trait surface lifted
/// from `references/openhuman/src/openhuman/channels/traits.rs` with
/// `serde` added so the daemon can persist inbound events for
/// debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelMessage {
    /// Platform-unique message id (e.g. Telegram `update_id`).
    pub id: String,
    /// Sender's platform user id.
    pub sender: String,
    /// Where to address replies (channel id / chat id / DM target).
    pub reply_target: String,
    /// Raw text payload.
    pub content: String,
    /// Platform name ("telegram" / "slack" / ...).
    pub channel: String,
    /// Unix-epoch seconds of receipt.
    pub timestamp: u64,
    /// Platform thread id (Slack `ts`, Discord thread, etc.).
    pub thread_ts: Option<String>,
}

/// Message to send through a channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SendMessage {
    /// Payload (post-sanitize).
    pub content: String,
    /// Platform-specific recipient.
    pub recipient: String,
    /// Optional subject (email / push variants); ignored on TG/Slack/Discord.
    pub subject: Option<String>,
    /// Threading id (Slack `thread_ts`); when set the platform should
    /// post as a reply.
    pub thread_ts: Option<String>,
}

impl SendMessage {
    /// Helper: build a plain message with no subject / thread.
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
        }
    }

    /// Builder-style: attach a thread id.
    pub fn in_thread(mut self, thread_ts: Option<String>) -> Self {
        self.thread_ts = thread_ts;
        self
    }
}

/// Core channel trait — implement for any IM platform.
///
/// **Listening contract**: implementations of [`Channel::listen`] run
/// for the daemon's lifetime, pushing one [`ChannelMessage`] per
/// inbound event to the supplied tokio mpsc sender. The future
/// returns `Ok(())` only on graceful shutdown (the sender was
/// dropped) and any other return value is treated as a fatal channel
/// error by the supervisor.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable platform name (matches credentials.json key).
    fn name(&self) -> &str;

    /// Send a single message. Returns the platform-side message id
    /// when available (Slack `ts`, Discord message id, …) for echo
    /// suppression in the outbound tailer.
    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>>;

    /// Long-running inbound listener (see trait-level docs).
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;

    /// Quick liveness probe (default: assume healthy). Supervisor uses
    /// this for periodic re-checks; failure logs a warn but doesn't
    /// tear the channel down (transient API outages are normal).
    async fn health_check(&self) -> bool {
        true
    }

    /// Per-message length ceiling, in **UTF-16 code units**, or `None`
    /// for no limit. When `Some(limit)`, the daemon splits an overflowing
    /// outbound reply into ordered sub-messages via
    /// [`crate::sanitize::split_for_channel`] before sending. The actual
    /// constant lives in the provider (e.g. Telegram's 4096) so the
    /// gateway/daemon stay channel-neutral — no `4096` or `"telegram"`
    /// branch leaks up. Default `None` = single send, today's behavior.
    fn max_message_len(&self) -> Option<usize> {
        None
    }
}
