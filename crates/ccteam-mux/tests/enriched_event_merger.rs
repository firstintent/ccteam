//! W4 EnrichedEvent merger acceptance tests (W3b deliverable).
//!
//! Per `docs/versions/v0-8-rmux/w4-enriched-event-merger.md` §"W4
//! acceptance":
//! - 100 paired (base + enrichment) → all 100 pair correctly
//! - 100 base-only (enrichment never arrives) → all emit BaseLossy
//!   after GRACE_MS
//! - 100 enrichment-only → all emit EnrichmentOnly immediately
//! - rapid-fire across 10 sessions → zero crosstalk
//!
//! All timing is driven by `tokio::time` with `start_paused = true`
//! (the `test-util` dev-feature), so GRACE_MS is exercised
//! deterministically with no real sleeps. Under a paused clock, Tokio
//! auto-advances time to the next timer when all tasks are otherwise
//! idle, so awaiting the output channel transparently fires the
//! deferred grace-expiry tasks.

use std::time::{Duration, SystemTime};

use ccteam_mux::enriched_event::{
    BaseEvent, BasePayload, EnrichmentEvent, EnrichmentPayload, EventKind, EventMerger,
    MergeOutcome, Vendor, DEFAULT_GRACE,
};
use ccteam_mux::MuxSessionId;

fn base(session: &str, seq: u64) -> BaseEvent {
    BaseEvent {
        session_id: MuxSessionId::new(session),
        kind: EventKind::ToolCallStarted,
        vendor: Vendor::Claude,
        sequence_id: seq,
        timestamp: SystemTime::UNIX_EPOCH,
        payload: BasePayload {
            captured: format!("Read#{seq}"),
        },
        // Slice 4: legacy merger acceptance tests stay in the `None`
        // identity cohort (preserves pre-Slice-4 behaviour byte-identical).
        // Slice-4 cohort partitioning is exercised by the SeqState unit
        // tests + the chat integration suite.
        identity: None,
    }
}

fn enrichment(session: &str, seq: u64) -> EnrichmentEvent {
    EnrichmentEvent {
        session_id: MuxSessionId::new(session),
        kind: EventKind::ToolCallStarted,
        vendor: Vendor::Claude,
        sequence_id: seq,
        timestamp: SystemTime::UNIX_EPOCH,
        payload: EnrichmentPayload {
            data: format!("{{\"tool\":\"Read\",\"seq\":{seq}}}"),
        },
        identity: None,
    }
}

/// Collect exactly `n` events off the receiver, auto-advancing the
/// paused clock as needed. Fails if the stream closes early.
async fn collect_n(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<ccteam_mux::EnrichedEvent>,
    n: usize,
) -> Vec<ccteam_mux::EnrichedEvent> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        match rx.recv().await {
            Some(ev) => out.push(ev),
            None => panic!("channel closed after {} of {n} events", out.len()),
        }
    }
    out
}

/// 100 base events, each immediately followed by its enrichment, all
/// pair correctly within the grace window.
#[tokio::test(start_paused = true)]
async fn paired_100_all_pair() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    for i in 0..100 {
        m.process_base(base("s", i)).await;
        m.process_enrichment(enrichment("s", i)).await;
    }
    let events = collect_n(&mut rx, 100).await;
    assert_eq!(events.len(), 100);
    for ev in &events {
        assert_eq!(ev.outcome, MergeOutcome::Paired, "every occurrence pairs");
        assert!(ev.base.is_some());
        assert!(ev.enrichment.is_some());
        assert_eq!(ev.session_id, MuxSessionId::new("s"));
    }
    // No deferred lossy fallback should fire: advance well past grace
    // and confirm the channel has nothing more.
    tokio::time::advance(DEFAULT_GRACE * 4).await;
    assert!(rx.try_recv().is_err(), "no extra lossy emissions");
    // Each sequence id appears exactly once.
    let mut seqs: Vec<u64> = events.iter().map(|e| e.sequence_id).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), 100, "no duplicate or dropped sequences");
}

/// 100 base-only events (no enrichment ever arrives) all emit BaseLossy
/// after the grace window elapses — and none before.
#[tokio::test(start_paused = true)]
async fn base_only_100_emit_lossy_after_grace() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    // NB: 100 *simultaneous* un-resolved bases would exceed the cap-64
    // ring buffer and trip the spec's BufferOverflow path ("sustained
    // rate > 128 events/sec"). The acceptance scenario models steady-
    // state arrival: we park in batches of 25 (well under the cap), let
    // the grace window elapse, and drain those slots as BaseLossy before
    // the next batch parks.
    let mut events = Vec::with_capacity(100);
    for batch in 0..4u64 {
        for j in 0..25u64 {
            m.process_base(base("s", batch * 25 + j)).await;
        }
        // Elapse the grace window; collect this batch's lossy fallbacks
        // (draining frees the buffer slots before the next batch parks).
        let mut drained = collect_n(&mut rx, 25).await;
        events.append(&mut drained);
    }
    assert_eq!(events.len(), 100);
    for ev in &events {
        assert_eq!(ev.outcome, MergeOutcome::BaseLossy);
        assert!(ev.base.is_some());
        assert!(ev.enrichment.is_none());
    }
    // Every sequence id 0..100 appears exactly once — none dropped.
    let mut seqs: Vec<u64> = events.iter().map(|e| e.sequence_id).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (0..100).collect::<Vec<_>>());
    tokio::time::advance(DEFAULT_GRACE * 4).await;
    assert!(rx.try_recv().is_err(), "exactly 100 lossy events, no more");
}

/// Grace timing is precise: a single parked base does NOT emit before
/// GRACE_MS elapses, and DOES emit at the grace boundary. Uses manual
/// `advance` (not auto-advance) so the boundary is exercised exactly.
#[tokio::test(start_paused = true)]
async fn base_lossy_waits_exactly_grace_ms() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    m.process_base(base("s", 0)).await;

    // One tick short of grace — the deferred task's sleep has not
    // completed, so nothing is emitted yet.
    tokio::time::advance(DEFAULT_GRACE - Duration::from_millis(1)).await;
    assert!(rx.try_recv().is_err(), "no lossy emit before grace elapses");

    // Cross the boundary — the lossy fallback fires. `recv().await`
    // parks the receiver so the runtime runs the woken grace task to
    // completion (it sends) before the receiver wakes with the message.
    tokio::time::advance(Duration::from_millis(2)).await;
    let ev = rx.recv().await.expect("lossy emit at grace boundary");
    assert_eq!(ev.outcome, MergeOutcome::BaseLossy);
}

/// 100 enrichment-only events (no base ever arrives) all emit
/// EnrichmentOnly immediately (P1 is authoritative; no grace wait).
#[tokio::test(start_paused = true)]
async fn enrichment_only_100_emit_immediately() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    for i in 0..100 {
        m.process_enrichment(enrichment("s", i)).await;
        // Immediately available — no clock advance needed.
        let ev = rx.try_recv().expect("enrichment-only emits immediately");
        assert_eq!(ev.outcome, MergeOutcome::EnrichmentOnly);
        assert!(ev.base.is_none());
        assert!(ev.enrichment.is_some());
        assert_eq!(ev.sequence_id, i);
    }
    tokio::time::advance(DEFAULT_GRACE * 4).await;
    assert!(rx.try_recv().is_err(), "no lossy fallback for enrichments");
}

/// A late base whose enrichment already emitted EnrichmentOnly
/// self-suppresses (no duplicate) — spec §algorithm.
#[tokio::test(start_paused = true)]
async fn late_base_self_suppresses_after_enrichment_only() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    m.process_enrichment(enrichment("s", 7)).await;
    let ev = rx.try_recv().expect("enrichment-only");
    assert_eq!(ev.outcome, MergeOutcome::EnrichmentOnly);

    // Base arrives late — it must NOT produce a second event.
    m.process_base(base("s", 7)).await;
    tokio::time::advance(DEFAULT_GRACE * 4).await;
    assert!(
        rx.try_recv().is_err(),
        "late base self-suppresses, no duplicate"
    );
}

/// An enrichment arriving within grace pairs with its parked base and
/// the deferred lossy task self-suppresses (no double emit).
#[tokio::test(start_paused = true)]
async fn enrichment_within_grace_pairs_and_suppresses_lossy() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    m.process_base(base("s", 1)).await;
    // Half a grace later, the enrichment arrives.
    tokio::time::advance(DEFAULT_GRACE / 2).await;
    m.process_enrichment(enrichment("s", 1)).await;

    let ev = rx.try_recv().expect("pairs on enrichment arrival");
    assert_eq!(ev.outcome, MergeOutcome::Paired);

    // Let the would-be grace task fire — it must find the slot consumed.
    tokio::time::advance(DEFAULT_GRACE * 2).await;
    assert!(rx.try_recv().is_err(), "no lossy double-emit after pairing");
}

/// Rapid-fire 1000 pairs (100 each) across 10 sessions — verify zero
/// crosstalk: each session's events carry only its own session_id and
/// sequence space, every pair resolved as Paired.
#[tokio::test(start_paused = true)]
async fn rapid_fire_10_sessions_zero_crosstalk() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    let sessions: Vec<String> = (0..10).map(|s| format!("sess-{s}")).collect();

    // Interleave: for each of 100 rounds, fire a base+enrichment for
    // every session, 50ms apart (well within grace), to stress the
    // per-session buffer isolation.
    for round in 0..100u64 {
        for s in &sessions {
            m.process_base(base(s, round)).await;
            m.process_enrichment(enrichment(s, round)).await;
        }
        tokio::time::advance(Duration::from_millis(50)).await;
    }

    let events = collect_n(&mut rx, 1000).await;
    assert_eq!(events.len(), 1000);

    // Bucket by session; each must have exactly 100, all Paired, with a
    // complete 0..100 sequence space and no foreign session id.
    use std::collections::HashMap;
    let mut by_session: HashMap<String, Vec<u64>> = HashMap::new();
    for ev in &events {
        assert_eq!(
            ev.outcome,
            MergeOutcome::Paired,
            "every cross-session occurrence pairs"
        );
        by_session
            .entry(ev.session_id.as_str().to_string())
            .or_default()
            .push(ev.sequence_id);
    }
    assert_eq!(by_session.len(), 10, "exactly 10 distinct sessions");
    for s in &sessions {
        let mut seqs = by_session
            .remove(s)
            .unwrap_or_else(|| panic!("session {s} produced no events"));
        assert_eq!(seqs.len(), 100, "session {s} got exactly its 100 events");
        seqs.sort_unstable();
        let expected: Vec<u64> = (0..100).collect();
        assert_eq!(
            seqs, expected,
            "session {s} sequence space intact, no crosstalk"
        );
    }

    tokio::time::advance(DEFAULT_GRACE * 4).await;
    assert!(rx.try_recv().is_err(), "no stray lossy emissions");
}

/// `flush_pending` drains still-parked bases as BaseLossy on shutdown.
#[tokio::test(start_paused = true)]
async fn flush_pending_drains_parked_bases() {
    let (m, mut rx) = EventMerger::new(DEFAULT_GRACE);
    // Park 5 bases without advancing past grace.
    for i in 0..5 {
        m.process_base(base("s", i)).await;
    }
    assert!(
        rx.try_recv().is_err(),
        "still within grace, none emitted yet"
    );

    let flushed = m.flush_pending().await;
    assert_eq!(flushed.len(), 5);
    for ev in &flushed {
        assert_eq!(ev.outcome, MergeOutcome::BaseLossy);
    }
    // The flushed events were also pushed through the channel.
    let via_channel = collect_n(&mut rx, 5).await;
    assert_eq!(via_channel.len(), 5);

    // The original grace tasks, when they fire, find empty buffers — no
    // duplicate emissions.
    tokio::time::advance(DEFAULT_GRACE * 4).await;
    assert!(
        rx.try_recv().is_err(),
        "flush + grace task do not double-emit"
    );
}
