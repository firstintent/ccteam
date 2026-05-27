//! Daemon-side **typed-event tap** — the production producer that drives
//! the [`crate::enriched_event::EventMerger`] from a live session's
//! [`MuxBackend::subscribe`] stream.
//!
//! This is the wiring the merger module's `TODO(V0.9-typed-event-consumer)`
//! anticipates: it registers the vendor's base patterns
//! ([`crate::patterns::base_patterns`]) against a session, subscribes to
//! its typed [`MuxEvent`] stream, and lifts each
//! [`MuxEvent::PatternMatched`] into a [`BaseEvent`] (with a minted
//! `sequence_id`) that it feeds to a per-session [`EventMerger`]. The
//! integrator separately feeds the lossless P1 side (Claude Code hook
//! subprocess / Codex JSON-RPC) as [`RawEnrichment`]s via the returned
//! [`TapHandle`]; the tap mints the matching `sequence_id` so the merger
//! pairs them, and forwards every merged [`EnrichedEvent`] to the output
//! receiver.
//!
//! ## What this module does NOT do
//!
//! It performs no orchestration policy (no rate-limit auto-resume, no
//! progress.jsonl writes) — it is purely the base/enrichment producer +
//! seq-minting layer that sits between `subscribe()` and the merger. The
//! consumer that reads the merged stream and acts on it is a separate
//! integration point.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;

use crate::enriched_event::{
    BaseEvent, BasePayload, EnrichedEvent, EnrichmentEvent, EnrichmentPayload, EventKind,
    EventMerger, Vendor,
};
use crate::patterns::{self, PatternVendor};
use crate::{MuxBackend, MuxEvent, MuxSessionId};

/// FIXED mapping from a base-pattern `regex_id` (see
/// [`crate::patterns`]) to the merger [`EventKind`] it produces.
///
/// Only the regex_ids that correspond to a logical merge-able / process
/// kind map to `Some`; surface-only hints (`permission_prompt`,
/// `token_usage`, `thinking`, `approval_prompt`) and any unknown id map
/// to `None` and are ignored by the tap (they do not feed the merger).
pub fn event_kind_for_regex_id(regex_id: &str) -> Option<EventKind> {
    match regex_id {
        "rate_limit" => Some(EventKind::RateLimitHit),
        "context_overflow" => Some(EventKind::ContextOverflow),
        "turn_done" => Some(EventKind::TurnDone),
        "tool_call_started" => Some(EventKind::ToolCallStarted),
        "tool_call_completed" => Some(EventKind::ToolCallCompleted),
        "user_prompt_submit" => Some(EventKind::UserPromptSubmitted),
        "session_reset" => Some(EventKind::SessionReset),
        // permission_prompt / token_usage / thinking / approval_prompt /
        // anything else → not fed to the merger.
        _ => None,
    }
}

/// A lossless P1 enrichment handed to the tap by the integrator
/// (daemon hook sink / Codex UDS bridge) **without** a `sequence_id`.
///
/// The tap mints the matching seq (so it pairs with the corresponding
/// base) and lifts this into an [`EnrichmentEvent`]. `payload` is the
/// opaque serialized P1 detail the merger forwards verbatim.
#[derive(Debug, Clone)]
pub struct RawEnrichment {
    pub kind: EventKind,
    pub payload: String,
}

/// Handle the integrator uses to push P1 enrichments into a running tap.
///
/// Clonable; dropping all clones lets the tap task wind down its
/// enrichment branch. [`Self::enrich`] is fire-and-forget — a send after
/// the task has stopped is silently ignored.
#[derive(Clone)]
pub struct TapHandle {
    tx: mpsc::UnboundedSender<RawEnrichment>,
}

impl TapHandle {
    /// Feed a lossless P1 enrichment. Ignores the error if the tap task
    /// has already stopped (the receiver was dropped).
    pub fn enrich(&self, e: RawEnrichment) {
        let _ = self.tx.send(e);
    }
}

/// The pending-partner seq-minting state, factored out so it is unit-
/// testable without the async tap / merger machinery.
///
/// A base and its enrichment for the same occurrence must carry the
/// **same** `sequence_id` so the merger pairs them. Rather than two
/// independent per-side counters (which cascade-mispair if one side ever
/// drops an occurrence), we use a single monotonic counter plus two
/// per-`EventKind` FIFO queues of "minted but not yet matched by the
/// other side" tokens.
#[derive(Debug, Default)]
struct SeqState {
    next_seq: u64,
    /// seqs minted by a base, awaiting their enrichment partner.
    pending_base: HashMap<EventKind, VecDeque<u64>>,
    /// seqs minted by an enrichment, awaiting their base partner.
    pending_enrich: HashMap<EventKind, VecDeque<u64>>,
}

impl SeqState {
    fn fresh(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Mint the `sequence_id` for an arriving base of `kind`.
    ///
    /// If an enrichment for this kind already minted a seq (it arrived
    /// first), reuse it so the two pair. Otherwise mint a fresh seq and
    /// record it as awaiting the enrichment side.
    ///
    /// Caveat — if a prior occurrence's enrichment arrives very late
    /// (after the next base was minted), it has no pending_base and mints
    /// a fresh seq → the merger emits it as EnrichmentOnly and the next
    /// base may age out as BaseLossy. For single-in-flight kinds
    /// (TurnDone, Idle) this cannot happen; for multi-in-flight kinds the
    /// consumer must treat a lossy partial as "a missed pairing occurred
    /// in this kind's recent history", not "the most recent occurrence
    /// was lossy".
    fn mint_base(&mut self, kind: EventKind) -> u64 {
        if let Some(q) = self.pending_enrich.get_mut(&kind) {
            if let Some(s) = q.pop_front() {
                return s;
            }
        }
        let s = self.fresh();
        self.pending_base.entry(kind).or_default().push_back(s);
        s
    }

    /// Mint the `sequence_id` for an arriving enrichment of `kind`.
    ///
    /// Symmetric to [`Self::mint_base`]: reuse a base's pending seq if one
    /// is waiting, else mint a fresh seq and record it as awaiting the
    /// base side. See the late-arrival caveat on [`Self::mint_base`].
    fn mint_enrich(&mut self, kind: EventKind) -> u64 {
        if let Some(q) = self.pending_base.get_mut(&kind) {
            if let Some(s) = q.pop_front() {
                return s;
            }
        }
        let s = self.fresh();
        self.pending_enrich.entry(kind).or_default().push_back(s);
        s
    }
}

/// Map the merger [`Vendor`] onto the pattern-table [`PatternVendor`].
fn pattern_vendor(vendor: Vendor) -> PatternVendor {
    match vendor {
        Vendor::Claude => PatternVendor::Claude,
        Vendor::Codex => PatternVendor::Codex,
    }
}

/// Return type of [`TypedEventTap::spawn`]: the integrator's enrichment
/// handle + the merged-event output receiver.
type SpawnResult = (TapHandle, mpsc::UnboundedReceiver<EnrichedEvent>);

/// Daemon-side typed-event tap. A unit struct namespacing [`Self::spawn`].
pub struct TypedEventTap;

impl TypedEventTap {
    /// Register the vendor's base patterns on `session_id`, subscribe to
    /// its typed event stream, and spawn the tap task that drives a
    /// per-session [`EventMerger`].
    ///
    /// Returns a [`TapHandle`] (for feeding P1 enrichments) and the
    /// receiver of merged [`EnrichedEvent`]s. The spawned task runs until
    /// the output receiver is dropped (or all upstream sources end).
    pub async fn spawn(
        session_id: MuxSessionId,
        vendor: Vendor,
        backend: Arc<dyn MuxBackend>,
        grace: Duration,
    ) -> anyhow::Result<SpawnResult> {
        // Register every base pattern for this vendor so the backend will
        // emit PatternMatched events on the subscribe stream.
        let pv = pattern_vendor(vendor);
        for entry in patterns::base_patterns(pv) {
            backend
                .register_pattern(&session_id, entry.id.to_string(), entry.regex.to_string())
                .await?;
        }

        let mut stream = backend.subscribe(&session_id).await?;
        let (merger, mut merger_rx) = EventMerger::new(grace);

        let (enrich_tx, mut enrich_rx) = mpsc::unbounded_channel::<RawEnrichment>();
        let handle = TapHandle { tx: enrich_tx };

        let (out_tx, out_rx) = mpsc::unbounded_channel::<EnrichedEvent>();

        tokio::spawn(async move {
            use futures::StreamExt;

            let mut seq = SeqState::default();
            // Once the subscribe stream ends we stop polling it; likewise
            // once every TapHandle is dropped the enrichment channel is
            // closed and we stop polling it. The task exits cleanly when
            // BOTH inputs are exhausted (or the consumer drops out_rx).
            // Guarding each branch on its `*_done` flag is what prevents a
            // closed channel's immediate `recv()->None` from busy-spinning
            // the select loop.
            let mut stream_done = false;
            let mut enrich_done = false;

            loop {
                tokio::select! {
                    // (a) The session's typed MuxEvent stream.
                    maybe_ev = stream.next(), if !stream_done => {
                        match maybe_ev {
                            Some(MuxEvent::PatternMatched { regex_id, captured }) => {
                                if let Some(kind) = event_kind_for_regex_id(&regex_id) {
                                    let sequence_id = seq.mint_base(kind);
                                    merger
                                        .process_base(BaseEvent {
                                            session_id: session_id.clone(),
                                            kind,
                                            vendor,
                                            sequence_id,
                                            timestamp: SystemTime::now(),
                                            payload: BasePayload { captured },
                                        })
                                        .await;
                                }
                                // Unmapped regex_id → ignored.
                            }
                            Some(MuxEvent::OutputIdle { .. }) => {
                                // None-kind → BaseOnly immediately; seq irrelevant.
                                let sequence_id = seq.mint_base(EventKind::Idle);
                                merger
                                    .process_base(BaseEvent {
                                        session_id: session_id.clone(),
                                        kind: EventKind::Idle,
                                        vendor,
                                        sequence_id,
                                        timestamp: SystemTime::now(),
                                        payload: BasePayload::default(),
                                    })
                                    .await;
                            }
                            Some(MuxEvent::ProcessExited { .. }) => {
                                let sequence_id = seq.mint_base(EventKind::ProcessExited);
                                merger
                                    .process_base(BaseEvent {
                                        session_id: session_id.clone(),
                                        kind: EventKind::ProcessExited,
                                        vendor,
                                        sequence_id,
                                        timestamp: SystemTime::now(),
                                        payload: BasePayload::default(),
                                    })
                                    .await;
                            }
                            // Other MuxEvent variants are not lifted into
                            // the merger (raw chunks / lag / resize /
                            // started / reconnect).
                            Some(_) => {}
                            None => {
                                stream_done = true;
                                if enrich_done {
                                    break;
                                }
                            }
                        }
                    }

                    // (b) The integrator's P1 enrichment channel.
                    maybe_enrich = enrich_rx.recv(), if !enrich_done => {
                        match maybe_enrich {
                            Some(RawEnrichment { kind, payload }) => {
                                let sequence_id = seq.mint_enrich(kind);
                                merger
                                    .process_enrichment(EnrichmentEvent {
                                        session_id: session_id.clone(),
                                        kind,
                                        vendor,
                                        sequence_id,
                                        timestamp: SystemTime::now(),
                                        payload: EnrichmentPayload { data: payload },
                                    })
                                    .await;
                            }
                            None => {
                                // All TapHandles dropped. Stop polling this
                                // branch (the `if !enrich_done` guard above)
                                // so the closed channel can't busy-spin the
                                // select loop; exit once the stream is also
                                // done.
                                enrich_done = true;
                                if stream_done {
                                    break;
                                }
                            }
                        }
                    }

                    // (c) Merged events out of the merger.
                    maybe_merged = merger_rx.recv() => {
                        match maybe_merged {
                            Some(ev) => {
                                if out_tx.send(ev).is_err() {
                                    // Consumer gone — tear down the tap.
                                    break;
                                }
                            }
                            None => {
                                // Merger dropped (cannot happen while we
                                // hold `merger`), but be defensive.
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok((handle, out_rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enriched_event::MergeOutcome;
    use crate::{BackendKind, MuxEventStream, MuxSessionSpec};
    use anyhow::Result;

    #[test]
    fn regex_id_mapping_covers_every_mapped_id() {
        assert_eq!(
            event_kind_for_regex_id("rate_limit"),
            Some(EventKind::RateLimitHit)
        );
        assert_eq!(
            event_kind_for_regex_id("context_overflow"),
            Some(EventKind::ContextOverflow)
        );
        assert_eq!(
            event_kind_for_regex_id("turn_done"),
            Some(EventKind::TurnDone)
        );
        assert_eq!(
            event_kind_for_regex_id("tool_call_started"),
            Some(EventKind::ToolCallStarted)
        );
        assert_eq!(
            event_kind_for_regex_id("tool_call_completed"),
            Some(EventKind::ToolCallCompleted)
        );
        assert_eq!(
            event_kind_for_regex_id("user_prompt_submit"),
            Some(EventKind::UserPromptSubmitted)
        );
        assert_eq!(
            event_kind_for_regex_id("session_reset"),
            Some(EventKind::SessionReset)
        );
    }

    #[test]
    fn regex_id_mapping_returns_none_for_unmapped_ids() {
        assert_eq!(event_kind_for_regex_id("permission_prompt"), None);
        assert_eq!(event_kind_for_regex_id("token_usage"), None);
        assert_eq!(event_kind_for_regex_id("thinking"), None);
        assert_eq!(event_kind_for_regex_id("approval_prompt"), None);
        assert_eq!(event_kind_for_regex_id("not_a_real_id"), None);
        assert_eq!(event_kind_for_regex_id(""), None);
    }

    #[test]
    fn seq_base_first_then_enrich_reuse_same_seq() {
        let mut s = SeqState::default();
        let b = s.mint_base(EventKind::TurnDone);
        let e = s.mint_enrich(EventKind::TurnDone);
        assert_eq!(b, e, "enrichment must reuse the base's pending seq");
    }

    #[test]
    fn seq_enrich_first_then_base_reuse_same_seq() {
        let mut s = SeqState::default();
        let e = s.mint_enrich(EventKind::ToolCallStarted);
        let b = s.mint_base(EventKind::ToolCallStarted);
        assert_eq!(e, b, "base must reuse the enrichment's pending seq");
    }

    #[test]
    fn seq_two_bases_before_any_enrich_get_distinct_seqs() {
        let mut s = SeqState::default();
        let b0 = s.mint_base(EventKind::ToolCallStarted);
        let b1 = s.mint_base(EventKind::ToolCallStarted);
        assert_ne!(b0, b1, "two unpaired bases must get distinct seqs");
        // And their enrichments pair FIFO: first enrich pairs b0, second b1.
        let e0 = s.mint_enrich(EventKind::ToolCallStarted);
        let e1 = s.mint_enrich(EventKind::ToolCallStarted);
        assert_eq!(e0, b0);
        assert_eq!(e1, b1);
    }

    #[test]
    fn seq_distinct_kinds_do_not_cross_pair() {
        let mut s = SeqState::default();
        let tool = s.mint_base(EventKind::ToolCallStarted);
        // An enrichment of a DIFFERENT kind must NOT reuse tool's seq.
        let turn = s.mint_enrich(EventKind::TurnDone);
        assert_ne!(tool, turn);
    }

    /// Minimal in-file mock backend: `subscribe` returns a stream we feed
    /// via a channel; `register_pattern` records nothing and returns Ok;
    /// every other trait method is `unimplemented!()` (the tap only calls
    /// these two).
    struct MockBackend {
        ev_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<MuxEvent>>>,
    }

    #[async_trait::async_trait]
    impl MuxBackend for MockBackend {
        async fn spawn(&self, _spec: MuxSessionSpec) -> Result<MuxSessionId> {
            unimplemented!()
        }
        async fn exists(&self, _id: &MuxSessionId) -> Result<bool> {
            unimplemented!()
        }
        async fn send_text(&self, _id: &MuxSessionId, _text: &str) -> Result<()> {
            unimplemented!()
        }
        async fn send_enter(&self, _id: &MuxSessionId) -> Result<()> {
            unimplemented!()
        }
        async fn capture(
            &self,
            _id: &MuxSessionId,
            _lines: usize,
            _with_ansi: bool,
        ) -> Result<Vec<u8>> {
            unimplemented!()
        }
        async fn pane_dims(&self, _id: &MuxSessionId) -> Result<Option<(u16, u16)>> {
            unimplemented!()
        }
        async fn pane_pid(&self, _id: &MuxSessionId) -> Result<Option<i32>> {
            unimplemented!()
        }
        async fn list_pane_pids(&self, _id: &MuxSessionId) -> Result<Vec<u32>> {
            unimplemented!()
        }
        async fn resize(&self, _id: &MuxSessionId, _cols: u16, _rows: u16) -> Result<()> {
            unimplemented!()
        }
        async fn subscribe(&self, _id: &MuxSessionId) -> Result<MuxEventStream> {
            let rx = self
                .ev_rx
                .lock()
                .await
                .take()
                .expect("subscribe called once");
            Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|ev| (ev, rx))
            })))
        }
        async fn register_pattern(
            &self,
            _id: &MuxSessionId,
            _regex_id: String,
            _regex: String,
        ) -> Result<()> {
            Ok(())
        }
        async fn kill(&self, _id: &MuxSessionId) -> Result<()> {
            unimplemented!()
        }
        async fn list_sessions(&self) -> Result<Vec<MuxSessionId>> {
            unimplemented!()
        }
        fn backend_kind(&self) -> BackendKind {
            BackendKind::InProc
        }
    }

    #[tokio::test]
    async fn pattern_matched_rate_limit_produces_base_only_enriched_event() {
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<MuxEvent>();
        let backend = Arc::new(MockBackend {
            ev_rx: tokio::sync::Mutex::new(Some(ev_rx)),
        });

        let (_handle, mut out_rx) = TypedEventTap::spawn(
            MuxSessionId::new("s"),
            Vendor::Claude,
            backend,
            Duration::from_millis(500),
        )
        .await
        .expect("spawn tap");

        ev_tx
            .send(MuxEvent::PatternMatched {
                regex_id: "rate_limit".to_string(),
                captured: "rate limit exceeded".to_string(),
            })
            .expect("send event");

        let ev = out_rx.recv().await.expect("merged event");
        assert_eq!(ev.kind, EventKind::RateLimitHit);
        assert_eq!(ev.outcome, MergeOutcome::BaseOnly);
        assert_eq!(
            ev.base.as_ref().map(|b| b.captured.as_str()),
            Some("rate limit exceeded")
        );
    }
}
