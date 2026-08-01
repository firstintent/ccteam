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

use super::translate::{
    fail_turn, finalize_from_prompt_result, AcpInjectionReservation, SessionTranslateState,
};
use super::transport::{AcpTransport, AcpWriteBarrier};
use crate::execution::session_status::write_status_file;
use crate::{ThreadEvent, TurnRouting};

/// Monotonic suffix so ids stay unique even for turns steered in the same ms.
static ACP_TURN_SEQ: AtomicU64 = AtomicU64::new(0);

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
    pub tuning: AcpTurnTuning,
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
            tuning,
        } = self;
        tokio::spawn(async move {
            let mut turn_id = first_turn_id;
            let mut turn_done = first_turn_done;
            let mut prompt_sent = first_prompt_sent;
            let mut text = first_text;
            loop {
                let _ = event_tx.send(ThreadEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                });
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
                    let _ =
                        tokio::time::timeout(tuning.finalize_barrier, turn_done.notified()).await;
                    if let Some(sleep) = tuning.post_finalize_sleep {
                        tokio::time::sleep(sleep).await;
                    }
                }
                // Finalize the current turn AND re-reserve the next queued turn
                // in one critical section: `buffer` must never observably drop
                // to None while work is queued, else a concurrent `submit_turn`
                // would see an idle session and spawn a second runner.
                let mut boundary_status = None;
                let next = match state.lock() {
                    Ok(mut st) => {
                        let events = match &result {
                            Ok(r) => finalize_from_prompt_result(&mut st, r),
                            Err(e) => fail_turn(&mut st, &e.to_string()),
                        };
                        // The turn boundary is the only moment context usage
                        // changes, so it is the only moment worth persisting.
                        boundary_status = Some(st.thread_status());
                        let next = match st.pending.pop_front() {
                            Some((tid, txt)) => {
                                let done = Arc::new(Notify::new());
                                let sent = Arc::new(AcpWriteBarrier::default());
                                st.begin_turn_with_prompt_barrier(
                                    tid.clone(),
                                    Arc::clone(&done),
                                    Some(Arc::clone(&sent)),
                                );
                                Some((tid, txt, done, sent))
                            }
                            None => None,
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
                        None
                    }
                };
                // Outside the lock: the write is best-effort file I/O and must
                // never hold up the next queued turn.
                if let Some(status) = boundary_status {
                    write_status_file(&project_dir, &sid, &status);
                }
                match next {
                    Some((tid, txt, done, sent)) => {
                        turn_id = tid;
                        text = txt;
                        turn_done = done;
                        prompt_sent = sent;
                    }
                    None => break,
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
