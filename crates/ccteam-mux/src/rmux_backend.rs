//! `RmuxBackend` — V0.8 W2a implementation of [`MuxBackend`] backed by
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
//! Known gap (W2b followup): [`MuxBackend::capture`]'s `with_ansi=true`
//! cannot be honored from the SDK's `PaneSnapshot` cell grid — ANSI
//! escape bytes are gone after the daemon parses the grid. W2a's impl
//! returns rendered plain-text bytes for both branches and documents
//! the gap; ccteam-web consumers that need raw bytes continue routing
//! through `ccteam-web::pty::PtyRegistry` until W2b ports the registry.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream;
use rmux_sdk::PaneLineStream;
use tokio::sync::{Mutex, OnceCell};

use rmux_sdk::{
    bootstrap::discovery::SDK_DAEMON_BINARY_ENV, EnsureSession, EnsureSessionPolicy, PaneInfo,
    PaneLineItem, PaneProcessState, ProcessSpec, Rmux, RmuxEndpoint, SessionName, TerminalSizeSpec,
};

use crate::patterns::{PatternMatcher, PatternVendor};
use crate::{MuxBackend, MuxEvent, MuxEventStream, MuxSessionId, MuxSessionSpec};

const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Default UDS path for the ccteam-hosted rmux daemon. Resolves to
/// `$HOME/.ccteam/run/mux.sock` on Unix; on Windows callers fall back
/// to the SDK-default named pipe.
///
/// The parent directory is created on first use (mode 0700 on Unix).
pub fn default_ccteam_mux_socket_path() -> PathBuf {
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

/// `RmuxBackend` — ccteam's MuxBackend impl over rmux-sdk 0.3.
///
/// The SDK `Rmux` handle is lazily initialized on first use through a
/// [`OnceCell`] so the daemon spawn cost (~50-200ms) is paid only when
/// the backend is actually used. Once connected, all subsequent calls
/// reuse the same transport.
pub struct RmuxBackend {
    rmux: OnceCell<Rmux>,
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
        Self::with_socket_path(default_ccteam_mux_socket_path())
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
            rmux: OnceCell::new(),
            socket_path,
            pattern_registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Lazily connect (or start) the daemon and cache the SDK handle.
    async fn rmux(&self) -> Result<&Rmux> {
        self.rmux
            .get_or_try_init(|| async {
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
                Ok::<Rmux, anyhow::Error>(rmux)
            })
            .await
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
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend: session lookup `{}`: {e}", id.0))?;
        let pane = session.pane(0, 0);
        let snapshot = pane
            .info()
            .await
            .map_err(|e| anyhow!("RmuxBackend: pane info `{}`: {e}", id.0))?;
        Ok(snapshot.panes.into_iter().next())
    }
}

#[async_trait]
impl MuxBackend for RmuxBackend {
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId> {
        let rmux = self.rmux().await?;
        let session_name = SessionName::new(spec.name.clone()).map_err(|e| {
            anyhow!(
                "RmuxBackend::spawn invalid session name `{}`: {e}",
                spec.name
            )
        })?;
        let env_strings: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut process = ProcessSpec::argv(spec.argv.iter().cloned());
        process.environment = Some(env_strings);
        let mut ensure = EnsureSession::named(session_name.clone())
            .policy(EnsureSessionPolicy::CreateOnly)
            .detached(true)
            .size(TerminalSizeSpec::new(spec.size.0, spec.size.1))
            .process(process);
        // working_directory is a String template on the SDK; lossy
        // unicode conversion is fine — paths originate from the
        // operator's environment which is UTF-8 in practice.
        ensure = ensure.working_directory(spec.working_dir.to_string_lossy().into_owned());

        rmux.ensure_session(ensure)
            .await
            .map_err(|e| anyhow!("RmuxBackend::spawn `{}`: {e}", spec.name))?;
        Ok(MuxSessionId::new(spec.name))
    }

    async fn exists(&self, id: &MuxSessionId) -> Result<bool> {
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        rmux.has_session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::exists `{}`: {e}", id.0))
    }

    async fn send_text(&self, id: &MuxSessionId, text: &str) -> Result<()> {
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::send_text session `{}`: {e}", id.0))?;
        session
            .pane(0, 0)
            .send_text(text)
            .await
            .map_err(|e| anyhow!("RmuxBackend::send_text `{}`: {e}", id.0))?;
        Ok(())
    }

    async fn send_enter(&self, id: &MuxSessionId) -> Result<()> {
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::send_enter session `{}`: {e}", id.0))?;
        session
            .pane(0, 0)
            .send_key("Enter")
            .await
            .map_err(|e| anyhow!("RmuxBackend::send_enter `{}`: {e}", id.0))?;
        Ok(())
    }

    async fn capture(&self, id: &MuxSessionId, lines: usize, _with_ansi: bool) -> Result<Vec<u8>> {
        // W2b followup: PaneSnapshot is the parsed grid — ANSI escape
        // bytes are not recoverable from cells. W2a returns the rendered
        // plain-text bytes for both `with_ansi=true` and `false`. Web
        // SSE consumers that need raw bytes continue routing through
        // `ccteam-web::pty::PtyRegistry` until W2b ports the registry.
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::capture session `{}`: {e}", id.0))?;
        let snapshot = session
            .pane(0, 0)
            .snapshot()
            .await
            .map_err(|e| anyhow!("RmuxBackend::capture `{}`: {e}", id.0))?;
        let visible_lines = snapshot.visible_lines();
        // Honor `lines` by taking the last N.
        let take = lines.min(visible_lines.len());
        let start = visible_lines.len().saturating_sub(take);
        let slice = &visible_lines[start..];
        Ok(slice.join("\n").into_bytes())
    }

    async fn pane_dims(&self, id: &MuxSessionId) -> Result<Option<(u16, u16)>> {
        let Some(info) = self.first_pane_info(id).await? else {
            return Ok(None);
        };
        // Trait contract: `(rows, cols)` (per W1 trait doc / TmuxBackend
        // impl — see `query_pane_dims_from_session`).
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
        // ccteam sessions are single-pane today; rmux's "pane" model
        // differs from tmux's so a session-wide pane listing diverges
        // semantically. W2a: return the first pane's pid as a
        // one-element vec. W2b refines if multi-pane callers appear.
        let Some(info) = self.first_pane_info(id).await? else {
            return Ok(Vec::new());
        };
        match info.process {
            PaneProcessState::Running { pid: Some(pid) } => Ok(vec![pid]),
            _ => Ok(Vec::new()),
        }
    }

    async fn resize(&self, id: &MuxSessionId, cols: u16, rows: u16) -> Result<()> {
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::resize session `{}`: {e}", id.0))?;
        session
            .pane(0, 0)
            .resize(TerminalSizeSpec::new(cols, rows))
            .await
            .map_err(|e| anyhow!("RmuxBackend::resize `{}`: {e}", id.0))?;
        Ok(())
    }

    async fn subscribe(&self, id: &MuxSessionId) -> Result<MuxEventStream> {
        // Snapshot the matcher (empty if no patterns registered).
        let matcher = {
            let reg = self.pattern_registry.lock().await;
            reg.get(id)
                .cloned()
                .unwrap_or_else(|| Arc::new(PatternMatcher::new()))
        };
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::subscribe session `{}`: {e}", id.0))?;
        let line_stream = session
            .pane(0, 0)
            .line_stream()
            .await
            .map_err(|e| anyhow!("RmuxBackend::subscribe line_stream `{}`: {e}", id.0))?;

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
                        // Emit the raw line (with the trailing \n the
                        // line stream stripped re-appended for byte
                        // parity with the FIFO path) first.
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
        let rmux = self.rmux().await?;
        let name = self.session_name(id).await?;
        // Look up first to make `kill` idempotent on absent sessions
        // (`session()` uses `ReuseOnly` policy which errors when the
        // session is missing; `has_session` is cheaper than catching
        // that error).
        if !rmux
            .has_session(name.clone())
            .await
            .map_err(|e| anyhow!("RmuxBackend::kill has_session `{}`: {e}", id.0))?
        {
            return Ok(());
        }
        let session = rmux
            .session(name)
            .await
            .map_err(|e| anyhow!("RmuxBackend::kill session `{}`: {e}", id.0))?;
        let _killed = session
            .kill()
            .await
            .map_err(|e| anyhow!("RmuxBackend::kill `{}`: {e}", id.0))?;
        // Both `true` (existed and was killed) and `false` (already
        // gone) map to our `Ok(())` per the trait contract.
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<MuxSessionId>> {
        let rmux = self.rmux().await?;
        let names = rmux
            .list_sessions()
            .await
            .map_err(|e| anyhow!("RmuxBackend::list_sessions: {e}"))?;
        Ok(names
            .into_iter()
            .map(|n| MuxSessionId::new(n.as_str().to_string()))
            .collect())
    }
}
