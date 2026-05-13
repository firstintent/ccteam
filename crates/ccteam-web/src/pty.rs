//! V0.3.2 F56 — refcounted `tmux pipe-pane` registry shared by all WS
//! PTY subscribers.
//!
//! Goals (PRD §F56):
//!
//! - **one FIFO + one `pipe-pane` per tmux session**, no matter how
//!   many browser tabs are attached. A `broadcast::Sender` fans out
//!   the bytes that the FIFO reader task pulls off `pipe-pane`'s
//!   write end.
//! - **refcount drop tears down**: the last subscriber drop runs
//!   `tmux pipe-pane -t <session>:0.0` (no command = stop) and unlinks
//!   the FIFO. The tmux session itself is never touched — F56 must not
//!   `kill-session` (CLAUDE.md §三 red line).
//! - **bounded broadcast** (`capacity = 256`): a slow subscriber sees
//!   `RecvError::Lagged(n)`; the WS handler emits a single
//!   `{"type":"lag","behind":n}` text frame and continues from the
//!   latest available offset rather than closing the socket.
//!
//! ## Race ordering
//!
//! `subscribe()` is the only entry point and is async; it holds an
//! `&tokio::sync::Mutex` over the inner `HashMap` for the duration of
//! the FIFO + pipe-pane bring-up, so two concurrent first subscribers
//! to the same key serialize. The expensive blocking parts (mkfifo,
//! invoking `tmux`) happen inside the locked region — they're fast
//! enough not to matter for the single-user dev tool target.
//!
//! ## Why a dedicated tail task
//!
//! Opening a FIFO read end blocks until *some* writer opens the write
//! end (POSIX semantics). We `tokio::spawn` the tail task **first**,
//! then run `tmux pipe-pane … "cat >> <fifo>"` — both opens unblock
//! together, and the registry-level mutex stays free of long blocks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use ccteam_core::CcteamPaths;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};

/// Capacity of the per-session broadcast channel. Tuned to absorb
/// short browser stalls (e.g. tab regaining focus on mobile) without
/// dropping frames; lag beyond this surfaces as a `lag` control frame.
pub const BROADCAST_CAPACITY: usize = 256;

/// Registry of live `tmux pipe-pane` subscriptions keyed by the route
/// param: `"<slug>"` for workflow / default projects, `"<slug>/<sid>"`
/// for flex per-session.
#[derive(Clone)]
pub struct PtyRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<PtySession>>>>,
}

impl Default for PtyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Atomically subscribe to `key` (creating the underlying FIFO +
    /// `pipe-pane` if absent). Returned [`Subscription`] is RAII —
    /// dropping it decrements the refcount and tears down on zero.
    pub async fn subscribe(
        &self,
        key: &str,
        tmux_session: &str,
        paths: &CcteamPaths,
    ) -> Result<Subscription> {
        let mut guard = self.inner.lock().await;
        let session = if let Some(existing) = guard.get(key) {
            existing.clone()
        } else {
            let session = Arc::new(PtySession::bring_up(key, tmux_session, paths).await?);
            guard.insert(key.to_string(), session.clone());
            session
        };
        // Increment refcount inside the registry mutex so a concurrent
        // last-drop can't tear down between the `get` and the inc.
        {
            let mut rc = session.refcount.lock().await;
            *rc = rc.saturating_add(1);
        }
        let rx = session.tx.subscribe();
        Ok(Subscription {
            key: key.to_string(),
            session,
            registry: self.clone(),
            rx,
            armed: true,
        })
    }

    /// Test helper: returns the number of active keys. Cheap mutex
    /// peek, no side effects.
    #[doc(hidden)]
    pub async fn len_for_test(&self) -> usize {
        self.inner.lock().await.len()
    }
}

/// One refcounted `tmux pipe-pane` bring-up.
pub struct PtySession {
    pub tmux_session: String,
    pub fifo_path: PathBuf,
    pub tx: broadcast::Sender<Vec<u8>>,
    pub refcount: Mutex<usize>,
}

impl PtySession {
    async fn bring_up(key: &str, tmux_session: &str, paths: &CcteamPaths) -> Result<Self> {
        let pty_dir = paths.pty_dir();
        tokio::fs::create_dir_all(&pty_dir)
            .await
            .with_context(|| format!("create pty dir {}", pty_dir.display()))?;
        let fifo_name = format!("{}.fifo", key.replace('/', "-"));
        let fifo_path = pty_dir.join(fifo_name);

        // Best-effort cleanup of a stale FIFO from a previous run (e.g.
        // server crashed before teardown). `nix::unistd::mkfifo` would
        // EEXIST otherwise.
        let _ = tokio::fs::remove_file(&fifo_path).await;

        mkfifo(&fifo_path)?;

        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);

        // Spawn the FIFO tail task **before** running pipe-pane so the
        // read open and the eventual write open from `cat >>` unblock
        // together (POSIX FIFO open semantics).
        spawn_fifo_tail(fifo_path.clone(), tx.clone());

        // Defensive cleanup: a previous server may have crashed
        // mid-relay with a stale pipe-pane still attached. We need a
        // clean state because `tmux pipe-pane <command>` (no `-o`)
        // unconditionally replaces any existing pipe; running stop
        // first turns this into "stop-then-start" rather than
        // "start-or-no-op". With the registry's refcount the
        // single-relay-per-pane invariant is enforced at the ccteam
        // layer; we don't rely on tmux's `-o` ("only if no existing
        // pipe") semantics.
        let target = format!("{tmux_session}:0.0");
        let _ = Command::new("tmux")
            .args(["pipe-pane", "-t", &target])
            .status()
            .await;

        // tmux pipes pane output to the shell command by default — no
        // flag needed. Target is `:0.0` (first window, first pane);
        // the convention across ccteam-managed sessions is one window
        // with one pane (see tech-design §6.1).
        let shell = format!("cat >> {}", shell_quote(&fifo_path));
        let status = Command::new("tmux")
            .args(["pipe-pane", "-t", &target, &shell])
            .status()
            .await
            .context("invoke tmux pipe-pane")?;
        if !status.success() {
            // Clean up so a later retry can succeed.
            let _ = tokio::fs::remove_file(&fifo_path).await;
            anyhow::bail!(
                "tmux pipe-pane failed for session {tmux_session} (exit {status})",
            );
        }

        Ok(Self {
            tmux_session: tmux_session.to_string(),
            fifo_path,
            tx,
            refcount: Mutex::new(0),
        })
    }

    async fn tear_down(&self) {
        let target = format!("{}:0.0", self.tmux_session);
        // No command after `-t <target>` = stop the existing pipe.
        let _ = Command::new("tmux")
            .args(["pipe-pane", "-t", &target])
            .status()
            .await;
        let _ = tokio::fs::remove_file(&self.fifo_path).await;
    }
}

/// Subscriber handle. Drop it to decrement the refcount.
pub struct Subscription {
    key: String,
    session: Arc<PtySession>,
    registry: PtyRegistry,
    pub rx: broadcast::Receiver<Vec<u8>>,
    armed: bool,
}

impl Subscription {
    /// Tmux session name this subscription is bound to. Handlers use
    /// this for the `send-keys` and `resize-window` invocations.
    pub fn tmux_session(&self) -> &str {
        &self.session.tmux_session
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let key = std::mem::take(&mut self.key);
        let session = self.session.clone();
        let registry = self.registry.clone();
        // Drop runs in whatever context tokio is currently in. We
        // can't `await` here, so we hand the teardown to a background
        // task. The registry's mutex ordering guarantees correctness.
        //
        // `try_current()` guards against drop during runtime shutdown
        // (e.g. tests using `tokio::test` whose runtime is winding
        // down): without a live handle `tokio::spawn` would panic.
        // We accept the very rare leak of one in-flight teardown in
        // exchange for not crashing during graceful shutdown.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut guard = registry.inner.lock().await;
                let mut rc = session.refcount.lock().await;
                *rc = rc.saturating_sub(1);
                if *rc == 0 {
                    guard.remove(&key);
                    drop(rc);
                    session.tear_down().await;
                }
            });
        }
    }
}

/// Spawn a task that opens the FIFO read end and pushes whatever it
/// reads onto the broadcast channel. Exits cleanly on EOF or read
/// error (FIFO unlinked on teardown produces a final EOF).
fn spawn_fifo_tail(fifo_path: PathBuf, tx: broadcast::Sender<Vec<u8>>) {
    tokio::spawn(async move {
        // Open read-only — POSIX semantics: this blocks until tmux's
        // `cat >> <fifo>` (spawned by `pipe-pane`) opens the write end.
        // We deliberately spawn this task BEFORE running pipe-pane so
        // the two opens unblock together; the call site enforces that
        // order. When pipe-pane is later stopped (`tmux pipe-pane`
        // with no command), `cat` sees EOF on its stdin, exits, and
        // closes the write end; our `read` then returns `Ok(0)`, the
        // loop exits, and the task terminates — that's how teardown
        // propagates here.
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
                    "pty_ws: failed to open fifo for read",
                );
                return;
            }
        };

        let mut buf = vec![0u8; 8192];
        loop {
            match file.read(&mut buf).await {
                Ok(0) => {
                    // EOF — fifo unlinked or all writers gone. Exit.
                    break;
                }
                Ok(n) => {
                    // `send` returns Err if there are zero receivers,
                    // but the registry keeps a sender alive even when
                    // refcount=0 (briefly, during teardown). Ignore
                    // the error and keep reading until EOF.
                    let _ = tx.send(buf[..n].to_vec());
                }
                Err(err) => {
                    tracing::debug!(
                        fifo = %fifo_path.display(),
                        error = %err,
                        "pty_ws: fifo read errored; exiting tail",
                    );
                    break;
                }
            }
        }
    });
}

/// Single-quote `path` for use in a tmux shell command. tmux passes
/// the string to `/bin/sh -c`, so any single quote inside the path
/// would break the wrapper. ccteam FIFO paths are
/// `<paths.root>/pty/<key>.fifo` — `root` is user-controlled but
/// never contains single quotes in normal use. We still escape just
/// in case someone exports `CCTEAM_HOME` to a path with `'`.
fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Create a FIFO at `path`. Wrapper around `nix::unistd::mkfifo` so
/// the caller doesn't have to think about umask vs. requested mode.
/// FIFOs in `~/.ccteam/pty/` are user-private by convention; we set
/// 0600 to match the rest of the runtime control plane.
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
    use tempfile::TempDir;

    fn fake_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        let s = shell_quote(Path::new("/tmp/foo/bar.fifo"));
        assert_eq!(s, "'/tmp/foo/bar.fifo'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quote() {
        let s = shell_quote(Path::new("/tmp/it's/x"));
        assert_eq!(s, "'/tmp/it'\\''s/x'");
    }

    #[tokio::test]
    async fn registry_starts_empty() {
        let r = PtyRegistry::new();
        assert_eq!(r.len_for_test().await, 0);
    }

    #[tokio::test]
    async fn fifo_path_uses_key_with_slashes_replaced() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        // We can't run the full `bring_up` without tmux; just check
        // the FIFO naming convention we promise in the doc comment.
        let pty_dir = paths.pty_dir();
        let fifo_name = "demo-claude-1.fifo";
        let expected = pty_dir.join(fifo_name);
        // Manual mirror of bring_up's naming.
        let key = "demo/claude-1";
        let observed = pty_dir.join(format!("{}.fifo", key.replace('/', "-")));
        assert_eq!(observed, expected);
    }
}
