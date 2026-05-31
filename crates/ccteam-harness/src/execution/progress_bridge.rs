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
        "event": "typed_event",
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
        "event": "merger_lossy_partial",
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}
