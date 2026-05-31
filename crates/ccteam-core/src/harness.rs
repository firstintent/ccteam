//! V0.6.0 F107 — `HarnessAdapter` trait migration (Option C).
//!
//! ## What this file owns
//!
//! - The **new** [`HarnessAdapter`] trait (5 async methods + `name` + `vendor`)
//!   aligned with Codex `ThreadManager::{submit, next_event}` protocol.
//! - Cross-vendor types: [`AgentVendor`], [`ExecutionMode`], [`ThreadHandle`],
//!   [`TurnInput`], [`TurnId`], [`ThreadEvent`], [`ThreadItem`],
//!   [`ThreadItemDetails`], [`ThreadErrorEvent`], [`SpawnCtx`].
//! - [`UnifiedTokenUsage`] *stub* (cost-crater will move the canonical
//!   definition into the `ccteam-cost` crate; until that lands, callers
//!   inside this crate import this local stub).
//! - Legacy persistence + helper types still consumed by orchestrator /
//!   web / state.json registry: [`HarnessSnapshot`], [`SubagentState`],
//!   [`SpawnOpts`], [`SessionHandle`] (now an internal data type — no
//!   longer part of the trait surface).
//! - Free fns kept from V0.4.0 because callers outside this file still
//!   read state.json: [`parse_cc_state_json`], [`parse_pid_from_state`],
//!   [`state_json_path`], [`sigterm_pid`], [`sigkill_pid`],
//!   [`parse_backgrounded_short_id`], plus codex marker constants.
//!
//! Adapter implementations live in [`crate::execution`]:
//!
//! - [`crate::execution::claude_bg::ClaudeBgAdapter`] (replaces V0.5.x
//!   `ClaudeCodeAdapter`; zero behaviour change — `start_thread` =
//!   spawn `claude --bg --agent <role>`, `close_thread` = SIGTERM pid
//!   from `state.json`).
//! - [`crate::execution::codex_exec::CodexExecAdapter`] (replaces V0.5.x
//!   `CodexAdapter`; `start_thread` = `tmux new-session -d -- codex`,
//!   `close_thread` = `send-keys q` + `tmux kill-session` fallback;
//!   Wave 3 F112 fills in `codex exec --json` + thread/start UDS).
//! - [`crate::execution::claude_tui::ClaudeTuiAdapter`] (Wave 1 STUB;
//!   Wave 2 F108 fills tmux long-session + send-keys -l + dual-track
//!   transcript polling).
//!
//! ## Red lines (unchanged from V0.5.x)
//!
//! - The snapshot pipeline is **presentation-only**. `progress.jsonl`
//!   remains the single source of truth for state transitions.
//! - `close_thread` is the **only** path that kills a long-running
//!   session, and it must be invoked exclusively from a user-initiated
//!   `ccteam session rm` (F49) — never silently.
//! - `ccteam-core` does not know team-name literals.
//!
//! ## Wave 1 binding contract
//!
//! The trait signature below is **locked**. Wave 2 (F108) + Wave 3
//! (F112) adapters MUST conform to this shape exactly. Changing the
//! signature in a later wave = Wave 1 not converged, rewind.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default sid for projects that haven't opted into multi-session
/// (V0.3 single-session projects).
pub const DEFAULT_CLAUDE_SID: &str = "claude-1";

/// Environment override for the directory under which Claude Code
/// writes per-job `state.json` files. Defaults to `~/.claude/jobs/` when
/// unset. Tests override this to a tempdir.
pub const CLAUDE_JOBS_DIR_ENV: &str = "CCTEAM_CLAUDE_JOBS_DIR";

/// Environment override for the `claude` binary path. Tests set this to
/// a fake script that emits a deterministic `backgrounded · <id>` line on
/// stdout so `start_thread` is hermetic.
pub const CLAUDE_BIN_ENV: &str = "CCTEAM_CLAUDE_BIN";

/// Marker line the codex agent prints in its tmux pane to publish
/// state to the observer (PRD §6.5 + dev-plan §3.2).
pub const CODEX_STATUS_MARKER: &str = "CODEX_STATUS:";

/// Number of trailing pane lines a codex observer is expected to
/// capture before feeding the pane body to the codex status parser.
pub const CODEX_STATUS_TAIL_LINES: usize = 5;

// =====================================================================
// V0.6.0 F107 — New trait surface
// =====================================================================

/// Vendor enum, a first-class trait field (F107 step 1).
///
/// Codex integration (F112 Wave 3) and Claude TUI integration (F108
/// Wave 2) both rely on this carrying through the entire spawn flow so
/// downstream code (pricing, cost roll-ups, UI labels, MCP wire format)
/// can route per-vendor without re-deriving from `name()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentVendor {
    Claude,
    Codex,
}

/// Execution mode classifier. Carried on [`ThreadHandle::mode`] so the
/// orchestrator (and downstream UI) can decide policy without
/// re-deriving from `vendor + adapter name`:
///
/// - `InProc`  — V0.5 `Task` tool / in-process subagent (no adapter,
///   kept here for orthogonality of [`ThreadHandle`]).
/// - `Bg`      — `claude --bg` background job, `codex exec --json`,
///   single-turn fresh-context spawn.
/// - `Chat`    — long-running tmux + claude TUI / `codex app-server`
///   UDS; multi-turn with context reuse (Wave 2 F108).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    InProc,
    Bg,
    Chat,
}

/// Cross-vendor thread handle, returned from
/// [`HarnessAdapter::start_thread`] and consumed by every other trait
/// method. Replaces the V0.5.x [`SessionHandle`] on the adapter surface
/// (legacy `SessionHandle` is still used internally by the orchestrator
/// for state.json persistence + web SSE wire format; orchestrator
/// translates `ThreadHandle ↔ SessionHandle` at the trait boundary).
///
/// `identity` semantics by adapter:
///
/// - [`crate::execution::claude_bg::ClaudeBgAdapter`] → `daemonShort`
///   `job_id` from `claude --bg`'s `backgrounded · <id>` stdout line.
/// - [`crate::execution::claude_tui::ClaudeTuiAdapter`] → tmux session
///   name `ccteam-chat-<slug>-<role>`.
/// - [`crate::execution::codex_exec::CodexExecAdapter`] → tmux session
///   name `ccteam-<slug>-<sid>`.
///
/// `raw_extras` is a free-form JSON bag for vendor-specific data the
/// orchestrator's translation layer may need (e.g. `{"tmux_session":
/// "<...>", "pid": <n>}` for bg / codex; arbitrary for future adapters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadHandle {
    pub vendor: AgentVendor,
    pub mode: ExecutionMode,
    pub identity: String,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub raw_extras: serde_json::Value,
}

/// Per-turn identifier. Adapter-defined shape — `claude --bg` synthesises
/// `bg-<job_id>` (one turn per spawn), TUI / app-server adapters issue
/// monotonically-incrementing per-thread turn ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub String);

impl TurnId {
    pub fn new<S: Into<String>>(id: S) -> Self {
        Self(id.into())
    }
}

/// User-facing turn input variants. Matches Codex `UserInput` shape
/// (`Text` / `Image` / `LocalImage` / `Skill` / `Mention`) but
/// flattened for ccteam's vendor-agnostic surface.
#[derive(Debug, Clone)]
pub enum TurnInput {
    /// Free-form user text (chat DM, group message, or kick prompt).
    UserText(String),
    /// File-system artifact / attachment placed in a known directory
    /// for the agent to read on the next turn.
    Artifact(PathBuf),
    /// `/compact` / `/new` / `/clear` style slash command — adapter
    /// translates to backend-specific operation (Claude → slash cmd
    /// passthrough; Codex → `compact_remote` JSON-RPC).
    SystemDirective(String),
    /// Rich-media image attachment (V0.6 Epic B).
    Image(PathBuf),
    /// External resolver feeding a tool-call result back to the agent.
    ToolResult {
        call_id: String,
        content: serde_json::Value,
    },
}

/// Vendor-agnostic event flowing out of [`HarnessAdapter::events`].
/// Schema mirrors Codex `ThreadEvent` (`exec_events.rs:11-37`) so the
/// orchestrator's translation layer maps 1:1 against Codex emitters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadEvent {
    ThreadStarted {
        thread_id: String,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
        usage: UnifiedTokenUsage,
    },
    TurnFailed {
        turn_id: String,
        err: ThreadErrorEvent,
    },
    ItemStarted {
        item: ThreadItem,
    },
    ItemUpdated {
        item: ThreadItem,
    },
    ItemCompleted {
        item: ThreadItem,
    },
    Error(ThreadErrorEvent),
}

/// Per-turn item the adapter emits (one or more per turn). Mirrors
/// Codex `ThreadItem`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadItem {
    pub id: String,
    pub details: ThreadItemDetails,
}

/// Item payload variants — mirror Codex `ThreadItemDetails`. Default
/// external serde tagging keeps newtype + struct variants
/// compatible (no `#[serde(tag = ...)]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadItemDetails {
    AgentMessage(String),
    Reasoning(String),
    CommandExecution {
        cmd: String,
        status: String,
    },
    FileChange {
        path: PathBuf,
        kind: String,
    },
    ToolCall {
        name: String,
        args: serde_json::Value,
    },
    WebSearch {
        query: String,
    },
    Error(String),
}

/// Error payload on [`ThreadEvent::TurnFailed`] / [`ThreadEvent::Error`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadErrorEvent {
    pub kind: String,
    pub message: String,
}

/// Spawn context for [`HarnessAdapter::start_thread`]. Replaces the
/// V0.5.x [`SpawnOpts`] on the trait surface.
///
/// **Wave 4 D14 fixup** — `model_id` was added so the adapter and the
/// downstream cost-estimation path (`ccteam_cost::estimate_cost`) can
/// account against the *actual* model the agent is configured to run,
/// instead of the vendor's fallback model.  `None` means "use the
/// vendor's default" (legacy V0.5 callers + tests that don't care about
/// per-model cost accuracy).
#[derive(Debug, Clone)]
pub struct SpawnCtx {
    pub slug: String,
    pub sid: String,
    pub cwd: PathBuf,
    pub project_dir: PathBuf,
    pub extra_args: Vec<String>,
    /// Concrete model id (e.g. `"claude-sonnet-4-5"`, `"gpt-5-codex"`)
    /// the adapter should use for this thread. `None` = vendor default
    /// (resolved at adapter level). Plumbed through to `ccteam-cost`
    /// for per-model pricing instead of falling back to the vendor's
    /// `fallback_model`.
    pub model_id: Option<String>,
}

/// V0.6.0 F107 — canonical [`UnifiedTokenUsage`] lives in `ccteam-cost`
/// (`crates/ccteam-cost/src/pricing.rs`) so cost / pricing logic and
/// vendor accounting stay in one crate. Re-exported here so trait users
/// can `use ccteam_core::harness::UnifiedTokenUsage` without depending
/// on `ccteam-cost` directly.
pub use ccteam_cost::UnifiedTokenUsage;

/// Minimal agent spec passed into [`HarnessAdapter::start_thread`].
/// Wave 1 keeps this thin — Wave 2 / Wave 3 will extend with persona /
/// tool-allowlist fields. The `executor` field on the workflow-level
/// `crate::workflow::AgentSpec` still drives adapter selection; this
/// struct is the trait-facing slice.
#[derive(Debug, Clone)]
pub struct AgentSpecBrief {
    /// Role name (used as `claude --bg --agent <role>` etc.).
    pub role: String,
}

/// V0.6.0 F107 [`HarnessAdapter`] trait — 5-method thread/turn shape
/// aligned with Codex `ThreadManager::{submit, next_event}` protocol.
///
/// **Binding contract** (Wave 1 lock; Wave 2/3 may not change this
/// signature): 5 async lifecycle methods + 2 sync identifier methods.
///
/// Implementations live in [`crate::execution`]:
/// - [`crate::execution::claude_bg::ClaudeBgAdapter`]
/// - [`crate::execution::claude_tui::ClaudeTuiAdapter`] (stub Wave 1)
/// - [`crate::execution::codex_exec::CodexExecAdapter`]
#[async_trait::async_trait]
pub trait HarnessAdapter: Send + Sync {
    /// Stable identifier, e.g. `"claude-bg"`, `"claude-tui"`,
    /// `"codex-exec"`.
    fn name(&self) -> &'static str;

    /// Vendor classifier — orchestrator routing + cost pricing key.
    fn vendor(&self) -> AgentVendor;

    /// Begin a new thread (one-shot for bg adapters, long-running for
    /// chat adapters). Returns a [`ThreadHandle`] carrying everything
    /// the trait's other methods + the orchestrator's translation
    /// layer need.
    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError>;

    /// Submit one user-input turn to an existing thread. Bg adapters
    /// (single-turn) return a synthetic turn id from the spawn line.
    async fn submit_turn(&self, h: &ThreadHandle, input: TurnInput)
        -> Result<TurnId, HarnessError>;

    /// Stream of thread events. Adapters that don't yet feed structured
    /// events return an empty stream (the orchestrator's legacy
    /// `progress.jsonl` poller still drives state transitions for Wave
    /// 1; Wave 2 / Wave 3 adapters will populate this stream and the
    /// orchestrator will gradually retire the legacy poller).
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;

    /// Resume an already-existing thread by persistent id (e.g. Claude
    /// session-id, Codex thread id). Bg adapters return
    /// [`HarnessError::NotImplemented`] because every spawn is a fresh
    /// 1M context (red line R3, Claude vendor).
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError>;

    /// Graceful close. Idempotent on missing PID / missing tmux
    /// session (matches V0.5.x `shutdown_session` semantics).
    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError>;
}

// =====================================================================
// MarkerReporter — V0.6.8 F196 cross-cutting tail_loop → supervisor
// =====================================================================

/// V0.6.8 F196 — fire-and-forget channel from a chat-mode adapter's
/// transcript tail loop back to the per-bot supervisor.
///
/// The Claude TUI tail loop polls the F176 `active-session-id` marker
/// to learn which `<sid>.jsonl` to drain. When the SessionStart hook
/// fails (state.json missing, env propagation broke, hook subprocess
/// errored, etc.), the marker never appears, the loop polls forever,
/// and the bot is silently dead despite a healthy tmux pane. F187
/// surfaces a one-shot WARN; F196 closes the loop by letting the
/// supervisor count consecutive marker-missing reports and escalate
/// to a session reset.
///
/// The trait is intentionally minimal — one observation per loop tick,
/// no action enum exposed to the adapter side. The supervisor owns
/// the state machine + threshold + heal escalation; the adapter only
/// signals "this tick the marker was/wasn't there." Wiring is
/// per-`(slug, role)`: each `BotSupervisor` registers itself under
/// the bot's identity before `events()` is spawned, and the tail loop
/// looks up the reporter on each observation.
///
/// Cycle safety: implementations should be cheap to clone (typically
/// `Arc<Weak<Self>>` indirection on the registry side) and tolerate
/// the bot supervisor being dropped — `report_marker_*` calls after
/// shutdown must be no-ops.
#[async_trait::async_trait]
pub trait MarkerReporter: Send + Sync {
    /// One tail-loop tick observed the F176 `active-session-id` marker
    /// missing (file absent or transcript jsonl referenced by sid not
    /// yet on disk). Supervisor increments its consecutive-miss
    /// counter; on hitting threshold it escalates to a session reset.
    async fn report_marker_missing(&self);

    /// One tail-loop tick observed a present + resolvable marker.
    /// Supervisor resets the consecutive-miss counter + self-heal
    /// attempts so a recovered bot doesn't keep its history pinned.
    async fn report_marker_found(&self);
}

// =====================================================================
// HarnessError — F107 adds NotImplemented{reason:String} (dynamic)
// =====================================================================

/// Error type returned by every fallible [`HarnessAdapter`] surface.
///
/// V0.6.0 F107 drops the old `&'static str` constraint on
/// `NotImplemented::reason` so stub adapters (Wave 1
/// [`crate::execution::claude_tui::ClaudeTuiAdapter`]) can carry a
/// dynamic message naming which wave will fill the gap.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// JSON parse / shape mismatch on the harness state channel.
    #[error("snapshot ingest failed: {0}")]
    IngestFailed(String),
    /// Process / tmux failure during `start_thread`.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    /// `close_thread` failure — SIGTERM rejected or tmux refused the
    /// kill request.
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),
    /// Adapter declares the surface unsupported. Dynamic reason so
    /// stubs can name the wave that fills the gap.
    #[error("not implemented: {reason}")]
    NotImplemented { reason: String },
    /// Generic submit failure (turn rejected by the harness).
    #[error("submit failed: {0}")]
    SubmitFailed(String),
    /// Unrecoverable IO error (filesystem reservation, etc.).
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for HarnessError {
    fn from(err: std::io::Error) -> Self {
        HarnessError::Io(err.to_string())
    }
}

// =====================================================================
// Legacy types kept for state.json persistence + web SSE wire format
// =====================================================================

/// Normalized status snapshot — V0.4.0 type, kept because the web layer
/// (SSE wire format) and `cost_summary` consumer expect this shape.
/// Adapters now expose snapshot parsing via free fns in their execution
/// modules instead of a trait method (V0.6.0 F107 dropped
/// `ingest_snapshot` from the trait surface).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    pub harness: String,
    pub model_display_name: String,
    pub context_used_pct: u8,
    pub cost_usd_total: f64,
    pub rate_limit_pct: Option<u8>,
    pub cwd: Option<PathBuf>,
    pub raw: serde_json::Value,
    pub captured_at: DateTime<Utc>,
}

/// Optional per-snapshot subagent state. Reserved for future PR (PRD §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentState {
    pub kind: String,
    pub label: Option<String>,
    pub running_for: Option<Duration>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
}

/// Legacy spawn options — retained for the in-CLI `ccteam session add`
/// path which constructs SessionRecord-shaped registry entries. New
/// trait surface uses [`SpawnCtx`] instead.
#[derive(Debug, Clone)]
pub struct SpawnOpts {
    pub harness: &'static str,
    pub slug: String,
    pub sid: String,
    pub cwd: PathBuf,
    pub role: String,
    pub extra_args: Vec<String>,
}

/// Legacy session handle — retained for state.json `SessionRecord`
/// persistence + orchestrator's in-memory `running` map. The
/// orchestrator translates a [`ThreadHandle`] returned by
/// [`HarnessAdapter::start_thread`] into a [`SessionHandle`] for these
/// downstream consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHandle {
    pub tmux_session: String,
    pub harness: String,
    pub sid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
    /// V0.8 W3 follow-up — `true` when this spawn is a mode-2
    /// foreground-in-mux bg session (`CCTEAM_CLAUDE_BG_VIA_MUX=1`).
    /// Such a spawn has no `~/.claude/jobs/<id>/state.json`; the
    /// orchestrator detects its completion via the mux session
    /// lifecycle ([`crate::orchestrator`] checks
    /// `ProcessBackend::exists(mux_session)`) instead of the F80
    /// state.json poll. serde-default `false` keeps existing
    /// state.json `SessionRecord` files loading unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub via_mux: bool,
    /// V0.8 W3 follow-up — the mux session name to probe for liveness
    /// when `via_mux` is set. `None` for legacy `--bg` + codex spawns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux_session: Option<String>,
}

impl SessionHandle {
    /// Build a [`SessionHandle`] from a [`ThreadHandle`] + the (slug,
    /// sid) pair the orchestrator owns. Helper for orchestrator's
    /// trait-boundary translation layer.
    pub fn from_thread_handle(h: &ThreadHandle, sid: &str) -> Self {
        let tmux_session = h
            .raw_extras
            .get("tmux_session")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let pid = h
            .raw_extras
            .get("pid")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok());
        let harness = match h.vendor {
            AgentVendor::Claude => match h.mode {
                ExecutionMode::Bg => "claude-code",
                _ => "claude-tui",
            },
            AgentVendor::Codex => "codex",
        };
        let job_id = match h.vendor {
            AgentVendor::Claude if h.mode == ExecutionMode::Bg => Some(h.identity.clone()),
            _ => None,
        };
        // V0.8 W3 follow-up — carry the foreground-in-mux markers so
        // the orchestrator routes completion through the mux session
        // lifecycle instead of the (nonexistent) F80 state.json.
        let via_mux = h
            .raw_extras
            .get("via_mux")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mux_session = h
            .raw_extras
            .get("mux_session")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Self {
            tmux_session,
            harness: harness.to_string(),
            sid: sid.to_string(),
            job_id,
            pid,
            started_at: h.started_at,
            via_mux,
            mux_session,
        }
    }
}

// =====================================================================
// Free helpers (state.json parsing, pid signalling, plucker utilities)
// =====================================================================

/// Resolve the absolute path to `state.json` for a Claude Code
/// background job. Honors `$CCTEAM_CLAUDE_JOBS_DIR` for hermetic tests.
pub fn state_json_path(job_id: &str) -> PathBuf {
    let base = std::env::var_os(CLAUDE_JOBS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        });
    base.join(job_id).join("state.json")
}

/// Parse a Claude Code `state.json` body into a [`HarnessSnapshot`].
/// Tolerant of missing / extra fields — only outright JSON-parse
/// failures bubble.
pub fn parse_cc_state_json(raw: &str) -> Result<HarnessSnapshot, HarnessError> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| HarnessError::IngestFailed(format!("parse state.json: {err}")))?;

    let model_display_name = pluck_str(&value, &["model"])
        .or_else(|| pluck_str(&value, &["model_display_name"]))
        .or_else(|| pluck_str(&value, &["cliVersion"]).map(|v| format!("claude {v}")))
        .unwrap_or_else(|| "unknown".to_string());
    let context_used_pct = pluck_pct(&value, &["context_pct"])
        .or_else(|| pluck_pct(&value, &["context_used_pct"]))
        .unwrap_or(0);
    let cost_usd_total = pluck_f64(&value, &["cost_usd"])
        .or_else(|| pluck_f64(&value, &["cost_usd_total"]))
        .unwrap_or(0.0);
    let rate_limit_pct = pluck_pct(&value, &["rate_limit_pct"]);
    let cwd = pluck_str(&value, &["cwd"])
        .or_else(|| pluck_str(&value, &["workdir"]))
        .map(PathBuf::from);

    Ok(HarnessSnapshot {
        harness: "claude-code".to_string(),
        model_display_name,
        context_used_pct,
        cost_usd_total,
        rate_limit_pct,
        cwd,
        raw: value,
        captured_at: Utc::now(),
    })
}

/// Extract the `pid` field from a Claude Code `state.json` body.
/// Returns `None` on missing field, wrong type, or unparseable body
/// (parse failures swallowed because callers — `close_thread` — must
/// remain idempotent).
pub fn parse_pid_from_state(raw: &str) -> Option<i32> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value.get("pid").and_then(|v| v.as_i64()).and_then(|n| {
        if n > 0 && n <= i32::MAX as i64 {
            Some(n as i32)
        } else {
            None
        }
    })
}

/// Scan `claude --bg` stdout for the `backgrounded · <id>` marker line
/// and return the short hex id. See
/// [`crate::execution::claude_bg`] for usage.
pub fn parse_backgrounded_short_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("backgrounded") {
            continue;
        }
        let last = trimmed.split_whitespace().next_back()?;
        if last.is_empty() || last == "backgrounded" {
            return None;
        }
        return Some(last.to_string());
    }
    None
}

/// SIGTERM the given pid. Returns `Ok(())` on success or if the process
/// no longer exists (`ESRCH`).
pub fn sigterm_pid(pid: i32) -> std::io::Result<()> {
    // SAFETY: `libc::kill` is FFI-safe with any pid / signal pair.
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err)
}

/// V0.5.0 F97 — SIGKILL the given pid. Used by `ccteam stop <slug>
/// --cleanup force-kill` (the default) + by the `ask-lead` timeout
/// fallback. Idempotent: ESRCH (no such process) is success.
pub fn sigkill_pid(pid: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err)
}

// =====================================================================
// Plucker helpers — tolerant of missing / mistyped fields
// =====================================================================

pub(crate) fn pluck<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

pub(crate) fn pluck_str(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    pluck(value, path)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn pluck_f64(value: &serde_json::Value, path: &[&str]) -> Option<f64> {
    pluck(value, path).and_then(|v| v.as_f64())
}

pub(crate) fn pluck_pct(value: &serde_json::Value, path: &[&str]) -> Option<u8> {
    pluck(value, path).and_then(|v| {
        v.as_u64()
            .map(|n| n.min(100) as u8)
            .or_else(|| v.as_f64().map(|n| n.clamp(0.0, 100.0).round() as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_snapshot_serde_round_trip() {
        let original = HarnessSnapshot {
            harness: "claude-code".into(),
            model_display_name: "Claude Sonnet 4.5".into(),
            context_used_pct: 42,
            cost_usd_total: 1.234,
            rate_limit_pct: Some(17),
            cwd: Some(PathBuf::from("/home/u/projects/dev-foo")),
            raw: serde_json::json!({"keep": "me"}),
            captured_at: Utc::now(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: HarnessSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn parse_state_json_full_shape() {
        let raw = r#"{
            "status": "running",
            "model": "Claude Sonnet 4.5",
            "context_pct": 73,
            "cost_usd": 4.56,
            "turn_count": 17,
            "pid": 12345,
            "workdir": "/home/u/projects/dev-foo"
        }"#;
        let snap = parse_cc_state_json(raw).unwrap();
        assert_eq!(snap.harness, "claude-code");
        assert_eq!(snap.model_display_name, "Claude Sonnet 4.5");
        assert_eq!(snap.context_used_pct, 73);
        assert!((snap.cost_usd_total - 4.56).abs() < 1e-9);
    }

    #[test]
    fn parse_state_json_malformed_returns_ingest_failed() {
        let raw = r#"{"model": "broken"#;
        let err = parse_cc_state_json(raw).unwrap_err();
        assert!(matches!(err, HarnessError::IngestFailed(_)));
    }

    /// V0.6.3 F144 — forward-compat: a future `claude` CLI that adds
    /// unknown fields to `state.json` must still parse cleanly. The
    /// `Value`-plucking parser ignores anything it doesn't recognise.
    #[test]
    fn parse_state_json_with_future_fields_does_not_panic() {
        let raw = r#"{
            "status": "running",
            "model": "Claude Opus 5",
            "context_pct": 12,
            "cost_usd": 0.3,
            "pid": 777,
            "newFutureField": {"deeply": {"nested": [1, 2, 3]}},
            "schema_version": 42,
            "rateLimitTier": "platinum"
        }"#;
        let snap = parse_cc_state_json(raw).expect("future fields must not break parsing");
        assert_eq!(snap.model_display_name, "Claude Opus 5");
        assert_eq!(snap.context_used_pct, 12);
        // The unknown fields survive verbatim in `raw` for forensics.
        assert_eq!(snap.raw["schema_version"], 42);
    }

    #[test]
    fn parse_pid_from_state_extracts_integer() {
        let raw = r#"{"pid": 4242, "status": "running"}"#;
        assert_eq!(parse_pid_from_state(raw), Some(4242));
    }

    #[test]
    fn parse_pid_from_state_missing_field_returns_none() {
        let raw = r#"{"status": "running"}"#;
        assert_eq!(parse_pid_from_state(raw), None);
    }

    #[test]
    fn not_implemented_error_carries_dynamic_reason() {
        let err = HarnessError::NotImplemented {
            reason: "Wave 2 F108 fills tmux long-session + send-keys".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("Wave 2"));
    }

    #[test]
    fn agent_vendor_serde_round_trip() {
        for v in [AgentVendor::Claude, AgentVendor::Codex] {
            let json = serde_json::to_string(&v).unwrap();
            let back: AgentVendor = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn execution_mode_serde_round_trip() {
        for m in [
            ExecutionMode::InProc,
            ExecutionMode::Bg,
            ExecutionMode::Chat,
        ] {
            let json = serde_json::to_string(&m).unwrap();
            let back: ExecutionMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn session_handle_from_thread_handle_bg_claude() {
        let th = ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Bg,
            identity: "deadbeef".to_string(),
            started_at: Utc::now(),
            raw_extras: serde_json::json!({"tmux_session": "ccteam-foo-claude-1"}),
        };
        let sh = SessionHandle::from_thread_handle(&th, "claude-1");
        assert_eq!(sh.sid, "claude-1");
        assert_eq!(sh.harness, "claude-code");
        assert_eq!(sh.tmux_session, "ccteam-foo-claude-1");
        assert_eq!(sh.job_id.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn session_handle_from_thread_handle_codex() {
        let th = ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Bg,
            identity: "ccteam-bar-codex-1".to_string(),
            started_at: Utc::now(),
            raw_extras: serde_json::json!({"tmux_session": "ccteam-bar-codex-1", "pid": 9001u64}),
        };
        let sh = SessionHandle::from_thread_handle(&th, "codex-1");
        assert_eq!(sh.harness, "codex");
        assert!(sh.job_id.is_none());
        assert_eq!(sh.pid, Some(9001));
    }
}
