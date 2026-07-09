//! W4 EnrichedEvent merger — priority-with-grace-window.
//!
//! A single user-visible occurrence (e.g. "the Read tool was called
//! with /foo") can surface from multiple channels of differing
//! fidelity:
//!
//! | Priority | Channel | Fidelity |
//! |---|---|---|
//! | **P1** | Codex JSON-RPC notification / Claude Code hook subprocess | lossless (Layer 4 typed) |
//! | P2 | rmux `line_stream` + a registered regex (`crate::patterns`) | lossy (Layer 2 TUI render) |
//! | P3 | rmux process events (`OutputIdle`, `ProcessExited`) | process-level only |
//!
//! The merger's job is to **emit at most one logical event per
//! occurrence**, sourcing the richest channel available, while staying
//! responsive when only the lossy channel ever fires. The algorithm is
//! a priority queue with a grace window (spec:
//! `docs/versions/v0-8-rmux/w4-enriched-event-merger.md`).
//!
//! ## Status: WIRED (V0.8 Slices 1 + 2) — pairing path live
//!
//! This module is a **self-contained, pure-logic library**: it consumes
//! [`BaseEvent`] / [`EnrichmentEvent`] values and produces
//! [`EnrichedEvent`] values. It performs **no I/O** and is fully unit-
//! testable with `tokio::time::pause()` (see the inline tests + the
//! acceptance suite in `tests/enriched_event_merger.rs`).
//!
//! It has a real production producer + consumer:
//! - *Producer*: [`crate::typed_event_tap::TypedEventTap`] registers a
//!   session's base patterns, subscribes, and lifts
//!   `MuxEvent::PatternMatched` / `OutputIdle` / `ProcessExited` into
//!   [`BaseEvent`]s fed to this merger.
//! - *Consumer*: `crate::execution::typed_events` reads the merged
//!   stream and writes `progress.jsonl` rows (flag-gated on
//!   `CCTEAM_TYPED_EVENTS`, integrated at the Claude chat-TUI spawn).
//!
//! **Slice 1** — the no-enrichment kinds ([`EnrichmentSource::None`] —
//! RateLimitHit / ContextOverflow / Idle / ProcessExited) emit
//! [`MergeOutcome::BaseOnly`] immediately → `typed_event` rows.
//!
//! **Slice 2** — the grace-window *pairing* path is live: a
//! session→[`crate::typed_event_tap::TapHandle`] registry lets the
//! orchestrator's `HookSink` route a Claude `Stop` hook to the matching
//! session's tap as a `TurnDone` [`EnrichmentEvent`]. A `turn_done` pane
//! pattern with no `Stop` hook within grace → [`MergeOutcome::BaseLossy`]
//! → a `merger_lossy_partial` row (the reliability fallback); a paired hook
//! → [`MergeOutcome::Paired`], suppressed. Scope: `TurnDone` only
//! (single-in-flight → safe pairing); multi-in-flight kinds need a
//! per-kind-counter redesign (Slice 3). Both `CCTEAM_TYPED_EVENTS` and
//! `CCTEAM_HOOK_VIA_DAEMON` must be set; the default path is untouched.
//!
//! ## Sequence-id contract
//!
//! The merger pairs by **(session_id, kind, sequence_id)**. It does NOT
//! mint `sequence_id`s — it trusts the caller (the daemon, at
//! integration time) to assign the *same* `sequence_id` to a base and
//! its enrichment for the same occurrence. Per spec §"Pairing key", the
//! daemon increments a per-(session, kind) counter on each base match
//! and the P1 source reads the matching value via a daemon RPC. In
//! these tests we construct paired events with identical `sequence_id`
//! directly.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::MuxSessionId;

/// Default grace window. Covers a cold-start hook subprocess fork
/// (~100–300ms observed on dev machines) without stalling real-time
/// downstreams. Spec §"Choice of GRACE_MS".
pub const DEFAULT_GRACE: Duration = Duration::from_millis(500);

/// Per-session ring-buffer capacity for both `recent_base` and
/// `pending_enrichment`. Spec §"Backpressure / overflow".
pub const BUFFER_CAPACITY: usize = 64;

/// Logical event class — the join axis between a base and its
/// enrichment. Mirrors the per-event-kind dispatch table in the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    ToolCallStarted,
    ToolCallCompleted,
    UserPromptSubmitted,
    AssistantMessageComplete,
    TurnDone,
    PlanPending,
    SessionReset,
    CompactDone,
    /// Rate-limit hit — P2 base is sufficient, no enrichment source.
    RateLimitHit,
    /// Context low / overflow — P2 base sufficient.
    ContextOverflow,
    /// Pane idle (P3 derived) — process-level, no enrichment.
    Idle,
    /// Process exited (P3) — process-level, no enrichment.
    ProcessExited,
    // NOTE: `TurnStarted` was removed in Slice 4 — the dispatch table mapped
    // both `TurnStarted` and `UserPromptSubmitted` to `turn/started` /
    // `user_prompt_submit` on the same notification, which would have
    // emitted duplicate rows from one occurrence. `UserPromptSubmitted` is
    // the canonical kind; `TurnStarted` had no producer or consumer.
}

/// The vendor whose channels feed a given occurrence. Determines which
/// P1 source (if any) supplies enrichment for a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Claude,
    Codex,
    Grok,
}

/// Where the lossless (P1) enrichment for a `(kind, vendor)` pair comes
/// from. [`EnrichmentSource::None`] means the P2 base is authoritative
/// and the merger emits immediately without opening a grace window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichmentSource {
    /// Claude Code hook subprocess event, named by its hook event
    /// (`pre_tool_use`, `post_tool_use`, `user_prompt_submit`, ...).
    ClaudeHook(&'static str),
    /// Codex `app-server` JSON-RPC notification, named by wire method
    /// (`notification/tool_call_begin`, `turn/completed`, ...).
    CodexJsonRpc(&'static str),
    /// No richer source — the P2 base (or P3 process event) is the
    /// authoritative source for this kind. Skips the grace window.
    None,
}

/// Dispatch: for a logical `(kind, vendor)` pair, what P1 source (if
/// any) enriches it. Spec §"Per-event-kind dispatch table" — 10 base
/// kinds × 2 vendors plus the `None` tail.
pub fn enrichment_source(kind: EventKind, vendor: Vendor) -> EnrichmentSource {
    use EnrichmentSource::*;
    use EventKind::*;
    use Vendor::*;
    match (kind, vendor) {
        (ToolCallStarted, Claude) => ClaudeHook("pre_tool_use"),
        (ToolCallStarted, Codex) => CodexJsonRpc("item/started"),
        // Grok ACP: events come from JSON-RPC directly; no P1 enrichment grace.
        (ToolCallStarted, Grok) => None,

        (ToolCallCompleted, Claude) => ClaudeHook("post_tool_use"),
        (ToolCallCompleted, Codex) => CodexJsonRpc("item/completed"),
        (ToolCallCompleted, Grok) => None,

        (UserPromptSubmitted, Claude) => ClaudeHook("user_prompt_submit"),
        // Codex has no TUI user-prompt echo wired today; kept for parity.
        (UserPromptSubmitted, Codex) => CodexJsonRpc("turn/started"),
        (UserPromptSubmitted, Grok) => None,

        (AssistantMessageComplete, Claude) => ClaudeHook("stop"),
        (AssistantMessageComplete, Codex) => CodexJsonRpc("item/agentMessage/delta"),
        (AssistantMessageComplete, Grok) => None,

        (TurnDone, Claude) => ClaudeHook("stop"),
        (TurnDone, Codex) => CodexJsonRpc("turn/completed"),
        (TurnDone, Grok) => None,

        (PlanPending, Claude) => ClaudeHook("pre_tool_use"),
        (PlanPending, Codex) => CodexJsonRpc("turn/plan/updated"),
        (PlanPending, Grok) => None,

        (SessionReset, Claude) => ClaudeHook("session_start"),
        (SessionReset, Codex) => CodexJsonRpc("thread/started"),
        (SessionReset, Grok) => None,

        (CompactDone, Claude) => ClaudeHook("stop"),
        (CompactDone, Codex) => CodexJsonRpc("thread/compacted"),
        (CompactDone, Grok) => None,

        // P2 base / P3 process is sufficient — no grace window.
        (RateLimitHit, _) => None,
        (ContextOverflow, _) => None,
        (Idle, _) => None,
        (ProcessExited, _) => None,
    }
}

/// The lossy/process-level base detection — a P2 `PatternMatched` or P3
/// process event, lifted into the merger's vocabulary.
#[derive(Debug, Clone)]
pub struct BaseEvent {
    pub session_id: MuxSessionId,
    pub kind: EventKind,
    pub vendor: Vendor,
    pub sequence_id: u64,
    pub timestamp: SystemTime,
    pub payload: BasePayload,
    /// Slice 4 — optional **identity** for cohort-based pairing.
    ///
    /// For `ToolCallStarted` Claude this is the tool name captured from
    /// the pane regex `^●\s+(\w+)\(`; for `ToolCallCompleted` the pane
    /// regex `^\s*⎿` captures nothing, so identity is `None` on the base
    /// side and cohort partitioning happens via the enrichment-side
    /// `tool_name`. For every other kind it is `None`. Identity carries
    /// into the merger's pairing predicate so two parallel tool calls of
    /// different tools never cross-pair (see
    /// `docs/versions/v0-8-rmux/w-slice-4-identity-and-codex.md`).
    pub identity: Option<String>,
}

/// The lossless (P1) enrichment — a Claude hook event or Codex JSON-RPC
/// notification, lifted into the merger's vocabulary.
#[derive(Debug, Clone)]
pub struct EnrichmentEvent {
    pub session_id: MuxSessionId,
    pub kind: EventKind,
    pub vendor: Vendor,
    pub sequence_id: u64,
    pub timestamp: SystemTime,
    pub payload: EnrichmentPayload,
    /// Slice 4 — see [`BaseEvent::identity`]. For tool-call hooks this
    /// is `Some(tool_name)` extracted from the hook payload by the
    /// consumer-side `identity_for(...)` helper. `None` preserves the
    /// pre-Slice-4 per-kind FIFO behaviour exactly.
    pub identity: Option<String>,
}

/// Lossy base detail. The captured regex group / process detail; the
/// merger treats this opaquely (it only pairs / forwards it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BasePayload {
    /// The captured regex group (P2) or a short process detail (P3).
    pub captured: String,
}

/// Lossless enrichment detail (typed P1 payload). Held as an opaque
/// JSON-ish string here — the daemon serialises the typed hook /
/// JSON-RPC payload into it at integration time; the merger never
/// inspects it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentPayload {
    pub data: String,
}

/// The merged, ready-to-emit event. `base` is always present; an
/// `enrichment` of `None` is either a lossy fallback (grace expired) or
/// a kind with no enrichment source.
#[derive(Debug, Clone)]
pub struct EnrichedEvent {
    pub session_id: MuxSessionId,
    pub kind: EventKind,
    pub timestamp: SystemTime,
    pub sequence_id: u64,
    /// Always present, except on the [`MergeOutcome::EnrichmentOnly`]
    /// path where the base never arrived (then `base` is `None`).
    pub base: Option<BasePayload>,
    /// `Some` = paired with a lossless P1 source; `None` = lossy
    /// fallback (the orchestrator emits a `*_partial` progress.jsonl
    /// variant) or no-enrichment kind.
    pub enrichment: Option<EnrichmentPayload>,
    /// How this event was produced — lets downstream distinguish a
    /// confident paired event from a lossy fallback.
    pub outcome: MergeOutcome,
}

/// Classifies how an [`EnrichedEvent`] was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Base + enrichment paired within the grace window — lossless.
    Paired,
    /// Base fired, enrichment never arrived within grace — lossy.
    BaseLossy,
    /// Enrichment fired with no matching base — fully lossless (P1 was
    /// always authoritative); emitted immediately.
    EnrichmentOnly,
    /// A kind with `EnrichmentSource::None` — emitted immediately from
    /// base, no grace window.
    BaseOnly,
    /// A `recent_base` slot was evicted under sustained overflow before
    /// it could pair or age out. Surfaced for observability.
    BufferOverflow,
}

/// A parked base awaiting its grace window. Stored in `recent_base`.
#[derive(Debug, Clone)]
struct ParkedBase {
    base: BaseEvent,
    /// Generation flips when this slot is consumed by a pairing
    /// enrichment; the deferred lossy-emit task checks it before firing
    /// so a late pair does not double-emit as lossy.
    consumed: bool,
}

/// A parked enrichment awaiting a late base. Stored in
/// `pending_enrichment` after being emitted as `EnrichmentOnly`, so a
/// subsequently-arriving base of the same `(kind, sequence_id)` can
/// self-suppress (spec §algorithm "base self-suppresses").
#[derive(Debug, Clone)]
struct ParkedEnrichment {
    enrich: EnrichmentEvent,
}

/// Dual ring buffers for one session. Bounded at [`BUFFER_CAPACITY`].
#[derive(Debug, Default)]
struct SessionBuffers {
    recent_base: VecDeque<ParkedBase>,
    pending_enrichment: VecDeque<ParkedEnrichment>,
}

/// Shared inner state, behind one async [`Mutex`]. The deferred
/// grace-expiry tasks each grab the lock briefly to check the consumed
/// flag, so the lock must be `tokio`'s (held across `.await` points).
struct Inner {
    grace: Duration,
    sessions: HashMap<MuxSessionId, SessionBuffers>,
    out: mpsc::UnboundedSender<EnrichedEvent>,
}

/// The priority-with-grace-window merger.
///
/// Driven in production by [`crate::typed_event_tap::TypedEventTap`]
/// (V0.8 Slices 1 + 2 — see the module doc). Both the `BaseOnly` path and
/// the grace-window pairing path (`Paired` / `BaseLossy`) are live.
///
/// Construct with [`EventMerger::new`], which hands back the output
/// [`mpsc::UnboundedReceiver`]. **All** emissions — immediate pairs,
/// deferred lossy fallbacks, enrichment-only, and overflow — flow
/// through that single channel. `process_*` therefore return `()`:
///
/// > Design note (spec API lists `process_base -> Option<EnrichedEvent>`):
/// > a `Base(lossy=true)` emission happens *after* the grace window, on
/// > a different task than the `process_base` call that parked it, so it
/// > cannot ride the call's return value. We unify on the channel — one
/// > output path, trivially testable, no two-channel skew.
#[derive(Clone)]
pub struct EventMerger {
    inner: Arc<Mutex<Inner>>,
}

impl EventMerger {
    /// Build a merger + its output receiver. `grace` is the per-base
    /// wait window (use [`DEFAULT_GRACE`] in production).
    pub fn new(grace: Duration) -> (Self, mpsc::UnboundedReceiver<EnrichedEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Inner {
            grace,
            sessions: HashMap::new(),
            out: tx,
        };
        (
            Self {
                inner: Arc::new(Mutex::new(inner)),
            },
            rx,
        )
    }

    /// Ingest a lossy P2/P3 base detection.
    ///
    /// - kind with no enrichment source → emit `BaseOnly` immediately.
    /// - enrichment already pending for this `(kind, sequence_id)` →
    ///   pair now, emit `Paired`, consume the enrichment.
    /// - else park the base and spawn a grace-expiry task that emits
    ///   `BaseLossy` if no enrichment arrives in time.
    pub async fn process_base(&self, base: BaseEvent) {
        let grace = {
            let mut guard = self.inner.lock().await;
            let grace = guard.grace;

            // Kinds with no P1 source emit immediately.
            if enrichment_source(base.kind, base.vendor) == EnrichmentSource::None {
                let ev = EnrichedEvent {
                    session_id: base.session_id.clone(),
                    kind: base.kind,
                    timestamp: base.timestamp,
                    sequence_id: base.sequence_id,
                    base: Some(base.payload.clone()),
                    enrichment: None,
                    outcome: MergeOutcome::BaseOnly,
                };
                let _ = guard.out.send(ev);
                return;
            }

            // Clone the sender up front so we can emit while holding a
            // mutable borrow of `buffers` (overflow eviction path).
            let out = guard.out.clone();
            let buffers = guard.sessions.entry(base.session_id.clone()).or_default();

            // Did an enrichment already arrive (EnrichmentOnly path)?
            // Pair now and self-suppress the would-be lossy fallback. The
            // predicate matches on `(kind, identity, sequence_id)` —
            // identity is the Slice-4 cohort key so parallel tool calls
            // of different tools never cross-pair across the
            // pending_enrichment / recent_base ring buffers (defence in
            // depth; `SeqState` already partitions by identity at the
            // mint layer in `typed_event_tap.rs`).
            if let Some(pos) = buffers.pending_enrichment.iter().position(|p| {
                p.enrich.kind == base.kind
                    && p.enrich.identity == base.identity
                    && p.enrich.sequence_id == base.sequence_id
            }) {
                let parked = buffers
                    .pending_enrichment
                    .remove(pos)
                    .expect("pos in range");
                // The enrichment was already emitted as EnrichmentOnly;
                // the base self-suppresses (no second event). Spec
                // §algorithm: "base self-suppresses if its sequence_id
                // matches a recently-consumed enrichment".
                let _ = parked; // intentionally dropped — no emit.
                return;
            }

            // Park the base for the grace window.
            if buffers.recent_base.len() >= BUFFER_CAPACITY {
                // Overflow: evict oldest, surface for observability.
                if let Some(evicted) = buffers.recent_base.pop_front() {
                    let ev = EnrichedEvent {
                        session_id: evicted.base.session_id.clone(),
                        kind: evicted.base.kind,
                        timestamp: evicted.base.timestamp,
                        sequence_id: evicted.base.sequence_id,
                        base: Some(evicted.base.payload.clone()),
                        enrichment: None,
                        outcome: MergeOutcome::BufferOverflow,
                    };
                    let _ = out.send(ev);
                }
            }
            buffers.recent_base.push_back(ParkedBase {
                base: base.clone(),
                consumed: false,
            });

            grace
        };

        // Deferred grace-expiry: sleep, then emit BaseLossy unless the
        // slot was consumed by a pairing enrichment meanwhile. Honors
        // the mock clock under `tokio::time::pause()`. Matches on
        // `(kind, identity, sequence_id)` per the Slice-4 cohort key.
        let inner = self.inner.clone();
        let key = base.session_id.clone();
        let kind = base.kind;
        let seq = base.sequence_id;
        let identity = base.identity.clone();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let mut guard = inner.lock().await;
            let send = if let Some(buffers) = guard.sessions.get_mut(&key) {
                if let Some(pos) = buffers.recent_base.iter().position(|p| {
                    p.base.kind == kind
                        && p.base.identity == identity
                        && p.base.sequence_id == seq
                        && !p.consumed
                }) {
                    let parked = buffers.recent_base.remove(pos).expect("pos in range");
                    Some(EnrichedEvent {
                        session_id: parked.base.session_id.clone(),
                        kind: parked.base.kind,
                        timestamp: parked.base.timestamp,
                        sequence_id: parked.base.sequence_id,
                        base: Some(parked.base.payload.clone()),
                        enrichment: None,
                        outcome: MergeOutcome::BaseLossy,
                    })
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(ev) = send {
                let _ = guard.out.send(ev);
            }
        });
    }

    /// Ingest a lossless P1 enrichment.
    ///
    /// - matching parked base for this `(kind, identity, sequence_id)` →
    ///   pair now, emit `Paired`, mark the base consumed so its grace
    ///   task self-suppresses.
    /// - else emit `EnrichmentOnly` immediately and park the enrichment
    ///   so a late base self-suppresses.
    pub async fn process_enrichment(&self, enrich: EnrichmentEvent) {
        let mut guard = self.inner.lock().await;
        let out = guard.out.clone();
        let buffers = guard.sessions.entry(enrich.session_id.clone()).or_default();

        if let Some(pos) = buffers.recent_base.iter().position(|p| {
            p.base.kind == enrich.kind
                && p.base.identity == enrich.identity
                && p.base.sequence_id == enrich.sequence_id
                && !p.consumed
        }) {
            // Pair: mark consumed (the grace task will skip it) and
            // remove the parked base now that it is resolved.
            let mut parked = buffers.recent_base.remove(pos).expect("pos in range");
            parked.consumed = true;
            let ev = EnrichedEvent {
                session_id: enrich.session_id.clone(),
                kind: enrich.kind,
                timestamp: enrich.timestamp,
                sequence_id: enrich.sequence_id,
                base: Some(parked.base.payload.clone()),
                enrichment: Some(enrich.payload.clone()),
                outcome: MergeOutcome::Paired,
            };
            let _ = out.send(ev);
            return;
        }

        // No base yet — emit EnrichmentOnly immediately (P1 is
        // authoritative), and park so a late base self-suppresses.
        let ev = EnrichedEvent {
            session_id: enrich.session_id.clone(),
            kind: enrich.kind,
            timestamp: enrich.timestamp,
            sequence_id: enrich.sequence_id,
            base: None,
            enrichment: Some(enrich.payload.clone()),
            outcome: MergeOutcome::EnrichmentOnly,
        };
        let _ = out.send(ev);

        if buffers.pending_enrichment.len() >= BUFFER_CAPACITY {
            buffers.pending_enrichment.pop_front();
        }
        buffers
            .pending_enrichment
            .push_back(ParkedEnrichment { enrich });
    }

    /// Drain every still-parked base as a `BaseLossy` fallback. Call on
    /// shutdown so no occurrence is silently lost. Returns the flushed
    /// events (also sent through the output channel for symmetry).
    pub async fn flush_pending(&self) -> Vec<EnrichedEvent> {
        let mut guard = self.inner.lock().await;
        let mut out = Vec::new();
        for buffers in guard.sessions.values_mut() {
            while let Some(parked) = buffers.recent_base.pop_front() {
                if parked.consumed {
                    continue;
                }
                out.push(EnrichedEvent {
                    session_id: parked.base.session_id.clone(),
                    kind: parked.base.kind,
                    timestamp: parked.base.timestamp,
                    sequence_id: parked.base.sequence_id,
                    base: Some(parked.base.payload.clone()),
                    enrichment: None,
                    outcome: MergeOutcome::BaseLossy,
                });
            }
            buffers.pending_enrichment.clear();
        }
        for ev in &out {
            let _ = guard.out.send(ev.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_table_covers_all_base_kinds_for_both_vendors() {
        use EventKind::*;
        // 8 enriched kinds must have a P1 source for both vendors; the 4
        // process/surface kinds map to None. (Slice 4 removed `TurnStarted`
        // — it shared its source with `UserPromptSubmitted`.)
        let enriched = [
            ToolCallStarted,
            ToolCallCompleted,
            UserPromptSubmitted,
            AssistantMessageComplete,
            TurnDone,
            PlanPending,
            SessionReset,
            CompactDone,
        ];
        for k in enriched {
            assert_ne!(
                enrichment_source(k, Vendor::Claude),
                EnrichmentSource::None,
                "{k:?} Claude must have a P1 source"
            );
            assert_ne!(
                enrichment_source(k, Vendor::Codex),
                EnrichmentSource::None,
                "{k:?} Codex must have a P1 source"
            );
        }
        for k in [RateLimitHit, ContextOverflow, Idle, ProcessExited] {
            assert_eq!(enrichment_source(k, Vendor::Claude), EnrichmentSource::None);
            assert_eq!(enrichment_source(k, Vendor::Codex), EnrichmentSource::None);
        }
    }

    #[tokio::test]
    async fn no_enrichment_kind_emits_immediately() {
        let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
        m.process_base(BaseEvent {
            session_id: MuxSessionId::new("s"),
            kind: EventKind::RateLimitHit,
            vendor: Vendor::Claude,
            sequence_id: 0,
            timestamp: SystemTime::now(),
            identity: None,
            payload: BasePayload {
                captured: "rate limit".into(),
            },
        })
        .await;
        let ev = rx.try_recv().expect("immediate emit");
        assert_eq!(ev.outcome, MergeOutcome::BaseOnly);
    }
}
