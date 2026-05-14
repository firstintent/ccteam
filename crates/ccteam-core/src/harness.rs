//! V0.4.0 F61 — `HarnessAdapter` trait + thin `ClaudeCodeAdapter`.
//!
//! V0.4.0's architectural pivot (`docs/v0-4-0/prd.md` §6.4) re-frames
//! ccteam around Claude Code's native Agent View (`claude --bg --agent`):
//!
//! - `ccteam` no longer manages tmux as the host for Claude Code
//!   sessions — `claude --bg` does that natively.
//! - `ccteam` no longer parses the TUI footer JSON channel for session
//!   state — `~/.claude/jobs/<job_id>/state.json` is the canonical
//!   observability surface.
//!
//! Data flow (Claude Code happy path, V0.4.0):
//!
//! ```text
//! ccteam orchestrator
//!   ↓ spawn
//! claude --bg --agent <role>  →  prints { "job_id": "..." } on stdout
//!   ↓
//! claude daemon (background)
//!   ↓ writes
//! ~/.claude/jobs/<job_id>/state.json   (model / context_pct / cost_usd / pid / ...)
//!   ↓ read on demand
//! ccteam observe → HarnessSnapshot → ccteam-web SSE
//! ```
//!
//! **Architectural red lines** (CLAUDE.md §三, V0.4.0 PRD §12):
//!
//! - The snapshot pipeline is **presentation-only**. Nothing in
//!   `orchestrator.rs` consumes a `HarnessSnapshot` — the orchestrator's
//!   business-state SoT is unchanged (see PRD §12 for the SoT contract).
//! - `shutdown_session` sends SIGTERM via the `pid` recorded in
//!   `state.json`. The Codex stub's tmux-based path is unrelated.
//! - `ccteam-core` does not know team-name literals or agent-role
//!   literals. The adapter knows its own harness identifier
//!   (`"claude-code"`, `"codex"`) but never inspects role names.
//!
//! **Why a "thin" adapter**: under V0.3.1's tmux-host model the
//! adapter owned spawn, observe, shutdown, AND a parallel TUI-footer
//! tee pipeline. V0.4.0 collapses spawn / observe / shutdown to three
//! thin `Command` invocations + one `read_to_string` — roughly an
//! 80% LOC drop. The hard work (rendering, agent role definition,
//! model selection) lives in Claude Code itself.

use std::path::PathBuf;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default sid for single-session projects that haven't opted into
/// the flex multi-session model. Used by the path resolver in
/// `crate::paths` to compute the per-session business-state file
/// path when `ProjectSessionContext` doesn't carry an explicit sid.
/// Retained from V0.3.1 F46 — V0.4.0 F61's CC sessions don't directly
/// consume this constant (the job_id is the primary key now), but
/// flex multi-session callsites still rely on it.
pub const DEFAULT_CLAUDE_SID: &str = "claude-1";

/// Adapter contract for any LLM "harness" ccteam may spawn. V0.4.0
/// F61 ships [`ClaudeCodeAdapter`] backed by `claude --bg --agent`;
/// F62 fills in `CodexAdapter` with a real implementation (replacing
/// V0.3.1 F47's `NotImplemented` stub).
///
/// Implementations are stateless — a single `'static` instance can be
/// shared across all sessions of that harness type.
pub trait HarnessAdapter: Send + Sync {
    /// Stable identifier, e.g. `"claude-code"`, `"codex"`. Used as the
    /// `harness` discriminator on snapshots + session handles.
    fn name(&self) -> &'static str;

    /// Ingest a fresh status snapshot from the harness's native
    /// observability channel. For Claude Code that's the contents of
    /// `~/.claude/jobs/<job_id>/state.json`; for Codex (F62) it's the
    /// codex CLI's state output. Callers are responsible for reading
    /// the file off disk and passing the string here — keeping the
    /// trait pure-functional makes mocking trivial.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError>;

    /// Best-effort enumeration of in-flight subagents. Returns an
    /// empty `Vec` when the harness doesn't surface this data —
    /// V0.4.0 Claude Code returns empty until upstream API exposes it;
    /// Codex stub returns empty too.
    fn subagent_states(&self, _snapshot: &HarnessSnapshot) -> Vec<SubagentState> {
        Vec::new()
    }

    /// Spawn a new harness session. For Claude Code this invokes
    /// `claude --bg --agent <role>` and captures the resulting
    /// `job_id` into the returned [`SessionHandle`]. The actual
    /// session lives in Claude Code's daemon, NOT inside a ccteam
    /// tmux session (V0.4.0 red line: ccteam does not host CC).
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError>;

    /// Graceful shutdown. For Claude Code: read pid from
    /// `~/.claude/jobs/<job_id>/state.json` and send `SIGTERM`. The
    /// ONLY caller in V0.4.0 is a user-initiated stop (e.g.
    /// `ccteam session rm`, MCP `stop_agent`) — never silent kill
    /// (red line CLAUDE.md §三).
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError>;
}

/// Normalized status snapshot from any harness. Field set is the
/// minimum union covered by every shipping harness (Claude Code's
/// `state.json` today; Codex's equivalent will fill the same shape
/// in F62). Unknown / missing fields fall back to defaults rather
/// than failing — `raw` carries the full upstream JSON for
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
    /// Human-readable model name (`"claude-opus-4-5"`, `"Claude Sonnet 4.5"`, etc.).
    pub model_display_name: String,
    /// Context window utilization, 0..=100. Missing → 0.
    pub context_used_pct: u8,
    /// Cumulative session cost in USD. Coerced to 0.0 when missing.
    pub cost_usd_total: f64,
    /// Five-hour rate-limit utilization (Claude Code field). `None`
    /// when the harness doesn't surface this surface — V0.4.0
    /// `state.json` does not yet expose it.
    pub rate_limit_pct: Option<u8>,
    /// Session cwd as reported by the harness. `None` when the
    /// `state.json` doesn't record it.
    pub cwd: Option<PathBuf>,
    /// Full upstream JSON. Carried verbatim so future harness
    /// upgrades don't require a ccteam release to surface new fields.
    pub raw: serde_json::Value,
    /// Wall-clock time the snapshot was ingested.
    pub captured_at: DateTime<Utc>,
}

/// Optional per-snapshot subagent state. V0.4.0 always empty — the
/// shape is reserved for when Claude Code's API exposes structured
/// subagent progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentState {
    /// Subagent type ("main" / "general-purpose" / "code-reviewer" / …).
    pub kind: String,
    /// Human label, when distinct from `kind`.
    pub label: Option<String>,
    /// How long the subagent has been running.
    pub running_for: Option<std::time::Duration>,
    /// Cumulative input tokens.
    pub tokens_in: Option<u64>,
    /// Cumulative output tokens.
    pub tokens_out: Option<u64>,
}

/// Inputs for [`HarnessAdapter::spawn_session`]. The role name is
/// fed straight into `claude --bg --agent <role>`; agent body lives
/// in the project's `.claude/agents/<role>.md` (Claude Code-managed).
#[derive(Debug, Clone)]
pub struct SpawnOpts {
    /// Harness identifier (`"claude-code"`, `"codex"`). Should match
    /// the adapter's [`HarnessAdapter::name`].
    pub harness: &'static str,
    /// Claude Code agent role. Resolves to `.claude/agents/<role>.md`
    /// inside `cwd`. Free-form string — the orchestrator passes whatever
    /// `workflow.yaml::agents.<role>` defines.
    pub role: String,
    /// Project slug (e.g. `"flex-foo"` after F22's team prefix).
    pub slug: String,
    /// Session id (e.g. `"claude-1"`, `"codex-2"`). Used by the
    /// orchestrator + web layer for indexing; not consumed by the
    /// harness command line itself.
    pub sid: String,
    /// Working directory for the spawned session — typically
    /// `~/projects/<team>-<slug>/`. Passed to Claude Code via
    /// `--workdir`.
    pub cwd: PathBuf,
}

/// Outcome of [`HarnessAdapter::spawn_session`]. Owned by the caller
/// (orchestrator state, flex `state.json::sessions[]`); the adapter
/// never caches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHandle {
    /// V0.4.0 F61: Claude Code background `job_id` as printed by
    /// `claude --bg --agent <role>` on stdout. This is the primary
    /// key for observability — `~/.claude/jobs/<job_id>/state.json`
    /// is read on every `ingest_snapshot` call.
    pub job_id: String,
    /// tmux session name. Legacy field retained for back-compat with
    /// flex multi-session callsites (`session add --harness claude`
    /// records this in `state.json::sessions[]`). For
    /// claude-bg-driven sessions, this is set to a synthetic
    /// `"claude-bg:<job_id>"` form — tmux is NOT actually running.
    /// Codex sessions (F62) will set this to a real tmux session name.
    pub tmux_session: String,
    /// Harness identifier — matches [`HarnessAdapter::name`].
    pub harness: String,
    /// Session id, mirroring [`SpawnOpts::sid`].
    pub sid: String,
    /// Background session pid as reported by Claude Code's
    /// `state.json`. `None` immediately post-spawn before the
    /// daemon has written its first state file; populated by the
    /// orchestrator on the first successful `ingest_snapshot`.
    pub pid: Option<u32>,
    /// Wall-clock start time.
    pub started_at: DateTime<Utc>,
}

/// V0.4.0 F61 error type for every fallible [`HarnessAdapter`]
/// surface. `NotImplemented` carries `&'static str` fields so const
/// callsites (`CodexAdapter`'s stub error path) compile to a
/// non-allocating literal.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// JSON parse / shape mismatch on the harness state.json channel.
    #[error("snapshot ingest failed: {0}")]
    IngestFailed(String),
    /// `claude --bg` invocation failed, or its stdout did not contain
    /// a parsable `job_id`.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    /// `shutdown_session` could not send SIGTERM or could not parse
    /// the pid from `state.json`.
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),
    /// Adapter declares the surface unsupported. Codex returns this
    /// in V0.3.1; F62 will fill in real impls. The `&'static str`
    /// payloads keep const-string callsites allocation-free.
    #[error("harness '{harness}' not implemented in V0.3.1: {reason}")]
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
// ClaudeCodeAdapter (V0.4.0 F61 thin refactor)
// =====================================================================

/// V0.4.0 F61 [`HarnessAdapter`] implementation for Anthropic's Claude
/// Code Agent View (`claude --bg --agent`). Stateless zero-size struct
/// — share one `'static` instance.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// Build a fresh `ClaudeCodeAdapter`. Const so callers can write
    /// `static CC: ClaudeCodeAdapter = ClaudeCodeAdapter::new();`.
    pub const fn new() -> Self {
        Self
    }
}

/// Environment variable used by tests to override the `claude` binary
/// path. Production code resolves `claude` from `$PATH` unless the
/// caller sets this. Tests in `tests/harness_thin_test.rs` set it to
/// a tmpdir-resident mock script.
const CLAUDE_BIN_ENV: &str = "CCTEAM_CLAUDE_BIN";

/// Environment variable used by tests to override the
/// `~/.claude/jobs/` root. Production code resolves it via
/// [`state_json_path`] (relative to `dirs::home_dir()`); tests set
/// this to a tmpdir so `ingest_snapshot` reads from a known location.
pub const CLAUDE_JOBS_DIR_ENV: &str = "CCTEAM_CLAUDE_JOBS_DIR";

impl HarnessAdapter for ClaudeCodeAdapter {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        parse_cc_state_json(raw, self.name())
    }

    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        let bin = std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string());
        let cwd_disp = opts.cwd.display().to_string();
        let output = Command::new(&bin)
            .args([
                "--bg",
                "--agent",
                &opts.role,
                "--output-format",
                "stream-json",
                "--workdir",
                &cwd_disp,
            ])
            .output()
            .map_err(|err| {
                HarnessError::SpawnFailed(format!(
                    "invoke `{bin} --bg --agent {role}`: {err}",
                    role = opts.role
                ))
            })?;

        if !output.status.success() {
            return Err(HarnessError::SpawnFailed(format!(
                "`claude --bg --agent {role}` exited {status}: {stderr}",
                role = opts.role,
                status = output.status,
                stderr = String::from_utf8_lossy(&output.stderr),
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let job_id = extract_job_id(&stdout).ok_or_else(|| {
            HarnessError::SpawnFailed(format!("no `job_id` field in claude --bg stdout: {stdout}",))
        })?;

        Ok(SessionHandle {
            job_id: job_id.clone(),
            // V0.4.0 F61: synthetic name. CC sessions no longer run
            // inside a ccteam-managed tmux session. The field is
            // retained for back-compat with flex `state.json::sessions[]`
            // callsites that still display it.
            tmux_session: format!("claude-bg:{job_id}"),
            harness: self.name().to_string(),
            sid: opts.sid,
            pid: None,
            started_at: Utc::now(),
        })
    }

    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError> {
        // Read the state.json to find the pid, then send SIGTERM.
        // Missing state.json or already-stopped pid → no-op (idempotent).
        let path = state_json_path(&handle.job_id);
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Already gone — treat as success.
                return Ok(());
            }
            Err(err) => {
                return Err(HarnessError::ShutdownFailed(format!(
                    "read {}: {err}",
                    path.display(),
                )));
            }
        };
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
            HarnessError::ShutdownFailed(format!("parse {}: {err}", path.display()))
        })?;
        let pid = v
            .get("pid")
            .and_then(|n| n.as_u64())
            .and_then(|n| i32::try_from(n).ok());
        let Some(pid) = pid else {
            // No pid recorded → nothing to kill.
            return Ok(());
        };

        // SIGTERM via libc kill. If the process has already exited,
        // kill(2) returns ESRCH; treat as success (idempotent).
        // Safety: kill is async-signal-safe and side-effect bounded.
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        if rc != 0 {
            let errno = std::io::Error::last_os_error();
            if errno.raw_os_error() == Some(libc::ESRCH) {
                // Process already gone — success.
                return Ok(());
            }
            return Err(HarnessError::ShutdownFailed(format!(
                "kill({pid}, SIGTERM) failed: {errno}",
            )));
        }
        Ok(())
    }
}

/// Parse the first balanced JSON object out of `stdout`. `claude --bg`
/// emits a stream-json line containing `{ "job_id": "...", ... }` on
/// success. We scan for the first `{` then balance brace depth so we
/// pick up the full object even when more JSON follows on the same
/// line.
fn extract_job_id(stdout: &str) -> Option<String> {
    let bytes = stdout.as_bytes();
    let start = bytes.iter().position(|b| *b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, b) in bytes.iter().enumerate().skip(start) {
        let c = *b;
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let slice = &stdout[start..=i];
                    let v: serde_json::Value = serde_json::from_str(slice).ok()?;
                    return v
                        .get("job_id")
                        .and_then(|j| j.as_str())
                        .map(|s| s.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a `~/.claude/jobs/<job_id>/state.json` body into a
/// normalized [`HarnessSnapshot`]. Schema (V0.4.0 PRD §6.4):
///
/// ```json
/// {
///   "status": "running | idle | error | completed",
///   "model": "claude-opus-4-5",
///   "context_pct": 0.45,
///   "cost_usd": 1.23,
///   "turn_count": 12,
///   "pid": 12345,
///   "last_activity": "2026-05-14T10:00:00Z",
///   "cwd": "/home/u/projects/dev-foo"
/// }
/// ```
///
/// Unknown / missing fields fall back to defaults rather than failing;
/// `context_pct` accepts either fraction (0.45) or percent (45) for
/// forward-compat with upstream changes.
pub fn parse_cc_state_json(raw: &str, harness_name: &str) -> Result<HarnessSnapshot, HarnessError> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| HarnessError::IngestFailed(format!("parse state.json: {err}")))?;
    let model_display_name = value
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    let context_used_pct = value
        .get("context_pct")
        .and_then(|p| p.as_f64())
        .map(|p| {
            // Accept fraction (0.0..=1.0) or percent (0..=100).
            let scaled = if p <= 1.0 { p * 100.0 } else { p };
            scaled.clamp(0.0, 100.0).round() as u8
        })
        .unwrap_or(0);
    let cost_usd_total = value
        .get("cost_usd")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    let cwd = value.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from);

    Ok(HarnessSnapshot {
        harness: harness_name.to_string(),
        model_display_name,
        context_used_pct,
        cost_usd_total,
        rate_limit_pct: None,
        cwd,
        raw: value,
        captured_at: Utc::now(),
    })
}

// =====================================================================
// CodexAdapter (V0.3.1 F47 stub — F62 fills it in)
// =====================================================================

/// V0.3.1 F47 [`HarnessAdapter`] stub for OpenAI's `codex` CLI. Every
/// fallible method returns [`HarnessError::NotImplemented`] with a
/// `&'static str` reason. The real implementation lands in F62 as part
/// of the V0.4.0 round (see `docs/v0-4-0/prd.md` §6.5).
///
/// Stateless zero-size — share one `'static` instance.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

/// V0.3.1 F47 stub reason — a single harmonized constant referenced
/// from every `HarnessAdapter` method. Keeping this `&'static str`
/// preserves the `NotImplemented::reason: &'static str` contract.
///
/// Both substrings the F47 verification tests check — `"V0.3.2"` and
/// `"docs/research/ccteam-codex-integration.md"` — are present here.
const CODEX_NOT_IMPLEMENTED_REASON: &str =
    "Codex adapter is trait-stub in V0.3.1; full Codex CLI integration deferred to V0.3.2+ \
     — see docs/research/ccteam-codex-integration.md M1-M5 and docs/v0-3-1/prd.md §F47. \
     Use --harness=claude or wait for V0.3.2.";

impl CodexAdapter {
    /// Build a fresh `CodexAdapter`. Const so callers can write
    /// `static CX: CodexAdapter = CodexAdapter::new();`.
    pub const fn new() -> Self {
        Self
    }
}

impl HarnessAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn ingest_snapshot(&self, _raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        Err(HarnessError::NotImplemented {
            harness: "codex",
            reason: CODEX_NOT_IMPLEMENTED_REASON,
        })
    }

    fn spawn_session(&self, _opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            harness: "codex",
            reason: CODEX_NOT_IMPLEMENTED_REASON,
        })
    }

    fn shutdown_session(&self, _handle: &SessionHandle) -> Result<(), HarnessError> {
        Err(HarnessError::NotImplemented {
            harness: "codex",
            reason: CODEX_NOT_IMPLEMENTED_REASON,
        })
    }
}

// =====================================================================
// Public helpers
// =====================================================================

/// Resolve the canonical path to a Claude Code background job's state
/// file: `~/.claude/jobs/<job_id>/state.json`. Tests can override the
/// jobs root via [`CLAUDE_JOBS_DIR_ENV`].
pub fn state_json_path(job_id: &str) -> PathBuf {
    let jobs_root = std::env::var(CLAUDE_JOBS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        });
    jobs_root.join(job_id).join("state.json")
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

    // ----- ClaudeCodeAdapter::ingest_snapshot / parse_cc_state_json -----

    #[test]
    fn ingest_full_state_json() {
        let raw = r#"{
            "status": "running",
            "model": "claude-opus-4-5",
            "context_pct": 0.45,
            "cost_usd": 1.23,
            "turn_count": 12,
            "pid": 12345,
            "last_activity": "2026-05-14T10:00:00Z",
            "cwd": "/home/u/projects/dev-foo"
        }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.harness, "claude-code");
        assert_eq!(snap.model_display_name, "claude-opus-4-5");
        // 0.45 fraction → 45 percent.
        assert_eq!(snap.context_used_pct, 45);
        assert!((snap.cost_usd_total - 1.23).abs() < 1e-9);
        assert_eq!(snap.rate_limit_pct, None);
        assert_eq!(
            snap.cwd.as_deref(),
            Some(std::path::Path::new("/home/u/projects/dev-foo")),
        );
    }

    #[test]
    fn ingest_state_json_with_percent_form() {
        // Forward-compat: upstream may switch to integer-percent form.
        let raw = r#"{ "model": "X", "context_pct": 73 }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.context_used_pct, 73);
    }

    #[test]
    fn ingest_missing_cost_falls_back_to_zero() {
        let raw = r#"{ "model": "Haiku 4.5", "context_pct": 0.05 }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.cost_usd_total, 0.0);
        assert_eq!(snap.rate_limit_pct, None);
        assert!(snap.cwd.is_none());
    }

    #[test]
    fn ingest_malformed_json_returns_ingest_failed() {
        let raw = r#"{"model": "broken"#;
        let err = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap_err();
        assert!(matches!(err, HarnessError::IngestFailed(_)));
    }

    #[test]
    fn ingest_unknown_fields_pass_through_in_raw() {
        let raw = r#"{
            "model": "Opus 4.7",
            "context_pct": 0.12,
            "future_field": {"nested": [1, 2, 3]}
        }"#;
        let snap = ClaudeCodeAdapter::new().ingest_snapshot(raw).unwrap();
        assert_eq!(snap.raw["future_field"]["nested"][1], 2);
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
    fn claude_code_adapter_name_is_stable() {
        // Stability test — `ccteam-web` SSE wire format embeds this
        // string into JSON payloads. A rename here cascades into the
        // dashboard JS + interfaces.md §15.6.
        assert_eq!(ClaudeCodeAdapter::new().name(), "claude-code");
    }

    #[test]
    fn subagent_states_default_empty_for_v0_4_0() {
        // V0.4.0 contract: ClaudeCodeAdapter never emits subagent
        // states (deferred — Claude Code doesn't expose structured
        // subagent data yet). Test pins the contract so a future PR
        // that wires it must update this expectation.
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

    // ----- extract_job_id -----

    #[test]
    fn extract_job_id_from_simple_object() {
        let stdout = r#"{"job_id":"abc123","status":"started"}"#;
        assert_eq!(extract_job_id(stdout).as_deref(), Some("abc123"));
    }

    #[test]
    fn extract_job_id_from_nested_object() {
        let stdout = r#"{"job_id":"jid-1","extra":{"nested":{"deeper":"value"}}}"#;
        assert_eq!(extract_job_id(stdout).as_deref(), Some("jid-1"));
    }

    #[test]
    fn extract_job_id_with_leading_garbage() {
        // Real claude CLI prefixes some banner output before the
        // stream-json line.
        let stdout = "Starting background session...\n{\"job_id\":\"xyz\"}\n";
        assert_eq!(extract_job_id(stdout).as_deref(), Some("xyz"));
    }

    #[test]
    fn extract_job_id_missing_returns_none() {
        let stdout = r#"{"status":"started"}"#;
        assert_eq!(extract_job_id(stdout), None);
    }

    // ----- state_json_path -----
    //
    // Env-mutating env-override coverage lives in
    // `tests/harness_thin_test.rs` (integration test, separate
    // process) per CLAUDE.md §六. Here we only assert the
    // production-default shape, which is deterministic without env.

    #[test]
    fn state_json_path_default_shape() {
        // Skip if CCTEAM_CLAUDE_JOBS_DIR is leaked from a sibling
        // integration test running concurrently.
        if std::env::var(CLAUDE_JOBS_DIR_ENV).is_ok() {
            return;
        }
        let p = state_json_path("abc123");
        let s = p.to_string_lossy();
        assert!(s.contains(".claude"), "{s}");
        assert!(s.contains("jobs"), "{s}");
        assert!(s.ends_with("abc123/state.json"), "{s}");
    }

    // ----- CodexAdapter (V0.3.1 F47 stub) -----

    #[test]
    fn codex_adapter_name_is_codex() {
        assert_eq!(CodexAdapter::new().name(), "codex");
    }

    #[test]
    fn codex_adapter_spawn_returns_not_implemented_with_v0_3_2_reason() {
        let opts = SpawnOpts {
            harness: "codex",
            role: "main".into(),
            slug: "flex-foo".into(),
            sid: "codex-1".into(),
            cwd: PathBuf::from("/tmp"),
        };
        match CodexAdapter::new().spawn_session(opts).unwrap_err() {
            HarnessError::NotImplemented { harness, reason } => {
                assert_eq!(harness, "codex");
                assert!(
                    reason.contains("V0.3.2"),
                    "reason should cite V0.3.2: {reason}"
                );
                assert!(
                    reason.contains("docs/research/ccteam-codex-integration.md"),
                    "reason should cite codex-integration research doc: {reason}",
                );
            }
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn codex_adapter_ingest_returns_not_implemented() {
        match CodexAdapter::new().ingest_snapshot("{}").unwrap_err() {
            HarnessError::NotImplemented { harness, reason } => {
                assert_eq!(harness, "codex");
                assert!(reason.contains("V0.3.2"));
                assert!(reason.contains("docs/research/ccteam-codex-integration.md"));
            }
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn codex_adapter_shutdown_returns_not_implemented() {
        let handle = SessionHandle {
            job_id: "n/a".into(),
            tmux_session: "ccteam-flex-foo-codex-1".into(),
            harness: "codex".into(),
            sid: "codex-1".into(),
            pid: None,
            started_at: Utc::now(),
        };
        match CodexAdapter::new().shutdown_session(&handle).unwrap_err() {
            HarnessError::NotImplemented { harness, reason } => {
                assert_eq!(harness, "codex");
                assert!(reason.contains("V0.3.2"));
                assert!(reason.contains("docs/research/ccteam-codex-integration.md"));
            }
            other => panic!("expected NotImplemented, got {other:?}"),
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
