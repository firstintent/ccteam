//! Shared helpers for reading Claude Code session transcripts. Used by
//! `parse_phase_end` (to find the assistant's terminal sigil) and
//! `cost_accumulate` (to read the assistant's `usage` block).

use std::path::Path;

use anyhow::{Context, Result};

/// Locate the most recent `role: assistant` `message` record in a
/// transcript JSONL file. Returns the inner `message` object so
/// callers can read whichever fields they need (`content`, `usage`,
/// ...). `None` means the transcript has no assistant messages yet.
pub fn last_assistant_message(path: &Path) -> Result<Option<serde_json::Value>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read transcript {}", path.display()))?;

    for line in content.lines().rev() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|s| s.as_str()) != Some("message") {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        if msg.get("role").and_then(|s| s.as_str()) != Some("assistant") {
            continue;
        }
        return Ok(Some(msg.clone()));
    }
    Ok(None)
}

/// Concatenate the `text` blocks in a Claude API content array, in
/// order. Tolerant of the legacy "string content" shape (returned
/// verbatim when `content` is a plain string).
pub fn message_text(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let arr = content.as_array()?;
    let parts: Vec<String> = arr
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|s| s.as_str()) == Some("text") {
                block.get("text").and_then(|s| s.as_str()).map(String::from)
            } else {
                None
            }
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
