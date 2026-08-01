//! Harness adapter API shared by Claude/Codex execution adapters.
//!
//! ## What this file owns
//!
//! - The **new** [`HarnessAdapter`] trait (5 lifecycle async methods +
//!   routed turn submission + `name` + `vendor`)
//!   aligned with Codex `ThreadManager::{submit, next_event}` protocol.
//! - Cross-vendor types: [`AgentVendor`], [`ExecutionMode`], [`ThreadHandle`],
//!   [`TurnInput`], [`TurnId`], [`ThreadEvent`], [`ThreadItem`],
//!   [`ThreadItemDetails`], [`ThreadErrorEvent`], [`SpawnCtx`].
//! - [`UnifiedTokenUsage`] re-exported from `ccteam-cost`, the canonical
//!   pricing/cost definition.
//! - Legacy persistence + helper types still consumed by orchestrator /
//!   web / state.json registry: [`HarnessSnapshot`], [`SubagentState`],
//!   [`SpawnOpts`], [`SessionHandle`] (now an internal data type — no
//!   longer part of the trait surface).
//! - Free fns kept from V0.4.0 because callers outside this file still
//!   read state.json: [`parse_cc_state_json`], [`parse_pid_from_state`],
//!   [`state_json_path`], [`sigterm_pid`], [`sigkill_pid`],
//!   [`parse_backgrounded_short_id`], plus codex marker constants.
//!
//! Concrete adapter implementations still live in `ccteam-core` for
//! this slice; they move here after their progress/workflow coupling is
//! removed. Pure execution support modules already live under
//! [`crate::execution`].
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
//! ## Trait binding contract
//!
//! The trait signature below is **locked per minor version**. v0.6
//! (F107/F108/F112) settled the 5 lifecycle + 2 identifier methods;
//! v0.8.5 deliberately extends it once with `handle_directive` +
//! `thread_status` (both no-default — see the trait doc). Extending the
//! surface is a planned, doc-first event; drive-by signature changes
//! within a wave are not.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Environment override for the `codex` binary path. Tests set this to
/// fake scripts for `codex exec --json` and `codex app-server` without
/// requiring the real CLI on PATH.
pub const CODEX_BIN_ENV: &str = "CCTEAM_CODEX_BIN";

/// Environment override for the `grok` binary path. Tests set this to
/// a fake ACP stdio script so harness tests stay hermetic (no real
/// network / grok login).
pub const GROK_BIN_ENV: &str = "CCTEAM_GROK_BIN";

/// Environment override for the `opencode` binary path. Tests set this
/// to the hermetic fake (`tests/fixtures/opencode_acp/fake_opencode_acp.py`).
pub const OPENCODE_BIN_ENV: &str = "CCTEAM_OPENCODE_BIN";

/// Environment override for the `kimi` binary path. Tests set this to
/// the hermetic fake (`tests/fixtures/kimi_acp/fake_kimi_acp.py`).
pub const KIMI_BIN_ENV: &str = "CCTEAM_KIMI_BIN";

/// Environment override for the global ccteam root. Harness-owned
/// adapters use this for per-session state sidecars without depending
/// on `ccteam-core::paths::CcteamPaths`.
pub const CCTEAM_HOME_ENV: &str = "CCTEAM_HOME";

/// Marker line the codex agent prints in its tmux pane to publish
/// state to the observer (PRD §6.5 + dev-plan §3.2).
pub const CODEX_STATUS_MARKER: &str = "CODEX_STATUS:";

/// Number of trailing pane lines a codex observer is expected to
/// capture before feeding the pane body to the codex status parser.
pub const CODEX_STATUS_TAIL_LINES: usize = 5;

// =====================================================================
// V0.6.0 F107 — New trait surface
// =====================================================================

/// Resolve the global ccteam root the same way `CcteamPaths::from_env`
/// resolves its `root` field: `CCTEAM_HOME` wins, otherwise
/// `$HOME/.ccteam`.
pub fn ccteam_root_from_env() -> Option<PathBuf> {
    std::env::var(CCTEAM_HOME_ENV)
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".ccteam")))
}

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
    Grok,
    Opencode,
    Kimi,
}

impl AgentVendor {
    /// Every known harness vendor — single source of truth for iteration.
    /// Prefer `for v in AgentVendor::ALL` over listing arms at call sites.
    pub const ALL: &'static [AgentVendor] = &[
        AgentVendor::Claude,
        AgentVendor::Codex,
        AgentVendor::Grok,
        AgentVendor::Opencode,
        AgentVendor::Kimi,
    ];

    pub fn cost_vendor(self) -> ccteam_cost::Vendor {
        match self {
            AgentVendor::Claude => ccteam_cost::Vendor::Claude,
            AgentVendor::Codex => ccteam_cost::Vendor::Codex,
            AgentVendor::Grok => ccteam_cost::Vendor::Grok,
            AgentVendor::Opencode => ccteam_cost::Vendor::Opencode,
            AgentVendor::Kimi => ccteam_cost::Vendor::Kimi,
        }
    }
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

/// v0.8.7 W2 (DB.1) — per-session permission posture for a chat session.
///
/// - `Skip` (default) — spawn `claude --dangerously-skip-permissions`
///   (today's behavior): every tool runs without prompting. Unchanged for
///   every existing session.
/// - `Hitl` (human-in-the-loop) — spawn with `--permission-mode default`
///   and DROP the skip flag so Claude's native permission ask-path stays
///   alive. A non-allowlist tool call then fires the `PermissionRequest`
///   hook, which ccteam turns into an IM approve/deny prompt. Allowlist /
///   auto-allowed tools never prompt (we leverage Claude's own allow-list).
///
/// Carried on [`SpawnCtx`] so it threads through every spawn path; stored
/// on the gateway session record so a resume re-applies it. Lives in
/// `ccteam-harness` (not `ccteam-core`) because the dependency direction is
/// `core → harness`; `ccteam-core` re-exports it next to [`AgentVendor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// `--dangerously-skip-permissions` (default; no prompts).
    #[default]
    Skip,
    /// `--permission-mode default`; non-allowlist tools prompt via hook.
    Hitl,
}

impl PermissionMode {
    /// Parse an optional wire token (`"skip"` / `"hitl"`, case-insensitive).
    /// `None` / empty ⇒ [`PermissionMode::Skip`] (the default everywhere).
    /// An unrecognized non-empty token is an `Err` so a typo surfaces
    /// instead of silently downgrading to skip.
    pub fn parse_opt(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).unwrap_or("") {
            "" | "skip" => Ok(PermissionMode::Skip),
            "hitl" => Ok(PermissionMode::Hitl),
            other => Err(format!(
                "invalid permission mode `{other}`: expected `skip` or `hitl`"
            )),
        }
    }

    /// Lowercase wire string (`"skip"` / `"hitl"`) for API / view shapes.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Skip => "skip",
            PermissionMode::Hitl => "hitl",
        }
    }

    /// True for [`PermissionMode::Hitl`] — the spawn drops the skip flag.
    pub fn is_hitl(self) -> bool {
        matches!(self, PermissionMode::Hitl)
    }
}

/// v0.8.11 E2 — the **protocol axis** of a session (PRD §〇): how a Claude
/// session is driven.
///
/// - `StreamJson` (default) — the lightweight chat path: a long-running
///   `claude` child over a bidirectional NDJSON pipe
///   ([`crate::ClaudeStreamJsonAdapter`]). No PTY / pane / hook chain.
/// - `Terminal` — the advanced path: a tmux PTY + `claude` TUI
///   ([`crate::execution::claude_tui::ClaudeTuiAdapter`]); needed only when
///   the user wants the byte-faithful terminal mirror / attach.
///
/// Named `protocol` (NOT `backend`) per PRD §七 ②: `backend` is reserved for
/// the v0.9 **host** axis. Codex sessions carry a protocol value too but it
/// is informational — codex always drives via its app-server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SessionProtocol {
    /// `stream-json` — the薄/default chat channel.
    #[default]
    StreamJson,
    /// `terminal` — tmux PTY + TUI (advanced; terminal mirror / attach).
    Terminal,
    /// `acp` — Agent Client Protocol stdio (Grok Build). Honest meta value;
    /// Claude has no ACP arm.
    Acp,
}

impl SessionProtocol {
    /// Parse an optional wire token (`"stream-json"` / `"terminal"` / `"acp"`).
    /// `None` / empty ⇒ [`SessionProtocol::StreamJson`] (the default).
    /// An unrecognized non-empty token is an `Err` so a typo surfaces.
    pub fn parse_opt(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).unwrap_or("") {
            "" | "stream-json" | "streamjson" | "stream_json" => Ok(SessionProtocol::StreamJson),
            "terminal" | "tmux" => Ok(SessionProtocol::Terminal),
            "acp" => Ok(SessionProtocol::Acp),
            other => Err(format!(
                "invalid protocol `{other}`: expected `stream-json`, `terminal`, or `acp`"
            )),
        }
    }

    /// Lowercase wire string (`"stream-json"` / `"terminal"` / `"acp"`).
    pub fn as_str(self) -> &'static str {
        match self {
            SessionProtocol::StreamJson => "stream-json",
            SessionProtocol::Terminal => "terminal",
            SessionProtocol::Acp => "acp",
        }
    }

    /// True for the default stream-json (paneless) channel.
    pub fn is_stream_json(self) -> bool {
        matches!(self, SessionProtocol::StreamJson)
    }

    /// True for the tmux/terminal (pane-backed) channel.
    pub fn is_terminal(self) -> bool {
        matches!(self, SessionProtocol::Terminal)
    }

    /// True for ACP stdio (Grok).
    pub fn is_acp(self) -> bool {
        matches!(self, SessionProtocol::Acp)
    }
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
/// - Claude bg adapters use the `daemonShort` job id from `claude
///   --bg`'s `backgrounded · <id>` stdout line.
/// - Claude TUI adapters use tmux session names like
///   `ccteam-chat-<slug>-<role>`.
/// - Codex exec adapters use tmux session names like
///   `ccteam-<slug>-<sid>`.
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
    /// Rich-media image attachment (V0.6 Epic B).
    Image(PathBuf),
    /// External resolver feeding a tool-call result back to the agent.
    ToolResult {
        call_id: String,
        content: serde_json::Value,
    },
}

/// How a user message submitted while a vendor turn is active should be
/// delivered.
///
/// This is **user-turn routing**, not system-prompt injection: ccteam forwards
/// the user's text unchanged through a vendor-native channel. Adapters report
/// unsupported paths explicitly; where both are supported, an idle session
/// starts a normal turn for either variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRouting {
    /// Merge the message into the active vendor turn at its next safe point.
    /// Adapters without a native steer/interject channel may explicitly
    /// degrade to [`TurnRouting::Queue`] rather than cancel the active turn.
    Inject,
    /// Preserve the message as a distinct FIFO follow-up turn.
    Queue,
}

/// What the adapter actually did with one accepted message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnDisposition {
    /// No vendor turn was active, so this message started one.
    Started,
    /// The message joined the already-active vendor turn.
    Injected,
    /// The message became a distinct FIFO follow-up turn.
    Queued,
}

/// Result of a routed user-message submission.
///
/// `disposition` is the path the adapter actually used. It can differ from the
/// requested routing when a vendor has no native injection channel and safely
/// degrades to a distinct FIFO turn. `input_id` is unique per accepted user
/// message even when several injections share one `turn_id`. `turn_id` remains
/// adapter-defined correlation (vendor-native where available, synthetic for
/// transports that do not return one).
pub struct TurnSubmission {
    pub turn_id: TurnId,
    pub input_id: String,
    pub disposition: TurnDisposition,
    completion_guard: Option<Box<dyn Send + 'static>>,
}

impl TurnSubmission {
    pub fn started(turn_id: TurnId) -> Self {
        Self::with_disposition(turn_id, TurnDisposition::Started)
    }

    pub fn started_with_input_id(turn_id: TurnId, input_id: impl Into<String>) -> Self {
        Self::with_input_id(turn_id, input_id, TurnDisposition::Started)
    }

    pub fn injected(turn_id: TurnId) -> Self {
        Self::with_disposition(turn_id, TurnDisposition::Injected)
    }

    pub fn injected_with_input_id(turn_id: TurnId, input_id: impl Into<String>) -> Self {
        Self::with_input_id(turn_id, input_id, TurnDisposition::Injected)
    }

    pub fn queued(turn_id: TurnId) -> Self {
        Self::with_disposition(turn_id, TurnDisposition::Queued)
    }

    fn with_disposition(turn_id: TurnId, disposition: TurnDisposition) -> Self {
        Self::with_input_id(turn_id, next_turn_input_id(), disposition)
    }

    fn with_input_id(
        turn_id: TurnId,
        input_id: impl Into<String>,
        disposition: TurnDisposition,
    ) -> Self {
        Self {
            input_id: input_id.into(),
            turn_id,
            disposition,
            completion_guard: None,
        }
    }

    /// Hold a vendor turn boundary until the caller records this accepted
    /// input. Used by adapters whose prompt can complete concurrently with the
    /// submission acknowledgement.
    pub fn hold_completion(mut self, guard: impl Send + 'static) -> Self {
        self.completion_guard = Some(Box::new(guard));
        self
    }

    /// Release any adapter completion fence after origin/transcript metadata is
    /// registered. Safe and idempotent for unfenced submissions.
    pub fn release_completion(&mut self) {
        self.completion_guard.take();
    }

    /// Mint an opaque, process-unique receipt id before a vendor request is
    /// sent (for protocols such as Codex `clientUserMessageId`).
    pub fn mint_input_id() -> String {
        next_turn_input_id()
    }
}

impl std::fmt::Debug for TurnSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnSubmission")
            .field("turn_id", &self.turn_id)
            .field("input_id", &self.input_id)
            .field("disposition", &self.disposition)
            .field("completion_fenced", &self.completion_guard.is_some())
            .finish()
    }
}

static TURN_INPUT_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_turn_input_id() -> String {
    let seq = TURN_INPUT_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("input-{nanos:x}-{seq:x}")
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
        /// Canonical model id for this turn (e.g. `claude-opus-4-8` from
        /// the transcript/stream `message.model`; codex `result.model`),
        /// for deterministic per-turn cost pricing. `None` when the
        /// channel carries no model (e.g. the tmux Stop hook) → the turn
        /// is unpriced (exposed, never billed at a fallback rate).
        #[serde(default)]
        model: Option<String>,
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

/// v8.1 neutral event name. This is intentionally an alias for the
/// established adapter event schema while downstream code is still
/// named around `ThreadEvent`.
pub type CanonicalEvent = ThreadEvent;

/// Vendor-neutral approval request placeholder.
///
/// v8.1 runs agent processes with skip-permissions and does not produce
/// or resolve approvals. The type lives here so future Claude hook and
/// Codex app-server approval surfaces can share one IM-facing shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalIR {
    pub req_id: String,
    pub vendor: AgentVendor,
    pub kind: ApprovalKind,
    pub risk: ApprovalRisk,
    pub scope: ApprovalScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Command,
    FileChange,
    Permission,
    Question,
    ToolUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRisk {
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    Once,
    Session,
    Always,
}

// =====================================================================
// v0.8.5 — Session command/interaction/status vocabulary (D1/D3/P3)
//
// These neutral types let the gateway stay a pure router: a slash
// command becomes a `Directive`, the adapter (the only thing that knows
// its vendor's command surface) answers with a `DirectiveOutcome`, and
// any choice the user must make travels as a `ChoicePrompt` /
// `ChoiceSelection` regardless of vendor or channel. `ChoicePrompt` is
// the interaction-layer forerunner of `ApprovalIR` (the semantic/risk
// layer): a future HITL approval translates one into the other and
// reuses the same pending-interaction registry + back-fill path.
// =====================================================================

/// A session-directed slash command, parsed by the gateway and handed to
/// the owning adapter's [`HarnessAdapter::handle_directive`]. The gateway
/// has zero knowledge of what `name` / `args` mean — that is the
/// adapter's (vendor's) sole interpretation authority.
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    /// Command name without the leading `/` (e.g. `"compact"`, `"model"`).
    pub name: String,
    /// Everything after the name, verbatim. Argument semantics are the
    /// adapter's to parse.
    pub args: String,
    /// Set on the re-entry call after a [`DirectiveOutcome::NeedsChoice`]
    /// — carries the user's selection so the adapter can apply it.
    pub choice: Option<ChoiceSelection>,
}

/// The five exhaustive answers an adapter can give to a [`Directive`].
/// Everything a vendor command can do maps onto exactly one of these, so
/// no slash ever silently degrades into literal model text.
#[derive(Debug, Clone, PartialEq)]
pub enum DirectiveOutcome {
    /// The command became a turn (Claude passthrough / Codex review /
    /// init …). The gateway tracks it like any other turn.
    Turn(TurnId),
    /// Completed in place (RPC / in-memory override). `receipt` is shown
    /// to the user verbatim.
    Done { receipt: String },
    /// The user must choose before the command can complete. The gateway
    /// renders the prompt and re-enters `handle_directive` with the
    /// selection carried on [`Directive::choice`].
    NeedsChoice(ChoicePrompt),
    /// Explicitly refused (TUI-only / unsupported / not enabled). A
    /// first-class answer, not an error.
    Rejected { reason: String },
    /// Semantic redirect — the command has no in-thread equivalent; the
    /// hint points the user at the gateway surface that does (e.g.
    /// `/new`).
    Redirect { hint: String },
}

// ── vendor-side session title (the write half of the title system) ──────────

/// Which session a [`HarnessAdapter::set_session_title`] push targets.
/// Deliberately NOT a bare [`ThreadHandle`]: a rename is legal on a STOPPED
/// session too (its `meta.json` is the SoT and outlives the live map), so the
/// target carries the on-disk coordinates every adapter can resolve from, and
/// `thread` is `Some` only while the session happens to be live. An adapter
/// whose title surface is a file (Claude's transcript `custom-title` entry)
/// therefore works in both states; one whose surface is a live RPC (Codex
/// `thread/name/set`) answers [`TitleSync::Deferred`] when `thread` is `None`.
#[derive(Debug, Clone)]
pub struct SessionTitleTarget {
    /// ccteam session id (`s{n}`).
    pub sid: String,
    /// The vendor's own session id as recorded in `meta.json` (Claude session
    /// UUID / Codex thread id / ACP session id). May be empty for vendors that
    /// don't expose one.
    pub vendor_uuid: String,
    /// The project working dir the session runs in — also where ccteam's
    /// per-session state (`.ccteam/chat/<sid>/`) lives.
    pub project_dir: PathBuf,
    /// Live handle when the session is currently in the gateway's live map.
    pub thread: Option<ThreadHandle>,
}

/// What happened on the VENDOR side when ccteam pushed a user rename.
/// Reported verbatim to the user (IM receipt / web toast) so a title that
/// only exists ccteam-side never reads as if the vendor had adopted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum TitleSync {
    /// The vendor's own title surface now carries the new title.
    Pushed,
    /// The vendor HAS a title surface, but it could not be reached right now
    /// (no live thread, or the vendor hasn't filed a transcript yet). The
    /// ccteam-side title stands; `detail` says why.
    Deferred(String),
    /// The vendor exposes no session-title surface at all — ccteam-side only.
    Unsupported,
}

/// A choice the user must make, produced either by an adapter
/// ([`DirectiveOutcome::NeedsChoice`]) or by an agent question (the chat
/// `AskUserQuestion` hook). Channel-neutral: each channel renders it its
/// own way (Telegram inline keyboard / web chips / numbered text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoicePrompt {
    /// Short opaque correlation id minted by the producer. MUST be ASCII,
    /// ≤16 bytes, and contain no `:` — the transport packs
    /// `"{token}:{idx}"` into one callback string and splits on the first
    /// `:`.
    pub token: String,
    pub title: String,
    pub options: Vec<ChoiceOption>,
    /// `true` when more than one option may be selected.
    pub multi: bool,
}

/// One selectable option. `id` is the real, semantic option id the
/// producer cares about (e.g. a model id); the channel never sees it —
/// it only round-trips the positional index, which the gateway maps back
/// to this `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
}

/// A normalized user selection — the single shape all three inbound forms
/// (button callback / numeric short-reply / full arg-form) collapse to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceSelection {
    /// Echoes [`ChoicePrompt::token`] so the registry can match the reply
    /// to its pending prompt.
    pub token: String,
    /// Selected real option ids (resolved from indices by the gateway).
    pub ids: Vec<String>,
    /// Free-text answer (the "Other" path); `None` for pure selections.
    pub free_text: Option<String>,
}

/// How a [`ContextUsage`] came to be known. Provenance travels WITH the
/// number so the honesty wording has exactly one home ([`ContextUsage::render`])
/// instead of one hand-written sentence per vendor adapter.
///
/// It deliberately does NOT change how a *known* value renders — a derived
/// number is no less real than a reported one, and decorating it would only
/// add noise to every statusline. It exists so the unknown case can be said
/// out loud, and so the web payload can explain where a number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// The vendor pushed occupancy directly (ACP `usage_update{used,size}`,
    /// Codex app-server token counts).
    Reported,
    /// Computed from per-turn token accounting (Claude transcript `usage`,
    /// grok's `_meta.totalTokens` — whose `inputTokens` carries the whole
    /// history, so the per-turn total tracks occupancy).
    Derived,
    /// Pulled from a status surface the vendor itself advertises (an ACP
    /// `availableCommands` entry). Used when the vendor has no push channel.
    Probed,
    /// No channel carries it. The default: a value nobody vouched for is
    /// unknown, never zero.
    #[default]
    Unknown,
}

/// Context-window usage for a session, vendor-agnostic. Numerator +
/// denominator in tokens; the percentage is derived, never stored.
///
/// `used_tokens` is an `Option` on purpose: "we have a window but nobody told
/// us the occupancy" is a real state (a just-resumed ACP session, a vendor
/// with no usage channel), and it must NOT be flattened to `0` — a zero reads
/// as "context is empty", which is a lie about a session that may be at 80%.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ContextUsage {
    /// Tokens currently occupying the window; `None` when unknown.
    #[serde(default)]
    pub used_tokens: Option<u64>,
    /// Total window size in tokens; `0` when unknown.
    pub window_tokens: u64,
    /// Where the numbers came from. Serde-defaulted so an older persisted
    /// `status.json` still deserializes (as [`ContextSource::Unknown`]).
    #[serde(default)]
    pub source: ContextSource,
}

impl ContextUsage {
    /// A fully-known usage from a source that vouches for both numbers.
    pub fn known(used_tokens: u64, window_tokens: u64, source: ContextSource) -> Self {
        Self {
            used_tokens: Some(used_tokens),
            window_tokens,
            source,
        }
    }

    /// A known window whose occupancy nobody reports (yet).
    pub fn window_only(window_tokens: u64) -> Self {
        Self {
            used_tokens: None,
            window_tokens,
            source: ContextSource::Unknown,
        }
    }

    /// Used/window as a 0–100 percentage. `None` when either half is unknown
    /// — callers must render "—", never a fabricated `0%`.
    pub fn pct(&self) -> Option<f32> {
        match (self.used_tokens, self.window_tokens) {
            (Some(used), window) if window > 0 => Some((used as f32 / window as f32) * 100.0),
            _ => None,
        }
    }

    /// Render this usage as the canonical absolute-value + percent form
    /// (P3): `"188k / 1M (19%)"`. When the window is unknown (zero), only
    /// the used count is shown (`"188k (window unknown)"`); when the
    /// occupancy is unknown, the window still shows and the numerator is a
    /// dash (`"— / 500k (usage unknown)"`). This is the **single** render
    /// point so `/sessions` (gateway) and Codex `/status` (adapter) always
    /// agree byte-for-byte.
    pub fn render(&self) -> String {
        match (self.used_tokens, self.window_tokens) {
            (Some(used), 0) => format!("{} (window unknown)", format_tokens(used)),
            (Some(used), window) => format!(
                "{} / {} ({:.0}%)",
                format_tokens(used),
                format_tokens(window),
                self.pct().unwrap_or(0.0)
            ),
            (None, 0) => "—".to_string(),
            (None, window) => format!("— / {} (usage unknown)", format_tokens(window)),
        }
    }
}

/// Humanize a token count for status display (P3). Whole-thousand /
/// whole-million values render without a trailing `.0` so the common
/// window sizes read cleanly: `200_000 → "200k"`, `1_000_000 → "1M"`.
/// Non-round values keep one decimal (`1234 → "1.2k"`); under 1000 is
/// printed verbatim (`188 → "188"`).
pub fn format_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        if n % 1_000 == 0 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        let m = n as f64 / 1_000_000.0;
        if n % 1_000_000 == 0 {
            format!("{m:.0}M")
        } else {
            format!("{m:.1}M")
        }
    }
}

/// Queryable session attributes (P3): `model` + context usage are
/// properties of the session, not a by-product of a log line. Produced
/// by [`HarnessAdapter::thread_status`]. `Default` (all-`None`) is the
/// explicit "this harness has no status to report" answer for bg
/// adapters — a *default impl* on the trait method is what's forbidden,
/// not a default *value*.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThreadStatus {
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    /// Reasoning-effort level the model will actually run at (Claude Opus
    /// 4.6+ / Codex), e.g. `low` / `medium` / `high` / `xhigh` / `max`.
    /// `None` for builds/models without an effort axis. Default-skipped so
    /// an older persisted `status.json` (no `effort`) still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The session's long-running goal (Claude / Codex `/goal`), surfaced in
    /// the statusline. `None` when no goal is set. Default-skipped for
    /// back-compat with older persisted `status.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalStatus>,
}

/// Account-level usage / rate-limits (Claude `get_usage` control_request; Codex
/// equivalent), vendor-agnostic. Surfaced in the IM `/status` operator
/// dashboard. This is the ACCOUNT's state (5-hour + weekly windows + extra
/// credits), NOT a per-session property — the gateway queries it once on any
/// live session. Percentages are 0-100; `*_resets_at` is the vendor's ISO-8601
/// string (the caller renders it). All-`None` = "no account-usage channel".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AccountUsage {
    /// Subscription tier (e.g. `"max"`, `"pro"`).
    pub subscription: Option<String>,
    /// 5-hour rolling window utilization (%), and when it resets.
    pub five_hour_pct: Option<u8>,
    pub five_hour_resets_at: Option<String>,
    /// 7-day (weekly) window utilization (%), when it resets, and its severity
    /// (`"normal"` / `"warning"` / … from the vendor `limits[]`).
    pub weekly_pct: Option<u8>,
    pub weekly_resets_at: Option<String>,
    pub weekly_severity: Option<String>,
    /// Pay-as-you-go extra-credit utilization (%), when enabled.
    pub credits_pct: Option<u8>,
}

/// One in-flight subagent / workflow task, reflected from the harness's OWN
/// task lifecycle — claude's stream-json `system:task_started` → terminal
/// `task_updated`/`task_notification`. ccteam does NOT fold or count events to
/// derive this: a task is "running" iff its `task_started` arrived with no
/// terminal status yet, so the list mirrors exactly what claude reports. A
/// plain `Agent` subagent and a workflow task both flow through the same
/// events; `task_type` (`"local_agent"` …) distinguishes them. Surfaced by
/// [`HarnessAdapter::running_tasks`] → the IM `/status` current-session view
/// (nested under the working session).
#[derive(Debug, Clone)]
pub struct RunningTask {
    /// claude's stable task id (`task_started.task_id`); the de-dup / removal key.
    pub task_id: String,
    /// The `subagent_type` (e.g. `general-purpose`, `code-reviewer`).
    pub kind: String,
    /// The task's short description (`task_started.description`).
    pub description: String,
    /// The task kind (`task_started.task_type`, e.g. `local_agent`).
    pub task_type: String,
    /// When `task_started` arrived — for the elapsed-time display. In-memory
    /// only (never persisted / serialized).
    pub started: std::time::Instant,
}

impl RunningTask {
    /// True for a task that legitimately OUTLIVES the turn that spawned it:
    /// background workflows (`local_workflow`) and background shells
    /// (`local_bash` = Bash `run_in_background` + Monitor watches; vocabulary
    /// probed live 2026-07-22) keep running after the spawning turn's result
    /// and report their own terminal `task_updated`/`task_notification`
    /// later. Sync subagents (`local_agent`) are turn-scoped. Single
    /// authority for BOTH the stream-json turn-end eviction net and the IM
    /// `/status` "authoritative working signal" — the two must never diverge:
    /// an outliving task left over from an earlier turn must not mask a
    /// genuinely stuck later turn, and conversely must survive turn end so an
    /// idle session still shows it.
    pub fn outlives_turn(&self) -> bool {
        matches!(self.task_type.as_str(), "local_workflow" | "local_bash")
    }
}

/// A long-running session goal (`/goal`). `condition` is the objective text;
/// `met` flips true when the agent reports it achieved. For Claude stream-json
/// this is sourced from the session transcript's `goal_status` attachment —
/// the bridge exposes no control_request or stream message for it (verified by
/// live probe), so it is read from the transcript like the TUI does.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GoalStatus {
    pub condition: String,
    pub met: bool,
}

impl ThreadStatus {
    /// Render this status as a compact one-line suffix for `/sessions`
    /// (P3), e.g. `"claude-opus-4-8[1m] · ctx 188k / 1M (19%)"`. Returns
    /// `None` when there is nothing to report (statusless / bg adapters,
    /// `Default`), so the caller appends nothing and the legacy
    /// `id:project:vendor:role` row is unchanged. Reuses
    /// [`ContextUsage::render`] so the absolute+percent form matches Codex
    /// `/status` exactly.
    pub fn status_suffix(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = self.model.as_deref().filter(|s| !s.is_empty()) {
            parts.push(m.to_string());
        }
        if let Some(e) = self.effort.as_deref().filter(|s| !s.is_empty()) {
            parts.push(e.to_string());
        }
        if let Some(ctx) = &self.context {
            parts.push(format!("ctx {}", ctx.render()));
        }
        if let Some(g) = &self.goal {
            let cond = g.condition.trim();
            if !cond.is_empty() {
                // Truncate a long objective so the one-line suffix stays tidy.
                let shown: String = if cond.chars().count() > 48 {
                    format!("{}…", cond.chars().take(47).collect::<String>())
                } else {
                    cond.to_string()
                };
                let marker = if g.met { "✅" } else { "🎯" };
                parts.push(format!("{marker} {shown}"));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
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
#[derive(Debug, Clone, Default)]
pub struct SpawnCtx {
    pub slug: String,
    pub sid: String,
    pub cwd: PathBuf,
    pub project_dir: PathBuf,
    pub extra_args: Vec<String>,
    /// Concrete model id (e.g. `"claude-sonnet-4-5"`, `"gpt-5.5"`)
    /// the adapter should use for this thread. `None` = vendor default
    /// (resolved at adapter level). Plumbed through to `ccteam-cost` for
    /// deterministic per-model pricing; a model absent from the pricing
    /// table prices to `None` (exposed as "—"), never a fallback rate.
    pub model_id: Option<String>,
    /// Explicit reasoning-effort token for this thread. `None` = vendor
    /// default: nothing is emitted and the vendor's own resolution holds —
    /// that is what an omitted effort means at every ccteam entry point.
    ///
    /// Applied through each vendor's own seam, with the value passed VERBATIM
    /// (ccteam does not police vendor value sets; the vendor owns that
    /// verdict, and the ladders a caller is offered come from
    /// [`crate::model_catalog::supported_efforts`]):
    /// - Claude stream-json: `--effort` argv;
    /// - Codex app-server: the sticky `effort` override on the first
    ///   `turn/start` (codex takes no argv for it);
    /// - Grok ACP: `--reasoning-effort` argv (its handshake declares the
    ///   levels in `_meta.reasoningEfforts`);
    /// - OpenCode / Kimi ACP: `session/set_config_option` on the axis id the
    ///   vendor declared in its handshake (`effort` / `thinking` — see
    ///   [`crate::execution::acp::ModelInfo::effort_config_id`]).
    ///
    /// A vendor REFUSING an explicit value fails the spawn
    /// ([`crate::execution::acp::spawn_pick_refused`]) rather than handing
    /// back a session quietly running on something else. The one surface that
    /// cannot carry it is the frozen terminal protocol, which refuses the
    /// spawn with a message naming stream-json instead of ignoring the pick.
    pub effort: Option<String>,
    /// v0.8.7 W2 (DB.1) — per-session permission posture. `Skip` (default)
    /// keeps today's `--dangerously-skip-permissions` spawn; `Hitl` drops
    /// that flag, spawns `--permission-mode default`, and installs the
    /// `PermissionRequest` hook so non-allowlist tools prompt the IM user.
    /// Claude-only lever today (codex uses its own sandbox model).
    pub permission_mode: PermissionMode,
    /// v0.8.7 review-fix (R-M1) — per-session secret for the cto scheduling
    /// gate, injected into the pane env as `CCTEAM_CHAT_SECRET` at spawn so
    /// the forwarded `session_*` call can be authenticated against the
    /// gateway's `sid -> {role, secret}` map instead of a plaintext role arg.
    /// Empty `""` = no secret (tests / codex / legacy callers); the env var
    /// is then omitted, matching prior behavior. NOT a hard boundary — see
    /// `ccteam_core::session_secret` for the single-uid threat-model scope.
    pub secret: String,
    /// v0.9.0 W3 (F3, tech-design §4.3) — when `Some`, this thread runs on a
    /// satellite host: the adapter must dial
    /// [`crate::execution::remote_exec::connect`] instead of spawning a
    /// local child, per the transport law (tech-design §0.4: execution
    /// location is a transport parameter, not an adapter branch). `None`
    /// (the overwhelming majority) = local spawn, unchanged.
    pub remote: Option<crate::execution::remote_exec::RemoteExecTarget>,
}

/// V0.6.0 F107 — canonical [`UnifiedTokenUsage`] lives in `ccteam-cost`
/// (`crates/ccteam-cost/src/pricing.rs`) so cost / pricing logic and
/// vendor accounting stay in one crate. Re-exported here so trait users
/// can `use ccteam_harness::UnifiedTokenUsage` without depending
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

/// [`HarnessAdapter`] trait — thread/turn lifecycle aligned with Codex
/// `ThreadManager::{submit, next_event}`, plus the v0.8.5 session command
/// + status surface.
///
/// **Surface** (signature locked per minor, extended doc-first): 5 async
/// lifecycle methods + 2 sync identifier methods (v0.6), plus
/// `handle_directive` + `thread_status` (v0.8.5). The two v0.8.5 methods
/// have **no default impl** on purpose: a new vendor that forgets its
/// command surface or status surface fails to compile instead of
/// silently degrading (`/cmd` → literal text, `/sessions` → blank).
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
    /// Shorthand for the application's current default: native active-turn
    /// injection. The routed method below is the adapter contract; this helper
    /// exists for adapter-local directives and focused tests.
    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let mut submitted = self
            .submit_turn_routed(h, input, TurnRouting::Inject)
            .await?;
        submitted.release_completion();
        Ok(submitted.turn_id)
    }

    /// Submit with an explicit active-turn routing intent. The application
    /// currently selects [`TurnRouting::Inject`] for every vendor; retaining
    /// the parameter here lets a future composer expose queue-vs-inject without
    /// teaching the application vendor protocols.
    ///
    /// Every adapter implements this method explicitly so a new vendor cannot
    /// accidentally inherit ambiguous queue-vs-inject behavior. Unsupported
    /// paths fail honestly instead of silently changing semantics.
    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError>;

    /// Stream of thread events. Adapters that don't yet feed structured
    /// events return an empty stream (the orchestrator's legacy
    /// `progress.jsonl` poller still drives state transitions for Wave
    /// 1; Wave 2 / Wave 3 adapters will populate this stream and the
    /// orchestrator will gradually retire the legacy poller).
    ///
    /// **Final-only contract (v0.8.5):** an adapter MUST emit the final
    /// agent text for a turn exactly once, as
    /// [`ThreadEvent::ItemCompleted`] carrying
    /// [`ThreadItemDetails::AgentMessage`]. [`ThreadEvent::ItemUpdated`]
    /// is delta / presentation only and consumers MAY drop it. The
    /// gateway's text-extraction helpers rely on this; a future vendor
    /// that put final text only in `ItemUpdated` would be silently
    /// dropped, so the contract lives here, not as a gateway assumption.
    /// Non-terminal observability notifications (token usage, plan,
    /// rate-limits) MUST NOT be yielded here — they are mirrored to
    /// `progress.jsonl` out of band and queried via [`thread_status`].
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;

    /// Resume an already-existing thread by persistent id (e.g. Claude
    /// session-id, Codex thread id). Bg adapters return
    /// [`HarnessError::NotImplemented`] because every spawn is a fresh
    /// 1M context (red line R3, Claude vendor).
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError>;

    /// Graceful close. Idempotent on missing PID / missing tmux
    /// session (matches V0.5.x `shutdown_session` semantics).
    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError>;

    /// Interpret a session-directed [`Directive`] (a `/command`). This is
    /// the adapter's **sole** authority over its vendor's command surface
    /// — the gateway is a pure router and never second-guesses the
    /// mapping. **No default impl** (aligns with the "vendor enum has no
    /// default" red line): a new vendor MUST declare its command surface
    /// explicitly, so "slash silently degrades to literal text" is
    /// impossible by construction. Non-interactive adapters (bg) MUST
    /// answer `Ok(DirectiveOutcome::Rejected { .. })` — an explicit
    /// answer, never `Err`.
    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError>;

    /// Report the session's queryable status (model + context usage) for
    /// `/sessions` and Codex `/status`. **No default impl** (same
    /// reasoning as `handle_directive`): a default would make `/sessions`
    /// silently blank for a new vendor. bg / statusless adapters answer
    /// `Ok(ThreadStatus::default())`.
    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError>;

    /// Report ACCOUNT-level usage / rate-limits (5-hour + weekly windows + extra
    /// credits) for the IM `/status` operator dashboard. Unlike `thread_status`
    /// this is OPTIONAL (a default `None`): only adapters with a usage channel
    /// implement it (Claude stream-json via the `get_usage` control_request).
    /// It is account-, not session-, scoped — the gateway queries it once on any
    /// one live session.
    async fn account_usage(&self, _h: &ThreadHandle) -> Option<AccountUsage> {
        None
    }

    /// The session's currently-running subagent / workflow tasks, as reported by
    /// the harness's OWN task lifecycle (never folded or counted by ccteam).
    /// OPTIONAL (default empty): only adapters with task introspection
    /// implement it (Claude stream-json reflects its `system:task_*` events).
    /// `/status` lists these under the current working session.
    async fn running_tasks(&self, _h: &ThreadHandle) -> Vec<RunningTask> {
        Vec::new()
    }

    /// Cheap liveness probe: is this thread's underlying process / channel
    /// still alive and able to accept a turn RIGHT NOW? Default `true`.
    ///
    /// Adapters whose thread can silently die out from under a held
    /// [`ThreadHandle`] — the stream-json `claude` child exiting on crash /
    /// OOM / a long idle window — override this so the gateway can probe before
    /// submitting and transparently RESUME-by-session-id (re-`start_thread`,
    /// which is resume-aware) instead of shipping the turn into a closed pipe
    /// and surfacing a "writer closed (child exited)" send failure. A `false`
    /// answer means "needs resume" (the conversation is recoverable from its
    /// transcript), NOT "gone forever". Must NOT block / do IO — it is called
    /// on the hot submit path.
    fn thread_is_live(&self, _h: &ThreadHandle) -> bool {
        true
    }

    /// Interrupt the session's CURRENTLY-RUNNING turn **without destroying
    /// the session** — the context survives, so the user can immediately
    /// `/model` switch / send a follow-up. This is the non-destructive twin
    /// of [`close_thread`] (which kills the whole session): only the in-flight
    /// turn stops; the session stays live + idle.
    ///
    /// **Out-of-band by contract**: the interrupt MUST reach the vendor even
    /// while a turn is mid-stream (running tools) — it cannot queue behind the
    /// running turn. The stream-json transport delivers it as a bidirectional
    /// `interrupt` control_request; the tmux TUI sends an ESC keypress to the
    /// pane; codex calls the `turn/interrupt` RPC on the active turn. The
    /// gateway reaches this via a dedicated `/interrupt` command (NOT the
    /// submit/turn queue), so the call never serializes behind the turn.
    ///
    /// **Red line**: this does NOT violate "never PROACTIVELY kill a turn".
    /// A user-typed `/interrupt` is an explicit user command (exactly like
    /// `/stop`), not the daemon autonomously killing work — the watchdog stays
    /// WARN-only. The default impl is an honest [`HarnessError::NotImplemented`]
    /// so an adapter without an interrupt mechanism degrades cleanly (the
    /// gateway surfaces the reason) instead of silently doing nothing.
    async fn interrupt_turn(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let _ = h;
        Err(HarnessError::NotImplemented {
            reason: format!(
                "interrupt not supported for the `{}` adapter (only the current \
                 turn can be interrupted on stream-json / terminal / codex; use \
                 /stop to end the session)",
                self.name()
            ),
        })
    }

    /// Push an explicit user rename to the VENDOR's own session-title surface,
    /// so a session renamed in ccteam reads the same way in the vendor's native
    /// UI (`claude --resume`'s picker, `codex`'s thread list).
    ///
    /// ccteam's `meta.json` stays the SoT for the title — this is a one-way
    /// mirror of an explicit `TitleSource::User` rename, never of the
    /// rule-based auto-title or of a title ccteam ADOPTED from the vendor
    /// (mirroring those back would fight the vendor's own heuristics).
    ///
    /// **Not a prompt** (red line): every implementation writes vendor session
    /// METADATA through that vendor's documented external-writer path — the
    /// Claude transcript's `custom-title` entry (the SDK `renameSession`
    /// contract), Codex's `thread/name/set` RPC. Nothing enters the model's
    /// conversation and no pane is driven.
    ///
    /// The default answer is [`TitleSync::Unsupported`]: a vendor with no title
    /// surface degrades HONESTLY (the frontends say so) rather than silently
    /// pretending the push landed. `Err` is reserved for a surface that exists
    /// and genuinely failed.
    async fn set_session_title(
        &self,
        target: &SessionTitleTarget,
        title: &str,
    ) -> Result<TitleSync, HarnessError> {
        let _ = (target, title);
        Ok(TitleSync::Unsupported)
    }
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
    /// The thread's underlying process / channel has died and the turn was
    /// **not** delivered — distinct from [`Self::SubmitFailed`] (a turn the
    /// harness actively *rejected*, which must NOT be blindly retried). The
    /// caller may resume-by-session-id and retry EXACTLY once; because nothing
    /// was sent, the retry cannot double-submit. stream-json returns this when
    /// its `claude` child has exited (registry miss / writer closed) before the
    /// line was handed off.
    #[error("thread died: {0}")]
    ThreadDied(String),
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
            AgentVendor::Grok => "grok",
            AgentVendor::Opencode => "opencode",
            AgentVendor::Kimi => "kimi",
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

pub fn pluck<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

pub fn pluck_str(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    pluck(value, path)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn pluck_f64(value: &serde_json::Value, path: &[&str]) -> Option<f64> {
    pluck(value, path).and_then(|v| v.as_f64())
}

pub fn pluck_pct(value: &serde_json::Value, path: &[&str]) -> Option<u8> {
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
    fn permission_mode_default_is_skip() {
        // The default everywhere is skip (preserves today's behavior).
        assert_eq!(PermissionMode::default(), PermissionMode::Skip);
        assert!(!PermissionMode::default().is_hitl());
    }

    #[test]
    fn permission_mode_parse_opt_covers_all_cases() {
        // None / empty / "skip" → Skip.
        assert_eq!(PermissionMode::parse_opt(None), Ok(PermissionMode::Skip));
        assert_eq!(
            PermissionMode::parse_opt(Some("")),
            Ok(PermissionMode::Skip)
        );
        assert_eq!(
            PermissionMode::parse_opt(Some("  ")),
            Ok(PermissionMode::Skip)
        );
        assert_eq!(
            PermissionMode::parse_opt(Some("skip")),
            Ok(PermissionMode::Skip)
        );
        // "hitl" (with whitespace) → Hitl.
        assert_eq!(
            PermissionMode::parse_opt(Some("hitl")),
            Ok(PermissionMode::Hitl)
        );
        assert_eq!(
            PermissionMode::parse_opt(Some(" hitl ")),
            Ok(PermissionMode::Hitl)
        );
        // A bad token is an Err (a typo surfaces, not a silent downgrade).
        assert!(PermissionMode::parse_opt(Some("bogus")).is_err());
        assert!(PermissionMode::parse_opt(Some("auto")).is_err());
    }

    #[test]
    fn permission_mode_serde_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&PermissionMode::Skip).unwrap(),
            "\"skip\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionMode::Hitl).unwrap(),
            "\"hitl\""
        );
        assert_eq!(PermissionMode::Hitl.as_str(), "hitl");
        let back: PermissionMode = serde_json::from_str("\"hitl\"").unwrap();
        assert_eq!(back, PermissionMode::Hitl);
    }

    #[test]
    fn session_protocol_default_parse_and_serde() {
        assert_eq!(SessionProtocol::default(), SessionProtocol::StreamJson);
        assert!(SessionProtocol::default().is_stream_json());
        // None / empty / "stream-json" → StreamJson.
        assert_eq!(
            SessionProtocol::parse_opt(None),
            Ok(SessionProtocol::StreamJson)
        );
        assert_eq!(
            SessionProtocol::parse_opt(Some("")),
            Ok(SessionProtocol::StreamJson)
        );
        assert_eq!(
            SessionProtocol::parse_opt(Some("stream-json")),
            Ok(SessionProtocol::StreamJson)
        );
        assert_eq!(
            SessionProtocol::parse_opt(Some(" terminal ")),
            Ok(SessionProtocol::Terminal)
        );
        assert!(SessionProtocol::parse_opt(Some("bogus")).is_err());
        // Wire string is kebab-case.
        assert_eq!(SessionProtocol::StreamJson.as_str(), "stream-json");
        assert_eq!(SessionProtocol::Terminal.as_str(), "terminal");
        assert_eq!(
            serde_json::to_string(&SessionProtocol::StreamJson).unwrap(),
            "\"stream-json\""
        );
        let back: SessionProtocol = serde_json::from_str("\"terminal\"").unwrap();
        assert_eq!(back, SessionProtocol::Terminal);
    }

    #[test]
    fn spawn_ctx_default_permission_mode_is_skip() {
        // `..Default::default()` on a SpawnCtx literal must leave skip.
        let ctx = SpawnCtx::default();
        assert_eq!(ctx.permission_mode, PermissionMode::Skip);
    }

    #[test]
    fn format_tokens_humanizes_round_and_fractional() {
        // P3: whole-thousand / whole-million read without a trailing `.0`.
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(188_000), "188k");
        // Fractional keeps one decimal.
        assert_eq!(format_tokens(1_234), "1.2k");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        // Sub-1000 verbatim.
        assert_eq!(format_tokens(188), "188");
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn context_usage_render_absolute_plus_percent() {
        let u = ContextUsage::known(188_000, 1_000_000, ContextSource::Derived);
        assert_eq!(u.render(), "188k / 1M (19%)");
        let baseline = ContextUsage::known(188_000, 200_000, ContextSource::Reported);
        assert_eq!(baseline.render(), "188k / 200k (94%)");
        // Unknown window → no percent.
        let unknown = ContextUsage::known(5_000, 0, ContextSource::Derived);
        assert_eq!(unknown.render(), "5k (window unknown)");
    }

    /// Provenance must NOT leak into a known value's rendering — a derived
    /// number reads exactly like a reported one, so every statusline surface
    /// keeps agreeing byte-for-byte.
    #[test]
    fn context_usage_render_ignores_source_for_known_values() {
        let rendered: Vec<String> = [
            ContextSource::Reported,
            ContextSource::Derived,
            ContextSource::Probed,
        ]
        .into_iter()
        .map(|src| ContextUsage::known(188_000, 1_000_000, src).render())
        .collect();
        assert!(
            rendered.iter().all(|r| r == "188k / 1M (19%)"),
            "{rendered:?}"
        );
    }

    /// The regression this type exists for: a known window with no reported
    /// occupancy must never render as `0 (0%)`. A resumed session has an empty
    /// counter and a full context — claiming 0% there is a lie, not a default.
    #[test]
    fn context_usage_unknown_occupancy_never_renders_as_zero() {
        let u = ContextUsage::window_only(500_000);
        assert_eq!(u.render(), "— / 500k (usage unknown)");
        assert_eq!(u.pct(), None);
        assert_eq!(u.source, ContextSource::Unknown);
        // Nothing known at all → a bare dash.
        assert_eq!(ContextUsage::default().render(), "—");
        assert_eq!(ContextUsage::default().pct(), None);
    }

    #[test]
    fn thread_status_suffix_combines_model_and_ctx() {
        let full = ThreadStatus {
            model: Some("claude-opus-4-8[1m]".into()),
            context: Some(ContextUsage::known(
                188_000,
                1_000_000,
                ContextSource::Derived,
            )),
            effort: None,
            goal: None,
        };
        assert_eq!(
            full.status_suffix().as_deref(),
            Some("claude-opus-4-8[1m] · ctx 188k / 1M (19%)")
        );
        // With effort: it sits between the model and the context segment.
        let with_effort = ThreadStatus {
            effort: Some("xhigh".into()),
            ..full.clone()
        };
        assert_eq!(
            with_effort.status_suffix().as_deref(),
            Some("claude-opus-4-8[1m] · xhigh · ctx 188k / 1M (19%)")
        );
        // With a goal: it trails the context segment (🎯 active / ✅ met).
        let with_goal = ThreadStatus {
            goal: Some(GoalStatus {
                condition: "ship the payment module".into(),
                met: false,
            }),
            ..full.clone()
        };
        assert_eq!(
            with_goal.status_suffix().as_deref(),
            Some("claude-opus-4-8[1m] · ctx 188k / 1M (19%) · 🎯 ship the payment module")
        );
        let met_goal = ThreadStatus {
            goal: Some(GoalStatus {
                condition: "ship the payment module".into(),
                met: true,
            }),
            ..full.clone()
        };
        assert_eq!(
            met_goal.status_suffix().as_deref(),
            Some("claude-opus-4-8[1m] · ctx 188k / 1M (19%) · ✅ ship the payment module")
        );
        // Model only.
        let model_only = ThreadStatus {
            model: Some("gpt-5".into()),
            context: None,
            effort: None,
            goal: None,
        };
        assert_eq!(model_only.status_suffix().as_deref(), Some("gpt-5"));
        // Default (statusless) → nothing to append.
        assert_eq!(ThreadStatus::default().status_suffix(), None);
    }

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
    fn turn_routing_round_trips_without_an_implicit_policy_default() {
        for routing in [TurnRouting::Inject, TurnRouting::Queue] {
            let json = serde_json::to_string(&routing).unwrap();
            let decoded: TurnRouting = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, routing);
        }
    }

    #[test]
    fn turn_submission_separates_vendor_turn_from_unique_input_receipt() {
        let turn = TurnId::new("vendor-turn-1");
        let started = TurnSubmission::started(turn.clone());
        let injected = TurnSubmission::injected(turn.clone());
        assert_eq!(started.turn_id, turn);
        assert_eq!(injected.turn_id, turn);
        assert_ne!(started.input_id, injected.input_id);
        assert_eq!(started.disposition, TurnDisposition::Started);
        assert_eq!(injected.disposition, TurnDisposition::Injected);
    }

    #[test]
    fn turn_submission_completion_guard_releases_only_after_registration() {
        struct FlagOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for FlagOnDrop {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut submission = TurnSubmission::injected(TurnId::new("vendor-turn-1"))
            .hold_completion(FlagOnDrop(std::sync::Arc::clone(&dropped)));
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        submission.release_completion();
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        submission.release_completion();
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
