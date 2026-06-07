//! V0.3.2 F56 / V0.8 W2b — thin adapter over
//! [`ccteam_harness::ProcessBackend::subscribe`].
//!
//! The refcounted `tmux pipe-pane` FIFO + broadcast machinery that used
//! to live here was ported into `ccteam-harness::tmux_backend` (W2c Site 3)
//! so that `ProcessBackend::subscribe` is the single owner of the pipe-pane
//! control plane. This module keeps its **public surface stable**
//! ([`PtyRegistry`] + [`Subscription`] with `rx:
//! broadcast::Receiver<Vec<u8>>` + [`Subscription::tmux_session`]) so
//! the `pty_ws` route handler is untouched; internally each
//! [`PtyRegistry::subscribe`] now drives a `MuxEventStream` and fans its
//! [`ccteam_harness::MuxEvent::OutputChunk`] bytes back into a
//! per-subscriber broadcast channel for compatibility with the existing
//! SSE/WS consumer loop.
//!
//! The refcount + FIFO teardown now lives entirely inside
//! `ccteam-harness`: the [`ccteam_harness::MuxEventStream`] returned by
//! `subscribe` carries an RAII guard; dropping the stream (which the
//! forwarder task owns) decrements the backend-side refcount and tears
//! down the pipe-pane on zero. [`Subscription`]'s Drop aborts the
//! forwarder task, dropping the stream.
//!
//! V0.9 removal note: once all WS consumers read `MuxEvent` directly,
//! the `broadcast::Receiver<Vec<u8>>` compat channel + this whole
//! adapter can be deleted and `pty_ws` can consume the stream inline.

use std::sync::Arc;

use anyhow::Result;
use ccteam_core::CcteamPaths;
use ccteam_harness::{MuxEvent, MuxSessionId, PaneBackend, TmuxBackend};
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Capacity of the per-subscriber compat broadcast channel. Matches the
/// historical F56 value so lag behavior downstream is unchanged.
pub const BROADCAST_CAPACITY: usize = 256;

/// v0.8.8 bug4 — how many lines of the current pane to replay as the first
/// frame on connect so an idle pane isn't blank. ~one terminal height plus a
/// little context before the live stream takes over.
const SCREEN_REPLAY_LINES: usize = 200;

/// Adapter registry. Kept so [`crate::state::AppState`] construction
/// (`PtyRegistry::new()`) and its `Clone` shape stay stable; all
/// refcount/FIFO state lives backend-side.
///
/// V0.8: the backend is selected via `ccteam_harness::terminal_from_env()` so web
/// SSE live streaming honors `CCTEAM_MUX_BACKEND` like the rest of
/// ccteam. Previously this hardcoded `TmuxBackend`, which silently
/// produced no stream under `CCTEAM_MUX_BACKEND=rmux` (it drove
/// `tmux pipe-pane` against a session that only exists in the rmux
/// daemon). Default (env unset) is rmux — the bundled backend; an
/// operator opts out with `CCTEAM_MUX_BACKEND=tmux`.
#[derive(Clone)]
pub struct PtyRegistry {
    backend: Arc<dyn PaneBackend>,
}

impl PtyRegistry {
    pub fn new() -> Self {
        let backend = ccteam_harness::terminal_from_env()
            .unwrap_or_else(|_| Arc::new(TmuxBackend::new()) as Arc<dyn PaneBackend>);
        Self { backend }
    }
}

impl Default for PtyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyRegistry {
    /// Subscribe to `key`'s pane output. Delegates to
    /// `TmuxBackend::subscribe(tmux_session)` and spawns a forwarder
    /// task that pushes `OutputChunk` bytes onto a fresh per-subscriber
    /// broadcast channel (the shape `pty_ws` already consumes).
    ///
    /// `key` and `paths` are retained in the signature for call-site
    /// stability; the backend keys its relay registry on the
    /// `tmux_session` name directly, so `key`/`paths` are now advisory
    /// (the FIFO dir is resolved backend-side from `CCTEAM_HOME`).
    pub async fn subscribe(
        &self,
        _key: &str,
        tmux_session: &str,
        _paths: &CcteamPaths,
    ) -> Result<Subscription> {
        let id = MuxSessionId::new(tmux_session.to_string());

        let (tx, rx) = broadcast::channel::<Vec<u8>>(BROADCAST_CAPACITY);
        // v0.8.8 bug4 — replay the current screen on connect. `subscribe`
        // streams only NEW pane output, so attaching to a live-but-idle pane
        // (a claude TUI parked at its prompt) renders BLANK until the next
        // byte. Capture the pane ONCE *before* subscribing (so the snapshot is
        // never reordered against buffered deltas) and seed it as the first
        // frame. Best-effort: a capture error / empty result just skips the
        // replay and the live stream proceeds as before. Fidelity: under the
        // default rmux backend `capture` returns rendered text (legible, not a
        // byte-faithful TUI) — a faithful raw-ANSI terminal needs
        // `CCTEAM_MUX_BACKEND=tmux` (raw capture-pane + pipe-pane).
        if let Ok(screen) = self.backend.capture(&id, SCREEN_REPLAY_LINES, true).await {
            if !screen.is_empty() {
                let _ = tx.send(screen);
            }
        }
        let mut stream = self.backend.subscribe(&id).await?;
        // Forwarder: drain the MuxEventStream, fan OutputChunk bytes
        // into the compat channel. The stream owns the backend-side
        // RAII relay guard — when this task is aborted (on Subscription
        // drop) the stream drops, releasing the refcount. Lag is
        // surfaced backend-side as MuxEvent::OutputDropped; the compat
        // broadcast independently re-derives Lagged for slow WS clients,
        // so we don't need to forward OutputDropped explicitly (the WS
        // handler's own `RecvError::Lagged` path still fires).
        let forwarder = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                match event {
                    MuxEvent::OutputChunk(bytes) => {
                        // Err only when there are zero receivers;
                        // harmless — keep draining so the backend stream
                        // stays alive and the relay isn't torn down
                        // prematurely.
                        let _ = tx.send(bytes);
                    }
                    // Backend-side OutputDropped (the forwarder itself
                    // fell behind the backend broadcast under scheduler
                    // starvation) is intentionally NOT forwarded: the WS
                    // client would see a seamless byte stream with an
                    // invisible hole. This is an accepted single-user
                    // dev-tool trade-off — the dominant lag case (a slow
                    // WS *subscriber*) is still surfaced downstream by
                    // this compat broadcast's own RecvError::Lagged,
                    // which the pty_ws handler maps to a `lag` frame.
                    MuxEvent::OutputDropped { .. } => {}
                    _ => {}
                }
            }
        });

        Ok(Subscription {
            tmux_session: tmux_session.to_string(),
            rx,
            forwarder: Some(forwarder),
        })
    }
}

/// Subscriber handle. Public shape preserved: `rx` is a
/// `broadcast::Receiver<Vec<u8>>` and [`Self::tmux_session`] returns the
/// bound session name. Drop aborts the forwarder task, which drops the
/// backend stream and releases the pipe-pane refcount.
pub struct Subscription {
    tmux_session: String,
    pub rx: broadcast::Receiver<Vec<u8>>,
    forwarder: Option<JoinHandle<()>>,
}

impl Subscription {
    /// Tmux session name this subscription is bound to. Handlers use
    /// this for the `send-keys` and `resize-window` invocations.
    pub fn tmux_session(&self) -> &str {
        &self.tmux_session
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // Abort the forwarder so the backend MuxEventStream is dropped,
        // releasing the refcount + tearing down the FIFO on zero.
        if let Some(handle) = self.forwarder.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_constructs() {
        // Smoke: PtyRegistry::new() must keep working for AppState
        // construction. No tmux needed — we don't subscribe here.
        let _r = PtyRegistry::new();
    }

    #[test]
    fn subscription_exposes_tmux_session_name() {
        // Build a Subscription without a backend stream to assert the
        // public accessor shape (pty_ws relies on `.tmux_session()` +
        // `.rx`). The forwarder is None so Drop is a no-op.
        let (_tx, rx) = broadcast::channel::<Vec<u8>>(4);
        let sub = Subscription {
            tmux_session: "demo-claude-1".to_string(),
            rx,
            forwarder: None,
        };
        assert_eq!(sub.tmux_session(), "demo-claude-1");
    }
}
