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
//! **subscribe() / register_pattern()**: W1 returns an error pointing
//! to W2. The refcounted `tmux pipe-pane` registry (currently in
//! `ccteam-web::pty::PtyRegistry`) gets ported into this impl in W2
//! alongside the regex-pattern matching layer.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;

use crate::tmux_ops::{
    capture_pane_tail_from_session, capture_pane_with_ansi_from_session, list_sessions,
    query_pane_dims_from_session, resize_window, TmuxSession,
};
use crate::{MuxBackend, MuxEventStream, MuxSessionId, MuxSessionSpec};

#[derive(Debug, Default, Clone)]
pub struct TmuxBackend {
    // Empty for W1; W2 grows the refcount registry (FIFO + broadcast
    // tx) used by `subscribe`.
    _private: (),
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> Arc<dyn MuxBackend> {
        Arc::new(Self::new())
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

    async fn subscribe(&self, _id: &MuxSessionId) -> Result<MuxEventStream> {
        // W1 scope cap: the refcounted `tmux pipe-pane` registry
        // (FIFO + broadcast::Sender) currently lives in
        // `ccteam-web::pty::PtyRegistry` and stays there for V0.8 W1.
        // W2 ports the registry into this impl and exposes only the
        // `MuxEventStream`. Callers of `MuxBackend::subscribe` MUST
        // not exist yet — `ccteam-web` continues to use `PtyRegistry`
        // directly until W2.
        //
        // Return an explicit error rather than a silent empty stream
        // so an accidental caller fails loudly.
        Err(anyhow!(
            "MuxBackend(tmux)::subscribe is not implemented in W1 — \
             use `ccteam-web::pty::PtyRegistry` until W2 ports the \
             refcounted pipe-pane relay into this impl"
        ))
    }

    async fn register_pattern(
        &self,
        _id: &MuxSessionId,
        _regex_id: String,
        _regex: String,
    ) -> Result<()> {
        // W1 stub. Real impl lands W2b alongside `subscribe`. We
        // intentionally return Ok(()) (not an error) so adapter-level
        // code can call this proactively now without erroring; once
        // subscribe is live in W2 the stub becomes a no-op-with-side-
        // effects-only-at-subscribe-time impl, which is fine.
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
}

// ─── helpers for non-async callers ────────────────────────────────────────
//
// Production callers that are already sync (screenshot, projects::
// refuse_active_session, run_peek, run_attach) continue to consume the
// sync free-fn surface in `crate::tmux_ops` directly. The trait is
// async because rmux is; sync callers don't pay for an executor here.
