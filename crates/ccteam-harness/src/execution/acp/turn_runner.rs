//! Per-session serial turn runner for ACP adapters (Grok + OpenCode).
//!
//! The gateway serializes nothing: like Claude/Codex it fires a fresh
//! `submit_turn` the instant a user steers mid-turn. But ACP `session/prompt`
//! is a single-turn request/response and the translate [`buffer`] holds exactly
//! one turn, so two overlapping prompts would clobber each other's chunks.
//!
//! Rather than hard-reject the second message (the old `a turn is already in
//! progress` error), `submit_turn` reserves the buffer if the session is idle
//! (and spawns this runner) or appends to [`SessionTranslateState::pending`] if
//! a turn is in flight. This runner drains the queue FIFO: after each turn
//! finalizes it re-reserves the next queued turn under the SAME lock, so no
//! window lets a concurrent `submit_turn` spawn a second runner. Net effect
//! matches Grok/OpenCode native queue semantics — prompts serialize and each
//! yields its own answer — instead of surfacing an error to the user.
//!
//! [`buffer`]: SessionTranslateState::buffer

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{broadcast, Notify};

use super::translate::{fail_turn, finalize_from_prompt_result, SessionTranslateState};
use super::transport::AcpTransport;
use crate::ThreadEvent;

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
    pub tuning: AcpTurnTuning,
}

impl AcpTurnRunner {
    /// Spawn the serial turn loop. `first_*` describe the turn `submit_turn`
    /// already reserved (via `begin_turn`); the loop owns draining any turns
    /// queued in [`SessionTranslateState::pending`] afterwards.
    pub fn spawn(self, first_turn_id: String, first_turn_done: Arc<Notify>, first_text: String) {
        let AcpTurnRunner {
            transport,
            state,
            event_tx,
            session_id,
            tuning,
        } = self;
        tokio::spawn(async move {
            let mut turn_id = first_turn_id;
            let mut turn_done = first_turn_done;
            let mut text = first_text;
            loop {
                let _ = event_tx.send(ThreadEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                });
                let result = transport
                    .call(
                        "session/prompt",
                        json!({
                            "sessionId": session_id,
                            "prompt": [{ "type": "text", "text": text }]
                        }),
                    )
                    .await;
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
                let (events, next) = match state.lock() {
                    Ok(mut st) => {
                        let events = match &result {
                            Ok(r) => finalize_from_prompt_result(&mut st, r),
                            Err(e) => fail_turn(&mut st, &e.to_string()),
                        };
                        let next = match st.pending.pop_front() {
                            Some((tid, txt)) => {
                                let done = Arc::new(Notify::new());
                                st.begin_turn(tid.clone(), Arc::clone(&done));
                                Some((tid, txt, done))
                            }
                            None => None,
                        };
                        (events, next)
                    }
                    Err(_) => {
                        tracing::error!(
                            vendor = tuning.label,
                            "acp turn runner: state lock poisoned"
                        );
                        (Vec::new(), None)
                    }
                };
                for ev in events {
                    let _ = event_tx.send(ev);
                }
                match next {
                    Some((tid, txt, done)) => {
                        turn_id = tid;
                        text = txt;
                        turn_done = done;
                    }
                    None => break,
                }
            }
        });
    }
}
