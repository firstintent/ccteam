//! ccteam-harness — unified mux abstraction for mode 1 / 2 / 3a / 3b child
//! supervision.
//!
//! V0.8 W1 lands the `ProcessBackend` async trait + two impls:
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

pub mod adapter;
pub mod daemon;
pub mod enriched_event;
pub mod execution;
pub mod hook_sink;
pub mod inproc_backend;
pub mod patterns;
pub mod rmux_backend;
pub mod tmux_backend;
pub mod tmux_ops;
pub mod typed_event_tap;
pub mod vendor_compat;

pub use adapter::{
    ccteam_root_from_env, format_tokens, parse_backgrounded_short_id, parse_cc_state_json,
    parse_pid_from_state, pluck, pluck_f64, pluck_pct, pluck_str, sigkill_pid, sigterm_pid,
    state_json_path, AccountUsage, AgentSpecBrief, AgentVendor, ApprovalIR, ApprovalKind,
    ApprovalRisk, ApprovalScope, CanonicalEvent, ChoiceOption, ChoicePrompt, ChoiceSelection,
    ContextUsage, Directive, DirectiveOutcome, ExecutionMode, GoalStatus, HarnessAdapter,
    HarnessError, HarnessSnapshot, PermissionMode, RunningTask, SessionHandle, SessionProtocol,
    SpawnCtx, SpawnOpts, SubagentState, ThreadErrorEvent, ThreadEvent, ThreadHandle, ThreadItem,
    ThreadItemDetails, ThreadStatus, TurnId, TurnInput, UnifiedTokenUsage, CCTEAM_HOME_ENV,
    CLAUDE_BIN_ENV, CLAUDE_JOBS_DIR_ENV, CODEX_BIN_ENV, CODEX_STATUS_MARKER,
    CODEX_STATUS_TAIL_LINES, DEFAULT_CLAUDE_SID,
};
pub use enriched_event::{
    enrichment_source, BaseEvent, BasePayload, EnrichedEvent, EnrichmentEvent, EnrichmentPayload,
    EnrichmentSource, EventKind, EventMerger, MergeOutcome, Vendor, DEFAULT_GRACE,
};
pub use execution::claude_stream_json::{persisted_session_model, ClaudeStreamJsonAdapter};
pub use execution::claude_tui::{chat_session_name, parse_chat_session_name, CHAT_SESSION_PREFIX};
pub use execution::codex_exec::codex_chat_session_name;
pub use execution::ClaudeBgAdapter;
pub use hook_sink::{default_ccteam_hook_socket_path, HookEvent, HookSink, HookSinkClient};
pub use inproc_backend::InProcBackend;
pub use rmux_backend::{default_ccteam_harness_socket_path, RmuxBackend};
pub use tmux_backend::TmuxBackend;
pub use typed_event_tap::{event_kind_for_regex_id, RawEnrichment, TapHandle, TypedEventTap};
pub use vendor_compat::warn_unknown_vendor_token;

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

/// Specification for `ProcessBackend::spawn`. Maps 1:1 onto
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
    /// registry ([`crate::patterns`] — `claude.rs` lands W2b; codex
    /// follows the Codex event catalog).
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
pub trait ProcessBackend: Send + Sync {
    /// Idempotent create-or-error spawn. Returns the session id.
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId>;

    /// True iff a session with this name exists right now.
    async fn exists(&self, id: &MuxSessionId) -> Result<bool>;

    /// True iff session exists AND its child PID is alive (defends
    /// against tmux stale-session-with-dead-pane state; for rmux this
    /// is daemon-tracked).
    ///
    /// Default impl composes `exists` + the OS-level `pid_is_alive`
    /// when a caller has a spawn-time pid. Backends with terminal pane
    /// semantics can override this with stronger live-child checks.
    async fn is_alive(&self, id: &MuxSessionId, expected_pid: Option<i32>) -> Result<bool> {
        if !self.exists(id).await? {
            return Ok(false);
        }
        match expected_pid {
            None => Ok(true),
            Some(pid) => Ok(tmux_ops::pid_is_alive(pid)),
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

    /// Send an Escape keystroke to cancel a TUI picker/modal (v0.8.5 D5
    /// `/esc` escape hatch). The ESC control byte (`0x1b`) delivered as
    /// literal input *is* Escape, so the default routes through
    /// [`Self::send_text`] — write-only, no pane scrape. Backends without a
    /// key channel (in-proc / mock) inherit a harmless ESC-as-text write.
    async fn send_escape(&self, id: &MuxSessionId) -> Result<()> {
        self.send_text(id, "\u{1b}").await
    }

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

    /// Which concrete backend this is. Lets callers introspect what
    /// [`default_backend`] / [`from_env`] actually returned (the trait
    /// object otherwise erases the impl type).
    fn backend_kind(&self) -> BackendKind;
}

/// Pane operations exposed only by process backends that host
/// interactive terminal sessions. This keeps [`ProcessBackend`] focused
/// on portable lifecycle + IO while preserving the current tmux/rmux
/// terminal feature surface for web snapshots, pty resize, and tests.
#[async_trait::async_trait]
pub trait PaneBackend: ProcessBackend {
    /// Capture the last N lines of pane output.
    /// `with_ansi=true` preserves escape sequences when the backend can
    /// provide them. Returns bytes; callers choose the display encoding.
    async fn capture(&self, id: &MuxSessionId, lines: usize, with_ansi: bool) -> Result<Vec<u8>>;

    /// Query pane dimensions `(rows, cols)`. `None` when the session is
    /// missing or the query fails.
    async fn pane_dims(&self, id: &MuxSessionId) -> Result<Option<(u16, u16)>>;

    /// Query the active pane's leader PID. Distinct from any spawn-time
    /// process handle because terminal backends can respawn internally.
    async fn pane_pid(&self, id: &MuxSessionId) -> Result<Option<i32>>;

    /// List the PIDs of every pane/process in this session.
    async fn list_pane_pids(&self, id: &MuxSessionId) -> Result<Vec<u32>>;

    /// Resize the pane geometry.
    async fn resize(&self, id: &MuxSessionId, cols: u16, rows: u16) -> Result<()>;
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

/// Resolve the configured [`BackendKind`] from `CCTEAM_MUX_BACKEND`
/// (defaults to `rmux`) WITHOUT constructing a backend.
///
/// Sync, cheap, and side-effect free — meant for sync CLI sites that
/// branch interactive terminal handover (`ccteam attach`) on the
/// backend without paying the cost of instantiating a backend (and,
/// for rmux, without lazily connecting a daemon). Only explicit `tmux`
/// and `inproc-test` opt out — everything else (unset, empty, or an
/// unknown/typo'd value) resolves to `Rmux`, the bundled
/// always-available backend. Callers that need a hard error on a typo
/// go through [`process_from_env`] or [`terminal_from_env`].
pub fn backend_kind_from_env() -> BackendKind {
    match std::env::var("CCTEAM_MUX_BACKEND").as_deref() {
        Ok("tmux") => BackendKind::Tmux,
        Ok("inproc-test") => BackendKind::InProc,
        _ => BackendKind::Rmux,
    }
}

/// Pick any process backend from the `CCTEAM_MUX_BACKEND` env var
/// (defaults to `rmux`, the bundled always-available backend). Explicit
/// `tmux` and `inproc-test` opt out; unset/empty falls through to rmux.
/// Returns `Arc<dyn ProcessBackend>`
/// so the value can be cloned freely through call chains; do NOT cache
/// as a process-wide singleton (per-test instantiation keeps mock impls
/// test-isolated). Unlike the infallible [`default_process_backend`],
/// this errors on an unknown/typo'd value so fallible callers surface
/// the mistake.
pub fn process_from_env() -> Result<Arc<dyn ProcessBackend>> {
    match std::env::var("CCTEAM_MUX_BACKEND").as_deref() {
        Ok("tmux") => Ok(Arc::new(TmuxBackend::new())),
        Ok("inproc-test") => Ok(Arc::new(InProcBackend::new())),
        Ok("rmux") | Ok("") | Err(_) => Ok(Arc::new(RmuxBackend::new())),
        Ok(other) => Err(anyhow!(
            "CCTEAM_MUX_BACKEND=`{other}` is unknown (expected tmux / rmux / inproc-test)"
        )),
    }
}

/// Pick a pane-capable backend from the `CCTEAM_MUX_BACKEND` env var.
/// In-proc tasks do not own a terminal pane, so `inproc-test` is
/// rejected here; callers that only need lifecycle + stdin/stdout
/// operations should use [`process_from_env`].
pub fn terminal_from_env() -> Result<Arc<dyn PaneBackend>> {
    match std::env::var("CCTEAM_MUX_BACKEND").as_deref() {
        Ok("tmux") => Ok(Arc::new(TmuxBackend::new())),
        Ok("inproc-test") => Err(anyhow!(
            "CCTEAM_MUX_BACKEND=`inproc-test` does not support terminal pane operations"
        )),
        Ok("rmux") | Ok("") | Err(_) => Ok(Arc::new(RmuxBackend::new())),
        Ok(other) => Err(anyhow!(
            "CCTEAM_MUX_BACKEND=`{other}` is unknown (expected tmux / rmux for pane operations)"
        )),
    }
}

/// Back-compat pane backend selector. New code that only needs generic
/// lifecycle/IO should use [`process_from_env`]; new code that needs
/// capture/dimensions/resize should use [`terminal_from_env`] or
/// [`default_backend`] explicitly.
pub fn from_env() -> Result<Arc<dyn PaneBackend>> {
    terminal_from_env()
}

/// Production call sites' generic backend selector. Honors
/// `CCTEAM_MUX_BACKEND` exactly like [`process_from_env`], but is
/// infallible: an unknown/garbage env value degrades to `RmuxBackend`
/// rather than erroring, so a config typo lands on the bundled
/// always-available backend instead of crashing a live agent.
pub fn default_process_backend() -> Arc<dyn ProcessBackend> {
    process_from_env().unwrap_or_else(|_| Arc::new(RmuxBackend::new()))
}

/// Production call sites' pane backend selector. Honors
/// `CCTEAM_MUX_BACKEND` exactly like [`terminal_from_env`], but is
/// infallible: an unknown/garbage env value degrades to `RmuxBackend`
/// rather than erroring, so a config typo lands on the bundled
/// always-available backend instead of crashing a live agent (or on a
/// possibly-absent tmux). `inproc-test` also degrades to rmux because
/// it has no pane to capture or resize.
///
/// V0.8 default: env-unset (and empty, and typo) resolves to `rmux` —
/// rmux is the bundled mux so ccteam works with no external tmux. An
/// operator opts out of rmux only with an explicit `CCTEAM_MUX_BACKEND=tmux`.
/// **Do not cache** — instantiate at the call site (or thread it through
/// from `main` / daemon startup).
pub fn default_backend() -> Arc<dyn PaneBackend> {
    terminal_from_env().unwrap_or_else(|_| Arc::new(RmuxBackend::new()))
}

/// Read-only control-plane enumeration of live chat-mode bot sessions
/// (`ccteam-chat-<slug>-<role>`) hosted by `backend`. Lists session *names*
/// via [`ProcessBackend::list_sessions`] — no pane scraping. The gateway uses
/// this to reconcile its tracked sessions against processes that outlived a
/// previous daemon (orphans).
pub async fn list_chat_sessions(backend: &dyn ProcessBackend) -> Result<Vec<String>> {
    Ok(backend
        .list_sessions()
        .await?
        .into_iter()
        .map(|id| id.0)
        .filter(|name| name.starts_with(CHAT_SESSION_PREFIX))
        .collect())
}

#[cfg(test)]
mod chat_session_enum_tests {
    use super::*;
    use std::path::PathBuf;

    fn spec(name: &str) -> MuxSessionSpec {
        MuxSessionSpec::new(name, vec!["true".into()], PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn list_chat_sessions_filters_to_chat_mode_names() {
        let backend = InProcBackend::new();
        backend
            .spawn(spec(&chat_session_name("dev-foo", "alice")))
            .await
            .unwrap();
        backend
            .spawn(spec(&chat_session_name("ghost-proj", "zombie")))
            .await
            .unwrap();
        // A non-chat tmux session must be filtered out.
        backend.spawn(spec("some-other-tmux")).await.unwrap();

        let mut chat = list_chat_sessions(&backend).await.unwrap();
        chat.sort();
        assert_eq!(
            chat,
            vec![
                "ccteam-chat-dev-foo-alice".to_string(),
                "ccteam-chat-ghost-proj-zombie".to_string(),
            ]
        );
    }

    /// v0.8.8 B1 — exercises the exact composition the CLI's
    /// `stop_project_chat_sessions` runs against the mux backend (so it works
    /// under the default `rmux`, not just shell `tmux`): enumerate via
    /// [`list_chat_sessions`], filter to a slug via [`parse_chat_session_name`]
    /// (dash-aware — the slug is the *first* parsed element), [`kill`] each
    /// match, then confirm the slug's sessions are gone while a dash-prefix
    /// sibling project and a non-chat session survive. Driven on a single
    /// [`InProcBackend`] within one runtime so spawn→list→kill stays coherent.
    #[tokio::test]
    async fn kill_chat_sessions_for_slug_leaves_other_slugs_and_non_chat() {
        let backend = InProcBackend::new();
        // `dev-foo` is ours; `dev` is a dash-PREFIX sibling slug that a naive
        // `starts_with("ccteam-chat-dev")` would wrongly match.
        let ours_a = chat_session_name("dev-foo", "alice");
        let ours_b = chat_session_name("dev-foo", "bob");
        let sibling = chat_session_name("dev", "carol"); // slug == `dev`, NOT ours
        let non_chat = "plain-session".to_string();
        for name in [&ours_a, &ours_b, &sibling, &non_chat] {
            backend.spawn(spec(name)).await.unwrap();
        }

        // Enumerate + filter to our slug exactly as the CLI does.
        let live = list_chat_sessions(&backend).await.unwrap();
        let mut matches: Vec<String> = live
            .into_iter()
            .filter(|name| {
                parse_chat_session_name(name)
                    .map(|(s, _last)| s == "dev-foo")
                    .unwrap_or(false)
            })
            .collect();
        matches.sort();
        assert_eq!(
            matches,
            vec![ours_a.clone(), ours_b.clone()],
            "filter must match only `dev-foo`, not the `dev` sibling or non-chat",
        );

        // Kill each match (idempotent).
        for name in &matches {
            backend
                .kill(&MuxSessionId::new(name.clone()))
                .await
                .unwrap();
        }

        // Ours are absent; sibling + non-chat survive.
        for name in [&ours_a, &ours_b] {
            assert!(
                !backend
                    .exists(&MuxSessionId::new(name.clone()))
                    .await
                    .unwrap(),
                "`{name}` must be absent after kill",
            );
        }
        for name in [&sibling, &non_chat] {
            assert!(
                backend
                    .exists(&MuxSessionId::new(name.clone()))
                    .await
                    .unwrap(),
                "`{name}` (not our slug) must survive",
            );
        }

        // Re-kill is a no-op (kill is idempotent — the stop red line relies on
        // this so a repeat `project stop` never errors).
        for name in [&ours_a, &ours_b] {
            backend
                .kill(&MuxSessionId::new(name.clone()))
                .await
                .unwrap();
        }
    }
}
