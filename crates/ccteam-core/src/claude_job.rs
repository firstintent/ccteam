//! V0.4.5 F80 — Liveness probe for `claude --bg --agent` background jobs.
//!
//! Every claude background session writes its lifecycle into
//! `~/.claude/jobs/<job_id>/state.json` (or `$CCTEAM_CLAUDE_JOBS_DIR`
//! when set — same env override `harness::state_json_path` reads).
//! This module is the SHARED helper both `queries::workflow_summary`
//! (read-side: phantom-running detection) and
//! `orchestrator::poll_completions` (write-side: stale-spawn cleanup)
//! call when they need to decide "is the bg job still alive, or did
//! its host process die without writing a matching `agent_done`?"
//!
//! ## Background
//!
//! Pre-F80 the only signal the orchestrator emitted for a finished
//! agent was the `agent_done` line in `progress.jsonl`. That line is
//! written inside `poll_completions` after observing `state.json::state ∈
//! {done, failed, crashed}`. When the daemon itself is SIGKILLed
//! (V0.4.5 still has the shutdown-deadlock force-kill path), in-flight
//! `claude --bg` sessions die without anything writing the matching
//! `agent_done`. The stale `agent_spawn` line lingers forever; the
//! web UI counts it as "running" until manually cleaned.
//!
//! F80 fix: every consumer that needs "is this spawn really still
//! running?" cross-references the spawn's recorded `job_id` against
//! `state.json`. Three terminal signals win:
//!
//! 1. `state.json` is missing entirely (job dir vanished).
//! 2. `firstTerminalAt` field is non-null (Claude Code's own
//!    end-of-session timestamp).
//! 3. `state` field is in the terminal set (`done`, `failed`,
//!    `crashed`, `stopped`, legacy `completed` / `error`).
//!
//! Any of those → [`probe_job`] returns [`JobLiveness::Terminal`] with
//! best-effort `cost_usd` (sourced from state.json when present, else
//! 0.0) and a coarse `status` string the orchestrator can stamp onto a
//! synthetic `agent_done` event.
//!
//! ## Red lines
//!
//! - **No mutation here.** The module only reads `state.json`; emitting
//!   any `agent_done` event is the caller's responsibility (matches the
//!   "progress.jsonl is SoT" red line — only the orchestrator writes
//!   workflow events).
//! - **`job_id = None` always counts as terminal.** Old agent_spawn
//!   lines written before F80 do not carry `job_id`; they all surface
//!   as `Terminal { status: "killed", cost_usd: 0.0 }` so the stale
//!   rows clear once `poll_completions` next runs. There is no
//!   migration path for pre-F80 `progress.jsonl` history beyond this
//!   one-shot drain.

use std::path::PathBuf;

use serde_json::Value;

/// Outcome of a single liveness probe.
#[derive(Debug, Clone, PartialEq)]
pub enum JobLiveness {
    /// `state.json` parsed cleanly and the job is still working.
    /// Treat the matching `agent_spawn` as legitimately running.
    Running,
    /// The job is gone (state.json missing / job_id unset) OR has
    /// finished (`firstTerminalAt` non-null OR `state` is terminal).
    /// Caller should emit a synthetic `agent_done` to retire the
    /// outstanding `agent_spawn`.
    Terminal {
        /// Coarse status string the orchestrator stamps onto the
        /// `agent_done` event. Values: `"completed"` (Claude reported
        /// `done`), `"error"` (`failed` / `crashed`), `"killed"`
        /// (state.json missing or job_id absent — daemon SIGKILL
        /// casualty).
        status: &'static str,
        /// Best-effort cumulative cost the orchestrator should append
        /// to its accumulator + state.cost_used_usd. Sourced from
        /// `state.json::cost_usd` / `cost_usd_total` when present,
        /// else 0.0.
        cost_usd: f64,
    },
}

/// Probe a `claude --bg` background job's liveness via
/// `harness::state_json_path(job_id)`.
///
/// Returns [`JobLiveness::Terminal`] with `status: "killed"` when:
/// - `job_id` is `None` (legacy agent_spawn row without F80 plumbing),
/// - the file does not exist (host job dir wiped),
/// - the file exists but is unparseable JSON (treat as gone — safer
///   than leaving a phantom running),
/// - `firstTerminalAt` is non-null,
/// - `state ∈ {failed, crashed, stopped, error}` (mapped to `"error"`)
/// - `state ∈ {done, completed}` (mapped to `"completed"`).
///
/// Returns [`JobLiveness::Running`] only when `state.json` parses and
/// none of the terminal signals fire — i.e. an active session whose
/// host process is still attached.
pub fn probe_job(job_id: Option<&str>) -> JobLiveness {
    let Some(id) = job_id else {
        return JobLiveness::Terminal {
            status: "killed",
            cost_usd: 0.0,
        };
    };
    let path = crate::harness::state_json_path(id);
    probe_state_json(&path)
}

/// Lower-level helper for tests — bypasses the `state_json_path`
/// resolver so unit tests can pass a `tempdir()` path directly without
/// fiddling with `$CCTEAM_CLAUDE_JOBS_DIR`.
pub fn probe_state_json(path: &std::path::Path) -> JobLiveness {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            return JobLiveness::Terminal {
                status: "killed",
                cost_usd: 0.0,
            }
        }
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            return JobLiveness::Terminal {
                status: "killed",
                cost_usd: 0.0,
            }
        }
    };
    classify(&value)
}

/// Pure classifier — useful for unit tests that already have the
/// parsed `Value` in hand (no IO).
pub fn classify(value: &Value) -> JobLiveness {
    let cost_usd = value
        .get("cost_usd")
        .or_else(|| value.get("cost_usd_total"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // F80 — Claude Code 2.1.x writes `firstTerminalAt` once the
    // session enters a terminal state. Non-null → finished, even if
    // `state` still reads `"working"` for a tick (race window).
    let first_terminal_at_present = value
        .get("firstTerminalAt")
        .map(|v| !v.is_null())
        .unwrap_or(false);

    let state_str = value
        .get("state")
        .and_then(|s| s.as_str())
        .or_else(|| value.get("status").and_then(|s| s.as_str()))
        .unwrap_or("working");
    let terminal_status = match state_str {
        "done" | "completed" => Some("completed"),
        "failed" | "crashed" | "error" => Some("error"),
        "stopped" => Some("stopped"),
        _ => None,
    };

    if let Some(status) = terminal_status {
        return JobLiveness::Terminal { status, cost_usd };
    }
    if first_terminal_at_present {
        return JobLiveness::Terminal {
            status: "completed",
            cost_usd,
        };
    }
    JobLiveness::Running
}

/// Resolve the absolute state.json path for a `(job_id)`. Thin
/// re-export of `harness::state_json_path` so call sites that want to
/// log the path don't need to import `harness` directly.
pub fn job_state_path(job_id: &str) -> PathBuf {
    crate::harness::state_json_path(job_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn probe_returns_killed_for_none_job_id() {
        match probe_job(None) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "killed");
                assert_eq!(cost_usd, 0.0);
            }
            other => panic!("expected killed, got {other:?}"),
        }
    }

    #[test]
    fn probe_state_json_returns_killed_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope/state.json");
        match probe_state_json(&missing) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "killed");
                assert_eq!(cost_usd, 0.0);
            }
            other => panic!("expected killed, got {other:?}"),
        }
    }

    #[test]
    fn probe_state_json_returns_killed_when_unparseable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        std::fs::write(&path, b"{ broken json").unwrap();
        match probe_state_json(&path) {
            JobLiveness::Terminal { status, .. } => assert_eq!(status, "killed"),
            other => panic!("expected killed, got {other:?}"),
        }
    }

    #[test]
    fn classify_running_when_state_working_and_no_first_terminal_at() {
        let v = json!({
            "state": "working",
            "firstTerminalAt": null,
            "cost_usd": 0.42,
        });
        assert_eq!(classify(&v), JobLiveness::Running);
    }

    #[test]
    fn classify_terminal_when_state_done() {
        let v = json!({
            "state": "done",
            "firstTerminalAt": "2026-05-15T12:00:00Z",
            "cost_usd": 1.25,
        });
        match classify(&v) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "completed");
                assert!((cost_usd - 1.25).abs() < 1e-9);
            }
            other => panic!("expected completed, got {other:?}"),
        }
    }

    #[test]
    fn classify_terminal_when_state_failed() {
        let v = json!({
            "state": "failed",
            "cost_usd": 0.10,
        });
        match classify(&v) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "error");
                assert!((cost_usd - 0.10).abs() < 1e-9);
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn classify_terminal_via_first_terminal_at_even_when_state_working() {
        // Race window: Claude Code wrote firstTerminalAt but state
        // field hasn't flipped yet. F80 treats this as terminal.
        let v = json!({
            "state": "working",
            "firstTerminalAt": "2026-05-15T12:00:00Z",
        });
        match classify(&v) {
            JobLiveness::Terminal { status, .. } => assert_eq!(status, "completed"),
            other => panic!("expected completed, got {other:?}"),
        }
    }

    #[test]
    fn classify_missing_cost_defaults_to_zero() {
        let v = json!({"state": "done"});
        match classify(&v) {
            JobLiveness::Terminal { cost_usd, .. } => assert_eq!(cost_usd, 0.0),
            other => panic!("expected terminal, got {other:?}"),
        }
    }
}
