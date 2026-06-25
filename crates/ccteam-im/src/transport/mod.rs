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

/// Kind of an inbound [`ChannelAttachment`] (V0.8.4 P2a).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A photo/image the agent should `Read` to see (e.g. an error
    /// screenshot).
    Image,
    /// A non-image document/file (pdf, log, …) the agent may `Read`.
    File,
}

/// An inbound file/image carried by a [`ChannelMessage`] (V0.8.4 P2a).
///
/// The channel listener downloads the bytes to a staging dir and records
/// the absolute `local_path` here; the gateway then names that path in
/// the turn text so the agent can `Read` it. (send-keys can only carry
/// text — there is no base64 content-block path — so attachments are
/// always "download → give path → `Read`".)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelAttachment {
    /// Image vs. generic file.
    pub kind: AttachmentKind,
    /// Sanitized original file name (no path separators / control chars).
    pub file_name: String,
    /// Absolute path on the daemon/agent **shared** filesystem.
    pub local_path: String,
    /// MIME type, when the platform reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Size in bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

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
    /// Inbound attachments (images / files) already downloaded to disk.
    /// Empty for text-only messages and non-Telegram channels. (P2a)
    #[serde(default)]
    pub attachments: Vec<ChannelAttachment>,
    /// Set when this inbound event is an option click (v0.8.5 D3) — e.g. a
    /// Telegram `callback_query` or a web chip click — instead of free
    /// text. `None` for ordinary messages.
    #[serde(default)]
    pub selection: Option<ChoiceReply>,
}

/// v0.8.20 F2 — a per-tenant IM bot's channel name is `"<platform>@<tenant_id>"`
/// (the `@` keeps the channel-map key unique per bot, so outbound replies route
/// to the RIGHT bot). The platform prefix (before `@`) is what platform-keyed
/// logic — the inbound ACL, `setMyCommands` — must use; the full name is the
/// routing key. The global/admin bot keeps a bare platform name (`"telegram"`).
pub fn platform_of(channel: &str) -> &str {
    channel.split('@').next().unwrap_or(channel)
}

/// Whether a channel name belongs to a per-tenant bot (`"<platform>@<tenant>"`)
/// rather than the global/admin bot (`"telegram"`/`"lark"`/…) or web (`"web"`).
pub fn is_tenant_bot_channel(channel: &str) -> bool {
    channel.contains('@')
}

/// v0.8.20 — the tenant id of a per-tenant bot channel (`"<platform>@<tenant>"`),
/// or `None` for the global/admin bot + web. Used to converge a tenant's IM bot
/// onto its web identity `user:<tenant>` (web↔IM convergence).
pub fn tenant_of_bot_channel(channel: &str) -> Option<&str> {
    channel.split_once('@').map(|(_platform, tid)| tid)
}

/// How an [`OutboundFile`] should be sent (V0.8.4 P2b).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutboundFileKind {
    /// Compressed image (Telegram `sendPhoto`).
    Photo,
    /// Generic file (Telegram `sendDocument`).
    Document,
}

/// A file to send back to a chat as an attachment on an outbound message
/// (V0.8.4 P2b). `path` is on the daemon/agent **shared** filesystem
/// (a remote `ProcessBackend` would need an "upload bytes" variant — a
/// recorded assumption, not designed here).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundFile {
    /// Absolute path to the file on disk.
    pub path: String,
    /// Optional caption (placed on the first attachment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Photo vs. document.
    pub kind: OutboundFileKind,
}

/// A single selectable option rendered on an outbound [`SendMessage`]
/// (v0.8.5 D3). Channel-local + deliberately opaque: `data` is whatever
/// the gateway minted (always `"{token}:{idx}"`) and rides the platform's
/// callback channel verbatim (Telegram `callback_data`, web chip id);
/// `label` is the button text. The channel never interprets either — it
/// renders `label`, returns `data` on click. Kept distinct from the
/// harness-layer `ChoiceOption` so the channel axis never imports a
/// harness type (two-axis decoupling discipline; not a compile barrier —
/// `ccteam-im` already depends on `ccteam-harness`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageOption {
    /// Opaque callback payload, always `"{token}:{idx}"`. MUST stay short
    /// (≤ ~20 bytes) — Telegram caps `callback_data` at 64 bytes.
    pub data: String,
    /// Human-readable button label.
    pub label: String,
    /// v0.8.7 review-fix (R-H1) — the stable option `id` from the source
    /// [`ChoiceOption`] (e.g. `"allow"` / `"deny"`). IM channels ignore it
    /// (they render `label`, echo `data` on click); it exists so a tokenless
    /// web client can carry the option's real id through its own
    /// `POST /sessions/{sid}/resolve {token, selection=id}` path — resolving
    /// the SAME token-keyed pending the IM callback does, never a turn.
    #[serde(default)]
    pub id: String,
}

/// An inbound option click carried on a [`ChannelMessage`] (v0.8.5 D3).
/// `data` echoes the [`MessageOption::data`] the user clicked; the gateway
/// splits it on the first `:` into `(token, idx)` and resolves `idx` back
/// to the real option id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChoiceReply {
    /// The clicked option's opaque callback payload (`"{token}:{idx}"`).
    pub data: String,
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
    /// Outbound file attachments (V0.8.4 P2b). Empty ⇒ a plain text send
    /// (`sendMessage`); non-empty ⇒ `sendPhoto`/`sendDocument`.
    #[serde(default)]
    pub attachments: Vec<OutboundFile>,
    /// Selectable options rendered as buttons / chips (v0.8.5 D3). Empty ⇒
    /// an ordinary message (zero behavior change). Channels without native
    /// buttons fall back to a numbered text list.
    #[serde(default)]
    pub options: Vec<MessageOption>,
}

impl SendMessage {
    /// Helper: build a plain message with no subject / thread / files.
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
            attachments: Vec::new(),
            options: Vec::new(),
        }
    }

    /// Builder-style: attach a thread id.
    pub fn in_thread(mut self, thread_ts: Option<String>) -> Self {
        self.thread_ts = thread_ts;
        self
    }

    /// Builder-style: attach outbound files.
    pub fn with_attachments(mut self, attachments: Vec<OutboundFile>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Builder-style: attach selectable options (v0.8.5 D3).
    pub fn with_options(mut self, options: Vec<MessageOption>) -> Self {
        self.options = options;
        self
    }
}

/// A gateway-owned command advertised in a channel's command menu
/// (v0.8.5 P1). Registered once at daemon startup via
/// [`Channel::register_commands`]; only channels with a native menu
/// (Telegram `setMyCommands`) act on it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandSpec {
    /// Command name including the leading `/`.
    pub name: String,
    /// One-line description shown in the channel's command menu.
    pub description: String,
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

    /// Edit a previously-sent message in place (V0.8.4 P1 — live progress
    /// status). Returns the platform message id (usually `message_id`
    /// unchanged). The **default degrades gracefully** to appending a new
    /// message via [`Channel::send`], so a channel without edit support
    /// still shows progress (just as extra messages rather than one live
    /// status). Telegram overrides with `editMessageText`.
    async fn edit_message(
        &self,
        recipient: &str,
        _message_id: &str,
        content: &str,
    ) -> anyhow::Result<Option<String>> {
        self.send(&SendMessage::new(content, recipient)).await
    }

    /// Register the gateway's own commands in the channel's command menu
    /// (v0.8.5 P1). **Default no-op** (same pattern as
    /// [`Channel::max_message_len`]): only a channel with a native menu
    /// overrides it (Telegram → `setMyCommands`). Keeps the daemon
    /// channel-neutral — no `"telegram"` branch leaks up.
    async fn register_commands(&self, _cmds: &[CommandSpec]) -> anyhow::Result<()> {
        Ok(())
    }

    /// Add the transient "seen/ack" reaction (👀-equivalent) to a message —
    /// the instant "received, processing" acknowledgement the gateway adds the
    /// moment an inbound message is dispatched to a session as a turn, filling
    /// the silent time-to-first-token gap. Removed by [`remove_reaction`] when
    /// the session's first turn event appears (the "💭 thinking…" sign).
    ///
    /// The 👀-equivalent is hardcoded per provider (the only use is this ack,
    /// so there is no generic emoji argument). Returns an opaque `handle` the
    /// provider needs to remove it later (e.g. Feishu's `reaction_id`), or
    /// `None` when the provider clears a reaction by `(chat, message)` alone
    /// (Telegram). **Default no-op `Ok(None)`** — web/discord/slack/mock/ws
    /// keep it (reactions are an IM-only affordance), so they need no change.
    /// Fire-and-forget at the call site: a reaction failure must NEVER break or
    /// delay turn/answer delivery.
    async fn add_reaction(
        &self,
        _chat_id: &str,
        _message_id: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Remove the reaction [`add_reaction`] added. `handle` is whatever that
    /// returned (the provider's reaction handle, e.g. Feishu's `reaction_id`),
    /// or `None` for providers that clear by `(chat, message)` alone.
    /// **Default no-op** — same providers as [`add_reaction`]. Fire-and-forget
    /// at the call site.
    async fn remove_reaction(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _handle: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared inbound-attachment staging helpers (V0.8.4 P2a). Every provider
// that downloads inbound files (telegram, lark, …) routes through these so
// the staging layout and the name-sanitization rule stay identical across
// channels — a fix in one place protects every channel's `image_path="…"`
// turn-text attribute.
// ─────────────────────────────────────────────────────────────────────────────

/// Staging dir for downloaded inbound attachments (channel-scoped — the
/// routing to a project/role happens later in the gateway).
pub(crate) fn inbound_staging_dir() -> std::path::PathBuf {
    crate::default_ccteam_root_public()
        .join("state")
        .join("im")
        .join("attachments")
        .join("inbound")
}

/// Pure: strip path separators / control chars and cap length so a
/// platform-supplied name can't traverse out of the staging dir. Also
/// drops `" < >` so the name can't break (or inject into) the
/// `<channel image_path="…">` turn-text attribute the gateway builds.
pub(crate) fn sanitize_attachment_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | '"' | '<' | '>'))
        .take(128)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.8.20 F2 — the platform prefix is what platform-keyed logic (ACL,
    /// menus) uses; the `@`-suffixed full name is the per-bot routing key.
    #[test]
    fn platform_of_and_tenant_bot_channel() {
        assert_eq!(platform_of("telegram"), "telegram");
        assert_eq!(platform_of("telegram@uabc"), "telegram");
        assert_eq!(platform_of("lark@u123"), "lark");
        assert_eq!(platform_of("web"), "web");
        assert!(!is_tenant_bot_channel("telegram"));
        assert!(!is_tenant_bot_channel("web"));
        assert!(is_tenant_bot_channel("telegram@uabc"));
        assert!(is_tenant_bot_channel("lark@u123"));
    }

    #[test]
    fn sanitize_attachment_name_blocks_traversal_and_control() {
        assert_eq!(sanitize_attachment_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_attachment_name("a/b/c.png"), "c.png");
        assert_eq!(sanitize_attachment_name("ok\u{0000}name.txt"), "okname.txt");
        assert_eq!(sanitize_attachment_name(""), "file");
        assert_eq!(sanitize_attachment_name("   "), "file");
        // Quotes/angle brackets would break the `image_path="…"` attr.
        assert_eq!(sanitize_attachment_name("foo\"bar.pdf"), "foobar.pdf");
        assert_eq!(sanitize_attachment_name("a<b>c.png"), "abc.png");
    }
}
