//! `TmuxBackend` — V0.8 W1 thin async facade over `tmux_ops`.
//!
//! Every trait method maps 1:1 onto the existing `tmux` CLI free
//! functions / `TmuxSession` methods inside [`crate::tmux_ops`]. The
//! impl is "wrap-only" by design: no new behavior, no new tmux calls.
//! The audit-blessed quirks (the post-spawn `resize-window` workaround,
//! the `is_alive` triple-check, idempotent `kill`, bare-name target
//! convention) all stay inside `tmux_ops` and are inherited by this
//! impl for free.
//!
//! **subscribe() / register_pattern()**: W2b ports the refcounted
//! `tmux pipe-pane` FIFO relay (formerly in `ccteam-web::pty`) into this
//! impl (see [`fifo_relay`]) and layers the [`crate::patterns`] regex
//! matcher on top ([`subscribe`]). `subscribe` returns a typed
//! [`crate::MuxEventStream`]; `register_pattern` compiles + stores a
//! regex per session.

mod fifo_relay;
mod subscribe;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::patterns::{PatternMatcher, PatternVendor};
use crate::tmux_ops::{
    capture_pane_tail_from_session, capture_pane_with_ansi_from_session, list_sessions,
    query_pane_dims_from_session, resize_window, TmuxSession,
};
use crate::{BackendKind, MuxBackend, MuxEventStream, MuxSessionId, MuxSessionSpec};

use fifo_relay::FifoRelayRegistry;

/// Per-session compiled pattern registry. Lives at the top level
/// (NOT inside the refcounted relay, which dies at refcount=0) so a
/// `register_pattern` call can precede any `subscribe`. `subscribe`
/// snapshots the current matcher (`Arc::clone`) into the stream — a
/// pattern added afterward applies only to subsequent subscribes,
/// matching `register_pattern`'s "effective at subscribe time"
/// contract.
type PatternRegistry = Arc<Mutex<HashMap<MuxSessionId, Arc<PatternMatcher>>>>;

#[derive(Clone, Default)]
pub struct TmuxBackend {
    /// Refcounted FIFO + broadcast relays, one per live session.
    relays: FifoRelayRegistry,
    /// Compiled regex patterns per session, consulted by `subscribe`.
    patterns: PatternRegistry,
}

impl std::fmt::Debug for TmuxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmuxBackend").finish_non_exhaustive()
    }
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> Arc<dyn MuxBackend> {
        Arc::new(Self::new())
    }

    /// Convenience: register all of a vendor's base patterns
    /// ([`crate::patterns::base_patterns`]) for `id` in one call. The
    /// base table is compile-test-verified, so this never fails on a
    /// shipped binary.
    pub async fn register_base_patterns(
        &self,
        id: &MuxSessionId,
        vendor: PatternVendor,
    ) -> Result<()> {
        let mut reg = self.patterns.lock().await;
        let entry = reg.entry(id.clone()).or_default();
        let matcher = Arc::make_mut(entry);
        for pat in crate::patterns::base_patterns(vendor) {
            matcher
                .register(pat.id.to_string(), pat.regex)
                .map_err(|e| anyhow!("base pattern `{}` failed to compile: {e}", pat.id))?;
        }
        Ok(())
    }
}

#[async_trait]
impl MuxBackend for TmuxBackend {
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId> {
        // Bridge to the existing blocking `start_with_env`, which
        // includes the post-spawn `resize-window` workaround
        // internally. Honor `spec.size` by setting the same
        // `-x C -y R` via a follow-up resize (so non-default sizes
        // land); `start_with_env` always uses 200×50 — for non-default
        // sizes we resize after.
        let session_name = spec.name.clone();
        let working_dir = spec.working_dir.clone();
        let argv = spec.argv.clone();
        let env = spec.env.clone();
        let size = spec.size;

        tokio::task::spawn_blocking(move || -> Result<()> {
            let session = TmuxSession::from_name(session_name.clone());
            let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            let env_refs: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            session
                .start_with_env(&working_dir, &argv_refs, &env_refs)
                .with_context(|| format!("MuxBackend(tmux)::spawn {session_name}"))?;
            // If caller asked for a non-default size, re-resize. The
            // `start_with_env` internal workaround forces 200×50;
            // honor `spec.size` after the fact (best-effort — newer
            // tmux versions correct on attach anyway).
            if size != (200, 50) {
                let _ = resize_window(&session_name, size.0, size.1);
            }
            Ok(())
        })
        .await
        .map_err(|join_err| anyhow!("MuxBackend(tmux)::spawn join error: {join_err}"))??;

        Ok(MuxSessionId::new(spec.name))
    }

    async fn exists(&self, id: &MuxSessionId) -> Result<bool> {
        let name = id.0.clone();
        let res = tokio::task::spawn_blocking(move || TmuxSession::from_name(name).exists())
            .await
            .map_err(|e| anyhow!("MuxBackend(tmux)::exists join error: {e}"))?;
        Ok(res)
    }

    async fn send_text(&self, id: &MuxSessionId, text: &str) -> Result<()> {
        let name = id.0.clone();
        let text = text.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            TmuxSession::from_name(name).send_keys_literal(&text)
        })
        .await
        .map_err(|e| anyhow!("MuxBackend(tmux)::send_text join error: {e}"))?
    }

    async fn send_enter(&self, id: &MuxSessionId) -> Result<()> {
        let name = id.0.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            TmuxSession::from_name(name).send_keys_enter()
        })
        .await
        .map_err(|e| anyhow!("MuxBackend(tmux)::send_enter join error: {e}"))?
    }

    async fn capture(&self, id: &MuxSessionId, lines: usize, with_ansi: bool) -> Result<Vec<u8>> {
        let name = id.0.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            if with_ansi {
                match capture_pane_with_ansi_from_session(&name, lines)? {
                    Some(bytes) => Ok(bytes),
                    None => Ok(Vec::new()),
                }
            } else {
                match capture_pane_tail_from_session(&name, lines, false) {
                    Some(s) => Ok(s.into_bytes()),
                    None => Ok(Vec::new()),
                }
            }
        })
        .await
        .map_err(|e| anyhow!("MuxBackend(tmux)::capture join error: {e}"))?
    }

    async fn pane_dims(&self, id: &MuxSessionId) -> Result<Option<(u16, u16)>> {
        let name = id.0.clone();
        tokio::task::spawn_blocking(move || query_pane_dims_from_session(&name))
            .await
            .map_err(|e| anyhow!("MuxBackend(tmux)::pane_dims join error: {e}"))?
    }

    async fn pane_pid(&self, id: &MuxSessionId) -> Result<Option<i32>> {
        let name = id.0.clone();
        tokio::task::spawn_blocking(move || TmuxSession::from_name(name).pane_pid())
            .await
            .map_err(|e| anyhow!("MuxBackend(tmux)::pane_pid join error: {e}"))?
    }

    async fn list_pane_pids(&self, id: &MuxSessionId) -> Result<Vec<u32>> {
        let name = id.0.clone();
        let pids =
            tokio::task::spawn_blocking(move || TmuxSession::from_name(name).list_pane_pids())
                .await
                .map_err(|e| anyhow!("MuxBackend(tmux)::list_pane_pids join error: {e}"))?;
        Ok(pids)
    }

    async fn resize(&self, id: &MuxSessionId, cols: u16, rows: u16) -> Result<()> {
        let name = id.0.clone();
        tokio::task::spawn_blocking(move || resize_window(&name, cols, rows))
            .await
            .map_err(|e| anyhow!("MuxBackend(tmux)::resize join error: {e}"))?
    }

    async fn subscribe(&self, id: &MuxSessionId) -> Result<MuxEventStream> {
        // Snapshot the current matcher for this session (empty if no
        // patterns registered). `subscribe` is the moment patterns
        // become effective for the returned stream.
        let matcher = {
            let reg = self.patterns.lock().await;
            reg.get(id)
                .cloned()
                .unwrap_or_else(|| Arc::new(PatternMatcher::new()))
        };
        // Attach to the refcounted FIFO relay (bringing up `pipe-pane`
        // + the FIFO on first subscriber). The guard's Drop releases
        // the refcount when the returned stream is dropped.
        let (rx, guard) = self.relays.attach(id).await?;
        Ok(subscribe::build_stream(rx, matcher, guard))
    }

    async fn register_pattern(
        &self,
        id: &MuxSessionId,
        regex_id: String,
        regex: String,
    ) -> Result<()> {
        // Compile + store under this session's matcher. Idempotent:
        // re-registering the same `regex_id` replaces the pattern.
        // Takes effect for subsequent `subscribe` calls (existing
        // streams hold their own snapshot).
        let mut reg = self.patterns.lock().await;
        let entry = reg.entry(id.clone()).or_default();
        let matcher = Arc::make_mut(entry);
        matcher
            .register(regex_id.clone(), &regex)
            .map_err(|e| anyhow!("register_pattern `{regex_id}`: invalid regex `{regex}`: {e}"))?;
        Ok(())
    }

    async fn kill(&self, id: &MuxSessionId) -> Result<()> {
        let name = id.0.clone();
        tokio::task::spawn_blocking(move || TmuxSession::from_name(name).kill())
            .await
            .map_err(|e| anyhow!("MuxBackend(tmux)::kill join error: {e}"))?
    }

    async fn list_sessions(&self) -> Result<Vec<MuxSessionId>> {
        let names = tokio::task::spawn_blocking(list_sessions)
            .await
            .map_err(|e| anyhow!("MuxBackend(tmux)::list_sessions join error: {e}"))?;
        Ok(names.into_iter().map(MuxSessionId).collect())
    }

    fn backend_kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
}

// ─── helpers for non-async callers ────────────────────────────────────────
//
// Production callers that are already sync (screenshot, projects::
// refuse_active_session, run_peek, run_attach) continue to consume the
// sync free-fn surface in `crate::tmux_ops` directly. The trait is
// async because rmux is; sync callers don't pay for an executor here.
