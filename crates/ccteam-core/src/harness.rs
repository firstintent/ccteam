//! V0.3.1 F46 — `HarnessAdapter` trait + `ClaudeCodeAdapter`.
//!
//! V0.3.1's strategic pivot (`docs/v0-3-1/prd.md` §1) re-frames ccteam
//! as a "session farm + observability layer" on top of multiple
//! harnesses (Claude Code today, Codex tomorrow). This module owns the
//! trait shape every harness fills in: ingest a structured status
//! snapshot from the harness's native channel, optionally enumerate
//! subagent state, spawn / shutdown the underlying tmux session.
//!
//! Data flow (Claude Code happy path):
//!
//! ```text
//! Claude Code TUI
//!   ↓ stdin JSON
//! ~/.claude/statusline-command.sh        (ccteam-managed wrapper)
//!   ↓ original render path                 ↓ NEW: dual-write
//! TUI footer string                       ~/.ccteam/harness/<slug>-<sid>.json
//!                                            ↓ notify watcher (ccteam-web)
//!                                         tokio::sync::broadcast<HarnessSnapshotEvent>
//!                                            ↓ SSE
//!                                         /sse/harness/<slug>[/<sid>]
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

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::CcteamPaths;
use crate::tmux::{session_name_for_slug, TmuxSession};

/// Default sid for projects that haven't opted into multi-session
/// (V0.3 single-session projects, or the first claude session in a
/// flex project before F49 lands per-session subdirs). Picked so the
/// dual-write file name still encodes harness identity, even on legacy
/// project layouts.
pub const DEFAULT_CLAUDE_SID: &str = "claude-1";

/// Adapter contract for any LLM "harness" ccteam may host inside a
/// tmux session. V0.3.1 F46 shipped [`ClaudeCodeAdapter`]; V0.3.1 F47
/// landed `CodexAdapter` as a trait stub that returned
/// `Err(NotImplemented)`; V0.4.0 F62 replaced that stub with a real
/// tmux + codex CLI implementation. The `NotImplemented` error
/// variant remains on [`HarnessError`] for any future harness whose
/// integration ships in stages.
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
    /// channel (Claude Code's statusline stdin JSON; Codex equivalent
    /// TBD). Returns a normalized [`HarnessSnapshot`] the web layer
    /// can consume without knowing which harness produced it.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError>;

    /// Best-effort enumeration of in-flight subagents. Returns an
    /// empty `Vec` when the harness doesn't surface this data —
    /// V0.3.1 Claude Code returns empty until upstream API exposes it
    /// (PRD §3.3 deferred to V0.4); Codex stub returns empty too.
    fn subagent_states(&self, _snapshot: &HarnessSnapshot) -> Vec<SubagentState> {
        Vec::new()
    }

    /// Spawn a new harness session. Wraps `tmux new-session -d` so the
    /// session is detached from any controlling terminal; the
    /// orchestrator / `ccteam attach` re-attaches via the returned
    /// [`SessionHandle`]'s `tmux_session` name.
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError>;

    /// Graceful shutdown. The ONLY caller in V0.3.1 is the user's
    /// explicit `ccteam session rm` invocation (F49) — never silent
    /// kill (red line CLAUDE.md §三 / PRD §3.3).
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError>;
}

/// Normalized status snapshot from any harness. Field set is the
/// minimum union covered by every shipping harness (Claude Code's
/// statusline JSON today; Codex's equivalent will fill the same
/// shape in V0.3.2). Unknown / missing fields fall back to defaults
/// rather than failing — `raw` carries the full upstream JSON for
/// forward-compat.
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
    /// Context window utilization, 0..=100. Statusline scripts often
    /// omit this on session-start; we coerce missing → 0.
    pub context_used_pct: u8,
    /// Cumulative session cost in USD. Coerced to 0.0 when the
    /// harness doesn't expose cost data (older Claude Code releases).
    pub cost_usd_total: f64,
    /// Five-hour rate-limit utilization (Claude Code field). `None`
    /// when the harness doesn't surface this surface.
    pub rate_limit_pct: Option<u8>,
    /// Session cwd as reported by the harness. Used by the
    /// orchestrator + path resolver to derive the `<slug>-<sid>`
    /// dual-write target. `None` for free-running sessions.
    pub cwd: Option<PathBuf>,
    /// Full upstream JSON. Carried verbatim so future harness
    /// upgrades don't require a ccteam release to surface new fields.
    pub raw: serde_json::Value,
    /// Wall-clock time the snapshot was ingested.
    pub captured_at: DateTime<Utc>,
}

/// Optional per-snapshot subagent state. V0.3.1 always empty — the
/// shape is reserved for V0.4+ when Claude Code's API exposes
/// structured subagent progress (PRD §3.3).
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

/// Inputs for [`HarnessAdapter::spawn_session`]. The adapter combines
/// `harness` + `slug` + `sid` to derive the tmux session name
/// (`ccteam-<slug>-<sid>` for flex projects, `ccteam-<slug>` for V0.3
/// single-session projects via the F49 path, but F46 always emits the
/// per-sid form so the foundation is uniform).
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
    /// `~/projects/<team>-<slug>/` (F49 may extend to per-sid subdirs).
    pub cwd: PathBuf,
    /// Extra args appended to the harness command line. For
    /// `ClaudeCodeAdapter`, these append to `claude
    /// --dangerously-skip-permissions ...`; F49 sets things like
    /// `--model …` here.
    pub extra_args: Vec<String>,
}

/// Outcome of [`HarnessAdapter::spawn_session`]. Owned by the caller
/// (F49 master `state.json::sessions[]`); the adapter never caches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHandle {
    /// tmux session name in the standard `ccteam-<slug>-<sid>` form.
    pub tmux_session: String,
    /// Harness identifier — matches [`HarnessAdapter::name`].
    pub harness: String,
    /// Session id, mirroring [`SpawnOpts::sid`].
    pub sid: String,
    /// Active pane PID, when tmux discloses it. `None` immediately
    /// post-spawn before the harness has a chance to fork.
    pub pid: Option<u32>,
    /// Wall-clock start time.
    pub started_at: DateTime<Utc>,
}

/// V0.3.1 F46 error type for every fallible [`HarnessAdapter`]
/// surface. `NotImplemented` carries `&'static str` fields so const
/// callsites (`CodexAdapter`'s stub error path, F47) compile to a
/// non-allocating literal.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// JSON parse / shape mismatch on the harness statusline channel.
    #[error("snapshot ingest failed: {0}")]
    IngestFailed(String),
    /// tmux / process failure during `spawn_session`.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    /// `shutdown_session` failure — graceful `/exit` send-keys flow
    /// timed out or tmux refused the kill request.
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),
    /// Adapter declares the surface unsupported. V0.3.1 F47 used this
    /// for the `CodexAdapter` stub until V0.4.0 F62 swapped in the
    /// real implementation. The variant is preserved on the enum for
    /// any future harness whose integration lands in stages; the
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
// ClaudeCodeAdapter
// =====================================================================

/// V0.3.1 F46 [`HarnessAdapter`] implementation for Anthropic's Claude
/// Code TUI. Stateless zero-size struct — share one `'static` instance.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeAdapter;

/// Statusline JSON field path constants — single source of truth so
/// the unit tests + `ingest_snapshot` agree.
const FIELD_MODEL_DISPLAY_NAME: &[&str] = &["model", "display_name"];
const FIELD_CONTEXT_USED_PCT: &[&str] = &["context_window", "used_percentage"];
const FIELD_COST_USD_TOTAL: &[&str] = &["cost", "total_cost_usd"];
const FIELD_RATE_LIMIT_PCT: &[&str] = &["rate_limits", "five_hour", "used_percentage"];
const FIELD_CWD: &[&str] = &["cwd"];

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

    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|err| HarnessError::IngestFailed(format!("parse statusline JSON: {err}")))?;

        let model_display_name = pluck_str(&value, FIELD_MODEL_DISPLAY_NAME)
            .unwrap_or_else(|| "unknown".to_string());
        let context_used_pct = pluck_pct(&value, FIELD_CONTEXT_USED_PCT).unwrap_or(0);
        let cost_usd_total = pluck_f64(&value, FIELD_COST_USD_TOTAL).unwrap_or(0.0);
        let rate_limit_pct = pluck_pct(&value, FIELD_RATE_LIMIT_PCT);
        let cwd = pluck_str(&value, FIELD_CWD).map(PathBuf::from);

        Ok(HarnessSnapshot {
            harness: self.name().to_string(),
            model_display_name,
            context_used_pct,
            cost_usd_total,
            rate_limit_pct,
            cwd,
            raw: value,
            captured_at: Utc::now(),
        })
    }

    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        let tmux_session = format!("{}-{}", session_name_for_slug(&opts.slug), opts.sid)
            // session_name_for_slug already prepends "ccteam-", so the
            // result is "ccteam-<slug>-<sid>" matching the F49 spec.
            .trim_start_matches('-')
            .to_string();

        // Defense in depth: refuse to spawn over an existing session
        // — that would mean F49 lost track of `next_sid_seq` and
        // attempted to reuse a sid.
        let session = TmuxSession::from_name(tmux_session.clone());
        if session.exists() {
            return Err(HarnessError::SpawnFailed(format!(
                "tmux session already exists: {tmux_session} \
                 (sid collision; F49 next_sid_seq accounting drifted)"
            )));
        }

        let mut argv: Vec<String> = vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
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

        Ok(SessionHandle {
            tmux_session,
            harness: self.name().to_string(),
            sid: opts.sid,
            pid,
            started_at: Utc::now(),
        })
    }

    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError> {
        let session = TmuxSession::from_name(handle.tmux_session.clone());
        if !session.exists() {
            // Already gone — treat as success (idempotent).
            return Ok(());
        }
        // Best-effort graceful exit: send `/exit\n` to the active pane,
        // wait 5s, then hard-kill if tmux still owns the session. A
        // failure on the send-keys step is non-fatal — the kill below
        // is the authoritative cleanup.
        if let Err(err) = send_exit_command(&handle.tmux_session) {
            tracing::warn!(
                error = %err,
                session = %handle.tmux_session,
                "ClaudeCodeAdapter::shutdown_session: send-keys /exit failed; \
                 falling through to tmux kill-session",
            );
        }

        std::thread::sleep(Duration::from_secs(5));

        if session.exists() {
            session
                .kill()
                .map_err(|err| HarnessError::ShutdownFailed(format!("tmux kill-session: {err:#}")))?;
        }
        Ok(())
    }
}

/// Send `/exit\n` literally to the named tmux session. We use an
/// explicit Command sequence (rather than `TmuxSession::send_keys`) so
/// `/exit` is interpreted by Claude Code as a slash-command, not as
/// part of an in-flight prompt.
fn send_exit_command(tmux_session: &str) -> std::io::Result<()> {
    let send_lit = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "-l", "--", "/exit"])
        .output()?;
    if !send_lit.status.success() {
        return Err(std::io::Error::other(format!(
            "tmux send-keys -l /exit: {}",
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
// CodexAdapter (V0.4.0 F62 — real tmux + codex CLI implementation)
// =====================================================================

/// V0.4.0 F62 [`HarnessAdapter`] implementation for OpenAI's `codex`
/// CLI. Replaces the V0.3.1 F47 trait stub (which returned
/// [`HarnessError::NotImplemented`] from every fallible method).
///
/// Architecture (`docs/v0-4-0/prd.md` §6.5 + dev-plan §4):
///
/// - **Transport**: tmux long-session, mirroring `ClaudeCodeAdapter`.
///   A detached tmux session named `ccteam-<slug>-<sid>` hosts the
///   `codex` process; `pane_pid` is read once post-spawn for liveness
///   checks. Distinct from F61's ClaudeCode `--bg` job_id path — codex
///   does not (yet) expose an equivalent background-runner protocol,
///   so tmux remains the canonical session container.
/// - **Status channel**: codex has no native statusline / stdin-JSON
///   surface. [`Self::ingest_snapshot`] reads the last few lines of
///   the tmux pane via `capture-pane -p` and greps for a
///   `CODEX_STATUS: <json>` marker line that the codex agent (or the
///   workflow's role .md) is expected to print after each turn. When
///   no marker is found, the adapter returns a permissive default
///   snapshot (`ctx_pct=0`, `cost=0`, model="codex") rather than
///   failing — the snapshot pipeline is presentation-only (red line
///   in CLAUDE.md §三 / PRD §3.3), so a missing tail is not an error.
/// - **State observation file**: a `~/.ccteam/codex/<sid>/state.json`
///   is written at spawn time mirroring `ClaudeCodeAdapter`'s
///   statusline dual-write file shape. This lets F66 orchestrator
///   surfaces consume codex state through the same observe-file
///   pattern as claude. Best-effort write: a failing write does not
///   block spawn (we still own the tmux session).
/// - **Shutdown**: graceful `q\r` send-keys (codex's documented
///   quit-bind), 500ms grace period, then `tmux kill-session -t
///   <name>` fallback. Only kills the session we own (matched by the
///   `SessionHandle::tmux_session` name) — never `kill-server` or
///   pattern-matched kills (red line in this PR's grep checks).
///
/// Stateless zero-size — share one `'static` instance.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

/// Marker line the codex agent prints in its tmux pane to publish
/// state to the observer (PRD §6.5 + dev-plan §3.2). The parser greps
/// the last few `capture-pane` lines for this prefix and JSON-decodes
/// the trailing payload. Spelled out as a `&'static str` so callers
/// (tests, future code-gen) can pattern-match the same literal.
pub const CODEX_STATUS_MARKER: &str = "CODEX_STATUS:";

/// Number of trailing pane lines an observer (the F66 watcher, the
/// integration test in `tests/codex_adapter_test.rs`) is expected to
/// capture from a codex tmux session before feeding the pane body to
/// [`CodexAdapter::ingest_snapshot`]. Five lines balances signal
/// (codex agents typically print the marker as the last line of a
/// reply) against pane noise (longer tails may include earlier
/// snapshots whose JSON has since been superseded). Bumped via PR if
/// real-world agents start emitting status further from the tail.
///
/// `ingest_snapshot` itself ignores tail size — it scans every line
/// the caller passed in — but exposing the constant lets callers and
/// tests agree on the canonical capture window.
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
    /// in some test contexts) — the caller treats that as a soft
    /// no-op and still owns the tmux session.
    fn state_json_path(sid: &str) -> Option<PathBuf> {
        let paths = CcteamPaths::from_env().ok()?;
        Some(paths.root.join("codex").join(sid).join("state.json"))
    }

    /// Best-effort write of the initial `state.json` post-spawn. Shape
    /// mirrors what `ClaudeCodeAdapter`'s statusline dual-write
    /// produces so the F66 / F65 orchestrator's observer can read both
    /// harnesses uniformly. Silent failure: returns `Ok(())` even if
    /// the directory create or rename failed (we logged the warn) so
    /// `spawn_session` doesn't propagate a non-fatal observer error
    /// to the caller's `SessionHandle` flow.
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
    /// payload (or `None` for the fallback shape). Field set matches
    /// `HarnessSnapshot`'s normalized shape; codex-side keys are
    /// optional and pluck through the existing `pluck_*` helpers for
    /// forward-compat with future codex agent contracts.
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
    /// data. Unlike [`ClaudeCodeAdapter::ingest_snapshot`] which
    /// consumes structured statusline JSON, this method accepts the
    /// raw tmux pane body (caller is the observer that already ran
    /// `capture-pane -p`) and greps for the `CODEX_STATUS:` marker.
    ///
    /// Permissive: missing / malformed marker returns the fallback
    /// snapshot (`model="codex"`, zero pct/cost) rather than failing.
    /// The snapshot pipeline is presentation-only — see CLAUDE.md
    /// §三 / PRD §3.3.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        let payload = Self::parse_status_line(raw);
        Ok(Self::snapshot_from_status(payload))
    }

    /// Spawn a codex tmux session. The command is `codex` with
    /// `opts.extra_args` appended verbatim — callers (F65 meta-agent
    /// MCP `spawn_agent`, the F49 CLI surface) thread role-specific
    /// arguments (`exec --sandbox workspace-write --cd <cwd>`, prompt
    /// strings, etc.) through `extra_args` to stay schema-stable.
    /// When `extra_args` is empty, codex starts in default interactive
    /// mode and waits for human input on the tmux pane.
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        let tmux_session = format!("{}-{}", session_name_for_slug(&opts.slug), opts.sid)
            // session_name_for_slug already prepends "ccteam-", so the
            // result is "ccteam-<slug>-<sid>" matching the F49 spec.
            .trim_start_matches('-')
            .to_string();

        // Defense in depth: refuse to spawn over an existing session
        // — that would mean the F49 master state.json::next_sid_seq
        // bookkeeping drifted and reused a sid.
        let session = TmuxSession::from_name(tmux_session.clone());
        if session.exists() {
            return Err(HarnessError::SpawnFailed(format!(
                "tmux session already exists: {tmux_session} \
                 (sid collision; F49 next_sid_seq accounting drifted)"
            )));
        }

        // Base argv: bare `codex` so the session is interactive by
        // default; F65 passes `exec --sandbox … "--cd" <cwd>` etc. via
        // `extra_args`. Mirrors `ClaudeCodeAdapter`'s argv layout.
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

        // Best-effort observe-file write so F66's reader sees a fresh
        // state.json immediately. Failure is logged and swallowed —
        // the tmux session is the source of truth, not this file.
        Self::write_initial_state(&opts.sid, pid);

        Ok(SessionHandle {
            tmux_session,
            harness: self.name().to_string(),
            sid: opts.sid,
            pid,
            started_at: Utc::now(),
        })
    }

    /// Graceful shutdown of a codex tmux session. Sends `q` + Enter
    /// (codex's documented quit binding), waits 500ms for the process
    /// to drain, then `tmux kill-session -t <handle.tmux_session>` as
    /// the authoritative cleanup. Targets only the named session — no
    /// `kill-server` / pattern kills (red line CLAUDE.md §三).
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError> {
        let session = TmuxSession::from_name(handle.tmux_session.clone());
        if !session.exists() {
            // Already gone — treat as success (idempotent, matches the
            // ClaudeCodeAdapter shutdown contract).
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
/// quit keybinding. Separate from `send_exit_command` (Claude Code's
/// `/exit` slash-command) so the two harnesses' shutdown grammars
/// stay distinct.
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
// Path derivation (statusline wrapper helper)
// =====================================================================

/// Decide which `<harness-dir>/<filename>.json` file the wrapper
/// should dual-write to, given the cwd reported by the harness's
/// statusline JSON. Returns `None` when the cwd doesn't fall under
/// `~/projects/` — meta-agent / random claude sessions outside the
/// orchestrator's scope drop silently.
///
/// Match rules (PRD §3.2.3 + dev-plan §2.1 #1.4):
///
/// 1. `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/...` →
///    `<harness-dir>/<slug>-<sid>.json` (F49 multi-session path).
/// 2. `~/projects/<team>-<slug>/...` (no `sessions/<sid>/` subdir) →
///    `<harness-dir>/<slug>-claude-1.json` (V0.3 single-session
///    default; sid stays `claude-1` for back-compat with the existing
///    `ccteam-<slug>` tmux name).
/// 3. `~/projects/<handle>-meta/...` → `<harness-dir>/_meta-<handle>.json`
///    (meta-agent — orchestrator outside the per-team scope).
/// 4. Anything else → `None` (random claude session, /etc, /tmp, …).
///
/// Path traversal defense: components containing `..` reject to
/// `None` rather than risking a write outside `harness_dir`.
pub fn derive_harness_path(cwd: &Path, paths: &CcteamPaths) -> Option<PathBuf> {
    if cwd.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let rel = cwd.strip_prefix(&paths.projects_root).ok()?;
    let mut comps = rel.components();
    let head_os = match comps.next()? {
        Component::Normal(n) => n,
        _ => return None,
    };
    let head = head_os.to_str()?;

    // Rule 3 — meta-agent (`<handle>-meta`).
    if let Some(handle) = head.strip_suffix("-meta") {
        if handle.is_empty() {
            return None;
        }
        return Some(paths.harness_dir().join(format!("_meta-{handle}.json")));
    }

    // Rules 1 + 2 — `<team>-<slug>` form. The slug is the head (per
    // F22, the team prefix is part of the slug). Look for the F49
    // `.ccteam/sessions/<sid>/` shape next.
    let slug = head;
    let sid = match comps.next() {
        Some(Component::Normal(n)) if n == ".ccteam" => match comps.next() {
            Some(Component::Normal(s)) if s == "sessions" => match comps.next() {
                Some(Component::Normal(sid_os)) => sid_os.to_str()?.to_string(),
                _ => DEFAULT_CLAUDE_SID.to_string(),
            },
            _ => DEFAULT_CLAUDE_SID.to_string(),
        },
        _ => DEFAULT_CLAUDE_SID.to_string(),
    };

    Some(paths.harness_dir().join(format!("{slug}-{sid}.json")))
}

/// Atomically dual-write `raw` (the harness statusline stdin JSON)
/// into the resolved `<harness-dir>/<slug>-<sid>.json` file. Uses the
/// canonical `<path>.tmp` + `rename` pattern so the SSE watcher's
/// `Modify` event never sees a half-written file.
///
/// Best-effort by design: if the target dir can't be created or the
/// resolved path is `None`, silently no-op so the wrapper script's
/// `2>/dev/null` exits cleanly.
pub fn write_harness_snapshot(paths: &CcteamPaths, cwd: &Path, raw: &str) -> std::io::Result<bool> {
    let Some(target) = derive_harness_path(cwd, paths) else {
        return Ok(false);
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = match target.file_name() {
        Some(name) => target.with_file_name(format!("{}.tmp", name.to_str().unwrap_or("snapshot"))),
        None => return Ok(false),
    };
    std::fs::write(&tmp, raw.as_bytes())?;
    std::fs::rename(&tmp, &target)?;
    Ok(true)
}

// =====================================================================
// Plucker helpers — tolerant of missing / mistyped fields, mirroring
// `~/.claude/statusline-command.sh`'s `jq // empty` style.
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
        // Statusline JSON occasionally encodes percentages as
        // integers, occasionally as floats. Accept either.
        v.as_u64()
            .map(|n| n.min(100) as u8)
            .or_else(|| v.as_f64().map(|n| n.clamp(0.0, 100.0).round() as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_paths(root: &Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

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
        // captured_at round-trips via RFC3339 — equality holds because
        // chrono's serde uses lossless format.
        assert_eq!(back, original);
    }

    // ----- derive_harness_path -----

    #[test]
    fn derive_path_multi_session_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        let cwd = paths
            .projects_root
            .join("dev-foo/.ccteam/sessions/claude-2");
        let got = derive_harness_path(&cwd, &paths).unwrap();
        assert_eq!(got, paths.harness_dir().join("dev-foo-claude-2.json"));
    }

    #[test]
    fn derive_path_single_session_default() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        let cwd = paths.projects_root.join("dev-foo");
        let got = derive_harness_path(&cwd, &paths).unwrap();
        assert_eq!(got, paths.harness_dir().join("dev-foo-claude-1.json"));
    }

    #[test]
    fn derive_path_meta_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        let cwd = paths.projects_root.join("rob-meta");
        let got = derive_harness_path(&cwd, &paths).unwrap();
        assert_eq!(got, paths.harness_dir().join("_meta-rob.json"));
    }

    #[test]
    fn derive_path_outside_projects_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        let cwd = std::path::PathBuf::from("/tmp/random");
        assert!(derive_harness_path(&cwd, &paths).is_none());
    }

    #[test]
    fn derive_path_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        // Even if the cwd looks like a valid project subpath, ParentDir
        // anywhere in the components must collapse to None.
        let cwd = paths
            .projects_root
            .join("dev-foo")
            .join("..")
            .join("..")
            .join("etc");
        assert!(derive_harness_path(&cwd, &paths).is_none());
    }

    // ----- ClaudeCodeAdapter::ingest_snapshot -----

    #[test]
    fn ingest_full_statusline_json() {
        let raw = r#"{
            "model": {"display_name": "Claude Sonnet 4.5"},
            "context_window": {"used_percentage": 73},
            "cost": {"total_cost_usd": 4.56},
            "rate_limits": {"five_hour": {"used_percentage": 22}},
            "cwd": "/home/u/projects/dev-foo"
        }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.harness, "claude-code");
        assert_eq!(snap.model_display_name, "Claude Sonnet 4.5");
        assert_eq!(snap.context_used_pct, 73);
        assert!((snap.cost_usd_total - 4.56).abs() < 1e-9);
        assert_eq!(snap.rate_limit_pct, Some(22));
        assert_eq!(
            snap.cwd.as_deref(),
            Some(Path::new("/home/u/projects/dev-foo"))
        );
    }

    #[test]
    fn ingest_missing_cost_falls_back_to_zero() {
        let raw = r#"{
            "model": {"display_name": "Claude Haiku 4.5"},
            "context_window": {"used_percentage": 5}
        }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.cost_usd_total, 0.0);
        assert_eq!(snap.rate_limit_pct, None);
        assert!(snap.cwd.is_none());
    }

    #[test]
    fn ingest_missing_rate_limit_keeps_none() {
        let raw = r#"{
            "model": {"display_name": "X"},
            "cost": {"total_cost_usd": 0.01}
        }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.rate_limit_pct, None);
    }

    #[test]
    fn ingest_malformed_json_returns_ingest_failed() {
        let raw = r#"{"model": "broken"#;
        let err = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap_err();
        assert!(matches!(err, HarnessError::IngestFailed(_)));
    }

    #[test]
    fn ingest_extra_unknown_fields_passes_through_in_raw() {
        let raw = r#"{
            "model": {"display_name": "Claude Opus 4.7"},
            "context_window": {"used_percentage": 12},
            "future_field": {"nested": [1, 2, 3]}
        }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        // `raw` carries the full upstream JSON for forward-compat.
        assert_eq!(snap.raw["future_field"]["nested"][1], 2);
        // Known fields parsed normally.
        assert_eq!(snap.context_used_pct, 12);
    }

    #[test]
    fn not_implemented_error_message_carries_static_strings() {
        let err = HarnessError::NotImplemented {
            harness: "codex",
            reason: "stub in V0.3.1",
        };
        let s = err.to_string();
        assert!(s.contains("codex"));
        assert!(s.contains("stub in V0.3.1"));
    }

    #[test]
    fn write_harness_snapshot_creates_dir_and_atomic_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        let cwd = paths.projects_root.join("dev-foo");
        let raw = r#"{"model":{"display_name":"X"}}"#;

        let wrote = write_harness_snapshot(&paths, &cwd, raw).unwrap();
        assert!(wrote);

        let target = paths.harness_dir().join("dev-foo-claude-1.json");
        assert!(target.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), raw);
        // No leftover .tmp.
        let tmp_target = paths.harness_dir().join("dev-foo-claude-1.json.tmp");
        assert!(!tmp_target.exists());
    }

    #[test]
    fn write_harness_snapshot_returns_false_outside_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = fake_paths(tmp.path());
        let cwd = std::path::PathBuf::from("/tmp/random");
        let wrote = write_harness_snapshot(&paths, &cwd, "{}").unwrap();
        assert!(!wrote);
        assert!(!paths.harness_dir().exists());
    }

    #[test]
    fn claude_code_adapter_name_is_stable() {
        // Stability test — `ccteam-web` SSE wire format embeds this
        // string into JSON payloads. A rename here cascades into the
        // dashboard JS + interfaces.md §15.6.
        assert_eq!(ClaudeCodeAdapter::new().name(), "claude-code");
    }

    #[test]
    fn subagent_states_default_empty_for_v0_3_1() {
        // V0.3.1 contract: ClaudeCodeAdapter never emits subagent
        // states (deferred per PRD §3.3 — Claude Code doesn't expose
        // structured subagent data yet). Test pins the contract so a
        // future PR that wires it must update this expectation.
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

    // ----- CodexAdapter (V0.4.0 F62 real implementation) -----
    //
    // Unit tests cover the pure-Rust surfaces that don't need tmux or
    // the codex CLI: name stability, snapshot parsing, fallback shape,
    // status JSON round-trip. The tmux-bearing surfaces
    // (`spawn_session`, `shutdown_session`) live in
    // `crates/ccteam-core/tests/codex_adapter_test.rs` under the
    // `codex-tests` feature gate (PR §3.5).

    #[test]
    fn codex_adapter_name_is_codex() {
        // Stability test — `ccteam-web` SSE wire format embeds this
        // string, mirroring `claude_code_adapter_name_is_stable`. F49
        // CLI dispatch and team.yaml `harness: codex` round-trip both
        // depend on this literal.
        assert_eq!(CodexAdapter::new().name(), "codex");
    }

    #[test]
    fn codex_adapter_ingest_empty_pane_returns_fallback_snapshot() {
        // No CODEX_STATUS line → permissive fallback (snapshot pipeline
        // is presentation-only; PRD §3.3). model="codex", zero pct/cost.
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
        // Two markers: the most recent (deepest in the pane) is used.
        let pane = "CODEX_STATUS: {\"model\":\"o1\",\"context_pct\":10}\n\
                    intermediate\n\
                    CODEX_STATUS: {\"model\":\"o3\",\"context_pct\":80}\n";
        let snap = CodexAdapter::new().ingest_snapshot(pane).unwrap();
        assert_eq!(snap.model_display_name, "o3");
        assert_eq!(snap.context_used_pct, 80);
    }

    #[test]
    fn codex_adapter_ingest_malformed_json_falls_back_silently() {
        // Marker line is present but JSON is broken → fallback shape;
        // no error propagated. Snapshot pipeline must not panic.
        let pane = "CODEX_STATUS: { broken json\n";
        let snap = CodexAdapter::new().ingest_snapshot(pane).unwrap();
        assert_eq!(snap.model_display_name, "codex");
        assert_eq!(snap.context_used_pct, 0);
    }

    #[test]
    fn codex_adapter_does_not_return_not_implemented() {
        // Regression guard for F62: pure-Rust surfaces (ingest, name)
        // must never produce `HarnessError::NotImplemented`. Spawn /
        // shutdown depend on tmux and are exercised under the
        // `codex-tests` feature, but pattern-matching here covers the
        // ingest path with a dummy input. Compile-level: if anyone
        // re-introduces the stub, this match-arm panics.
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
        // Mirrors `subagent_states_default_empty_for_v0_3_1`. Codex's
        // subagent surface is still TBD pending upstream API.
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
