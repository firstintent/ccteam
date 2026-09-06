//! Per-session serial turn runner for ACP adapters.
//!
//! The gateway serializes nothing: like Claude/Codex it fires a fresh
//! `submit_turn` the instant a user steers mid-turn. But ACP `session/prompt`
//! is a single-turn request/response and the translate [`buffer`] holds exactly
//! one turn, so two overlapping prompts would clobber each other's chunks.
//!
//! Rather than overlap two `session/prompt` requests, routed submissions either
//! use a vendor-native interjection channel or append a distinct follow-up to
//! [`SessionTranslateState::pending`]. This runner drains that queue FIFO. It
//! also seals and drains native-interjection reservations before finalizing a
//! prompt, so an accepted message cannot cross into the next vendor turn.
//!
//! [`buffer`]: SessionTranslateState::buffer

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{broadcast, Notify};

use super::context_probe::AcpContextProbe;
use super::translate::{
    fail_turn, finalize_from_prompt_result, AcpInjectionReservation, SessionTranslateState,
};
use super::transport::{AcpTransport, AcpWriteBarrier};
use crate::execution::session_status::write_status_file;
use crate::{ContextSource, ThreadEvent, TurnRouting};

/// Monotonic suffix so ids stay unique even for turns steered in the same ms.
static ACP_TURN_SEQ: AtomicU64 = AtomicU64::new(0);

/// How long a context probe waits for its reply to reach the translate buffer
/// after the `session/prompt` response lands. The vendor answers a probe
/// locally with no model call, so this covers only the gap between the
/// response task and the notification dispatcher — not the vendor's work.
const PROBE_DISPATCH_GRACE: Duration = Duration::from_millis(200);

/// A unique per-process turn id (`t-<millis>-<seq>`). Used as the ACP adapter's
/// turn correlation id — must be unique so queued turns and the experience log
/// (`chat_turn_completed` join) never collide.
pub fn next_acp_turn_id() -> String {
    let seq = ACP_TURN_SEQ.fetch_add(1, Ordering::Relaxed);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("t-{millis}-{seq}")
}

/// Vendor-specific timing knobs for the shared turn runner.
#[derive(Clone, Copy)]
pub struct AcpTurnTuning {
    /// Max wait for the dispatcher to reach the turn boundary (all
    /// `agent_message_chunk` buffered) before finalizing anyway.
    pub finalize_barrier: Duration,
    /// Extra settle sleep after the barrier before finalize (OpenCode lets a
    /// racing `usage_update` land first). `None` skips it.
    pub post_finalize_sleep: Option<Duration>,
    /// Vendor label for lock-poison diagnostics.
    pub label: &'static str,
}

/// Per-session wiring shared across every turn the runner drives.
pub struct AcpTurnRunner {
    pub transport: Arc<AcpTransport>,
    pub state: Arc<StdMutex<SessionTranslateState>>,
    pub event_tx: broadcast::Sender<ThreadEvent>,
    pub session_id: String,
    /// Where to persist the status snapshot: the ccteam project dir + sid that
    /// own `<project>/.ccteam/chat/<sid>/status.json`.
    pub project_dir: PathBuf,
    pub sid: String,
    /// Pull-mode context surface for a vendor that pushes no usage at all.
    /// `None` for vendors that report it (grok, opencode) — asking them would
    /// be pure cost.
    pub context_probe: Option<AcpContextProbe>,
    pub tuning: AcpTurnTuning,
}

/// What the runner does next, decided under the state lock so the turn slot is
/// never observably free while more work is scheduled.
enum NextTurn {
    /// A user turn (the first one, or one queued while another ran).
    User {
        turn_id: String,
        text: String,
        done: Arc<Notify>,
        sent: Arc<AcpWriteBarrier>,
    },
    /// The runner's own context probe. Occupies the turn slot exactly like a
    /// user turn — a concurrent `submit_turn` queues behind it instead of
    /// racing for the buffer — but publishes no events, so it never reaches
    /// `turns.jsonl`, the IM reply path, or a delegation parent.
    Probe {
        probe: AcpContextProbe,
        done: Arc<Notify>,
        sent: Arc<AcpWriteBarrier>,
    },
    Stop,
}

/// Whether this session still owes us an occupancy reading.
///
/// True only when no push/derive channel has spoken: `Unknown` (nobody ever
/// did) or `Probed` (last value came from us, and occupancy has moved since).
/// A vendor that reports `Reported`/`Derived` is never probed — the pull path
/// is strictly the fallback for vendors with no push channel.
fn probe_is_due(state: &SessionTranslateState, probe: &AcpContextProbe) -> bool {
    if !matches!(
        state.used_source,
        ContextSource::Unknown | ContextSource::Probed
    ) {
        return false;
    }
    // Only ask for what the vendor itself advertises — if the command is gone
    // from its catalog, stop asking rather than prompt blindly.
    state
        .available_commands
        .iter()
        .any(|c| c.name == probe.command)
}

/// Decision made under the per-session translate-state lock. Every ACP vendor
/// uses this same state machine so queue-vs-inject semantics cannot drift.
pub enum AcpTurnRoute {
    Start {
        turn_id: String,
        turn_done: Arc<Notify>,
        prompt_sent: Arc<AcpWriteBarrier>,
    },
    Inject {
        active_turn_id: String,
        prompt_sent: Option<Arc<AcpWriteBarrier>>,
        reservation: AcpInjectionReservation,
    },
    Queue {
        turn_id: String,
        degraded_from_inject: bool,
        /// 1-based waiting position in this session's own FIFO. ACP owns that
        /// FIFO, so it can say where the message sits — and a dispatcher told
        /// only "pending" re-sent the same instruction three times (#201).
        position: usize,
    },
}

/// Route one user message against the current ACP turn.
///
/// Idle always means a normal `session/prompt`. While active, native-inject
/// vendors reuse the active turn id; an explicit Queue (or an Inject request on
/// a queue-only ACP adapter) becomes a distinct FIFO turn.
pub fn route_acp_turn(
    state: &mut SessionTranslateState,
    text: &str,
    routing: TurnRouting,
    supports_native_inject: bool,
) -> AcpTurnRoute {
    if let Some(active) = state.buffer.as_ref() {
        if routing == TurnRouting::Inject && supports_native_inject {
            if let Some(reservation) = state
                .injection_gate
                .as_ref()
                .and_then(|gate| gate.reserve())
            {
                return AcpTurnRoute::Inject {
                    active_turn_id: active.turn_id.clone(),
                    prompt_sent: state.prompt_sent.clone(),
                    reservation,
                };
            }
        }

        let turn_id = next_acp_turn_id();
        state.pending.push_back((turn_id.clone(), text.to_string()));
        return AcpTurnRoute::Queue {
            turn_id,
            degraded_from_inject: routing == TurnRouting::Inject,
            position: state.pending.len(),
        };
    }

    let turn_id = next_acp_turn_id();
    let turn_done = Arc::new(Notify::new());
    let prompt_sent = Arc::new(AcpWriteBarrier::default());
    state.begin_turn_with_prompt_barrier(
        turn_id.clone(),
        Arc::clone(&turn_done),
        Some(Arc::clone(&prompt_sent)),
    );
    AcpTurnRoute::Start {
        turn_id,
        turn_done,
        prompt_sent,
    }
}

impl AcpTurnRunner {
    /// Spawn the serial turn loop. `first_*` describe the turn `submit_turn`
    /// already reserved (via `begin_turn`); the loop owns draining any turns
    /// queued in [`SessionTranslateState::pending`] afterwards.
    pub fn spawn(
        self,
        first_turn_id: String,
        first_turn_done: Arc<Notify>,
        first_prompt_sent: Arc<AcpWriteBarrier>,
        first_text: String,
    ) {
        let AcpTurnRunner {
            transport,
            state,
            event_tx,
            session_id,
            project_dir,
            sid,
            context_probe,
            tuning,
        } = self;
        tokio::spawn(async move {
            let mut turn_id = first_turn_id;
            let mut turn_done = first_turn_done;
            let mut prompt_sent = first_prompt_sent;
            let mut text = first_text;
            // `None` for a user turn; `Some(probe)` while the runner is asking
            // the vendor for its own context reading.
            let mut probing: Option<AcpContextProbe> = None;
            loop {
                if probing.is_none() {
                    let _ = event_tx.send(ThreadEvent::TurnStarted {
                        turn_id: turn_id.clone(),
                    });
                }
                let result = transport
                    .call_with_write_barrier(
                        "session/prompt",
                        json!({
                            "sessionId": session_id,
                            "prompt": [{ "type": "text", "text": text }]
                        }),
                        Arc::clone(&prompt_sent),
                    )
                    .await;
                // The prompt response closes admission for this vendor turn.
                // Reservations were minted under the same state lock used
                // here, so sealing then draining them prevents a late control
                // RPC from crossing into a queued successor.
                let injection_gate = match state.lock() {
                    Ok(st) => st.injection_gate.clone(),
                    Err(_) => None,
                };
                if let Some(gate) = injection_gate.as_ref() {
                    gate.seal();
                    gate.wait_drained().await;
                }
                if result.is_ok() {
                    // Wait until the dispatcher drained through the turn boundary
                    // so finalize sees every chunk. A stored permit returns
                    // instantly when the boundary already arrived (common case).
                    //
                    // A probe waits far less: its whole reply precedes the
                    // response on the wire, so the only thing to cover is the
                    // dispatcher-vs-response task race, not vendor think time.
                    // Charging it the full barrier would hold the turn slot —
                    // and delay a user's next message — for nothing. Coming up
                    // short is safe by construction: an unparseable reply
                    // leaves the previous reading in place.
                    let barrier = if probing.is_some() {
                        PROBE_DISPATCH_GRACE.min(tuning.finalize_barrier)
                    } else {
                        tuning.finalize_barrier
                    };
                    let _ = tokio::time::timeout(barrier, turn_done.notified()).await;
                    if let Some(sleep) = tuning.post_finalize_sleep {
                        tokio::time::sleep(sleep).await;
                    }
                }
                // Finalize the current turn AND re-reserve the next queued turn
                // in one critical section: `buffer` must never observably drop
                // to None while work is queued, else a concurrent `submit_turn`
                // would see an idle session and spawn a second runner.
                let mut boundary_status = None;
                let finished_probe = probing.take();
                let next = match state.lock() {
                    Ok(mut st) => {
                        let mut events = Vec::new();
                        match (&finished_probe, &result) {
                            // A probe's reply is data, not conversation: read
                            // it out of the buffer and publish nothing. A
                            // failed probe leaves occupancy exactly as it was.
                            (Some(probe), Ok(_)) => {
                                st.signal_turn_done();
                                let reply = st.take_buffer().map(|b| b.text).unwrap_or_default();
                                if let Some(ctx) = (probe.parse)(&reply) {
                                    if let Some(used) = ctx.used_tokens {
                                        st.set_used_tokens(used, ctx.source);
                                    }
                                    st.window_tokens = Some(ctx.window_tokens);
                                } else {
                                    tracing::debug!(
                                        vendor = tuning.label,
                                        command = probe.command,
                                        "acp context probe: unrecognised reply, occupancy stays unknown"
                                    );
                                }
                            }
                            (Some(probe), Err(e)) => {
                                st.signal_turn_done();
                                let _ = st.take_buffer();
                                tracing::debug!(
                                    vendor = tuning.label,
                                    command = probe.command,
                                    error = %e,
                                    "acp context probe failed"
                                );
                            }
                            (None, Ok(r)) => events = finalize_from_prompt_result(&mut st, r),
                            (None, Err(e)) => events = fail_turn(&mut st, &e.to_string()),
                        }
                        // The turn boundary is the only moment context usage
                        // changes, so it is the only moment worth persisting.
                        boundary_status = Some(st.thread_status());
                        // Decide the successor while still holding the slot:
                        // `buffer` must never observably drop to None while
                        // work remains, else a concurrent `submit_turn` would
                        // see an idle session and spawn a second runner.
                        let next = if let Some((tid, txt)) = st.pending.pop_front() {
                            let done = Arc::new(Notify::new());
                            let sent = Arc::new(AcpWriteBarrier::default());
                            st.begin_turn_with_prompt_barrier(
                                tid.clone(),
                                Arc::clone(&done),
                                Some(Arc::clone(&sent)),
                            );
                            NextTurn::User {
                                turn_id: tid,
                                text: txt,
                                done,
                                sent,
                            }
                        } else if let Some(probe) = context_probe
                            .filter(|_| finished_probe.is_none())
                            .filter(|p| probe_is_due(&st, p))
                        {
                            // Only ever scheduled after a USER turn, so a probe
                            // can never schedule another probe.
                            let done = Arc::new(Notify::new());
                            let sent = Arc::new(AcpWriteBarrier::default());
                            st.begin_turn_with_prompt_barrier(
                                format!("probe-{}", next_acp_turn_id()),
                                Arc::clone(&done),
                                Some(Arc::clone(&sent)),
                            );
                            NextTurn::Probe { probe, done, sent }
                        } else {
                            NextTurn::Stop
                        };
                        // Publish the old boundary before another submit can
                        // observe idle, and before this loop publishes the next
                        // queued TurnStarted. Sending to broadcast is sync.
                        for event in events {
                            let _ = event_tx.send(event);
                        }
                        next
                    }
                    Err(_) => {
                        tracing::error!(
                            vendor = tuning.label,
                            "acp turn runner: state lock poisoned"
                        );
                        NextTurn::Stop
                    }
                };
                // Outside the lock: the write is best-effort file I/O and must
                // never hold up the next queued turn.
                if let Some(status) = boundary_status {
                    write_status_file(&project_dir, &sid, &status);
                }
                match next {
                    NextTurn::User {
                        turn_id: tid,
                        text: txt,
                        done,
                        sent,
                    } => {
                        turn_id = tid;
                        text = txt;
                        turn_done = done;
                        prompt_sent = sent;
                    }
                    NextTurn::Probe { probe, done, sent } => {
                        text = format!("/{}", probe.command);
                        turn_done = done;
                        prompt_sent = sent;
                        probing = Some(probe);
                    }
                    NextTurn::Stop => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_state(turn_id: &str) -> SessionTranslateState {
        let mut state = SessionTranslateState::default();
        state.begin_turn_with_prompt_barrier(
            turn_id,
            Arc::new(Notify::new()),
            Some(Arc::new(AcpWriteBarrier::default())),
        );
        state
    }

    #[test]
    fn idle_message_starts_for_both_routing_modes() {
        for routing in [TurnRouting::Inject, TurnRouting::Queue] {
            let mut state = SessionTranslateState::default();
            let route = route_acp_turn(&mut state, "hello", routing, true);
            assert!(matches!(route, AcpTurnRoute::Start { .. }));
            assert!(state.buffer.is_some());
            assert!(state.prompt_sent.is_some());
            assert!(state.pending.is_empty());
        }
    }

    #[test]
    fn native_inject_reuses_active_turn_without_queueing() {
        let mut state = active_state("active-1");
        let route = route_acp_turn(&mut state, "steer", TurnRouting::Inject, true);
        match route {
            AcpTurnRoute::Inject { active_turn_id, .. } => {
                assert_eq!(active_turn_id, "active-1")
            }
            _ => panic!("expected native inject"),
        }
        assert!(state.pending.is_empty());
    }

    #[test]
    fn explicit_queue_and_unsupported_inject_are_distinct_fifo_turns() {
        let mut explicit = active_state("active-1");
        let route = route_acp_turn(&mut explicit, "later", TurnRouting::Queue, true);
        assert!(matches!(
            route,
            AcpTurnRoute::Queue {
                degraded_from_inject: false,
                ..
            }
        ));
        assert_eq!(explicit.pending.len(), 1);

        let mut degraded = active_state("active-2");
        let route = route_acp_turn(&mut degraded, "later", TurnRouting::Inject, false);
        assert!(matches!(
            route,
            AcpTurnRoute::Queue {
                degraded_from_inject: true,
                ..
            }
        ));
        assert_eq!(degraded.pending.len(), 1);
    }

    #[tokio::test]
    async fn sealed_turn_queues_late_inject_and_waits_for_reservation() {
        let mut state = active_state("active-1");
        let route = route_acp_turn(&mut state, "reserved", TurnRouting::Inject, true);
        let reservation = match route {
            AcpTurnRoute::Inject { reservation, .. } => reservation,
            _ => panic!("expected native inject reservation"),
        };
        let gate = state.injection_gate.clone().unwrap();
        gate.seal();

        let late = route_acp_turn(&mut state, "late", TurnRouting::Inject, true);
        assert!(matches!(
            late,
            AcpTurnRoute::Queue {
                degraded_from_inject: true,
                ..
            }
        ));

        let waiter = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.wait_drained().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(reservation);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("reservation drains")
            .unwrap();
    }
}
