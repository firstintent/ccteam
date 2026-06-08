//! `RmuxBackend` — implementation of [`ProcessBackend`] backed by the
//! `rmux-sdk` 0.3 daemon. **Byte-faithful**: `subscribe` and `capture`
//! route raw pane bytes verbatim through the SDK's raw-byte
//! `output_stream()` (NOT the lossy rendered `line_stream()`/`snapshot()`).
//!
//! Daemon spawn protocol: ccteam re-hosts the rmux daemon inside its
//! own binary via the `--__internal-daemon <socket>` argv form (see
//! [`crate::daemon::run_internal_daemon`]). The SDK's
//! `connect_or_start` reads `RMUX_SDK_DAEMON_BINARY` to locate the
//! daemon binary; `RmuxBackend::new` sets that env var to
//! `std::env::current_exe()` so the SDK spawns ccteam itself rather
//! than a separate `rmux` artifact.
//!
//! Surface:
//!
//! - `spawn` / `exists` / `send_text` / `send_enter` / `capture` /
//!   `pane_dims` / `pane_pid` / `list_pane_pids` / `resize` / `kill` /
//!   `list_sessions` — all wired through SDK primitives.
//! - `subscribe` — drives the SDK `pane.output_stream()` (raw bytes,
//!   live tail): each `PaneOutputChunk::Bytes` becomes one
//!   **byte-verbatim** `MuxEvent::OutputChunk` plus a
//!   `MuxEvent::PatternMatched` per registered-regex hit (the bytes are
//!   also buffered into completed lines for the matcher, exactly as the
//!   `TmuxBackend` does); a `PaneOutputChunk::Lag` becomes
//!   `MuxEvent::OutputDropped`. No FIFO machinery — the daemon owns the
//!   broadcast.
//! - `capture` — best-effort raw-byte drain of the daemon's retained
//!   output backlog via `output_stream_starting_at(Oldest)` +
//!   `poll_once`. `with_ansi` is honored: `true` → raw byte-faithful
//!   ANSI (web terminal / snapshot / screenshot depend on it); `false`
//!   → the drained bytes rendered through a vt100 state machine to
//!   plain text (so `peek`/CLI never leak control sequences to the
//!   user's terminal). NOT the rendered `snapshot()` grid.
//! - `register_pattern` — compiles + stores into a shared
//!   [`crate::patterns::PatternMatcher`] per session; `subscribe`
//!   snapshots it (same type the TmuxBackend uses).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream;
use rmux_sdk::PaneOutputStream;
use tokio::sync::Mutex;

use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, EnsureSession, EnsureSessionPolicy, PaneInfo,
    PaneOutputChunk, PaneOutputStart, PaneProcessState, ProcessSpec, Rmux, RmuxEndpoint, RmuxError,
    SessionName, TerminalSizeSpec,
};

use crate::patterns::{PatternMatcher, PatternVendor};
use crate::{
    BackendKind, MuxEvent, MuxEventStream, MuxSessionId, MuxSessionSpec, PaneBackend,
    ProcessBackend,
};

const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Loose per-line byte estimate used to turn `capture`'s `lines` cap into
/// an approximate byte budget. Generous (raw ANSI lines run wider than
/// their visible columns); the cap is inexact by contract.
const BACKLOG_BYTES_PER_LINE: usize = 512;

/// Upper bound on `poll_once` round trips when draining the retained
/// backlog in `capture`. Each poll pulls up to one daemon batch (256
/// events); this guards against a daemon that trickles a huge scrollback
/// across many tiny batches from spinning unbounded. The normal exit is
/// an empty batch (backlog exhausted) or hitting the byte budget.
const BACKLOG_DRAIN_MAX_POLLS: usize = 64;

/// Fallback grid width (columns) for `capture(with_ansi=false)` when the
/// live pane dims can't be queried. Wide enough that a typical TUI's
/// content isn't truncated when re-rendered to plain text.
const CAPTURE_PLAIN_FALLBACK_COLS: u16 = 200;

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

/// State threaded through `subscribe`'s `unfold` over the SDK raw-byte
/// output stream. `line_buffer` accumulates bytes across chunks so the
/// matcher runs on completed lines (split on `\n`); `pending` holds the
/// `PatternMatched` events derived from the most recent chunk, yielded
/// after its (byte-verbatim) `OutputChunk`.
struct RmuxStreamState {
    output_stream: PaneOutputStream,
    line_buffer: Vec<u8>,
    matcher: Arc<PatternMatcher>,
    pending: VecDeque<MuxEvent>,
}

/// Append `bytes` to `line_buffer`, splitting on `\n`. For each completed
/// line, run the matcher and push a `PatternMatched` for each hit onto
/// `out`. Partial trailing bytes stay buffered for the next chunk. This
/// is the rmux mirror of `tmux_backend::subscribe::drain_lines_into` —
/// the raw OutputChunk bytes are forwarded verbatim by the caller; this
/// only derives the (dormant-by-default) line-level pattern matches.
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

/// `RmuxBackend` — ccteam's ProcessBackend impl over rmux-sdk 0.5.
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
        // The returned `PaneOutputStream` owns a cloned transport, so it
        // outlives the `Rmux` handle and the lock — safe to construct
        // inside `call` and return past the closure. `output_stream()`
        // anchors at `PaneOutputStart::Now` (live tail) and delivers raw
        // pane bytes verbatim (no `from_utf8_lossy`, no line reassembly).
        let output_stream = self
            .call(&label, |rmux| {
                let name = name.clone();
                async move {
                    let session = rmux.session(name).await?;
                    session.pane(0, 0).output_stream().await
                }
            })
            .await?;

        // unfold over (output_stream, line_buffer, matcher, pending).
        // Mirrors `tmux_backend::subscribe`: one raw `Bytes` chunk yields
        // 1 byte-verbatim OutputChunk + N PatternMatched (the chunk's
        // bytes are buffered into completed lines for the matcher). The
        // OutputChunk is emitted FIRST. `pending` holds the extras between
        // `next` calls. No FIFO/refcount — the daemon owns the broadcast
        // and the stream's own drop guard unsubs.
        let state = RmuxStreamState {
            output_stream,
            line_buffer: Vec::new(),
            matcher,
            pending: VecDeque::new(),
        };
        let s = stream::unfold(state, |mut st| async move {
            loop {
                if let Some(ev) = st.pending.pop_front() {
                    return Some((ev, st));
                }
                match st.output_stream.next().await {
                    Ok(Some(PaneOutputChunk::Bytes { bytes, .. })) => {
                        // Feed the raw bytes into the line buffer + matcher
                        // (queues N PatternMatched into `pending`), then
                        // emit the chunk VERBATIM first by pushing it to
                        // the front. `loop` → `pop_front` yields it first,
                        // the pattern hits after — exactly the tmux path.
                        drain_lines_into(&mut st.line_buffer, &bytes, &st.matcher, &mut st.pending);
                        st.pending.push_front(MuxEvent::OutputChunk(bytes));
                        // loop → pop_front yields the OutputChunk first.
                    }
                    Ok(Some(PaneOutputChunk::Lag(notice))) => {
                        // Drop the partial-line buffer: the byte stream is
                        // discontinuous after a lag (next bytes may not
                        // begin on a line boundary), so concatenating would
                        // synthesize a bogus line. Mirrors the SDK line
                        // stream + tmux lag handling.
                        st.line_buffer.clear();
                        return Some((
                            MuxEvent::OutputDropped {
                                behind: notice.missed_events,
                            },
                            st,
                        ));
                    }
                    Ok(Some(_)) => {
                        // PaneOutputChunk is #[non_exhaustive]; a future
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
    /// Capture is a **best-effort raw-byte drain** of the daemon's
    /// retained output backlog — NOT the rendered `snapshot()` grid.
    ///
    /// We open a fresh `output_stream_starting_at(Oldest)` (which replays
    /// the daemon's retained byte backlog, then would tail live) and drain
    /// only the *retained backlog* via `poll_once()`: each `poll_once` is
    /// exactly one cursor round trip that never sleeps, so it returns the
    /// immediately-available batch and we stop as soon as a batch comes
    /// back empty (backlog exhausted → we'd be blocking on live tail).
    ///
    /// `with_ansi` is **honored** (it gates the drained bytes' shape):
    /// - `true` → the concatenated bytes are returned **raw** (full,
    ///   byte-faithful ANSI). The web terminal / pane-snapshot /
    ///   screenshot callers all pass `true` and depend on this.
    /// - `false` → the raw bytes are rendered through a vt100 state
    ///   machine ([`ansi_bytes_to_plain_text`]) to **plain text** with
    ///   every control sequence (mouse-tracking, alt-screen, color,
    ///   cursor) consumed — matching the tmux path's rendered-then-
    ///   stripped semantics, so `peek`/CLI never dump control sequences
    ///   into the user's terminal (which would leave it in
    ///   mouse-reporting mode → garbage on scroll).
    ///
    /// `lines` is now an *approximate* cap on how much backlog to return
    /// (converted to a loose byte budget); it is intentionally inexact.
    /// `PaneSnapshot` has no raw-bytes accessor (it is a parsed cell
    /// grid), so the backlog stream is the only byte-faithful source.
    async fn capture(&self, id: &MuxSessionId, lines: usize, with_ansi: bool) -> Result<Vec<u8>> {
        let name = self.session_name(id).await?;
        let label = format!("capture `{}`", id.0);
        // Loose byte budget derived from the line cap. Pane lines are
        // ~80-200 cols; with ANSI escapes a generous per-line estimate
        // keeps a full screen of backlog while bounding pathological
        // scrollback. Capping is approximate by contract.
        let byte_budget = lines.saturating_mul(BACKLOG_BYTES_PER_LINE).max(1);
        let raw = self
            .call(&label, |rmux| {
                let name = name.clone();
                async move {
                    let session = rmux.session(name).await?;
                    let mut stream = session
                        .pane(0, 0)
                        .output_stream_starting_at(PaneOutputStart::Oldest)
                        .await?;
                    let mut out: Vec<u8> = Vec::new();
                    // Bounded drain: collect the retained backlog batches.
                    // `poll_once` never blocks on live output — an empty
                    // batch means the backlog is exhausted (we'd otherwise be
                    // tailing). The iteration guard caps work if the daemon
                    // trickles the backlog across many small batches.
                    for _ in 0..BACKLOG_DRAIN_MAX_POLLS {
                        let batch = stream.poll_once().await?;
                        if batch.is_empty() {
                            break;
                        }
                        for chunk in batch {
                            if let PaneOutputChunk::Bytes { bytes, .. } = chunk {
                                out.extend_from_slice(&bytes);
                            }
                            // Lag chunks carry no replayable payload here;
                            // skip them (a backlog drain races no live tail).
                        }
                        if out.len() >= byte_budget {
                            break;
                        }
                    }
                    // Keep the most recent `byte_budget` bytes (the tail is
                    // what a terminal seed wants). The drop guard on `stream`
                    // unsubscribes on drop here.
                    if out.len() > byte_budget {
                        let start = out.len() - byte_budget;
                        out.drain(..start);
                    }
                    Ok(out)
                }
            })
            .await?;

        if with_ansi {
            // Byte-faithful path: return the raw backlog UNCHANGED. The web
            // terminal / pane-snapshot / screenshot callers depend on this.
            return Ok(raw);
        }

        // Plain-text path (peek/CLI): render the raw backlog through a
        // vt100 state machine so EVERY control sequence (mouse-tracking,
        // alt-screen, color, cursor) is consumed and none can leak into
        // the user's terminal. Size the grid from the live pane dims when
        // available; otherwise fall back to a sane wide default with the
        // row count clamped from the `lines` cap.
        let (rows, cols) = match self.pane_dims(id).await {
            Ok(Some((r, c))) if r > 0 && c > 0 => (r, c),
            _ => (lines.clamp(1, 1000) as u16, CAPTURE_PLAIN_FALLBACK_COLS),
        };
        Ok(ansi_bytes_to_plain_text(&raw, cols, rows))
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

/// Render a raw terminal byte stream (full ANSI) to **plain text** by
/// feeding it through a vt100 state machine and reading back the rendered
/// screen contents.
///
/// This is the seam that lets `capture(with_ansi=false)` return stripped
/// text on the rmux backend (matching the tmux path's rendered-then-
/// stripped semantics). `vt100` interprets and *consumes* every control
/// sequence — mouse-tracking enable (`\x1b[?1000h` / `?1006h`),
/// alt-screen (`\x1b[?1049h`), bracketed paste (`\x1b[?2004h`), SGR color,
/// cursor moves — so none can leak through into the returned bytes. This
/// is exactly why `peek` must use it: dumping the raw backlog instead
/// would leave the user's terminal in mouse-reporting mode.
///
/// `cols`/`rows` size the parser grid (clamped to ≥1). Trailing blank
/// lines from the rendered grid are trimmed so the output isn't padded
/// with empty rows. Pure + side-effect free → unit-testable without a
/// live daemon.
fn ansi_bytes_to_plain_text(raw: &[u8], cols: u16, rows: u16) -> Vec<u8> {
    let cols = cols.max(1);
    let rows = rows.max(1);
    // scrollback = 0: we render only the current screen grid (the tail),
    // mirroring screenshot.rs's `Parser::new(rows, cols, 0)`.
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(raw);
    // `Screen::contents` returns the rendered grid as plain text (one
    // `\n` per row, no trailing newline), with all escape sequences
    // already consumed by the parser.
    let mut text = parser.screen().contents();
    // Trim trailing blank lines so peek output isn't padded with the
    // empty rows of the fixed-height grid.
    let trimmed_len = text.trim_end_matches(['\n', ' ', '\t']).len();
    text.truncate(trimmed_len);
    text.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::{PatternMatcher, PatternVendor};
    use std::io;

    /// `capture(with_ansi=false)`'s plain-text seam must STRIP every
    /// control sequence — most importantly the inner TUI's
    /// mouse-tracking-enable + alt-screen + bracketed-paste, whose raw
    /// leak into the user's terminal was the v0.8.9 peek regression
    /// ("乱码" on scroll). vt100 consumes them all; only visible text
    /// survives, and not a single `0x1b` (ESC) byte remains.
    #[test]
    fn ansi_bytes_to_plain_text_strips_all_control_sequences() {
        // mouse-enable (1000/1006), alt-screen (1049), SGR red (31),
        // bracketed-paste (2004), interleaved with visible text.
        let raw = b"\x1b[?1000h\x1b[?1006h\x1b[?1049h\x1b[31mhello\r\n\x1b[?2004hworld\x1b[0m";
        let out = ansi_bytes_to_plain_text(raw, 80, 24);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("hello"), "visible text `hello` survives: {s:?}");
        assert!(s.contains("world"), "visible text `world` survives: {s:?}");
        // No ESC byte may leak (mouse-reporting mode never gets enabled).
        assert!(
            !out.contains(&0x1b),
            "no 0x1b (ESC) byte may leak through: {out:?}"
        );
        // The mouse-tracking enable in particular must be gone.
        assert!(
            !s.contains("[?1000"),
            "mouse-tracking-enable must not leak: {s:?}"
        );
        assert!(!s.contains("[?1049"), "alt-screen must not leak: {s:?}");
    }

    /// Plain visible text round-trips: no escapes to consume, the text
    /// comes back intact (modulo grid trimming).
    #[test]
    fn ansi_bytes_to_plain_text_roundtrips_plain_text() {
        let out = ansi_bytes_to_plain_text(b"hello\r\nworld", 80, 24);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("hello"), "plain `hello` round-trips: {s:?}");
        assert!(s.contains("world"), "plain `world` round-trips: {s:?}");
        assert!(!out.contains(&0x1b));
        // Trailing blank grid rows are trimmed (no padding to row 24).
        assert!(!s.ends_with('\n'), "trailing blank lines trimmed: {s:?}");
    }

    fn collect_pattern_ids(out: &VecDeque<MuxEvent>) -> Vec<String> {
        out.iter()
            .filter_map(|e| match e {
                MuxEvent::PatternMatched { regex_id, .. } => Some(regex_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// A completed line runs the matcher; no partial bytes are left.
    /// Mirrors the tmux backend's `drain_lines_into` contract so rmux's
    /// (now byte-faithful) subscribe still derives line-level pattern
    /// matches identically.
    #[test]
    fn drain_lines_runs_matcher_on_complete_line() {
        let m = PatternMatcher::base(PatternVendor::Claude);
        let mut buf = Vec::new();
        let mut out = VecDeque::new();
        // `\xe2\x97\x8f` is the UTF-8 for `●`.
        drain_lines_into(&mut buf, b"\xe2\x97\x8f Read(/foo)\n", &m, &mut out);
        assert!(collect_pattern_ids(&out).contains(&"tool_call_started".to_string()));
        assert!(buf.is_empty(), "no partial bytes left after a full line");
    }

    /// Bytes without a trailing `\n` stay buffered across chunks; the
    /// match fires only once the newline arrives.
    #[test]
    fn drain_lines_buffers_partial_until_newline() {
        let m = PatternMatcher::base(PatternVendor::Claude);
        let mut buf = Vec::new();
        let mut out = VecDeque::new();
        drain_lines_into(&mut buf, b"> implement ", &m, &mut out);
        assert!(out.is_empty());
        assert!(!buf.is_empty());
        drain_lines_into(&mut buf, b"login\n", &m, &mut out);
        assert!(collect_pattern_ids(&out).contains(&"user_prompt_submit".to_string()));
        assert!(buf.is_empty());
    }

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
