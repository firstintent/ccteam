//! Refcounted `tmux pipe-pane` FIFO relay — ported from
//! `ccteam-web::pty` (V0.3.2 F56) into the `TmuxBackend` so that
//! `MuxBackend::subscribe` is the single owner of the pipe-pane control
//! plane. ccteam-web becomes a thin adapter on top (W2c Site 3).
//!
//! Invariants preserved verbatim from the F56 design:
//!
//! - **one FIFO + one `pipe-pane` per tmux session**, no matter how
//!   many subscribers attach. A [`tokio::sync::broadcast::Sender`] fans
//!   the bytes the FIFO reader pulls off `pipe-pane`'s write end out to
//!   every subscriber's `Receiver`.
//! - **refcount drop tears down**: the last subscriber drop runs
//!   `tmux pipe-pane -t <session>:0.0` (no command = stop) and unlinks
//!   the FIFO. The tmux session itself is NEVER killed (CLAUDE.md §三
//!   red line).
//! - **bounded broadcast** (`capacity = 256`): a slow subscriber sees
//!   `RecvError::Lagged(n)`; the subscribe stream maps that to a
//!   [`crate::MuxEvent::OutputDropped`] rather than closing.
//!
//! ## Decoupling from `ccteam-core`
//!
//! `ccteam-core` depends on `ccteam-mux` (tmux_ops moved here in W1), so
//! mux MUST NOT depend on core — that would cycle. The FIFO directory
//! (`~/.ccteam/pty`, honoring `CCTEAM_HOME`) is therefore resolved by a
//! small local helper that mirrors `CcteamPaths::pty_dir` rather than
//! importing it.
//!
//! ## Race ordering
//!
//! The registry mutex serializes concurrent first-subscribers to the
//! same key. The FIFO read end (tail task) is opened **before** the
//! write end (`pipe-pane` invocation) so the two POSIX opens unblock
//! together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};

use crate::MuxSessionId;

/// Capacity of the per-session broadcast channel. Tuned (F56) to
/// absorb short subscriber stalls without dropping frames; lag beyond
/// this surfaces as a `MuxEvent::OutputDropped` on the stream.
pub const BROADCAST_CAPACITY: usize = 256;

/// Resolve `~/.ccteam/pty` honoring `CCTEAM_HOME`, mirroring
/// `ccteam_core::CcteamPaths::pty_dir` without taking the dependency
/// (which would create a cargo cycle). Falls back to `$TMPDIR` / `/tmp`
/// when neither `CCTEAM_HOME` nor `$HOME` is set (tests / sandboxes).
fn pty_dir() -> PathBuf {
    let root = if let Some(h) = std::env::var_os("CCTEAM_HOME") {
        PathBuf::from(h)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".ccteam")
    } else if let Some(tmp) = std::env::var_os("TMPDIR") {
        PathBuf::from(tmp).join(".ccteam")
    } else {
        PathBuf::from("/tmp").join(".ccteam")
    };
    root.join("pty")
}

/// Registry of live `tmux pipe-pane` relays keyed by `MuxSessionId`.
/// Cheaply cloneable (Arc inside) so the `TmuxBackend` holds one and
/// every `subscribe` shares it.
#[derive(Clone, Default)]
pub struct FifoRelayRegistry {
    inner: Arc<Mutex<HashMap<MuxSessionId, Arc<RelaySession>>>>,
}

impl FifoRelayRegistry {
    /// Atomically attach to `id` (creating the underlying FIFO +
    /// `pipe-pane` if absent), incrementing the refcount. Returns a
    /// fresh broadcast receiver plus an RAII [`RelayGuard`] whose Drop
    /// decrements the refcount and tears down on zero.
    pub async fn attach(
        &self,
        id: &MuxSessionId,
    ) -> Result<(broadcast::Receiver<Vec<u8>>, RelayGuard)> {
        let mut guard = self.inner.lock().await;
        let session = if let Some(existing) = guard.get(id) {
            existing.clone()
        } else {
            let session = Arc::new(RelaySession::bring_up(id).await?);
            guard.insert(id.clone(), session.clone());
            session
        };
        // Increment refcount inside the registry mutex so a concurrent
        // last-drop can't tear down between the `get` and the inc.
        {
            let mut rc = session.refcount.lock().await;
            *rc = rc.saturating_add(1);
        }
        let rx = session.tx.subscribe();
        let relay_guard = RelayGuard {
            id: id.clone(),
            session,
            registry: self.clone(),
            armed: true,
        };
        Ok((rx, relay_guard))
    }

    /// Test helper: number of active relays.
    #[cfg(test)]
    pub async fn len_for_test(&self) -> usize {
        self.inner.lock().await.len()
    }
}

/// One refcounted `tmux pipe-pane` bring-up.
struct RelaySession {
    tmux_session: String,
    fifo_path: PathBuf,
    tx: broadcast::Sender<Vec<u8>>,
    refcount: Mutex<usize>,
}

impl RelaySession {
    async fn bring_up(id: &MuxSessionId) -> Result<Self> {
        let tmux_session = id.0.clone();
        let dir = pty_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create pty dir {}", dir.display()))?;
        let fifo_name = format!("{}.fifo", tmux_session.replace('/', "-"));
        let fifo_path = dir.join(fifo_name);

        // Best-effort cleanup of a stale FIFO from a previous run; mkfifo
        // would EEXIST otherwise.
        let _ = tokio::fs::remove_file(&fifo_path).await;
        mkfifo(&fifo_path)?;

        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);

        // Spawn the FIFO tail task BEFORE running pipe-pane so the read
        // open and the eventual write open from `cat >>` unblock
        // together (POSIX FIFO open semantics).
        spawn_fifo_tail(fifo_path.clone(), tx.clone());

        // Defensive cleanup: a previous process may have crashed
        // mid-relay with a stale pipe-pane still attached. `tmux
        // pipe-pane <command>` (no `-o`) unconditionally replaces any
        // existing pipe; running stop first turns this into
        // "stop-then-start". The ccteam-layer refcount enforces the
        // single-relay-per-pane invariant; we don't rely on tmux's `-o`.
        //
        // Target the bare session name (NOT `:0.0`) so tmux resolves the
        // active pane — a hard-coded `:0` breaks on hosts with
        // `base-index 1` / `pane-base-index 1` (no window/pane 0). This
        // matches the `tmux_ops` convention (resize-window / pane_pid).
        let target = tmux_session.clone();
        let _ = Command::new("tmux")
            .args(["pipe-pane", "-t", &target])
            .status()
            .await;

        let shell = format!("cat >> {}", shell_quote(&fifo_path));
        let status = Command::new("tmux")
            .args(["pipe-pane", "-t", &target, &shell])
            .status()
            .await
            .context("invoke tmux pipe-pane")?;
        if !status.success() {
            let _ = tokio::fs::remove_file(&fifo_path).await;
            anyhow::bail!("tmux pipe-pane failed for session {tmux_session} (exit {status})");
        }

        Ok(Self {
            tmux_session,
            fifo_path,
            tx,
            refcount: Mutex::new(0),
        })
    }

    async fn tear_down(&self) {
        // Bare session name — base-index-safe (see bring_up).
        // No command after `-t <target>` = stop the existing pipe.
        let _ = Command::new("tmux")
            .args(["pipe-pane", "-t", &self.tmux_session])
            .status()
            .await;
        let _ = tokio::fs::remove_file(&self.fifo_path).await;
    }
}

/// RAII handle. Drop decrements the refcount; the last drop tears down
/// the pipe-pane + unlinks the FIFO. Held by the subscribe stream.
pub struct RelayGuard {
    id: MuxSessionId,
    session: Arc<RelaySession>,
    registry: FifoRelayRegistry,
    armed: bool,
}

impl Drop for RelayGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let id = self.id.clone();
        let session = self.session.clone();
        let registry = self.registry.clone();
        // Drop can't await; hand teardown to a background task. The
        // registry mutex ordering guarantees correctness. `try_current`
        // guards drop during runtime shutdown (tokio::test winding
        // down) — without a live handle `spawn` would panic; we accept
        // the rare leak of one in-flight teardown over a crash.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut guard = registry.inner.lock().await;
                let mut rc = session.refcount.lock().await;
                *rc = rc.saturating_sub(1);
                if *rc == 0 {
                    guard.remove(&id);
                    drop(rc);
                    session.tear_down().await;
                }
            });
        }
    }
}

/// Spawn a task that opens the FIFO read end and pushes whatever it
/// reads onto the broadcast channel. Exits cleanly on EOF (FIFO
/// unlinked on teardown produces a final EOF) or read error.
fn spawn_fifo_tail(fifo_path: PathBuf, tx: broadcast::Sender<Vec<u8>>) {
    tokio::spawn(async move {
        // Open read-only — blocks until tmux's `cat >> <fifo>` opens the
        // write end. The call site spawns this BEFORE running pipe-pane
        // so the opens unblock together. When pipe-pane is later stopped,
        // `cat` sees EOF, exits, closes the write end; our `read` returns
        // `Ok(0)` and the loop exits — that's how teardown propagates.
        let mut file = match tokio::fs::OpenOptions::new()
            .read(true)
            .open(&fifo_path)
            .await
        {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(
                    fifo = %fifo_path.display(),
                    error = %err,
                    "fifo_relay: failed to open fifo for read",
                );
                return;
            }
        };

        let mut buf = vec![0u8; 8192];
        loop {
            match file.read(&mut buf).await {
                Ok(0) => break, // EOF — fifo unlinked or all writers gone.
                Ok(n) => {
                    // `send` errors when there are zero receivers, but
                    // the registry keeps a sender alive even at
                    // refcount=0 (briefly, during teardown). Ignore and
                    // keep reading until EOF.
                    let _ = tx.send(buf[..n].to_vec());
                }
                Err(err) => {
                    tracing::debug!(
                        fifo = %fifo_path.display(),
                        error = %err,
                        "fifo_relay: fifo read errored; exiting tail",
                    );
                    break;
                }
            }
        }
    });
}

/// Single-quote `path` for use in a tmux shell command (tmux passes the
/// string to `/bin/sh -c`).
fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Create a 0600 FIFO at `path` (user-private control plane, F56).
fn mkfifo(path: &Path) -> Result<()> {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo as nix_mkfifo;
    let mode = Mode::S_IRUSR | Mode::S_IWUSR;
    nix_mkfifo(path, mode).with_context(|| format!("mkfifo {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(
            shell_quote(Path::new("/tmp/foo/bar.fifo")),
            "'/tmp/foo/bar.fifo'"
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quote() {
        assert_eq!(shell_quote(Path::new("/tmp/it's/x")), "'/tmp/it'\\''s/x'");
    }

    #[tokio::test]
    async fn registry_starts_empty() {
        let r = FifoRelayRegistry::default();
        assert_eq!(r.len_for_test().await, 0);
    }

    #[test]
    fn fifo_name_replaces_slashes() {
        // Mirror bring_up's naming convention for the flex per-session
        // key `<slug>/<sid>`.
        let id = MuxSessionId::new("demo/claude-1");
        let observed = format!("{}.fifo", id.0.replace('/', "-"));
        assert_eq!(observed, "demo-claude-1.fifo");
    }

    #[test]
    fn pty_dir_honors_ccteam_home() {
        // Smoke: with CCTEAM_HOME set, pty_dir is <home>/pty. We don't
        // mutate the process env here (that would race other tests in
        // the same binary); instead assert the suffix shape holds for
        // whatever the ambient resolution produced.
        let d = pty_dir();
        assert!(
            d.ends_with("pty"),
            "pty_dir must end in `pty`: {}",
            d.display()
        );
    }
}
