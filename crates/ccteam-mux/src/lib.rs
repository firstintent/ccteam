//! ccteam-mux — unified mux abstraction for mode 1 / 2 / 3a / 3b child
//! supervision.
//!
//! V0.8 W1 lands the `MuxBackend` async trait + two impls:
//!
//! - [`TmuxBackend`] — thin async facade over the existing `tmux` CLI
//!   primitives (which now live in [`tmux_ops`] inside this crate, with
//!   `ccteam-core::tmux` re-exporting them for back-compat). This is
//!   the V0.8 default; preserves V0.6.x behavior 1:1.
//! - [`InProcBackend`] — mode-1 stub that drives a `tokio::task` and
//!   exposes the same trait surface. Most ops return
//!   [`MuxError::NotApplicable`] / no-op `Ok(())`; useful for tests and
//!   the eventual mode-1 unification.
//!
//! V0.8 W2 will add `RmuxBackend` (wraps `rmux-sdk`); V0.9 retires
//! `tmux_ops` once W2 has burned in.
//!
//! See `docs/versions/v0-8-rmux/w1-mux-backend-trait-draft.md` for the
//! detailed trait surface + the 10 audit-driven deltas this impl
//! preserves (resize, list_pane_pids, pane_pid distinct from spawn-time
//! pid, Option<dims>, drop-string-capture, interactive-attach-as-argv,
//! is_alive default-method, kill -0 stays OS-level, target-string
//! asymmetry hidden by SessionId opacity, refcount FIFO bookkeeping
//! lives inside subscribe()).

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::Stream;

pub mod daemon;
pub mod inproc_backend;
pub mod rmux_backend;
pub mod tmux_backend;
pub mod tmux_ops;

pub use inproc_backend::InProcBackend;
pub use rmux_backend::{default_ccteam_mux_socket_path, RmuxBackend};
pub use tmux_backend::TmuxBackend;

/// Vendor-agnostic identity for a mux-backed session.
///
/// For `TmuxBackend` this is the bare tmux session name (the
/// canonical, base-index-safe target — see audit §4-B). For
/// `RmuxBackend` (W2) this becomes opaque, hiding the
/// `<session>:0.0` vs bare-name asymmetry that the tmux CLI surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MuxSessionId(pub String);

impl MuxSessionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MuxSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle category — determines daemon supervision policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxSessionKind {
    /// mode 2 bg — exit code is the natural termination signal.
    Ephemeral,
    /// mode 3 chat — long-lived; exit is anomaly; respawn on certain
    /// failures.
    LongLived,
    /// dev-server / daemon child — explicit kill only.
    Daemon,
}

/// Specification for `MuxBackend::spawn`. Maps 1:1 onto
/// `tmux new-session -d -e KEY=VAL... -s <name> -c <wd> -x C -y R <argv>`
/// for the TmuxBackend impl; rmux uses the same fields against
/// `rmux_sdk::EnsureSession` (W2).
#[derive(Debug, Clone)]
pub struct MuxSessionSpec {
    /// Display label and (for tmux) the canonical session name.
    pub name: String,
    /// Command line. argv[0] is the binary; the rest are args.
    pub argv: Vec<String>,
    pub working_dir: PathBuf,
    /// Extra env pairs forwarded into the session (`tmux -e KEY=VAL`
    /// or `rmux ProcessSpec.environment`).
    pub env: Vec<(String, String)>,
    /// PTY size at spawn. Default `(200, 50)` — see tmux_ops::
    /// `TmuxSession::start_with_env` doc for the 1×1-collapse hazard
    /// this defends against under daemon (no controlling TTY) launch.
    pub size: (u16, u16),
    pub kind: MuxSessionKind,
}

impl MuxSessionSpec {
    /// Builder-style ctor with the audit-blessed default `(200, 50)` pane
    /// size + `Ephemeral` kind.
    pub fn new(name: impl Into<String>, argv: Vec<String>, working_dir: PathBuf) -> Self {
        Self {
            name: name.into(),
            argv,
            working_dir,
            env: Vec::new(),
            size: (200, 50),
            kind: MuxSessionKind::Ephemeral,
        }
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    pub fn with_size(mut self, cols: u16, rows: u16) -> Self {
        self.size = (cols, rows);
        self
    }

    pub fn with_kind(mut self, kind: MuxSessionKind) -> Self {
        self.kind = kind;
        self
    }
}

/// Typed event the daemon emits per session.
///
/// `OutputChunk` is the raw bytes; orchestrator NEVER consumes this
/// directly per the "no business-side grep" red line. Higher layers
/// (the `PatternMatched` translator inside the backend impl)
/// subscribe to chunks internally and emit only the higher-level
/// variants outward.
#[derive(Debug, Clone)]
pub enum MuxEvent {
    Started {
        pid: i32,
    },
    /// Raw bytes from the pane stream (post-`pipe-pane` /
    /// post-rmux-output-stream). W1 emits these for web SSE consumers;
    /// orchestrator state-machine paths MUST NOT consume.
    OutputChunk(Vec<u8>),
    /// Backwards-compat for slow subscribers under `broadcast::Lagged`
    /// semantics. Mirrors the `{"type":"lag","behind":N}` web frame.
    OutputDropped {
        behind: u64,
    },
    OutputIdle {
        duration: Duration,
    },
    /// A registered pattern matched. `regex_id` is from the static
    /// registry (`crates/ccteam-core/src/mux/patterns/{claude,codex}.rs`,
    /// landing W2b).
    PatternMatched {
        regex_id: String,
        captured: String,
    },
    ProcessExited {
        code: i32,
    },
    PaneResized {
        cols: u16,
        rows: u16,
    },
    /// Daemon-restart story: emitted when reconnecting to a daemon
    /// that has outlived the orchestrator process. RmuxBackend (W2)
    /// uses this; TmuxBackend never emits.
    DaemonReconnected,
}

pub type MuxEventStream = Pin<Box<dyn Stream<Item = MuxEvent> + Send>>;

/// Backend identity for `from_env` selection and free-fn dispatch
/// (e.g. `interactive_attach_argv`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Tmux,
    Rmux,
    InProc,
}

/// The single abstraction over child-process supervision used by
/// ccteam.
///
/// Implementations:
/// - [`TmuxBackend`] — wraps `tmux` CLI (V0.8 default)
/// - [`InProcBackend`] — mode 1 in-proc tasks (stub for W1)
/// - `RmuxBackend` — lands W2
///
/// Async because rmux's primitives are; the tmux impl bridges to
/// blocking `Command::output()` via `tokio::task::spawn_blocking`
/// when it matters, or runs synchronously inline when the call is
/// cheap.
#[async_trait::async_trait]
pub trait MuxBackend: Send + Sync {
    /// Idempotent create-or-error spawn. Returns the session id.
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId>;

    /// True iff a session with this name exists right now.
    async fn exists(&self, id: &MuxSessionId) -> Result<bool>;

    /// True iff session exists AND its child PID is alive (defends
    /// against tmux stale-session-with-dead-pane state; for rmux this
    /// is daemon-tracked).
    ///
    /// Default impl composes `exists` + `pane_pid` + the OS-level
    /// `pid_is_alive` (the latter stays outside the trait — see audit
    /// delta 8).
    async fn is_alive(&self, id: &MuxSessionId, expected_pid: Option<i32>) -> Result<bool> {
        if !self.exists(id).await? {
            return Ok(false);
        }
        match expected_pid {
            None => Ok(true),
            Some(pid) => {
                if !tmux_ops::pid_is_alive(pid) {
                    return Ok(false);
                }
                match self.pane_pid(id).await? {
                    Some(actual) => Ok(actual == pid),
                    None => Ok(false),
                }
            }
        }
    }

    /// Write raw text to the session's stdin/pty (no trailing Enter).
    async fn send_text(&self, id: &MuxSessionId, text: &str) -> Result<()>;

    /// Send a literal Enter keystroke.
    async fn send_enter(&self, id: &MuxSessionId) -> Result<()>;

    /// Convenience: send_text + send_enter.
    async fn send_line(&self, id: &MuxSessionId, text: &str) -> Result<()> {
        self.send_text(id, text).await?;
        self.send_enter(id).await
    }

    /// Capture the last N lines of pane output.
    /// `with_ansi=true` preserves escape sequences (for vt100
    /// rendering). `with_ansi=false` returns stripped plain text.
    /// Returns bytes — String form was dropped (audit delta 5).
    async fn capture(&self, id: &MuxSessionId, lines: usize, with_ansi: bool) -> Result<Vec<u8>>;

    /// Query pane dimensions `(rows, cols)`. `None` when the session
    /// is missing or the query fails — screenshot fallback to 80×24
    /// needs the None branch (audit delta 4).
    async fn pane_dims(&self, id: &MuxSessionId) -> Result<Option<(u16, u16)>>;

    /// Query the active pane's leader PID. Distinct from any
    /// spawn-time PID handle — internal respawn drifts the live value
    /// (audit delta 3).
    async fn pane_pid(&self, id: &MuxSessionId) -> Result<Option<i32>>;

    /// List the PIDs of every pane in this session. F164 reattach
    /// path + claude_tui resume tests consume this directly even
    /// though rmux may abstract "pane" — "child PIDs in this session"
    /// remains a load-bearing signal (audit delta 2).
    async fn list_pane_pids(&self, id: &MuxSessionId) -> Result<Vec<u32>>;

    /// Resize the pane geometry. Required for `pty_ws::resize_window`
    /// browser xterm.js parity (audit delta 1).
    async fn resize(&self, id: &MuxSessionId, cols: u16, rows: u16) -> Result<()>;

    /// Subscribe to the typed event stream. Stream ends when session
    /// ends. The refcount + FIFO bookkeeping (F56) is internalized
    /// inside the impl (audit delta 10).
    ///
    /// **W1 status**: `TmuxBackend::subscribe` returns an error pointing
    /// to W2. The existing `ccteam-web::pty::PtyRegistry` continues to
    /// own the `pipe-pane` refcount relay for V0.8. W2 ports the
    /// registry into `TmuxBackend` and exposes only the stream.
    async fn subscribe(&self, id: &MuxSessionId) -> Result<MuxEventStream>;

    /// Register a regex pattern for daemon-side matching. Once
    /// matched, emits `MuxEvent::PatternMatched { regex_id }` on the
    /// session's subscribe stream. Idempotent (re-registering same
    /// regex_id replaces the pattern).
    ///
    /// **W1 status**: stub — full implementation lands W2b once
    /// `subscribe` is live.
    async fn register_pattern(
        &self,
        id: &MuxSessionId,
        regex_id: String,
        regex: String,
    ) -> Result<()>;

    /// Idempotent cleanup — Ok(()) if session doesn't exist.
    async fn kill(&self, id: &MuxSessionId) -> Result<()>;

    /// List all live sessions managed by this backend.
    async fn list_sessions(&self) -> Result<Vec<MuxSessionId>>;
}

/// Build argv for an interactive terminal handover (`tmux attach -t
/// <name>` for the tmux backend). The CLI invokes this via blocking
/// `Command::status()` on its own controlling tty — async doesn't fit
/// terminal handover, so this is intentionally NOT a trait method
/// (audit delta 6).
pub fn interactive_attach_argv(backend: BackendKind, session_name: &str) -> Vec<String> {
    match backend {
        BackendKind::Tmux => vec![
            "tmux".to_string(),
            "attach".to_string(),
            "-t".to_string(),
            session_name.to_string(),
        ],
        BackendKind::Rmux => vec![
            // V0.8 W2 placeholder. RmuxBackend's interactive client
            // CLI shape is verified in W3 — until then this is unused
            // by production callers (`from_env` rejects "rmux").
            "rmux".to_string(),
            "attach".to_string(),
            session_name.to_string(),
        ],
        BackendKind::InProc => {
            // No terminal to attach to for in-proc tasks. Caller
            // should never reach this branch in production; return a
            // shape that fails fast if spawned.
            vec!["false".to_string()]
        }
    }
}

/// Pick a backend from the `CCTEAM_MUX_BACKEND` env var (defaults to
/// `tmux`). Returns `Arc<dyn MuxBackend>` so the value can be cloned
/// freely through call chains; do NOT cache as a process-wide
/// singleton (per-test instantiation keeps mock impls test-isolated
/// when those land in W2b).
pub fn from_env() -> Result<Arc<dyn MuxBackend>> {
    match std::env::var("CCTEAM_MUX_BACKEND").as_deref() {
        Ok("rmux") => Ok(Arc::new(RmuxBackend::new())),
        Ok("inproc-test") => Ok(Arc::new(InProcBackend::new())),
        Ok("tmux") | Ok("") | Err(_) => Ok(Arc::new(TmuxBackend::new())),
        Ok(other) => Err(anyhow!(
            "CCTEAM_MUX_BACKEND=`{other}` is unknown (expected tmux / rmux / inproc-test)"
        )),
    }
}

/// Convenience for production call sites that want the default
/// backend without env override. Equivalent to constructing a fresh
/// `TmuxBackend`. **Do not cache** — instantiate at the call site (or
/// thread it through from `main` / daemon startup).
pub fn default_backend() -> Arc<dyn MuxBackend> {
    Arc::new(TmuxBackend::new())
}
