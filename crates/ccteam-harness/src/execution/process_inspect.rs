//! V0.8 W2c — shared OS-level process-liveness probe.
//!
//! Both `ClaudeTuiAdapter` (F164 reattach) and future Codex liveness
//! checks need to answer "is the process behind this mux session's pane
//! actually the vendor binary we expect?" — distinct from "does the mux
//! session exist?" (a tmux session can outlive its dead pane via
//! `remain-on-exit on`).
//!
//! The probe walks every pane PID the backend reports (`list_pane_pids`)
//! and asks the OS for the command name via `ps -p <pid> -o comm=`.
//!
//! ## Red-line compliance
//!
//! This reads only the process **command name** (`comm`), never pane
//! **text content**. `ps` is an OS-level inspection — NOT a mux-level
//! concern — so it stays a direct subprocess rather than a trait method
//! (the `ProcessBackend` surface deliberately exposes pane PIDs but not
//! "what binary is the PID running"). The banned `tmux capture-pane`
//! pane-scrape is never invoked here.

/// Async liveness probe: returns `true` iff any pane PID reported by
/// `backend.list_pane_pids(id)` has a `ps -p <pid> -o comm=` command
/// name that **contains** `needle` (e.g. `"claude"` matches `claude`,
/// `claude-code`, `fake-claude`; `"codex"` matches `codex`).
///
/// Returns `false` when the session has no panes, all pids are gone, or
/// none match. PID `0` (the sentinel some tmux states surface) is
/// skipped. A failed `ps` invocation for one pid is treated as a
/// non-match and the loop continues to the next pid.
pub async fn pane_runs_process(
    backend: &dyn crate::PaneBackend,
    id: &crate::MuxSessionId,
    needle: &str,
) -> anyhow::Result<bool> {
    let pids = backend.list_pane_pids(id).await?;
    for pid in pids {
        if pid == 0 {
            continue;
        }
        let out = tokio::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .await?;
        if out.status.success() && String::from_utf8_lossy(&out.stdout).trim().contains(needle) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BackendKind, MuxEventStream, MuxSessionId, MuxSessionSpec, PaneBackend, ProcessBackend,
    };

    struct TestPaneBackend {
        pids: Vec<u32>,
    }

    #[async_trait::async_trait]
    impl ProcessBackend for TestPaneBackend {
        async fn spawn(&self, spec: MuxSessionSpec) -> anyhow::Result<MuxSessionId> {
            Ok(MuxSessionId::new(spec.name))
        }

        async fn exists(&self, _id: &MuxSessionId) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn send_text(&self, _id: &MuxSessionId, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send_enter(&self, _id: &MuxSessionId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn subscribe(&self, _id: &MuxSessionId) -> anyhow::Result<MuxEventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn register_pattern(
            &self,
            _id: &MuxSessionId,
            _regex_id: String,
            _regex: String,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn kill(&self, _id: &MuxSessionId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn list_sessions(&self) -> anyhow::Result<Vec<MuxSessionId>> {
            Ok(Vec::new())
        }

        fn backend_kind(&self) -> BackendKind {
            BackendKind::Tmux
        }
    }

    #[async_trait::async_trait]
    impl PaneBackend for TestPaneBackend {
        async fn capture(
            &self,
            _id: &MuxSessionId,
            _lines: usize,
            _with_ansi: bool,
        ) -> anyhow::Result<Vec<u8>> {
            Ok(Vec::new())
        }

        async fn pane_dims(&self, _id: &MuxSessionId) -> anyhow::Result<Option<(u16, u16)>> {
            Ok(None)
        }

        async fn pane_pid(&self, _id: &MuxSessionId) -> anyhow::Result<Option<i32>> {
            Ok(None)
        }

        async fn list_pane_pids(&self, _id: &MuxSessionId) -> anyhow::Result<Vec<u32>> {
            Ok(self.pids.clone())
        }

        async fn resize(&self, _id: &MuxSessionId, _cols: u16, _rows: u16) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Empty pane PID lists short-circuit to `false` without ever
    /// shelling out to `ps`. Confirms the empty-pids fast path + that
    /// the helper is wired to the pane trait correctly.
    #[tokio::test]
    async fn empty_pids_returns_false() {
        let backend = TestPaneBackend { pids: Vec::new() };
        let id = MuxSessionId::new("nonexistent-session");
        let runs = pane_runs_process(&backend, &id, "claude")
            .await
            .expect("probe must not error on empty pids");
        assert!(!runs, "no panes ⇒ no matching process");
    }

    /// PID 0 is skipped; with the only entry being 0, the probe never
    /// calls `ps` and returns false.
    #[tokio::test]
    async fn pid_zero_returns_false() {
        let backend = TestPaneBackend { pids: vec![0] };
        let id = MuxSessionId::new("x");
        assert!(!pane_runs_process(&backend, &id, "definitely-not-a-comm")
            .await
            .unwrap());
    }
}
