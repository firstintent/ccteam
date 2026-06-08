//! Minimal progress.jsonl helpers used by harness-owned adapters.
//!
//! `ccteam-core` owns the richer query surface, but harness cannot depend
//! on core without reintroducing a cargo cycle. Keep only the small append
//! and row-builder subset needed by execution adapters here.

use std::io::Write as _;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use crate::ccteam_root_from_env;

pub const CHAT_SESSION_RESET: &str = "chat_session_reset";
pub const CHAT_SESSION_STARTED: &str = "chat_session_started";
pub const CHAT_TURN_USER_PROMPT: &str = "chat_turn_user_prompt";
pub const CHAT_TURN_COMPLETED: &str = "chat_turn_completed";
pub const CHAT_SESSION_RESET_WITH_RECOVERY: &str = "chat_session_reset_with_recovery";
pub const CHAT_COMPACT_DONE: &str = "chat_compact_done";
pub const CHAT_HOP_ESCALATE: &str = "chat_hop_escalate";
pub const CHAT_TOOL_CALL_STARTED: &str = "chat_tool_call_started";
pub const CHAT_BOT_PERMANENT_FAILURE: &str = "chat_bot_permanent_failure";
pub const CHAT_MARKER_SELF_HEAL_ATTEMPT: &str = "chat_marker_self_heal_attempt";
pub const CHAT_BOT_MARKER_STUCK: &str = "chat_bot_marker_stuck";
pub const CHAT_TURN_RUNNING_LONG: &str = "chat_turn_running_long";
pub const CHAT_TURN_TIMEOUT: &str = "chat_turn_timeout";
/// v0.8.7 review-fix (R-L1) — a HITL session is PARKED awaiting a human
/// approve/deny on a non-allowlist tool call. Emitted when the permission
/// prompt is outstanding so an operator (status / dashboard / `progress`)
/// sees the agent is blocked, not stuck.
pub const CHAT_PERMISSION_PROMPT_OUTSTANDING: &str = "chat_permission_prompt_outstanding";
pub const CODEX_PLAN_UPDATED: &str = "codex_plan_updated";
pub const CODEX_TOKEN_USAGE: &str = "codex_token_usage";
pub const CODEX_THREAD_STATUS: &str = "codex_thread_status";
pub const CODEX_RATE_LIMIT: &str = "codex_rate_limit";

pub fn hooks_script_from_env() -> Option<PathBuf> {
    ccteam_root_from_env().map(|root| root.join("hooks").join("hook.sh"))
}

pub fn progress_jsonl_from_env(slug: &str) -> Option<PathBuf> {
    ccteam_root_from_env().map(|root| root.join("progress").join(format!("{slug}.jsonl")))
}

pub fn append_event(path: &Path, event: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;

    let _lock =
        ProgressFileLock::lock(&file).with_context(|| format!("lock {}", path.display()))?;

    let mut line = Vec::new();
    serde_json::to_writer(&mut line, event).context("serialize progress event")?;
    line.push(b'\n');
    file.write_all(&line)
        .with_context(|| format!("write event to {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
struct ProgressFileLock(std::os::fd::RawFd);

#[cfg(unix)]
impl ProgressFileLock {
    fn lock(file: &std::fs::File) -> std::io::Result<Self> {
        let fd = file.as_raw_fd();
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc == 0 {
            Ok(Self(fd))
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
impl Drop for ProgressFileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct ProgressFileLock;

#[cfg(not(unix))]
impl ProgressFileLock {
    fn lock(_file: &std::fs::File) -> std::io::Result<Self> {
        Ok(Self)
    }
}

pub fn build_chat_tool_call_started_event(role: &str, tool: &str) -> Value {
    json!({
        "event": CHAT_TOOL_CALL_STARTED,
        "role": role,
        "tool": tool,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.8.7 review-fix (R-L1) — a HITL permission prompt is OUTSTANDING: the
/// session is parked awaiting a human approve/deny for `tool` (`summary` is
/// the one-line tool-call preview). Lets an operator see a parked agent
/// instead of mistaking the silence for a stuck/dead session. `ttl_secs` is
/// the prompt's deadline (deny on lapse — fail-safe).
pub fn build_chat_permission_prompt_outstanding_event(
    role: &str,
    tool: &str,
    summary: &str,
    ttl_secs: u64,
) -> Value {
    let trimmed: String = summary.chars().take(256).collect();
    json!({
        "event": CHAT_PERMISSION_PROMPT_OUTSTANDING,
        "role": role,
        "tool": tool,
        "summary": trimmed,
        "ttl_secs": ttl_secs,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_started_event(role: &str, project_dir: &str) -> Value {
    json!({
        "event": CHAT_SESSION_STARTED,
        "role": role,
        "project_dir": project_dir,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_user_prompt_event(role: &str, turn_id: &str, prompt_excerpt: &str) -> Value {
    let trimmed: String = prompt_excerpt.chars().take(256).collect();
    json!({
        "event": CHAT_TURN_USER_PROMPT,
        "role": role,
        "turn_id": turn_id,
        "prompt_excerpt": trimmed,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_completed_event(
    role: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
) -> Value {
    json!({
        "event": CHAT_TURN_COMPLETED,
        "role": role,
        "turn_id": turn_id,
        "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_event(role: &str, sid: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "sid": sid,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_event_with_reason(role: &str, sid: &str, reason: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "sid": sid,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_with_recovery_event(
    role: &str,
    sid: &str,
    recovered_turns: usize,
) -> Value {
    json!({
        "event": CHAT_SESSION_RESET_WITH_RECOVERY,
        "role": role,
        "sid": sid,
        "recovered_turns": recovered_turns,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_compact_done_event(role: &str) -> Value {
    json!({
        "event": CHAT_COMPACT_DONE,
        "role": role,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_hop_escalate_event(role: &str, hop_count: u32, last_bot: &str) -> Value {
    json!({
        "event": CHAT_HOP_ESCALATE,
        "role": role,
        "hop_count": hop_count,
        "last_bot": last_bot,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_bot_permanent_failure_event(role: &str, reason: &str, attempts: u32) -> Value {
    let trimmed: String = reason.chars().take(512).collect();
    json!({
        "event": CHAT_BOT_PERMANENT_FAILURE,
        "role": role,
        "reason": trimmed,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_marker_self_heal_attempt_event(role: &str, attempt_n: u32) -> Value {
    json!({
        "event": CHAT_MARKER_SELF_HEAL_ATTEMPT,
        "role": role,
        "attempt_n": attempt_n,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_bot_marker_stuck_event(role: &str, attempts: u32) -> Value {
    json!({
        "event": CHAT_BOT_MARKER_STUCK,
        "role": role,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_running_long_event(
    role: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    json!({
        "event": CHAT_TURN_RUNNING_LONG,
        "role": role,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_timeout_event(
    role: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    json!({
        "event": CHAT_TURN_TIMEOUT,
        "role": role,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "stuck": true,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_typed_event_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    json!({
        "kind": "typed_event",
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_merger_lossy_partial_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    json!({
        "kind": "merger_lossy_partial",
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_codex_plan_updated_event(
    thread_id: &str,
    turn_id: &str,
    explanation: Option<&str>,
    plan: Value,
) -> Value {
    let mut v = json!({
        "event": CODEX_PLAN_UPDATED,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "plan": plan,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(explanation) = explanation {
        v.as_object_mut().unwrap().insert(
            "explanation".to_string(),
            Value::String(explanation.to_string()),
        );
    }
    v
}

pub fn build_codex_token_usage_event(
    thread_id: &str,
    turn_id: &str,
    total: Value,
    last: Value,
    model_context_window: Option<i64>,
) -> Value {
    let mut v = json!({
        "event": CODEX_TOKEN_USAGE,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "total": total,
        "last": last,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(window) = model_context_window {
        v.as_object_mut().unwrap().insert(
            "model_context_window".to_string(),
            Value::Number(window.into()),
        );
    }
    v
}

pub fn build_codex_thread_status_event(
    thread_id: &str,
    status: &str,
    active_flags: Vec<String>,
) -> Value {
    json!({
        "event": CODEX_THREAD_STATUS,
        "vendor": "codex",
        "thread_id": thread_id,
        "status": status,
        "active_flags": active_flags,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_codex_rate_limit_event(snapshot: Value) -> Value {
    json!({
        "event": CODEX_RATE_LIMIT,
        "vendor": "codex",
        "snapshot": snapshot,
        "ts": Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_event_writes_exactly_one_jsonl_record_for_multiline_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": "chat_tool_use",
            "tool": "Bash",
            "cmd": "printf 'a\\nb\\n'",
        });

        append_event(&path, &event).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(lines[0]).unwrap(), event);
    }
}
