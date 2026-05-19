//! V0.6.1 — per-bot in-process mpsc fast-path for chat traffic.
//!
//! The original V0.6.0 design used the filesystem as a queue between
//! the daemon's inbound consumer task and the per-bot supervisor:
//! inbound writes `<project>/.ccteam/chat/<bot>/inbox/msg-*.md`, and a
//! per-tick `drain_inboxes` scan reads them back. Same shape on the
//! outbound side: `spawn_events_consumer` appends `turns.jsonl`, and a
//! per-tick `drain_outboxes` byte-cursor scan reads the new rows.
//!
//! Both producer and consumer live in the **same daemon process**.
//! Using the filesystem as a queue between two tokio tasks means we
//! pay the supervisor tick (default 5s) as latency floor on each
//! direction — ~10s of structural delay per chat reply just from
//! polling.
//!
//! This module is the direct-path replacement:
//!
//! - The file is still written (durability + debug + safety-net
//!   recovery if the daemon dies between enqueue and consume).
//! - But the hot path flows through a per-bot `tokio::sync::mpsc`
//!   channel: inbound consumer enqueues an `InboxItem`; per-bot inbox
//!   task drains it and calls `BotSupervisor::handle_inbound`
//!   immediately; analogous flow for outbound.
//! - `drain_inboxes` / `drain_outboxes` stay alive as a slow safety
//!   net (60s tick) — they catch orphan files from a daemon crash or
//!   any mpsc race miss.
//!
//! The filesystem SoT red line is preserved: writes still happen,
//! `progress.jsonl` / `turns.jsonl` are still the recovery source.
//! Only the *wakeup mechanism* changed from polling to direct channel
//! delivery.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

/// One inbound mailbox envelope ready for `BotSupervisor::handle_inbound`.
///
/// Carries the payload directly so the consumer doesn't have to
/// re-read or parse the on-disk envelope. `path` is retained so the
/// consumer can `unlink` after a successful submit (which keeps the
/// safety-net drain_inboxes pass idempotent — it only sees orphans).
#[derive(Debug, Clone)]
pub struct InboxItem {
    /// Correlation id from the originating IM platform
    /// (e.g. `tg-{message_id}`).
    pub cid: String,
    /// Workflow slug — picks the bot supervisor.
    pub slug: String,
    /// Bot role.
    pub role: String,
    /// Sanitized payload (router-stripped of the `@<handle>` mention).
    pub payload: String,
    /// On-disk envelope path — `unlink` after handle_inbound succeeds.
    pub path: PathBuf,
    /// Wall-clock millis when the daemon enqueued this item — the
    /// consumer logs `queue_age_ms` against `now - enqueue_unix_ms`.
    pub enqueue_unix_ms: u128,
}

/// One outbound assistant row ready for dispatch to the IM platform.
///
/// `cursor_after` is the byte offset of `turns.jsonl` *after* this row
/// was appended; the dispatcher saves it as the new
/// `outbound.cursor` only on successful TG ack so a crash mid-burst
/// re-dispatches the unsent tail on restart.
#[derive(Debug, Clone)]
pub struct OutboundItem {
    /// turns_mirror's `TurnRecord.turn_id` — flows from claude
    /// `item.id` through the events consumer.
    pub turn_id: String,
    /// `"assistant"` / `"user"` / `"tool"` — only assistant rows are
    /// forwarded; non-assistant rows still advance the cursor so a
    /// daemon restart doesn't re-process them.
    pub role: String,
    /// Reply body the IM platform receives verbatim.
    pub content: String,
    /// Byte offset of `turns.jsonl` after this row's append. Used to
    /// persist `outbound.cursor` on successful TG ack.
    pub cursor_after: u64,
    /// Wall-clock millis when the daemon enqueued this row.
    pub enqueue_unix_ms: u128,
}

/// Per-bot send-side handles. The daemon owns these (one entry per
/// `<slug>/<role>` key); inbound consumer + supervisor events task
/// look up the right entry to push items into the bot-specific tasks.
#[derive(Clone)]
pub struct BotChannels {
    /// Inbound side — daemon `spawn_inbound_consumer` pushes here
    /// after writing the envelope file.
    pub inbox_tx: mpsc::Sender<InboxItem>,
    /// Outbound side — `BotSupervisor::spawn_events_consumer` pushes
    /// here after appending `turns.jsonl`.
    pub outbound_tx: mpsc::Sender<OutboundItem>,
}

/// `<slug>/<role>` → channels. Shared across the daemon's inbound
/// consumer task, the per-bot supervisor's events task, and the main
/// loop's `ensure_bot_channels` registration pass.
pub type BotChannelMap = Arc<Mutex<HashMap<String, BotChannels>>>;

/// Key the map uses — same convention as `SupervisorRegistry::key`.
pub fn bot_key(slug: &str, role: &str) -> String {
    format!("{slug}/{role}")
}

/// Bounded mpsc buffer. 64 is well past what one bot can drain in any
/// realistic burst (TG rate-limits to ~1/sec per chat); a slower
/// consumer applies natural backpressure on the producer.
pub const CHANNEL_BUF: usize = 64;
