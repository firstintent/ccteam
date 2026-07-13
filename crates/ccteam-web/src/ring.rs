//! Per-session SSE replay ring (v0.8.22 P1 — review §3.1-3).
//!
//! The gateway's event broadcast only reaches whoever is subscribed AT THE
//! MOMENT an event fires; a reconnecting SSE client (network blip, laptop
//! sleep, tab backgrounded) that resubscribes a moment later has already
//! missed anything sent during the gap — including a HITL approval prompt,
//! which then silently vanishes from view while its 120s TTL keeps ticking
//! toward an auto-deny the user never saw coming.
//!
//! [`SessionEventRing`] is a small (64-entry), per-sid, monotonically
//! sequenced replay buffer AND the live fan-out tap every per-connection SSE
//! handler subscribes through (`crate::routes::sessions_api::handle_session_events`
//! no longer touches the gateway's raw broadcast directly — see
//! [`Self::subscribe`]). Recording happens at ONE choke point
//! ([`Self::record`]), called by:
//!
//! - the persistent feeder task ([`spawn_ring_feeder`], spawned once from
//!   [`crate::state::AppState::with_gateway`]) — keeps the ring populated
//!   even while nobody is connected, which is exactly the gap a reconnect
//!   needs backfilled;
//! - [`crate::routes::sessions_api`]'s pending-approval reseed, which
//!   synthesizes + records a fresh entry when an outstanding approval isn't
//!   already covered by the ring.
//!
//! Every recorded entry gets a per-sid, strictly-increasing `seq` (never
//! reused/reset even as older entries are evicted past capacity) that is
//! set as the wire-level SSE `id:` field — the client's `Last-Event-ID` on
//! reconnect. A reconnecting client's `seq` gap is replayed from the ring;
//! a client with no `seq` at all (a brand-new tab) gets no ring replay (the
//! ordinary "start fresh" SSE contract) — but still gets the UNCONDITIONAL
//! pending-approval reseed, since that reads live from
//! `PendingInteractions`, not from ring contents.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use ccteam_im::gateway::{Gateway, GatewayEvent, GatewayEventKind};
use tokio::sync::broadcast;

/// Bounded per-sid history: enough to ride out a typical reconnect blip
/// without growing unbounded memory for a long-lived chatty session.
pub(crate) const RING_CAPACITY: usize = 64;

/// Fan-out channel capacity for the ring's live tap. Generous relative to
/// how often ANY one session emits events, so an idle-ish per-connection
/// SSE consumer is very unlikely to `Lagged` (the existing
/// `reconnect_hint` path handles it gracefully if it ever does).
const TAP_CAPACITY: usize = 1024;

/// One buffered/tapped frame: its ring-assigned sequence number (monotonic
/// per sid) + the [`GatewayEvent`] itself.
#[derive(Debug, Clone)]
pub(crate) struct RingEntry {
    pub seq: u64,
    pub event: GatewayEvent,
}

#[derive(Default)]
struct RingState {
    next_seq: u64,
    buf: VecDeque<RingEntry>,
}

/// Shared per-sid replay ring + live tap. Cheap to clone via `Arc`; the
/// internal bookkeeping mutex is a plain `std::sync::Mutex` since every
/// operation is O(capacity) and never holds across an `.await`.
pub(crate) struct SessionEventRing {
    rings: Mutex<HashMap<String, RingState>>,
    tap: broadcast::Sender<RingEntry>,
}

impl SessionEventRing {
    pub(crate) fn new() -> Self {
        let (tap, _rx) = broadcast::channel(TAP_CAPACITY);
        Self {
            rings: Mutex::new(HashMap::new()),
            tap,
        }
    }

    /// Subscribe to the live tap: every entry [`Self::record`] accepts,
    /// across every sid (the per-connection SSE handler filters down to its
    /// target sid, mirroring how it used to filter the gateway's raw
    /// broadcast).
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RingEntry> {
        self.tap.subscribe()
    }

    /// Record `event` (already known to belong to `sid`) into its ring,
    /// evicting the oldest entry past [`RING_CAPACITY`], AND fan it out to
    /// every live subscriber. Returns the assigned seq. A closed/lagging tap
    /// (no subscribers, or a saturated one) is not an error — the ring
    /// itself is still the durable side of this record.
    pub(crate) fn record(&self, sid: &str, event: GatewayEvent) -> u64 {
        let seq = {
            let mut guard = self.rings.lock().unwrap();
            let state = guard.entry(sid.to_string()).or_default();
            state.next_seq += 1;
            let seq = state.next_seq;
            state.buf.push_back(RingEntry {
                seq,
                event: event.clone(),
            });
            while state.buf.len() > RING_CAPACITY {
                state.buf.pop_front();
            }
            seq
        };
        let _ = self.tap.send(RingEntry { seq, event });
        seq
    }

    /// Every buffered entry for `sid` with `seq > since`, oldest first.
    /// Best-effort catch-up: if `since` predates everything still buffered
    /// (the gap outran the ring's capacity), this simply returns everything
    /// left — the closest available approximation of "what you missed".
    pub(crate) fn replay_since(&self, sid: &str, since: u64) -> Vec<RingEntry> {
        let guard = self.rings.lock().unwrap();
        guard
            .get(sid)
            .map(|s| s.buf.iter().filter(|e| e.seq > since).cloned().collect())
            .unwrap_or_default()
    }
}

/// True for [`GatewayEvent`]s the web SSE never renders (v0.8.19): the 👀
/// ack `Reaction` is an IM-only affordance (Telegram/Lark message
/// reaction); the web chat has its own UI. Shared by the ring feeder (never
/// record them) and [`crate::routes::sessions_api`]'s pending-approval
/// reseed path.
pub(crate) fn is_im_only_event(ev: &GatewayEvent) -> bool {
    matches!(ev.kind, GatewayEventKind::Reaction { .. })
}

/// v0.9.0 W4 (F4) — capacity for the team view's GLOBAL replay ring
/// (`GET /api/v1/agents/events`). Bigger than the per-sid [`RING_CAPACITY`]
/// since one stream now carries every session's events.
pub(crate) const GLOBAL_RING_CAPACITY: usize = 256;

/// v0.9.0 W4 — the team view's cross-session replay ring + live tap: the
/// SAME shape as [`SessionEventRing`] (monotonic `seq`, bounded backlog,
/// broadcast live tap for `Last-Event-ID` reconnects) but ONE stream instead
/// of one-per-sid — every event the gateway broadcasts (minus the IM-only
/// `Reaction`) lands here in arrival order. **ACL is the ROUTE's job, not
/// this ring's** — same division of labor [`SessionEventRing`] already has
/// with auth (this type has no concept of `Identity`); `crate::routes::agents`
/// filters replayed + tapped frames by `ev.slug` before they reach a client.
pub(crate) struct GlobalEventRing {
    state: Mutex<RingState>,
    tap: broadcast::Sender<RingEntry>,
}

impl GlobalEventRing {
    pub(crate) fn new() -> Self {
        let (tap, _rx) = broadcast::channel(TAP_CAPACITY);
        Self {
            state: Mutex::new(RingState::default()),
            tap,
        }
    }

    /// Subscribe to the live tap: every entry [`Self::record`] accepts.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RingEntry> {
        self.tap.subscribe()
    }

    /// Record `event`, evicting the oldest entry past
    /// [`GLOBAL_RING_CAPACITY`], AND fan it out to every live subscriber.
    /// Returns the assigned seq.
    pub(crate) fn record(&self, event: GatewayEvent) -> u64 {
        let seq = {
            let mut guard = self.state.lock().unwrap();
            guard.next_seq += 1;
            let seq = guard.next_seq;
            guard.buf.push_back(RingEntry {
                seq,
                event: event.clone(),
            });
            while guard.buf.len() > GLOBAL_RING_CAPACITY {
                guard.buf.pop_front();
            }
            seq
        };
        let _ = self.tap.send(RingEntry { seq, event });
        seq
    }

    /// Every buffered entry with `seq > since`, oldest first — the same
    /// best-effort catch-up contract as [`SessionEventRing::replay_since`].
    pub(crate) fn replay_since(&self, since: u64) -> Vec<RingEntry> {
        let guard = self.state.lock().unwrap();
        guard
            .buf
            .iter()
            .filter(|e| e.seq > since)
            .cloned()
            .collect()
    }
}

/// Spawn the ONE persistent global-ring-feeder task for this gateway (mirrors
/// [`spawn_ring_feeder`] below, see its doc for the "why a persistent feeder"
/// rationale). Records every non-IM-only event regardless of `sid`/`slug` —
/// the team view route filters by ACL at read time.
pub(crate) fn spawn_global_ring_feeder(
    gateway: Arc<tokio::sync::Mutex<Gateway>>,
    ring: Arc<GlobalEventRing>,
) {
    tokio::spawn(async move {
        let mut rx = gateway.lock().await.subscribe_events();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if is_im_only_event(&ev) {
                        continue;
                    }
                    ring.record(ev);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Spawn the ONE persistent ring-feeder task for this gateway — called once
/// from [`crate::state::AppState::with_gateway`] (the composition root).
/// Subscribes a fresh broadcast receiver off the gateway and forwards every
/// sid-tagged, non-IM-only event into `ring` for as long as the gateway
/// lives (i.e. the daemon process) — independent of whether any SSE client
/// is currently connected, which is the whole point: the ring must still be
/// populated during a client's disconnected gap. A `Lagged` broadcast error
/// just means the FEEDER itself missed events (nothing to backfill from —
/// the ring's job is "best-effort small window", not a durable log), so it
/// keeps going with whatever arrives next.
pub(crate) fn spawn_ring_feeder(
    gateway: Arc<tokio::sync::Mutex<Gateway>>,
    ring: Arc<SessionEventRing>,
) {
    tokio::spawn(async move {
        let mut rx = gateway.lock().await.subscribe_events();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if is_im_only_event(&ev) {
                        continue;
                    }
                    if let Some(sid) = ev.sid.clone() {
                        ring.record(&sid, ev);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, sid: &str) -> GatewayEvent {
        GatewayEvent {
            id: id.to_string(),
            channel: "web".to_string(),
            chat_id: "c".to_string(),
            thread_ts: None,
            content: "hi".to_string(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(sid.to_string()),
            slug: None,
        }
    }

    #[test]
    fn record_assigns_monotonic_seq_per_sid() {
        let ring = SessionEventRing::new();
        let a = ring.record("s1", ev("a", "s1"));
        let b = ring.record("s1", ev("b", "s1"));
        let c = ring.record("s2", ev("c", "s2"));
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        // A different sid starts its own counter.
        assert_eq!(c, 1);
    }

    #[test]
    fn replay_since_returns_only_the_gap() {
        let ring = SessionEventRing::new();
        ring.record("s1", ev("a", "s1"));
        let b = ring.record("s1", ev("b", "s1"));
        ring.record("s1", ev("c", "s1"));
        let replayed = ring.replay_since("s1", b - 1);
        assert_eq!(replayed.len(), 2, "b and c, not a");
        assert_eq!(replayed[0].event.id, "b");
        assert_eq!(replayed[1].event.id, "c");
    }

    #[test]
    fn replay_since_current_seq_is_empty() {
        let ring = SessionEventRing::new();
        let last = ring.record("s1", ev("a", "s1"));
        assert!(ring.replay_since("s1", last).is_empty());
    }

    #[test]
    fn ring_evicts_oldest_past_capacity() {
        let ring = SessionEventRing::new();
        for i in 0..(RING_CAPACITY + 10) {
            ring.record("s1", ev(&format!("e{i}"), "s1"));
        }
        let replayed = ring.replay_since("s1", 0);
        assert_eq!(replayed.len(), RING_CAPACITY, "capped at RING_CAPACITY");
        // The oldest surviving entry is #10 (0..10 evicted).
        assert_eq!(replayed[0].event.id, "e10");
    }

    #[test]
    fn replay_since_unknown_sid_is_empty() {
        let ring = SessionEventRing::new();
        assert!(ring.replay_since("nope", 0).is_empty());
    }

    #[test]
    fn is_im_only_event_flags_only_reaction() {
        assert!(!is_im_only_event(&ev("a", "s1")));
        let mut reaction = ev("a", "s1");
        reaction.kind = GatewayEventKind::Reaction {
            message_id: "tg-1".into(),
            on: true,
        };
        assert!(is_im_only_event(&reaction));
    }

    /// A live subscriber (the tap) sees every recorded entry, carrying the
    /// SAME seq the ring assigned.
    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_taps_recorded_entries() {
        let ring = SessionEventRing::new();
        let mut rx = ring.subscribe();
        let seq = ring.record("s1", ev("a", "s1"));
        let entry = rx.try_recv().expect("tapped");
        assert_eq!(entry.seq, seq);
        assert_eq!(entry.event.id, "a");
    }
}
