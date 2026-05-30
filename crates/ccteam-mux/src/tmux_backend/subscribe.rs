//! `TmuxBackend::subscribe` stream construction.
//!
//! Given a broadcast::Receiver of raw FIFO byte chunks (from
//! [`super::fifo_relay`]) plus a snapshot of the session's registered
//! [`PatternMatcher`], builds the outward [`crate::MuxEventStream`]:
//!
//! - each FIFO chunk yields a [`crate::MuxEvent::OutputChunk`];
//! - bytes are buffered into completed lines (split on `\n`); each
//!   completed line is run through the matcher, and every hit yields a
//!   [`crate::MuxEvent::PatternMatched`];
//! - a `broadcast::RecvError::Lagged(n)` yields
//!   [`crate::MuxEvent::OutputDropped`] and clears the partial-line
//!   buffer (the next chunk may not start on a line boundary — mirrors
//!   rmux's `PaneLineStream` lag handling);
//! - `RecvError::Closed` ends the stream.
//!
//! The line buffer + matcher are **per-subscriber** (each stream owns
//! its own), so a slow/lagged subscriber never corrupts another's line
//! splitting. The [`super::fifo_relay::RelayGuard`] travels inside the
//! unfold state so the refcount is released exactly when the stream is
//! dropped.

use std::collections::VecDeque;
use std::sync::Arc;

use futures::stream;
use tokio::sync::broadcast;

use crate::patterns::PatternMatcher;
use crate::tmux_backend::fifo_relay::RelayGuard;
use crate::{MuxEvent, MuxEventStream};

/// Per-subscriber stream state threaded through `unfold`.
struct StreamState {
    rx: broadcast::Receiver<Vec<u8>>,
    line_buffer: Vec<u8>,
    matcher: Arc<PatternMatcher>,
    /// Held purely for its Drop side effect (refcount dec + teardown).
    _guard: RelayGuard,
    /// Events already produced from the most recent chunk but not yet
    /// yielded (one chunk → 1 OutputChunk + N PatternMatched).
    pending: VecDeque<MuxEvent>,
}

/// Build the outward event stream from a relay receiver + matcher
/// snapshot + the RAII relay guard.
pub(crate) fn build_stream(
    rx: broadcast::Receiver<Vec<u8>>,
    matcher: Arc<PatternMatcher>,
    guard: RelayGuard,
) -> MuxEventStream {
    let state = StreamState {
        rx,
        line_buffer: Vec::new(),
        matcher,
        _guard: guard,
        pending: VecDeque::new(),
    };
    let s = stream::unfold(state, |mut st| async move {
        loop {
            // Drain anything already produced before awaiting more.
            if let Some(ev) = st.pending.pop_front() {
                return Some((ev, st));
            }
            match st.rx.recv().await {
                Ok(bytes) => {
                    // Emit the raw chunk first, then any line-derived
                    // pattern hits. Queue them in `pending` and loop to
                    // pop the first.
                    drain_lines_into(&mut st.line_buffer, &bytes, &st.matcher, &mut st.pending);
                    st.pending.push_front(MuxEvent::OutputChunk(bytes));
                    // loop → pop_front yields the OutputChunk first.
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Lag is not fatal (F56). Drop the partial-line
                    // buffer (next chunk may not start on a line
                    // boundary) and surface the gap.
                    st.line_buffer.clear();
                    return Some((MuxEvent::OutputDropped { behind: n }, st));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Sender dropped — relay torn down. End the stream.
                    return None;
                }
            }
        }
    });
    Box::pin(s)
}

/// Append `bytes` to `line_buffer`, splitting on `\n`. For each
/// completed line, run the matcher and push a `PatternMatched` for each
/// hit onto `out`. Partial trailing bytes stay buffered.
fn drain_lines_into(
    line_buffer: &mut Vec<u8>,
    bytes: &[u8],
    matcher: &PatternMatcher,
    out: &mut VecDeque<MuxEvent>,
) {
    for &b in bytes {
        if b == b'\n' {
            let line_bytes = std::mem::take(line_buffer);
            let line = String::from_utf8_lossy(&line_bytes);
            for (regex_id, captured) in matcher.match_line(&line) {
                out.push_back(MuxEvent::PatternMatched { regex_id, captured });
            }
        } else {
            line_buffer.push(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::{PatternMatcher, PatternVendor};

    fn collect_ids(out: &VecDeque<MuxEvent>) -> Vec<String> {
        out.iter()
            .filter_map(|e| match e {
                MuxEvent::PatternMatched { regex_id, .. } => Some(regex_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn complete_line_runs_matcher() {
        let m = PatternMatcher::base(PatternVendor::Claude);
        let mut buf = Vec::new();
        let mut out = VecDeque::new();
        // `\xe2\x97\x8f` is the UTF-8 for `●`.
        drain_lines_into(&mut buf, b"\xe2\x97\x8f Read(/foo)\n", &m, &mut out);
        assert!(collect_ids(&out).contains(&"tool_call_started".to_string()));
        assert!(buf.is_empty(), "no partial bytes left after a full line");
    }

    #[test]
    fn partial_line_is_buffered_until_newline() {
        let m = PatternMatcher::base(PatternVendor::Claude);
        let mut buf = Vec::new();
        let mut out = VecDeque::new();
        // First chunk: no newline → buffered, no matches.
        drain_lines_into(&mut buf, b"> implement ", &m, &mut out);
        assert!(out.is_empty());
        assert!(!buf.is_empty());
        // Second chunk completes the line.
        drain_lines_into(&mut buf, b"login\n", &m, &mut out);
        assert!(collect_ids(&out).contains(&"user_prompt_submit".to_string()));
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn lagged_receiver_reports_lag_then_closed() {
        // Build a tiny broadcast, overflow it to force Lagged, and
        // assert recv surfaces Lagged then Closed — the two arms
        // build_stream maps to OutputDropped / end-of-stream. (Building
        // the full stream needs a RelayGuard, which needs a registry +
        // tmux; this exercises the channel semantics build_stream relies
        // on.)
        let (tx, rx) = broadcast::channel::<Vec<u8>>(2);
        for i in 0..4u8 {
            let _ = tx.send(vec![i]);
        }
        drop(tx);
        let mut rx = rx;
        let mut saw_lag = false;
        loop {
            match rx.recv().await {
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => saw_lag = true,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        assert!(saw_lag, "overflowed receiver must report Lagged");
    }
}
