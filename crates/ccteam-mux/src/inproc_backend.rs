//! `InProcBackend` — V0.8 W1 stub for mode-1 in-proc tasks.
//!
//! Wraps a `tokio::task::JoinHandle` per spawned "session". Most
//! trait methods that don't apply to in-proc tasks (send_text /
//! send_enter / capture / pane_dims / resize / subscribe /
//! register_pattern) return errors; the lifecycle subset (spawn /
//! exists / kill / list_sessions / pane_pid / list_pane_pids) does
//! useful work.
//!
//! This is **not** production-load-bearing in W1. It exists so the
//! trait can be constructed against an impl that doesn't require
//! `tmux` on PATH (unit tests, mode-1 unification work in W2+, and
//! the dispatch hook in `from_env("inproc-test")`).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::{MuxBackend, MuxEventStream, MuxSessionId, MuxSessionSpec};

struct InProcSession {
    handle: JoinHandle<()>,
}

#[derive(Default)]
pub struct InProcBackend {
    inner: Arc<Mutex<HashMap<String, InProcSession>>>,
}

impl InProcBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for InProcBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcBackend").finish_non_exhaustive()
    }
}

#[async_trait]
impl MuxBackend for InProcBackend {
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId> {
        let mut guard = self.inner.lock().await;
        if guard.contains_key(&spec.name) {
            return Err(anyhow!(
                "InProcBackend::spawn: session `{}` already exists",
                spec.name
            ));
        }
        // The stub task does nothing — production wiring lands in
        // W2+. We park the task so `exists` / `list_sessions` return
        // truthful state until `kill` aborts it.
        let handle = tokio::spawn(async {
            // Park indefinitely; aborted by `kill`.
            futures::future::pending::<()>().await;
        });
        let id = MuxSessionId::new(spec.name.clone());
        guard.insert(spec.name, InProcSession { handle });
        Ok(id)
    }

    async fn exists(&self, id: &MuxSessionId) -> Result<bool> {
        let guard = self.inner.lock().await;
        Ok(guard
            .get(&id.0)
            .map(|s| !s.handle.is_finished())
            .unwrap_or(false))
    }

    async fn send_text(&self, _id: &MuxSessionId, _text: &str) -> Result<()> {
        Err(anyhow!(
            "InProcBackend::send_text: not applicable to in-proc tasks"
        ))
    }

    async fn send_enter(&self, _id: &MuxSessionId) -> Result<()> {
        Err(anyhow!(
            "InProcBackend::send_enter: not applicable to in-proc tasks"
        ))
    }

    async fn capture(
        &self,
        _id: &MuxSessionId,
        _lines: usize,
        _with_ansi: bool,
    ) -> Result<Vec<u8>> {
        Err(anyhow!(
            "InProcBackend::capture: not applicable to in-proc tasks"
        ))
    }

    async fn pane_dims(&self, _id: &MuxSessionId) -> Result<Option<(u16, u16)>> {
        Ok(None)
    }

    async fn pane_pid(&self, _id: &MuxSessionId) -> Result<Option<i32>> {
        // In-proc tasks share the host process PID; reporting it
        // would mislead `is_alive(expected_pid)` callers (the
        // host pid is always alive). Return None so callers fall
        // through to "exists" semantics.
        Ok(None)
    }

    async fn list_pane_pids(&self, _id: &MuxSessionId) -> Result<Vec<u32>> {
        Ok(Vec::new())
    }

    async fn resize(&self, _id: &MuxSessionId, _cols: u16, _rows: u16) -> Result<()> {
        Err(anyhow!(
            "InProcBackend::resize: not applicable to in-proc tasks"
        ))
    }

    async fn subscribe(&self, _id: &MuxSessionId) -> Result<MuxEventStream> {
        // No pane → no chunks. Returning an empty stream is the
        // ergonomic choice (subscribers that don't actually need
        // chunks can attach and immediately hit EOF).
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn register_pattern(
        &self,
        _id: &MuxSessionId,
        _regex_id: String,
        _regex: String,
    ) -> Result<()> {
        Ok(())
    }

    async fn kill(&self, id: &MuxSessionId) -> Result<()> {
        let mut guard = self.inner.lock().await;
        if let Some(session) = guard.remove(&id.0) {
            session.handle.abort();
        }
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<MuxSessionId>> {
        let guard = self.inner.lock().await;
        Ok(guard
            .iter()
            .filter(|(_, s)| !s.handle.is_finished())
            .map(|(k, _)| MuxSessionId::new(k.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec(name: &str) -> MuxSessionSpec {
        MuxSessionSpec::new(name, vec!["true".into()], PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn spawn_then_exists() {
        let backend = InProcBackend::new();
        let id = backend.spawn(spec("alpha")).await.unwrap();
        assert!(backend.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn spawn_then_kill() {
        let backend = InProcBackend::new();
        let id = backend.spawn(spec("beta")).await.unwrap();
        backend.kill(&id).await.unwrap();
        assert!(!backend.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn double_spawn_errors() {
        let backend = InProcBackend::new();
        backend.spawn(spec("gamma")).await.unwrap();
        let err = backend.spawn(spec("gamma")).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn list_sessions_reflects_lifecycle() {
        let backend = InProcBackend::new();
        let a = backend.spawn(spec("a")).await.unwrap();
        let _b = backend.spawn(spec("b")).await.unwrap();
        let live: Vec<String> = backend
            .list_sessions()
            .await
            .unwrap()
            .into_iter()
            .map(|id| id.0)
            .collect();
        assert!(live.contains(&"a".to_string()));
        assert!(live.contains(&"b".to_string()));

        backend.kill(&a).await.unwrap();
        let after: Vec<String> = backend
            .list_sessions()
            .await
            .unwrap()
            .into_iter()
            .map(|id| id.0)
            .collect();
        assert!(!after.contains(&"a".to_string()));
        assert!(after.contains(&"b".to_string()));
    }
}
