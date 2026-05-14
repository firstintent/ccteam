//! V0.4.0 F61 — `HarnessAdapter` trait + `ClaudeCodeAdapter` (thin).
//!
//! Strategic pivot: V0.3.1 wired Claude Code as a `tmux new-session`
//! host with a `~/.claude/statusline-command.sh` wrapper teeing stdin
//! into `ccteam hook harness-snapshot` for status. F61 retires that
//! pipeline in favor of Claude Code's native background-job surface:
//!
//! ```text
//! ccteam orchestrator
//!   ↓ spawn_session
//! Command::new("claude").args(["--bg", "--agent", role, ...])
//!   ↓ stdout (first line JSON)
//! { "job_id": "<id>", ... }
//!   ↓ ingest_snapshot
//! read ~/.claude/jobs/<job_id>/state.json
//!   ↓ parse_cc_state_json
//! HarnessSnapshot (status, model, ctx_pct, cost_usd, turn_count)
//! ```
//!
//! **Architectural red lines** (CLAUDE.md §三, PRD §3.3):
//!
//! - The snapshot pipeline is **presentation-only**. Nothing in
//!   `orchestrator.rs` consumes a `HarnessSnapshot` — `progress.jsonl`
//!   remains the single source of truth for state transitions.
//! - `shutdown_session` is the **only** path that kills a long-running
//!   session, and it must be invoked exclusively from a user-initiated
//!   `ccteam session rm` (F49). Watchdog / silence classifier never
//!   call it.
//! - `ccteam-core` does not know team-name literals. The adapter knows
//!   its own harness identifier (`"claude-code"`, `"codex"`) but never
//!   inspects team kinds — that's the orchestrator / web layer's job.
//! - `ClaudeCodeAdapter` no longer depends on tmux (only `CodexAdapter`
//!   still does — codex lacks a `--bg` surface today). All tmux session
//!   plumbing for codex is in this file's CodexAdapter section.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::CcteamPaths;
use crate::tmux::{session_name_for_slug, TmuxSession};

/// Default sid for projects that haven't opted into multi-session
/// (V0.3 single-session projects). Kept as a `pub const` for callers
/// (F49 path resolution, doctor smoke tests) that need to refer to the
/// single-session default without hard-coding the string.
pub const DEFAULT_CLAUDE_SID: &str = "claude-1";

/// Environment override for the directory under which Claude Code
/// writes per-job `state.json` files. Defaults to `~/.claude/jobs/` when
/// unset. Tests override this to a tempdir so the F61 `ingest_snapshot`
/// path doesn't read the live user's home directory.
pub const CLAUDE_JOBS_DIR_ENV: &str = "CCTEAM_CLAUDE_JOBS_DIR";

/// Environment override for the `claude` binary path. Tests set this to
/// a fake script that emits a deterministic `{"job_id":"..."}` line on
/// stdout so `spawn_session` is hermetic. Production reads `claude` from
/// `$PATH` when this is unset.
pub const CLAUDE_BIN_ENV: &str = "CCTEAM_CLAUDE_BIN";

/// Adapter contract for any LLM "harness" ccteam may host. V0.4.0 F61
/// rewrites [`ClaudeCodeAdapter`] to use Claude Code's native
/// `--bg --agent <role>` background-job surface (no tmux, no statusline
/// wrapper); [`CodexAdapter`] (F62) keeps tmux + capture-pane status
/// reading because codex has no equivalent background-runner protocol.
///
/// Implementations are stateless — a single `'static` instance can be
/// shared across all sessions of that harness type. Callers choose the
/// adapter via the `harness` field on `team.yaml::sessions[]` (F47) or
/// the F49 `ccteam session add --harness <kind>` flag.
pub trait HarnessAdapter: Send + Sync {
    /// Stable identifier, e.g. `"claude-code"`, `"codex"`. Used as the
    /// `harness` discriminator on snapshots + session handles, and in
    /// `HarnessError::NotImplemented::harness`.
    fn name(&self) -> &'static str;

    /// Ingest a fresh status snapshot from the harness's native
    /// channel. For F61 [`ClaudeCodeAdapter`] this is the contents of
    /// `~/.claude/jobs/<job_id>/state.json`; for [`CodexAdapter`] it's
    /// a tmux pane capture body. Returns a normalized
    /// [`HarnessSnapshot`] the web layer can consume without knowing
    /// which harness produced it.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError>;

    /// Best-effort enumeration of in-flight subagents. Returns an
    /// empty `Vec` when the harness doesn't surface this data —
    /// V0.4.0 Claude Code returns empty until upstream API exposes
    /// structured subagent progress (PRD §3.3 deferred).
    fn subagent_states(&self, _snapshot: &HarnessSnapshot) -> Vec<SubagentState> {
        Vec::new()
    }

    /// Spawn a new harness session. F61 [`ClaudeCodeAdapter`] launches
    /// `claude --bg --agent <role>` as a child process and parses the
    /// resulting `job_id` from stdout; [`CodexAdapter`] (F62) wraps
    /// `tmux new-session -d` so the codex CLI's interactive REPL stays
    /// attachable.
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError>;

    /// Graceful shutdown. The ONLY caller in V0.4.0 is the user's
    /// explicit `ccteam session rm` invocation (F49) — never silent
    /// kill (red line CLAUDE.md §三 / PRD §3.3).
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError>;
}

/// Normalized status snapshot from any harness. F61 sources this for
/// `claude-code` by reading `~/.claude/jobs/<job_id>/state.json`
/// (Claude Code's native job state file). Field set is the minimum
/// union covered by every shipping harness; unknown / missing fields
/// fall back to defaults rather than failing — `raw` carries the full
/// upstream JSON for forward-compat.
///
/// `harness` is `String` (not `&'static str`) so the struct
/// round-trips through `serde_json` cleanly. The trait method
/// `name() -> &'static str` is the free-standing const-string
/// surface; constructors copy it into `harness` via `to_string()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    /// Harness identifier — matches [`HarnessAdapter::name`].
    pub harness: String,
    /// Human-readable model name (`"Claude Sonnet 4.5"`, etc.).
    pub model_display_name: String,
    /// Context window utilization, 0..=100. Missing fields coerce to 0.
    pub context_used_pct: u8,
    /// Cumulative session cost in USD. Coerced to 0.0 when the
    /// harness doesn't expose cost data.
    pub cost_usd_total: f64,
    /// Five-hour rate-limit utilization (Claude Code field). `None`
    /// when the harness doesn't surface this surface — V0.4.0 F61's
    /// `state.json` shape may omit rate-limit data on early jobs.
    pub rate_limit_pct: Option<u8>,
    /// Session cwd as reported by the harness. `None` for sessions
    /// whose state.json predates the `workdir` field.
    pub cwd: Option<PathBuf>,
    /// Full upstream JSON. Carried verbatim so future harness
    /// upgrades don't require a ccteam release to surface new fields.
    pub raw: serde_json::Value,
    /// Wall-clock time the snapshot was ingested.
    pub captured_at: DateTime<Utc>,
}

/// Optional per-snapshot subagent state. V0.4.0 always empty — the
/// shape is reserved for a future PR when Claude Code's state.json
/// exposes structured subagent progress (PRD §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentState {
    /// Subagent type ("main" / "general-purpose" / "code-reviewer" / …).
    pub kind: String,
    /// Human label, when distinct from `kind`.
    pub label: Option<String>,
    /// How long the subagent has been running.
    pub running_for: Option<Duration>,
    /// Cumulative input tokens.
    pub tokens_in: Option<u64>,
    /// Cumulative output tokens.
    pub tokens_out: Option<u64>,
}

/// Inputs for [`HarnessAdapter::spawn_session`]. F61 adds the `role`
/// field for `claude --bg --agent <role>` dispatch; `extra_args`
/// remains for F62's CodexAdapter (codex callers thread sandbox /
/// prompt args verbatim through the field).
#[derive(Debug, Clone)]
pub struct SpawnOpts {
    /// Harness identifier (`"claude-code"`, `"codex"`). Should match
    /// the adapter's [`HarnessAdapter::name`].
    pub harness: &'static str,
    /// Project slug (e.g. `"flex-foo"` after F22's team prefix).
    pub slug: String,
    /// Session id (e.g. `"claude-1"`, `"codex-2"`).
    pub sid: String,
    /// Working directory for the spawned session — typically
    /// `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/`.
    pub cwd: PathBuf,
    /// V0.4.0 F61 — agent role name for `claude --bg --agent <role>`
    /// dispatch. Resolves against `.claude/agents/<role>.md` in the
    /// project tree (or the harness's own role catalogue). Ignored by
    /// [`CodexAdapter`] (codex doesn't have a role surface yet).
    pub role: String,
    /// Extra args appended to the harness command line. Retained for
    /// [`CodexAdapter`] (F62) which uses it for `exec --sandbox …`,
    /// prompt strings, etc. Not used by F61 ClaudeCodeAdapter.
    pub extra_args: Vec<String>,
}

/// Outcome of [`HarnessAdapter::spawn_session`]. Owned by the caller
/// (F49 master `state.json::sessions[]`); the adapter never caches.
/// F61 adds `job_id` for the Claude Code `--bg` background-job
/// identifier — codex sessions leave this `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHandle {
    /// tmux session name. F61 ClaudeCodeAdapter no longer uses tmux,
    /// so this stays a placeholder (`ccteam-<slug>-<sid>`) for callers
    /// (F49 state.json registry) that still index by name. Codex
    /// sessions populate this with the real tmux session name.
    pub tmux_session: String,
    /// Harness identifier — matches [`HarnessAdapter::name`].
    pub harness: String,
    /// Session id, mirroring [`SpawnOpts::sid`].
    pub sid: String,
    /// V0.4.0 F61 — Claude Code background-job id (from `claude --bg`
    /// stdout). `None` for codex sessions and for legacy state.json
    /// rows written before F61.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Active pane / process PID, when the harness discloses it.
    /// `None` immediately post-spawn before the harness has a chance
    /// to fork.
    pub pid: Option<u32>,
    /// Wall-clock start time.
    pub started_at: DateTime<Utc>,
}

/// V0.4.0 F61 error type for every fallible [`HarnessAdapter`]
/// surface. `NotImplemented` carries `&'static str` fields so const
/// callsites compile to a non-allocating literal.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// JSON parse / shape mismatch on the harness state channel.
    #[error("snapshot ingest failed: {0}")]
    IngestFailed(String),
    /// Process / tmux failure during `spawn_session`.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    /// `shutdown_session` failure — SIGTERM rejected or tmux refused
    /// the kill request.
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),
    /// Adapter declares the surface unsupported. Retained on the enum
    /// for any future harness whose integration lands in stages; the
    /// `&'static str` payloads keep const-string callsites
    /// allocation-free.
    #[error("harness '{harness}' not implemented: {reason}")]
    NotImplemented {
        harness: &'static str,
        reason: &'static str,
    },
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
// ClaudeCodeAdapter — F61 thin (claude --bg + state.json reader)
// =====================================================================

/// V0.4.0 F61 [`HarnessAdapter`] implementation for Anthropic's Claude
/// Code. Stateless zero-size struct — share one `'static` instance.
///
/// Replaces the V0.3.1 F46 design (tmux + statusline wrapper). All
/// session lifecycle now flows through Claude Code's native `--bg`
/// background-job surface:
///
/// - **spawn_session**: `claude --bg --agent <role> --output-format
///   stream-json --workdir <cwd>` as a child process; first stdout
///   line is JSON containing `job_id`.
/// - **ingest_snapshot**: reads `~/.claude/jobs/<job_id>/state.json`
///   (or `$CCTEAM_CLAUDE_JOBS_DIR/<job_id>/state.json` in tests) and
///   parses Claude Code's native state shape (`status`, `model`,
///   `context_pct`, `cost_usd`, `turn_count`).
/// - **shutdown_session**: SIGTERM via `libc::kill` on the pid from
///   `state.json`; ESRCH ("no such process") is idempotent success.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// Build a fresh `ClaudeCodeAdapter`. Const so users can write
    /// `static CC: ClaudeCodeAdapter = ClaudeCodeAdapter::new();`.
    pub const fn new() -> Self {
        Self
    }
}

impl HarnessAdapter for ClaudeCodeAdapter {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    /// Parse a Claude Code `state.json` body. `raw` is the file
    /// contents (caller resolved the path via [`state_json_path`]).
    /// Unknown / missing fields fall back to defaults rather than
    /// failing — only outright malformed JSON returns `IngestFailed`.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        parse_cc_state_json(raw)
    }

    /// Spawn `claude --bg --agent <role>` with `cwd` set via process
    /// `current_dir` (Claude Code has no `--workdir` flag — the real CLI
    /// reads cwd from the child process's working directory and writes
    /// it back into `state.json::cwd`). Parses the first non-empty
    /// stdout line of the shape `backgrounded · <daemonShort>` and
    /// records the short id as `SessionHandle::job_id`.
    ///
    /// Test override: `$CCTEAM_CLAUDE_BIN` swaps in a fake script for
    /// hermetic unit tests. Production reads `claude` from `$PATH`.
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        if opts.role.is_empty() {
            return Err(HarnessError::SpawnFailed(
                "claude --bg requires a non-empty role (SpawnOpts::role)".into(),
            ));
        }

        let bin = std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string());
        let mut cmd = Command::new(&bin);
        // `--dangerously-skip-permissions` is the production pattern per
        // CLAUDE.md §三 — without it `claude --bg` parks at the workspace
        // trust dialog and the session never makes forward progress.
        // Project isolation comes from `cwd` (per-project dir), not from
        // a permission prompt the bg session cannot answer.
        cmd.arg("--bg")
            .arg("--agent")
            .arg(&opts.role)
            .arg("--dangerously-skip-permissions")
            .current_dir(&opts.cwd);

        let output = cmd
            .output()
            .map_err(|err| HarnessError::SpawnFailed(format!("invoke {bin}: {err}")))?;

        if !output.status.success() {
            return Err(HarnessError::SpawnFailed(format!(
                "claude --bg exited non-zero ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let job_id = parse_backgrounded_short_id(&stdout).ok_or_else(|| {
            HarnessError::SpawnFailed(format!(
                "claude --bg stdout missing `backgrounded · <id>` line: {}",
                stdout.trim()
            ))
        })?;

        // Preserve the per-sid name shape so F49 state.json registry
        // entries (indexed by `tmux_session`) stay stable across the
        // F61 cutover. The string is now a logical identifier — F61
        // ClaudeCodeAdapter does not own a tmux session, but downstream
        // code (logs, state.json, web UI labels) still expects this
        // form. CodexAdapter (which keeps tmux) populates it identically.
        let tmux_session = format!("{}-{}", session_name_for_slug(&opts.slug), opts.sid)
            .trim_start_matches('-')
            .to_string();

        Ok(SessionHandle {
            tmux_session,
            harness: self.name().to_string(),
            sid: opts.sid,
            job_id: Some(job_id),
            // pid is sourced from the state.json the job writes itself;
            // we leave it None at spawn time and let the F66 observer
            // (or a synchronous follow-up `ingest_snapshot` call) fill it.
            pid: None,
            started_at: Utc::now(),
        })
    }

    /// Graceful shutdown via SIGTERM to the pid recorded in
    /// `state.json`. Idempotent: if `state.json` is missing or the pid
    /// no longer exists (ESRCH), returns `Ok(())` without erroring.
    /// Falls back to a permissive shutdown if `job_id` is `None` (the
    /// session was a F62 codex row routed to the wrong adapter, or a
    /// pre-F61 legacy row) — matches the `shutdown_session` contract:
    /// silent success on best-effort termination.
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError> {
        let Some(job_id) = handle.job_id.as_deref() else {
            tracing::warn!(
                sid = %handle.sid,
                "ClaudeCodeAdapter::shutdown_session: handle has no job_id; \
                 nothing to terminate (legacy row?)"
            );
            return Ok(());
        };
        let path = state_json_path(job_id);
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    job_id = %job_id,
                    "ClaudeCodeAdapter::shutdown_session: state.json missing — \
                     job already cleaned up"
                );
                return Ok(());
            }
            Err(err) => {
                return Err(HarnessError::ShutdownFailed(format!(
                    "read {}: {err}",
                    path.display()
                )))
            }
        };

        let pid = match parse_pid_from_state(&raw) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    job_id = %job_id,
                    "ClaudeCodeAdapter::shutdown_session: state.json lacks pid; \
                     nothing to terminate"
                );
                return Ok(());
            }
        };

        sigterm_pid(pid)
            .map_err(|err| HarnessError::ShutdownFailed(format!("SIGTERM pid {pid}: {err}")))
    }
}

/// Scan `claude --bg` stdout for the `backgrounded · <id>` marker line
/// and return the short hex id. Real CLI output:
///
/// ```text
/// warning: no agent named 'explorer' — spawning with default template
/// backgrounded · 9432490e
///   claude agents             list sessions
///   claude attach 9432490e    open in this terminal
///   ...
/// ```
///
/// Picks the first line that starts with `backgrounded` (after trimming
/// leading whitespace) and returns the last whitespace-separated token
/// — that's the id Claude prints. Returns `None` if no such line
/// exists, signalling the CLI shape drifted (caller bubbles as
/// `SpawnFailed`).
fn parse_backgrounded_short_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("backgrounded") {
            continue;
        }
        let last = trimmed.split_whitespace().next_back()?;
        // Accept anything non-empty — production prints 8-hex (e.g.
        // `9432490e`), but we don't lock to that shape so a future
        // longer id doesn't break the parser.
        if last.is_empty() || last == "backgrounded" {
            return None;
        }
        return Some(last.to_string());
    }
    None
}

/// Resolve the absolute path to `state.json` for a Claude Code
/// background job. Honors `$CCTEAM_CLAUDE_JOBS_DIR` for hermetic tests;
/// otherwise resolves under `~/.claude/jobs/<job_id>/state.json`.
///
/// Returns a `PathBuf` unconditionally — callers that don't already
/// own the home directory get a best-effort path rooted at `/` when
/// HOME is unset (rare, but happens in CI / minimal sandboxes). Any
/// downstream read will then fail loudly with `ENOENT`, which is the
/// right behavior: bubbling up "no home" silently would be worse.
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
///
/// Tolerant of missing / extra fields — only outright JSON-parse
/// failures bubble. The real CC `--bg` shape (probed against
/// `claude 2.1.141` on 2026-05-14):
///
/// ```json
/// {
///   "state": "working" | "done" | "failed" | "crashed",
///   "tempo": "active" | "idle",
///   "cwd": "/tmp",
///   "daemonShort": "9432490e",
///   "sessionId": "9432490e-90f8-...",
///   "cliVersion": "2.1.141",
///   "template": "bg",
///   "intent": "...",
///   "output": { "result": "..." },
///   "createdAt": "2026-05-14T15:12:43.111Z",
///   "updatedAt": "2026-05-14T15:13:00.579Z"
/// }
/// ```
///
/// The legacy F61 schema (`status` / `model` / `context_pct` /
/// `cost_usd` / `workdir`) is also accepted as a fallback in case a
/// future Claude Code build re-exposes those fields. The raw `Value`
/// preserves everything for downstream consumers.
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
/// (parse failures are swallowed because callers — `shutdown_session`
/// — must remain idempotent).
fn parse_pid_from_state(raw: &str) -> Option<i32> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value.get("pid").and_then(|v| v.as_i64()).and_then(|n| {
        if n > 0 && n <= i32::MAX as i64 {
            Some(n as i32)
        } else {
            None
        }
    })
}

/// SIGTERM the given pid. Returns `Ok(())` on success or if the
/// process no longer exists (`ESRCH`). Other errors bubble up as
/// `std::io::Error` so the caller can surface them via
/// `HarnessError::ShutdownFailed`.
fn sigterm_pid(pid: i32) -> std::io::Result<()> {
    // SAFETY: `libc::kill` is FFI-safe with any pid / signal pair.
    // We map ESRCH (no such process) to Ok so shutdown is idempotent.
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // ESRCH = "no such process" — already dead, idempotent success.
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err)
}

// =====================================================================
// CodexAdapter (V0.4.0 F62 — real tmux + codex CLI implementation)
// =====================================================================

/// V0.4.0 F62 [`HarnessAdapter`] implementation for OpenAI's `codex`
/// CLI. Codex has no `--bg` background-job equivalent today, so this
/// adapter keeps the tmux long-session container that V0.3.1 F46
/// originally shared with `ClaudeCodeAdapter`.
///
/// Architecture (`docs/v0-4-0/prd.md` §6.5 + dev-plan §4):
///
/// - **Transport**: tmux long-session. A detached tmux session named
///   `ccteam-<slug>-<sid>` hosts the `codex` process; `pane_pid` is
///   read once post-spawn for liveness checks. Distinct from F61's
///   ClaudeCode `--bg` job_id path.
/// - **Status channel**: codex has no native statusline / stdin-JSON
///   surface. [`Self::ingest_snapshot`] reads the last few lines of
///   the tmux pane via `capture-pane -p` and greps for a
///   `CODEX_STATUS: <json>` marker line that the codex agent (or the
///   workflow's role .md) is expected to print after each turn. When
///   no marker is found, the adapter returns a permissive default
///   snapshot rather than failing — the snapshot pipeline is
///   presentation-only (red line CLAUDE.md §三 / PRD §3.3).
/// - **State observation file**: a `~/.ccteam/codex/<sid>/state.json`
///   is written at spawn time for the F66 watcher.
/// - **Shutdown**: graceful `q\r` send-keys (codex's documented
///   quit-bind), 500ms grace period, then `tmux kill-session -t
///   <name>` fallback. Targets only the named session.
///
/// Stateless zero-size — share one `'static` instance.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

/// Marker line the codex agent prints in its tmux pane to publish
/// state to the observer (PRD §6.5 + dev-plan §3.2). Spelled out as
/// `&'static str` so callers (tests, future code-gen) can pattern-match
/// the same literal.
pub const CODEX_STATUS_MARKER: &str = "CODEX_STATUS:";

/// Number of trailing pane lines a codex observer is expected to
/// capture before feeding the pane body to
/// [`CodexAdapter::ingest_snapshot`]. Five lines balances signal vs.
/// pane noise (PRD §6.5).
pub const CODEX_STATUS_TAIL_LINES: usize = 5;

impl CodexAdapter {
    /// Build a fresh `CodexAdapter`. Const so callers can write
    /// `static CX: CodexAdapter = CodexAdapter::new();`.
    pub const fn new() -> Self {
        Self
    }

    /// Resolve `~/.ccteam/codex/<sid>/state.json` for a session. The
    /// directory mirrors the F46 `harness/` dual-write layout — one
    /// directory per harness so observer code can iterate cleanly.
    ///
    /// Returns `None` if `CcteamPaths::from_env()` fails (HOME unset
    /// in some test contexts).
    fn state_json_path(sid: &str) -> Option<PathBuf> {
        let paths = CcteamPaths::from_env().ok()?;
        Some(paths.root.join("codex").join(sid).join("state.json"))
    }

    /// Best-effort write of the initial `state.json` post-spawn.
    /// Silent failure: returns `()` even if the directory create or
    /// rename failed (we logged the warn) so `spawn_session` doesn't
    /// propagate a non-fatal observer error to the caller.
    fn write_initial_state(sid: &str, pid: Option<u32>) {
        let Some(target) = Self::state_json_path(sid) else {
            return;
        };
        let body = serde_json::json!({
            "status": "starting",
            "pid": pid,
            "model": "codex",
            "context_pct": 0,
            "cost_usd": 0.0,
        });
        let raw = match serde_json::to_string(&body) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, sid = %sid, "codex state.json serialise");
                return;
            }
        };
        if let Some(parent) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %err, path = %parent.display(), "codex state.json mkdir");
                return;
            }
        }
        if let Err(err) = std::fs::write(&target, raw.as_bytes()) {
            tracing::warn!(error = %err, path = %target.display(), "codex state.json write");
        }
    }

    /// Parse a tmux `capture-pane -p` body for the `CODEX_STATUS:`
    /// marker line. Returns the JSON payload of the **last** matching
    /// line (most recent status wins). Free function so tests can
    /// drive it without spawning tmux.
    fn parse_status_line(pane: &str) -> Option<serde_json::Value> {
        pane.lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix(CODEX_STATUS_MARKER))
            .and_then(|rest| serde_json::from_str(rest.trim()).ok())
    }

    /// Build a `HarnessSnapshot` from a parsed `CODEX_STATUS:` JSON
    /// payload (or `None` for the fallback shape).
    fn snapshot_from_status(payload: Option<serde_json::Value>) -> HarnessSnapshot {
        let value = payload.unwrap_or(serde_json::Value::Null);
        let model_display_name = pluck_str(&value, &["model"])
            .or_else(|| pluck_str(&value, &["model_display_name"]))
            .unwrap_or_else(|| "codex".to_string());
        let context_used_pct = pluck_pct(&value, &["context_pct"])
            .or_else(|| pluck_pct(&value, &["context_used_pct"]))
            .unwrap_or(0);
        let cost_usd_total = pluck_f64(&value, &["cost_usd"])
            .or_else(|| pluck_f64(&value, &["cost_usd_total"]))
            .unwrap_or(0.0);
        let rate_limit_pct = pluck_pct(&value, &["rate_limit_pct"]);
        let cwd = pluck_str(&value, &["cwd"]).map(PathBuf::from);

        HarnessSnapshot {
            harness: "codex".to_string(),
            model_display_name,
            context_used_pct,
            cost_usd_total,
            rate_limit_pct,
            cwd,
            raw: value,
            captured_at: Utc::now(),
        }
    }
}

impl HarnessAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    /// Ingest a tmux pane capture (passed as `raw`) for codex status
    /// data. Permissive: missing / malformed marker returns the
    /// fallback snapshot rather than failing.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        let payload = Self::parse_status_line(raw);
        Ok(Self::snapshot_from_status(payload))
    }

    /// Spawn a codex tmux session. The command is `codex` with
    /// `opts.extra_args` appended verbatim — callers (F65 meta-agent
    /// MCP `spawn_agent`, the F49 CLI surface) thread role-specific
    /// arguments through `extra_args` to stay schema-stable.
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        let tmux_session = format!("{}-{}", session_name_for_slug(&opts.slug), opts.sid)
            .trim_start_matches('-')
            .to_string();

        let session = TmuxSession::from_name(tmux_session.clone());
        if session.exists() {
            return Err(HarnessError::SpawnFailed(format!(
                "tmux session already exists: {tmux_session} \
                 (sid collision; F49 next_sid_seq accounting drifted)"
            )));
        }

        // Base argv: bare `codex` so the session is interactive by
        // default; F65 passes `exec --sandbox … "--cd" <cwd>` etc. via
        // `extra_args`.
        let mut argv: Vec<String> = vec!["codex".to_string()];
        argv.extend(opts.extra_args.iter().cloned());
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

        session
            .start(&opts.cwd, &argv_refs)
            .map_err(|err| HarnessError::SpawnFailed(format!("tmux new-session: {err:#}")))?;

        let pid = session
            .pane_pid()
            .ok()
            .flatten()
            .and_then(|n| u32::try_from(n).ok());

        Self::write_initial_state(&opts.sid, pid);

        Ok(SessionHandle {
            tmux_session,
            harness: self.name().to_string(),
            sid: opts.sid,
            job_id: None,
            pid,
            started_at: Utc::now(),
        })
    }

    /// Graceful shutdown of a codex tmux session. Sends `q` + Enter,
    /// waits 500ms, then `tmux kill-session -t <handle.tmux_session>`
    /// as the authoritative cleanup. Targets only the named session.
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError> {
        let session = TmuxSession::from_name(handle.tmux_session.clone());
        if !session.exists() {
            return Ok(());
        }
        if let Err(err) = send_codex_quit_keys(&handle.tmux_session) {
            tracing::warn!(
                error = %err,
                session = %handle.tmux_session,
                "CodexAdapter::shutdown_session: send-keys q failed; \
                 falling through to tmux kill-session",
            );
        }
        std::thread::sleep(Duration::from_millis(500));
        if session.exists() {
            session.kill().map_err(|err| {
                HarnessError::ShutdownFailed(format!("tmux kill-session: {err:#}"))
            })?;
        }
        Ok(())
    }
}

/// Send `q` + Enter to the named tmux session — codex's standard
/// quit keybinding.
fn send_codex_quit_keys(tmux_session: &str) -> std::io::Result<()> {
    let send_lit = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "-l", "--", "q"])
        .output()?;
    if !send_lit.status.success() {
        return Err(std::io::Error::other(format!(
            "tmux send-keys -l q: {}",
            String::from_utf8_lossy(&send_lit.stderr)
        )));
    }
    let send_enter = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "Enter"])
        .output()?;
    if !send_enter.status.success() {
        return Err(std::io::Error::other(format!(
            "tmux send-keys Enter: {}",
            String::from_utf8_lossy(&send_enter.stderr)
        )));
    }
    Ok(())
}

// =====================================================================
// Plucker helpers — tolerant of missing / mistyped fields
// =====================================================================

fn pluck<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

fn pluck_str(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    pluck(value, path)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn pluck_f64(value: &serde_json::Value, path: &[&str]) -> Option<f64> {
    pluck(value, path).and_then(|v| v.as_f64())
}

fn pluck_pct(value: &serde_json::Value, path: &[&str]) -> Option<u8> {
    pluck(value, path).and_then(|v| {
        v.as_u64()
            .map(|n| n.min(100) as u8)
            .or_else(|| v.as_f64().map(|n| n.clamp(0.0, 100.0).round() as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- HarnessSnapshot round-trip -----

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

    // ----- parse_cc_state_json -----

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
        assert_eq!(
            snap.cwd.as_deref(),
            Some(std::path::Path::new("/home/u/projects/dev-foo"))
        );
        // turn_count + status preserved in raw for F66 / web layer.
        assert_eq!(snap.raw["turn_count"], 17);
        assert_eq!(snap.raw["status"], "running");
    }

    #[test]
    fn parse_state_json_missing_cost_falls_back_to_zero() {
        let raw = r#"{"model":"Claude Haiku 4.5","context_pct":5}"#;
        let snap = parse_cc_state_json(raw).unwrap();
        assert_eq!(snap.cost_usd_total, 0.0);
        assert_eq!(snap.rate_limit_pct, None);
        assert!(snap.cwd.is_none());
    }

    #[test]
    fn parse_state_json_malformed_returns_ingest_failed() {
        let raw = r#"{"model": "broken"#;
        let err = parse_cc_state_json(raw).unwrap_err();
        assert!(matches!(err, HarnessError::IngestFailed(_)));
    }

    #[test]
    fn parse_state_json_extra_fields_passthrough_in_raw() {
        let raw = r#"{
            "model": "Claude Opus 4.7",
            "context_pct": 12,
            "future_field": {"nested": [1, 2, 3]}
        }"#;
        let snap = parse_cc_state_json(raw).unwrap();
        assert_eq!(snap.raw["future_field"]["nested"][1], 2);
        assert_eq!(snap.context_used_pct, 12);
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
    fn parse_pid_from_state_malformed_returns_none() {
        // Idempotent: parse failure must not bubble through
        // shutdown_session's call site.
        assert_eq!(parse_pid_from_state("{broken"), None);
    }

    // ----- state_json_path helper -----

    #[test]
    fn state_json_path_env_override() {
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os(CLAUDE_JOBS_DIR_ENV);
        std::env::set_var(CLAUDE_JOBS_DIR_ENV, tmp.path());
        let path = state_json_path("abc123");
        assert_eq!(path, tmp.path().join("abc123").join("state.json"));
        match prev {
            Some(v) => std::env::set_var(CLAUDE_JOBS_DIR_ENV, v),
            None => std::env::remove_var(CLAUDE_JOBS_DIR_ENV),
        }
    }

    // ----- ClaudeCodeAdapter -----

    #[test]
    fn claude_code_adapter_name_is_stable() {
        // Stability test — `ccteam-web` SSE wire format embeds this
        // string into JSON payloads.
        assert_eq!(ClaudeCodeAdapter::new().name(), "claude-code");
    }

    #[test]
    fn claude_code_adapter_ingest_uses_state_json_parser() {
        // The adapter's `ingest_snapshot` is a thin wrapper around
        // `parse_cc_state_json` — exercise it directly to pin that
        // contract.
        let raw = r#"{"model":"X","context_pct":5,"cost_usd":0.0}"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.harness, "claude-code");
        assert_eq!(snap.model_display_name, "X");
    }

    #[test]
    fn subagent_states_default_empty() {
        let snap = HarnessSnapshot {
            harness: "claude-code".into(),
            model_display_name: "X".into(),
            context_used_pct: 0,
            cost_usd_total: 0.0,
            rate_limit_pct: None,
            cwd: None,
            raw: serde_json::Value::Null,
            captured_at: Utc::now(),
        };
        assert!(ClaudeCodeAdapter::new().subagent_states(&snap).is_empty());
    }

    #[test]
    fn not_implemented_error_message_carries_static_strings() {
        let err = HarnessError::NotImplemented {
            harness: "future-harness",
            reason: "not yet wired",
        };
        let s = err.to_string();
        assert!(s.contains("future-harness"));
        assert!(s.contains("not yet wired"));
    }

    // ----- CodexAdapter (F62 unchanged surfaces) -----

    #[test]
    fn codex_adapter_name_is_codex() {
        assert_eq!(CodexAdapter::new().name(), "codex");
    }

    #[test]
    fn codex_adapter_ingest_empty_pane_returns_fallback_snapshot() {
        let snap = CodexAdapter::new()
            .ingest_snapshot("")
            .expect("permissive fallback");
        assert_eq!(snap.harness, "codex");
        assert_eq!(snap.model_display_name, "codex");
        assert_eq!(snap.context_used_pct, 0);
        assert_eq!(snap.cost_usd_total, 0.0);
        assert_eq!(snap.rate_limit_pct, None);
        assert!(snap.cwd.is_none());
    }

    #[test]
    fn codex_adapter_ingest_status_line_parses() {
        let pane = "(some scrollback)\n\
                    CODEX_STATUS: {\"model\":\"o3\",\"context_pct\":42,\"cost_usd\":1.25}\n\
                    (trailing prompt)\n";
        let snap = CodexAdapter::new().ingest_snapshot(pane).unwrap();
        assert_eq!(snap.harness, "codex");
        assert_eq!(snap.model_display_name, "o3");
        assert_eq!(snap.context_used_pct, 42);
        assert!((snap.cost_usd_total - 1.25).abs() < 1e-9);
    }

    #[test]
    fn codex_adapter_ingest_last_status_line_wins() {
        let pane = "CODEX_STATUS: {\"model\":\"o1\",\"context_pct\":10}\n\
                    intermediate\n\
                    CODEX_STATUS: {\"model\":\"o3\",\"context_pct\":80}\n";
        let snap = CodexAdapter::new().ingest_snapshot(pane).unwrap();
        assert_eq!(snap.model_display_name, "o3");
        assert_eq!(snap.context_used_pct, 80);
    }

    #[test]
    fn codex_adapter_ingest_malformed_json_falls_back_silently() {
        let pane = "CODEX_STATUS: { broken json\n";
        let snap = CodexAdapter::new().ingest_snapshot(pane).unwrap();
        assert_eq!(snap.model_display_name, "codex");
        assert_eq!(snap.context_used_pct, 0);
    }

    #[test]
    fn codex_adapter_does_not_return_not_implemented() {
        let res = CodexAdapter::new().ingest_snapshot("");
        match res {
            Ok(_) => {}
            Err(HarnessError::NotImplemented { .. }) => {
                panic!("CodexAdapter must not return NotImplemented post-F62")
            }
            Err(other) => panic!("unexpected ingest error: {other:?}"),
        }
    }

    #[test]
    fn codex_adapter_subagent_states_default_empty() {
        let snap = HarnessSnapshot {
            harness: "codex".into(),
            model_display_name: "X".into(),
            context_used_pct: 0,
            cost_usd_total: 0.0,
            rate_limit_pct: None,
            cwd: None,
            raw: serde_json::Value::Null,
            captured_at: Utc::now(),
        };
        assert!(CodexAdapter::new().subagent_states(&snap).is_empty());
    }
}
