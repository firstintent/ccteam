//! Minimal progress.jsonl helpers used by harness-owned adapters.
//!
//! `ccteam-core` owns the richer query surface, but harness cannot depend
//! on core without reintroducing a cargo cycle. Keep only the small append
//! and row-builder subset needed by execution adapters here.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use crate::ccteam_root_from_env;

pub const CHAT_SESSION_RESET: &str = "chat_session_reset";
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
    serde_json::to_writer(&mut file, event).context("serialize progress event")?;
    file.write_all(b"\n")
        .with_context(|| format!("write newline to {}", path.display()))?;
    Ok(())
}

pub fn build_chat_session_reset_event_with_reason(role: &str, reason: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "reason": reason,
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
