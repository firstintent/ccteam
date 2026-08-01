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

/// `image_path` / `file_path` — the attribute key for an attachment kind.
/// Shared by the IM `<channel …>` first-attachment attribute, the IM
/// `[attachment …]` extra lines, and the web turn's attachment lines, so
/// the grammar the ccteam MCP instructions teach every vendor session
/// ("Read the file at image_path/file_path") never drifts between entry
/// surfaces.
pub fn attachment_path_key(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image_path",
        AttachmentKind::File => "file_path",
    }
}

/// The turn-text line naming one attachment for the agent to `Read` —
/// `[attachment image_path="…"]` / `[attachment file_path="…"]`. The ONE
/// line grammar both entry surfaces (IM extra attachments + web turn
/// attachments) emit; see [`attachment_path_key`].
pub fn attachment_line(att: &ChannelAttachment) -> String {
    format!(
        "[attachment {}=\"{}\"]",
        attachment_path_key(att.kind),
        att.local_path
    )
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

/// Setup-helper event: an inbound message whose sender was parsed successfully
/// but rejected by the provider-level allowlist gate.
///
/// The daemon records these to a small JSONL file so the web Settings flow can
/// show a user the id to allow — their own Lark `ou_...` open_id, or their
/// Telegram `chat_id` — without asking them to read server logs. The message is
/// still denied and never reaches the gateway.
///
/// One shape for every platform on purpose: the discovery flow is identical, so
/// a second struct would be the same fact living in two homes. [`Self::channel`]
/// says which bot saw it and therefore how to read [`Self::sender_id`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RejectedSenderProbe {
    /// Channel key that saw the event: `"lark"` / `"telegram"` for the
    /// global/admin bot, or `"<platform>@<tenant_id>"` for a per-user bot.
    pub channel: String,
    /// The id to place in the allowlist — a Lark sender `open_id` (`ou_...`),
    /// or a Telegram `chat_id`.
    pub sender_id: String,
    /// Conversation id (Lark `oc_...`; for Telegram the same value as
    /// [`Self::sender_id`]), kept for diagnostics and masked by the web API.
    pub chat_id: String,
    /// Raw provider message id (Lark `om_...`; Telegram the numeric id).
    pub message_id: String,
    /// Event time in Unix seconds, copied from the message when present.
    pub timestamp: u64,
}

impl RejectedSenderProbe {
    /// Append this probe to `path` as one JSONL line, creating the parent dir.
    ///
    /// Best effort by construction: a discovery aid must never interfere with
    /// message handling, so every failure logs a WARN and returns. Lives here
    /// rather than in a provider so both Lark and Telegram write the one
    /// format the web setup flow reads.
    pub async fn append_to(&self, path: &std::path::Path) {
        use tokio::io::AsyncWriteExt;
        let line = match serde_json::to_string(self) {
            Ok(line) => format!("{line}\n"),
            Err(err) => {
                tracing::warn!(error = %err, "rejected-sender probe encode failed");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::warn!(
                    path = %parent.display(), error = %err,
                    "rejected-sender probe dir create failed"
                );
                return;
            }
        }
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(line.as_bytes()).await {
                    tracing::warn!(
                        path = %path.display(), error = %err,
                        "rejected-sender probe append failed"
                    );
                } else if let Err(err) = file.flush().await {
                    tracing::warn!(
                        path = %path.display(), error = %err,
                        "rejected-sender probe flush failed"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(), error = %err,
                    "rejected-sender probe open failed"
                );
            }
        }
    }
}

/// Shared provider-edge handling for an inbound sender rejected by an IM
/// allowlist.
///
/// Rejection stays fail-closed: this helper has no inbound sender and cannot
/// forward the rejected payload to the gateway. It only preserves the setup
/// probe and sends one static, actionable notice per sender for this channel
/// listener's lifetime. Keeping both actions here prevents Telegram, Lark, and
/// future providers from drifting back to a silent message black hole.
#[derive(Debug, Default)]
pub(crate) struct RejectedSenderNotifier {
    probe_path: Option<std::path::PathBuf>,
    notice_state: tokio::sync::Mutex<RejectedSenderNoticeState>,
}

/// A rejected-sender flood must not grow daemon memory or bot replies without
/// bound. Normal personal bots have one or a handful of candidates; reaching
/// this ceiling means the listener is under abuse, so it stays fail-closed and
/// keeps writing setup probes but stops notifying new identities until reload.
const MAX_REJECTED_SENDERS_PER_LISTENER: usize = 1024;
const MAX_REJECTED_NOTICES_PER_MINUTE: usize = 20;
const REJECTED_NOTICE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Default)]
struct RejectedSenderNoticeState {
    notified_senders: std::collections::HashSet<String>,
    notice_times: std::collections::VecDeque<std::time::Instant>,
    capacity_warning_emitted: bool,
    rate_warning_emitted: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum RejectedSenderNoticeDecision {
    Notify,
    AlreadyNotified,
    AtCapacity { emit_warning: bool },
    RateLimited { emit_warning: bool },
}

impl RejectedSenderNoticeState {
    fn admit(&mut self, sender_id: &str, now: std::time::Instant) -> RejectedSenderNoticeDecision {
        if self.notified_senders.contains(sender_id) {
            return RejectedSenderNoticeDecision::AlreadyNotified;
        }
        if self.notified_senders.len() >= MAX_REJECTED_SENDERS_PER_LISTENER {
            let emit_warning = !self.capacity_warning_emitted;
            self.capacity_warning_emitted = true;
            return RejectedSenderNoticeDecision::AtCapacity { emit_warning };
        }

        self.notice_times
            .retain(|sent_at| now.duration_since(*sent_at) < REJECTED_NOTICE_WINDOW);
        if self.notice_times.len() >= MAX_REJECTED_NOTICES_PER_MINUTE {
            let emit_warning = !self.rate_warning_emitted;
            self.rate_warning_emitted = true;
            return RejectedSenderNoticeDecision::RateLimited { emit_warning };
        }

        self.rate_warning_emitted = false;
        self.notified_senders.insert(sender_id.to_string());
        self.notice_times.push_back(now);
        RejectedSenderNoticeDecision::Notify
    }
}

impl RejectedSenderNotifier {
    pub(crate) fn with_probe_path(path: std::path::PathBuf) -> Self {
        Self {
            probe_path: Some(path),
            notice_state: tokio::sync::Mutex::new(RejectedSenderNoticeState::default()),
        }
    }

    /// Record every rejected event for the web binding flow, then notify the
    /// sender at most once. Both operations are best effort and never weaken
    /// the allowlist decision made by the caller.
    pub(crate) async fn record_and_notify<C>(&self, channel: &C, probe: RejectedSenderProbe)
    where
        C: Channel + ?Sized,
    {
        if let Some(path) = self.probe_path.as_ref() {
            probe.append_to(path).await;
        }

        let decision = {
            self.notice_state
                .lock()
                .await
                .admit(&probe.sender_id, std::time::Instant::now())
        };
        match decision {
            RejectedSenderNoticeDecision::Notify => {}
            RejectedSenderNoticeDecision::AlreadyNotified => {
                tracing::debug!(
                    channel = %probe.channel,
                    sender_id = %probe.sender_id,
                    "dropping another event from rejected IM sender"
                );
                return;
            }
            RejectedSenderNoticeDecision::AtCapacity { emit_warning } => {
                if emit_warning {
                    tracing::warn!(
                        channel = %probe.channel,
                        max_senders = MAX_REJECTED_SENDERS_PER_LISTENER,
                        "rejected-sender notice capacity reached; suppressing notices for new identities until listener reload"
                    );
                }
                tracing::debug!(
                    channel = %probe.channel,
                    sender_id = %probe.sender_id,
                    "rejected-sender binding notice suppressed at capacity"
                );
                return;
            }
            RejectedSenderNoticeDecision::RateLimited { emit_warning } => {
                if emit_warning {
                    tracing::warn!(
                        channel = %probe.channel,
                        max_notices = MAX_REJECTED_NOTICES_PER_MINUTE,
                        "rejected-sender notice rate limit reached; suppressing this burst"
                    );
                }
                tracing::debug!(
                    channel = %probe.channel,
                    sender_id = %probe.sender_id,
                    "rejected-sender binding notice suppressed by rate limit"
                );
                return;
            }
        }

        tracing::warn!(
            channel = %probe.channel,
            sender_id = %probe.sender_id,
            "dropping event from rejected IM sender; sending one binding notice"
        );
        let notice = rejected_sender_notice(&probe.sender_id);
        if let Err(err) = channel
            .send(&SendMessage::new(notice, probe.chat_id.clone()))
            .await
        {
            tracing::warn!(
                channel = %probe.channel,
                sender_id = %probe.sender_id,
                error = %err,
                "rejected-sender binding notice send failed"
            );
        }
    }
}

fn rejected_sender_notice(sender_id: &str) -> String {
    format!(
        "此 IM 身份尚未绑定，消息未交给任何 agent。\n绑定 ID: {sender_id}\n请用此 bot 所属的 ccteam 账号打开「设置 → 接入」，绑定该 ID 后重试。"
    )
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
pub fn sanitize_attachment_name(name: &str) -> String {
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
    use crate::transport::providers::mock::MockChannel;

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

    #[tokio::test]
    async fn rejected_sender_is_probed_each_time_but_notified_once() {
        let tmp = tempfile::tempdir().unwrap();
        let probe_path = tmp.path().join("rejected-senders.jsonl");
        let notifier = RejectedSenderNotifier::with_probe_path(probe_path.clone());
        let channel = MockChannel::new().with_name("telegram@u123");
        let first = RejectedSenderProbe {
            channel: "telegram@u123".into(),
            sender_id: "339498819".into(),
            chat_id: "339498819".into(),
            message_id: "1".into(),
            timestamp: 10,
        };
        let mut second = first.clone();
        second.message_id = "2".into();
        second.timestamp = 11;

        notifier.record_and_notify(&channel, first.clone()).await;
        notifier.record_and_notify(&channel, second.clone()).await;

        let outbox = channel.outbox().await;
        assert_eq!(outbox.len(), 1, "repeat rejects must not spam the sender");
        assert_eq!(outbox[0].recipient, "339498819");
        assert_eq!(
            outbox[0].content,
            "此 IM 身份尚未绑定，消息未交给任何 agent。\n绑定 ID: 339498819\n请用此 bot 所属的 ccteam 账号打开「设置 → 接入」，绑定该 ID 后重试。"
        );

        let lines = tokio::fs::read_to_string(&probe_path).await.unwrap();
        let probes = lines
            .lines()
            .map(|line| serde_json::from_str::<RejectedSenderProbe>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            probes,
            vec![first, second],
            "every reject remains discoverable"
        );

        // The helper can only send outbound. It cannot inject the rejected
        // payload (or its notice) into the gateway-facing inbound stream.
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        channel.listen(tx).await.unwrap();
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn rejected_sender_notice_state_is_rate_and_memory_bounded() {
        let mut state = RejectedSenderNoticeState::default();
        let now = std::time::Instant::now();

        for index in 0..MAX_REJECTED_NOTICES_PER_MINUTE {
            assert_eq!(
                state.admit(&format!("ou_{index}"), now),
                RejectedSenderNoticeDecision::Notify
            );
        }
        assert_eq!(
            state.admit("ou_burst", now),
            RejectedSenderNoticeDecision::RateLimited { emit_warning: true }
        );
        assert_eq!(
            state.admit("ou_burst", now),
            RejectedSenderNoticeDecision::RateLimited {
                emit_warning: false
            },
            "one burst must only emit one warning"
        );

        let after_window = now + REJECTED_NOTICE_WINDOW + std::time::Duration::from_secs(1);
        assert_eq!(
            state.admit("ou_burst", after_window),
            RejectedSenderNoticeDecision::Notify,
            "a legitimate sender may retry after the burst window"
        );

        for index in state.notified_senders.len()..MAX_REJECTED_SENDERS_PER_LISTENER {
            state.notified_senders.insert(format!("fill_{index}"));
        }
        assert_eq!(
            state.notified_senders.len(),
            MAX_REJECTED_SENDERS_PER_LISTENER
        );
        assert_eq!(
            state.admit("ou_over_capacity", after_window),
            RejectedSenderNoticeDecision::AtCapacity { emit_warning: true }
        );
        assert_eq!(
            state.admit("ou_over_capacity", after_window),
            RejectedSenderNoticeDecision::AtCapacity {
                emit_warning: false
            }
        );
        assert_eq!(
            state.notified_senders.len(),
            MAX_REJECTED_SENDERS_PER_LISTENER
        );
    }
}
