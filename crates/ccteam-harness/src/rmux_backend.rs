//! `RmuxBackend` — V0.8 W2a implementation of [`ProcessBackend`] backed by
//! the `rmux-sdk` 0.3 daemon.
//!
//! Daemon spawn protocol: ccteam re-hosts the rmux daemon inside its
//! own binary via the `--__internal-daemon <socket>` argv form (see
//! [`crate::daemon::run_internal_daemon`]). The SDK's
//! `connect_or_start` reads `RMUX_SDK_DAEMON_BINARY` to locate the
//! daemon binary; `RmuxBackend::new` sets that env var to
//! `std::env::current_exe()` so the SDK spawns ccteam itself rather
//! than a separate `rmux` artifact.
//!
//! W2b scope (this revision):
//!
//! - `spawn` / `exists` / `send_text` / `send_enter` / `capture` /
//!   `pane_dims` / `pane_pid` / `list_pane_pids` / `resize` / `kill` /
//!   `list_sessions` — all wired through SDK primitives (W2a).
//! - `subscribe` — drives the SDK `pane.line_stream()`: each
//!   `PaneLineItem::Line` becomes a `MuxEvent::OutputChunk` plus a
//!   `MuxEvent::PatternMatched` per registered-regex hit; a
//!   `PaneLineItem::Lag` becomes `MuxEvent::OutputDropped`. No FIFO
//!   machinery — the daemon owns the broadcast.
//! - `register_pattern` — compiles + stores into a shared
//!   [`crate::patterns::PatternMatcher`] per session; `subscribe`
//!   snapshots it (same type the TmuxBackend uses).
//!
//! Known gap (W2b followup): [`crate::PaneBackend::capture`]'s `with_ansi=true`
//! cannot be honored from the SDK's `PaneSnapshot` cell grid — ANSI
//! escape bytes are gone after the daemon parses the grid. W2a's impl
//! returns rendered plain-text bytes for both branches and documents
//! the gap; ccteam-web consumers that need raw bytes continue routing
//! through `ccteam-web::pty::PtyRegistry` until W2b ports the registry.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream;
use rmux_sdk::PaneLineStream;
use tokio::sync::Mutex;

use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, EnsureSession, EnsureSessionPolicy, PaneInfo,
    PaneLineItem, PaneProcessState, ProcessSpec, Rmux, RmuxEndpoint, RmuxError, SessionName,
    TerminalSizeSpec,
};

use crate::patterns::{PatternMatcher, PatternVendor};
use crate::{
    BackendKind, MuxEvent, MuxEventStream, MuxSessionId, MuxSessionSpec, PaneBackend,
    ProcessBackend,
};

const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Default UDS path for the ccteam-hosted rmux daemon. Resolves to
/// `$HOME/.ccteam/run/mux.sock` on Unix; on Windows callers fall back
/// to the SDK-default named pipe.
///
/// The parent directory is created on first use (mode 0700 on Unix).
pub fn default_ccteam_harness_socket_path() -> PathBuf {
    let home = dirs_home_or_tmp();
    home.join(".ccteam").join("run").join("mux.sock")
}

fn dirs_home_or_tmp() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home);
    }
    // Fall back to current_dir → /tmp so tests with no $HOME still
    // resolve a path. Production callers always have $HOME.
    if let Some(dir) = std::env::var_os("TMPDIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("/tmp")
}

/// Ensure `<socket>/..` exists with restrictive perms.
fn ensure_socket_parent(socket: &Path) -> Result<()> {
    if let Some(parent) = socket.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create rmux socket parent {}", parent.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                let _ = std::fs::set_permissions(parent, perms);
            }
        }
    }
    Ok(())
}

/// Classify an SDK error as a dead / closed-transport failure that a
/// fresh `connect_or_start` could recover from.
///
/// Keys on [`RmuxError::Transport`] with the same `io::ErrorKind` set
/// the SDK itself treats as a clean transport close
/// (`is_clean_shutdown_close` in the SDK's `rmux` handle) —
/// `UnexpectedEof | ConnectionReset | BrokenPipe | NotConnected` — plus
/// `ConnectionRefused`, which is the stale-socket case after a daemon
/// crash or reboot (the socket file lingers but nothing is listening).
///
/// Deliberately narrow: `io::ErrorKind::Other`, protocol errors, and
/// per-session errors (`PaneNotFound`, etc.) are NOT treated as
/// transport-dead — reconnecting would not change their outcome.
fn is_dead_transport(err: &RmuxError) -> bool {
    use std::io::ErrorKind;
    matches!(
        err,
        RmuxError::Transport { source, .. }
            if matches!(
                source.kind(),
                ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionReset
                    | ErrorKind::BrokenPipe
                    | ErrorKind::NotConnected
                    | ErrorKind::ConnectionRefused
            )
    )
}

/// Per-session compiled pattern registry, keyed by `MuxSessionId`.
/// Shares the [`PatternMatcher`] type with `TmuxBackend`; `subscribe`
/// snapshots the matcher (`Arc::clone`) into the line-stream translator.
type PatternRegistry = Arc<Mutex<HashMap<MuxSessionId, Arc<PatternMatcher>>>>;

/// State threaded through `subscribe`'s `unfold` over the SDK line
/// stream. `pending` holds the `PatternMatched` events derived from the
/// most recent line, yielded after its `OutputChunk`.
struct RmuxStreamState {
    line_stream: PaneLineStream,
    matcher: Arc<PatternMatcher>,
    pending: VecDeque<MuxEvent>,
}

/// `RmuxBackend` — ccteam's ProcessBackend impl over rmux-sdk 0.3.
///
/// The SDK `Rmux` handle is lazily connected on first use and cached in
/// a [`Mutex`]-guarded `Option<Arc<Rmux>>` so the daemon spawn cost
/// (~50-200ms) is paid only when the backend is actually used.
///
/// Unlike a `OnceCell`, the cache is **invalidatable**: if the daemon
/// dies (machine reboot, daemon crash, OOM) the cached handle's
/// transport is dead and every subsequent operation would fail
/// permanently. Each operation therefore runs through [`Self::call`],
/// which inspects the SDK error: on a dead-transport / closed-connection
/// error it drops the stale handle, reconnects via `connect_or_start`
/// (which re-spawns the daemon only if no socket answers), and retries
/// the operation once. The Arc is cloned out under the lock and the lock
/// is released before the operation runs, so backend calls do not
/// serialize on each other; reconnect is serialized by an
/// [`Arc::ptr_eq`] check so concurrent reconnects spawn at most one fresh
/// handle (the loser observes the winner's new Arc and reuses it).
pub struct RmuxBackend {
    rmux: Mutex<Option<Arc<Rmux>>>,
    socket_path: PathBuf,
    pattern_registry: PatternRegistry,
}

impl std::fmt::Debug for RmuxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RmuxBackend")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Default for RmuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RmuxBackend {
    /// Construct a new backend. Side effect: sets
    /// `RMUX_SDK_DAEMON_BINARY` to `std::env::current_exe()` so the SDK
    /// spawns ccteam itself as the daemon binary on first
    /// `connect_or_start`. Idempotent — if the env var is already set
    /// by the operator, the existing value wins (production-friendly
    /// for `CCTEAM_RMUX_BIN` style overrides should they materialize).
    pub fn new() -> Self {
        Self::with_socket_path(default_ccteam_harness_socket_path())
    }

    /// Variant that pins the UDS endpoint explicitly. Used by the
    /// integration test which routes through a tempfile socket.
    pub fn with_socket_path(socket_path: PathBuf) -> Self {
        // Set RMUX_SDK_DAEMON_BINARY synchronously at construction time
        // so by the time any async `rmux()` call hits, the SDK already
        // sees the env var. Advisor note 2: never set this inside the
        // async OnceCell init — that's racy and (on edition 2024)
        // unsafe.
        if std::env::var_os(SDK_DAEMON_BINARY_ENV).is_none() {
            if let Ok(exe) = std::env::current_exe() {
                // SAFETY: tests using this backend run single-threaded
                // up to the OnceCell init point; production sets this
                // once per process at orchestrator startup.
                std::env::set_var(SDK_DAEMON_BINARY_ENV, exe.as_os_str());
            }
        }
        Self {
            rmux: Mutex::new(None),
            socket_path,
            pattern_registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Build a fresh SDK handle by connecting to — or starting — the
    /// daemon. `connect_or_start` first tries the existing socket
    /// (cheap) and only spawns a new daemon if none answers, so calling
    /// this to recover from a dead handle is safe: a live daemon is
    /// reused, a dead one is replaced.
    async fn connect_fresh(&self) -> Result<Arc<Rmux>> {
        ensure_socket_parent(&self.socket_path)?;
        let endpoint = RmuxEndpoint::UnixSocket(self.socket_path.clone());
        let rmux = Rmux::builder()
            .endpoint(endpoint)
            .default_timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .connect_or_start()
            .await
            .map_err(|e| {
                anyhow!(
                    "RmuxBackend connect_or_start at {}: {e}",
                    self.socket_path.display()
                )
            })?;
        Ok(Arc::new(rmux))
    }

    /// Return the cached handle, connecting (or starting the daemon) on
    /// first use. The Arc is cloned out under the lock; the lock is
    /// released before the caller uses the handle.
    async fn rmux(&self) -> Result<Arc<Rmux>> {
        let mut guard = self.rmux.lock().await;
        if let Some(rmux) = guard.as_ref() {
            return Ok(Arc::clone(rmux));
        }
        let rmux = self.connect_fresh().await?;
        *guard = Some(Arc::clone(&rmux));
        Ok(rmux)
    }

    /// Invalidate `stale` and return a freshly connected handle. The
    /// [`Arc::ptr_eq`] guard makes concurrent reconnects converge: only
    /// the first caller (whose `stale` still matches the cached Arc)
    /// reconnects; later callers observe the replacement and reuse it,
    /// so a daemon hiccup never triggers a reconnect storm of multiple
    /// `connect_or_start` daemon spawns.
    async fn reconnect(&self, stale: &Arc<Rmux>) -> Result<Arc<Rmux>> {
        let mut guard = self.rmux.lock().await;
        match guard.as_ref() {
            Some(current) if !Arc::ptr_eq(current, stale) => {
                // Someone else already reconnected — reuse their handle.
                Ok(Arc::clone(current))
            }
            _ => {
                let fresh = self.connect_fresh().await?;
                *guard = Some(Arc::clone(&fresh));
                Ok(fresh)
            }
        }
    }

    /// Run one SDK operation with reconnect-on-dead-transport.
    ///
    /// `op` is invoked with the cached handle; if it returns a
    /// dead-transport error ([`is_dead_transport`]) the cache is
    /// invalidated, the daemon reconnected, and `op` retried **once**.
    /// Any other error — and the retry's error, dead-transport or not —
    /// propagates. `label` prefixes the surfaced anyhow error.
    async fn call<T, F, Fut>(&self, label: &str, op: F) -> Result<T>
    where
        F: Fn(Arc<Rmux>) -> Fut,
        Fut: Future<Output = rmux_sdk::Result<T>>,
    {
        let rmux = self.rmux().await?;
        match op(Arc::clone(&rmux)).await {
            Ok(value) => Ok(value),
            Err(err) if is_dead_transport(&err) => {
                let fresh = self.reconnect(&rmux).await?;
                op(fresh)
                    .await
                    .map_err(|e| anyhow!("RmuxBackend::{label} (after reconnect): {e}"))
            }
            Err(err) => Err(anyhow!("RmuxBackend::{label}: {err}")),
        }
    }

    async fn session_name(&self, id: &MuxSessionId) -> Result<SessionName> {
        SessionName::new(id.0.clone())
            .map_err(|e| anyhow!("RmuxBackend: invalid session name `{}`: {e}", id.0))
    }

    /// Convenience: register all of a vendor's base patterns
    /// ([`crate::patterns::base_patterns`]) for `id` in one call.
    /// Mirrors [`crate::TmuxBackend::register_base_patterns`].
    pub async fn register_base_patterns(
        &self,
        id: &MuxSessionId,
        vendor: PatternVendor,
    ) -> Result<()> {
        let mut registry = self.pattern_registry.lock().await;
        let entry = registry.entry(id.clone()).or_default();
        let matcher = Arc::make_mut(entry);
        for pat in crate::patterns::base_patterns(vendor) {
            matcher
                .register(pat.id.to_string(), pat.regex)
                .map_err(|e| anyhow!("base pattern `{}` failed to compile: {e}", pat.id))?;
        }
        Ok(())
    }

    /// Look up the first pane's `PaneInfo` for this session, when the
    /// session exists and has at least one pane. Used by `pane_pid`,
    /// `pane_dims`, and `list_pane_pids`.
    async fn first_pane_info(&self, id: &MuxSessionId) -> Result<Option<PaneInfo>> {
        let name = self.session_name(id).await?;
        let label = format!("first_pane_info `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move {
                let session = rmux.session(name).await?;
                let snapshot = session.pane(0, 0).info().await?;
                Ok(snapshot.panes.into_iter().next())
            }
        })
        .await
    }
}

#[async_trait]
impl ProcessBackend for RmuxBackend {
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId> {
        let session_name = SessionName::new(spec.name.clone()).map_err(|e| {
            anyhow!(
                "RmuxBackend::spawn invalid session name `{}`: {e}",
                spec.name
            )
        })?;
        let env_strings: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        // working_directory is a String template on the SDK; lossy
        // unicode conversion is fine — paths originate from the
        // operator's environment which is UTF-8 in practice.
        let working_dir = spec.working_dir.to_string_lossy().into_owned();
        let label = format!("spawn `{}`", spec.name);
        self.call(&label, |rmux| {
            let session_name = session_name.clone();
            let env_strings = env_strings.clone();
            let argv = spec.argv.clone();
            let working_dir = working_dir.clone();
            let size = spec.size;
            async move {
                let mut process = ProcessSpec::argv(argv);
                process.environment = Some(env_strings);
                let ensure = EnsureSession::named(session_name)
                    .policy(EnsureSessionPolicy::CreateOnly)
                    .detached(true)
                    .size(TerminalSizeSpec::new(size.0, size.1))
                    .process(process)
                    .working_directory(working_dir);
                rmux.ensure_session(ensure).await?;
                Ok(())
            }
        })
        .await?;
        Ok(MuxSessionId::new(spec.name))
    }

    async fn exists(&self, id: &MuxSessionId) -> Result<bool> {
        let name = self.session_name(id).await?;
        let label = format!("exists `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move { rmux.has_session(name).await }
        })
        .await
    }

    async fn is_alive(&self, id: &MuxSessionId, expected_pid: Option<i32>) -> Result<bool> {
        if !self.exists(id).await? {
            return Ok(false);
        }
        match expected_pid {
            None => Ok(true),
            Some(pid) => {
                if !crate::tmux_ops::pid_is_alive(pid) {
                    return Ok(false);
                }
                match PaneBackend::pane_pid(self, id).await? {
                    Some(actual) => Ok(actual == pid),
                    None => Ok(false),
                }
            }
        }
    }

    async fn send_text(&self, id: &MuxSessionId, text: &str) -> Result<()> {
        let name = self.session_name(id).await?;
        let label = format!("send_text `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move {
                let session = rmux.session(name).await?;
                session.pane(0, 0).send_text(text).await?;
                Ok(())
            }
        })
        .await
    }

    async fn send_enter(&self, id: &MuxSessionId) -> Result<()> {
        let name = self.session_name(id).await?;
        let label = format!("send_enter `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move {
                let session = rmux.session(name).await?;
                session.pane(0, 0).send_key("Enter").await?;
                Ok(())
            }
        })
        .await
    }

    async fn subscribe(&self, id: &MuxSessionId) -> Result<MuxEventStream> {
        // Snapshot the matcher (empty if no patterns registered).
        let matcher = {
            let reg = self.pattern_registry.lock().await;
            reg.get(id)
                .cloned()
                .unwrap_or_else(|| Arc::new(PatternMatcher::new()))
        };
        let name = self.session_name(id).await?;
        let label = format!("subscribe `{}`", id.0);
        // The returned `PaneLineStream` owns a cloned transport, so it
        // outlives the `Rmux` handle and the lock — safe to construct
        // inside `call` and return past the closure.
        let line_stream = self
            .call(&label, |rmux| {
                let name = name.clone();
                async move {
                    let session = rmux.session(name).await?;
                    session.pane(0, 0).line_stream().await
                }
            })
            .await?;

        // unfold over (line_stream, matcher, pending). One `Line` may
        // yield 1 OutputChunk + N PatternMatched; `pending` holds the
        // extras between `next` calls. No FIFO/refcount — the daemon
        // owns the broadcast and the stream's own drop guard unsubs.
        let state = RmuxStreamState {
            line_stream,
            matcher,
            pending: VecDeque::new(),
        };
        let s = stream::unfold(state, |mut st| async move {
            loop {
                if let Some(ev) = st.pending.pop_front() {
                    return Some((ev, st));
                }
                match st.line_stream.next().await {
                    Ok(Some(PaneLineItem::Line { text })) => {
                        for (regex_id, captured) in st.matcher.match_line(&text) {
                            st.pending
                                .push_back(MuxEvent::PatternMatched { regex_id, captured });
                        }
                        // Emit the rendered line as a chunk, re-appending
                        // a `\n` the line stream stripped. This is an
                        // *approximate* reconstruction (the stream also
                        // strips any `\r`), so it is NOT byte-faithful to
                        // the original pane bytes — adequate for SSE
                        // display + pattern matching (which fires off the
                        // rendered line above), not for byte-exact replay.
                        let mut bytes = text.into_bytes();
                        bytes.push(b'\n');
                        return Some((MuxEvent::OutputChunk(bytes), st));
                    }
                    Ok(Some(PaneLineItem::Lag(notice))) => {
                        return Some((
                            MuxEvent::OutputDropped {
                                behind: notice.missed_events,
                            },
                            st,
                        ));
                    }
                    Ok(Some(_)) => {
                        // PaneLineItem is #[non_exhaustive]; a future
                        // variant we don't model is skipped (loop).
                        continue;
                    }
                    Ok(None) => return None,
                    Err(_) => return None,
                }
            }
        });
        Ok(Box::pin(s))
    }

    async fn register_pattern(
        &self,
        id: &MuxSessionId,
        regex_id: String,
        regex: String,
    ) -> Result<()> {
        // Compile + store into this session's shared matcher. Idempotent
        // — same regex_id replaces the pattern. Effective for subsequent
        // `subscribe` calls (existing streams hold their own snapshot).
        let mut registry = self.pattern_registry.lock().await;
        let entry = registry.entry(id.clone()).or_default();
        let matcher = Arc::make_mut(entry);
        matcher
            .register(regex_id.clone(), &regex)
            .map_err(|e| anyhow!("register_pattern `{regex_id}`: invalid regex `{regex}`: {e}"))?;
        Ok(())
    }

    async fn kill(&self, id: &MuxSessionId) -> Result<()> {
        let name = self.session_name(id).await?;
        let label = format!("kill `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move {
                // Look up first to make `kill` idempotent on absent
                // sessions (`session()` uses `ReuseOnly` policy which
                // errors when the session is missing; `has_session` is
                // cheaper than catching that error).
                if !rmux.has_session(name.clone()).await? {
                    return Ok(());
                }
                let session = rmux.session(name).await?;
                // Both `true` (existed and was killed) and `false`
                // (already gone) map to `Ok(())` per the trait contract.
                let _killed = session.kill().await?;
                Ok(())
            }
        })
        .await
    }

    async fn list_sessions(&self) -> Result<Vec<MuxSessionId>> {
        let names = self
            .call(
                "list_sessions",
                |rmux| async move { rmux.list_sessions().await },
            )
            .await?;
        Ok(names
            .into_iter()
            .map(|n| MuxSessionId::new(n.as_str().to_string()))
            .collect())
    }

    fn backend_kind(&self) -> BackendKind {
        BackendKind::Rmux
    }
}

#[async_trait]
impl PaneBackend for RmuxBackend {
    async fn capture(&self, id: &MuxSessionId, lines: usize, _with_ansi: bool) -> Result<Vec<u8>> {
        // W2b followup: PaneSnapshot is the parsed grid — ANSI escape
        // bytes are not recoverable from cells. W2a returns the rendered
        // plain-text bytes for both `with_ansi=true` and `false`.
        let name = self.session_name(id).await?;
        let label = format!("capture `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move {
                let session = rmux.session(name).await?;
                let snapshot = session.pane(0, 0).snapshot().await?;
                let visible_lines = snapshot.visible_lines();
                let take = lines.min(visible_lines.len());
                let start = visible_lines.len().saturating_sub(take);
                let slice = &visible_lines[start..];
                Ok(slice.join("\n").into_bytes())
            }
        })
        .await
    }

    async fn pane_dims(&self, id: &MuxSessionId) -> Result<Option<(u16, u16)>> {
        let Some(info) = self.first_pane_info(id).await? else {
            return Ok(None);
        };
        Ok(Some((info.size.rows, info.size.cols)))
    }

    async fn pane_pid(&self, id: &MuxSessionId) -> Result<Option<i32>> {
        let Some(info) = self.first_pane_info(id).await? else {
            return Ok(None);
        };
        match info.process {
            PaneProcessState::Running { pid: Some(pid) } => Ok(Some(pid as i32)),
            _ => Ok(None),
        }
    }

    async fn list_pane_pids(&self, id: &MuxSessionId) -> Result<Vec<u32>> {
        let Some(info) = self.first_pane_info(id).await? else {
            return Ok(Vec::new());
        };
        match info.process {
            PaneProcessState::Running { pid: Some(pid) } => Ok(vec![pid]),
            _ => Ok(Vec::new()),
        }
    }

    async fn resize(&self, id: &MuxSessionId, cols: u16, rows: u16) -> Result<()> {
        let name = self.session_name(id).await?;
        let label = format!("resize `{}`", id.0);
        self.call(&label, |rmux| {
            let name = name.clone();
            async move {
                let session = rmux.session(name).await?;
                session
                    .pane(0, 0)
                    .resize(TerminalSizeSpec::new(cols, rows))
                    .await?;
                Ok(())
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// `is_dead_transport` must fire for the closed/dead-connection
    /// `io::ErrorKind`s that a fresh `connect_or_start` can recover from
    /// — and must NOT fire for protocol / per-session errors (reconnect
    /// would not change their outcome) or for unrelated I/O kinds.
    #[test]
    fn classifies_dead_transport_errors() {
        let dead_kinds = [
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::NotConnected,
            io::ErrorKind::ConnectionRefused,
        ];
        for kind in dead_kinds {
            let err = RmuxError::transport("op", io::Error::new(kind, "boom"));
            assert!(
                is_dead_transport(&err),
                "{kind:?} must be classified dead-transport"
            );
        }

        // A transport error with an unrelated kind is NOT dead-transport
        // (too broad to reconnect on).
        let other = RmuxError::transport("op", io::Error::other("weird"));
        assert!(!is_dead_transport(&other));

        // Non-transport errors are never dead-transport.
        let unsupported = RmuxError::unsupported("feature", "hint");
        assert!(!is_dead_transport(&unsupported));
        let invalid = RmuxError::invalid_regex("[", "unterminated");
        assert!(!is_dead_transport(&invalid));
    }

    /// `reconnect` with a stale handle that no longer matches the cached
    /// Arc must reuse the cached handle (the concurrent-reconnect loser
    /// path) WITHOUT contacting a daemon. We seed the slot with an inert
    /// `Rmux` (construction does not contact a daemon) and hand
    /// `reconnect` a *different* Arc as the "stale" one, so `ptr_eq` is
    /// false and the cached handle is returned as-is.
    #[tokio::test]
    async fn reconnect_reuses_cache_when_another_caller_already_replaced_it() {
        let backend = RmuxBackend::with_socket_path(PathBuf::from("/nonexistent/never.sock"));
        // Seed the cache with an inert handle (the "winner's" fresh Arc).
        let winner = Arc::new(Rmux::new());
        *backend.rmux.lock().await = Some(Arc::clone(&winner));

        // A different Arc plays the role of a handle some other op was
        // holding when it hit a dead transport.
        let stale = Arc::new(Rmux::new());
        assert!(!Arc::ptr_eq(&winner, &stale));

        let recovered = backend
            .reconnect(&stale)
            .await
            .expect("reconnect must reuse the cached handle without a daemon");
        assert!(
            Arc::ptr_eq(&recovered, &winner),
            "reconnect must return the already-cached handle, not connect afresh"
        );
    }

    /// `rmux()` caches on first call: a second call returns the same Arc.
    /// Seeded via the inert-handle slot so no daemon is needed.
    #[tokio::test]
    async fn rmux_returns_cached_handle_on_second_call() {
        let backend = RmuxBackend::with_socket_path(PathBuf::from("/nonexistent/never.sock"));
        let seeded = Arc::new(Rmux::new());
        *backend.rmux.lock().await = Some(Arc::clone(&seeded));

        let first = backend.rmux().await.expect("cached handle");
        let second = backend.rmux().await.expect("cached handle");
        assert!(Arc::ptr_eq(&first, &seeded));
        assert!(Arc::ptr_eq(&first, &second));
    }
}
