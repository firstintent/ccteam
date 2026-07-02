//! Latency-analysis logging helpers (V0.6.1 chat-mode timing instrumentation).
//!
//! The TG-mode-3 chat reply path crosses 7 stages (TG ingress → router →
//! mailbox → inbox drain → tmux send-keys → claude turn → hooks →
//! turns_mirror → outbound tail → TG egress). Each stage now emits one
//! `tracing::info!` with the fields below so a single grep can
//! reconstruct a full per-message timeline.
//!
//! Field conventions (kept stable across crates so log post-processing
//! doesn't need stage-specific parsers):
//!
//! - `event = "latency"` — common marker so `journalctl | grep latency`
//!   yields the timing rows and nothing else.
//! - `cid` — correlation id. For TG: `"tg-{message_id}"` (synthesized
//!   in `TelegramChannel::listen`). Flows through `ChannelMessage::id`
//!   and is preserved verbatim into the
//!   downstream logs that can see it (stages A–D, G). Stages F + parts
//!   of E correlate via `turn_id` instead — the Claude session never
//!   sees the cid.
//! - `stage` — short tag (`"tg.ingress"`, `"imd.route"`, ...).
//! - `elapsed_ms` — within-stage wall-clock duration.
//! - `queue_age_ms` / `tail_age_ms` — cross-stage wait time (e.g. how
//!   long an envelope sat in the inbox before the 5s drain tick picked
//!   it up).

use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock millis since UNIX epoch. Used by latency logs to compute
/// cross-stage age deltas (a downstream stage diffs this against an
/// upstream-recorded `ts` to get queue/tail wait time).
pub fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
