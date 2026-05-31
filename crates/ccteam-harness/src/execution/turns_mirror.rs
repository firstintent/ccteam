//! V0.6.0 F108 / F118 — ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`.
//!
//! The Anthropic transcript at
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` is the wire SoT
//! for one *session*; it disappears when the user runs `/clear` or when
//! Claude rotates session-ids on compaction. ccteam owns a
//! parallel mirror — `<project>/.ccteam/chat/<bot>/turns.jsonl` — that:
//!
//! - never gets rotated by Claude,
//! - records exactly the (user / assistant / usage / tool-call) summary
//!   the F108 dual-track event stream emitted, and
//! - is the input to [`crate::execution::session_recovery`]'s F118
//!   `rebuild_from_turns_jsonl` flow.
//!
//! Schema (one [`TurnRecord`] per line):
//!
//! ```jsonl
//! {"turn_id":"...","ts":"2026-05-17T...","vendor":"claude","role":"<bot>",
//!  "user":"...","assistant":"...","usage":{...},"tool_calls":[...]}
//! ```
//!
//! Append is atomic (POSIX `O_APPEND` + one `write_all`; record bodies
//! fit comfortably under PIPE_BUF). Reads tolerate half-flushed tails
//! (lines that fail to deserialize are skipped).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One conversation turn the F108 dual-track stream observed. Optional
/// fields default to empty / null so half-completed turns (e.g. the
/// assistant errored before producing text) still round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub turn_id: String,
    pub ts: DateTime<Utc>,
    /// Vendor scalar (`"claude"` / `"codex"`). Plain string here so the
    /// jsonl is hand-greppable; the orchestrator never mixes vendors in
    /// one turns.jsonl file in V0.6.0.
    pub vendor: String,
    /// Bot role name (also `workflow.yaml chat.bot_name`).
    pub role: String,
    /// User-side prompt text. Empty when the turn was driven by a
    /// `SystemDirective` (e.g. `/compact`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// Assistant-side reply text (concatenation of every `text` block
    /// emitted on this turn).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub assistant: String,
    /// Token / cost accounting. Free-form `Value` so the wire shape
    /// stays aligned with whatever `UnifiedTokenUsage` evolves to.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub usage: Value,
    /// Brief summaries of any tool calls the assistant emitted this
    /// turn. Keeps the mirror useful for F118 recovery without bloating
    /// the file with full tool-input bodies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallSummary>,
}

/// Compact tool-call entry: `name`, optional file-path / arg excerpt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Default subdirectory under `<project>/.ccteam/chat/<bot>/`.
const CHAT_BASE: &str = ".ccteam/chat";

/// Resolve `<project>/.ccteam/chat/<bot>/turns.jsonl`.
pub fn turns_jsonl_path(project_dir: &Path, bot_role: &str) -> PathBuf {
    project_dir
        .join(CHAT_BASE)
        .join(bot_role)
        .join("turns.jsonl")
}

/// Resolve `<project>/.ccteam/chat/<bot>/`. Created by [`ensure_dir`].
pub fn chat_dir(project_dir: &Path, bot_role: &str) -> PathBuf {
    project_dir.join(CHAT_BASE).join(bot_role)
}

/// `mkdir -p <project>/.ccteam/chat/<bot>/`. Idempotent.
pub fn ensure_dir(project_dir: &Path, bot_role: &str) -> Result<()> {
    let p = chat_dir(project_dir, bot_role);
    fs::create_dir_all(&p).with_context(|| format!("create {}", p.display()))?;
    Ok(())
}

/// Append `record` as one JSONL line. Creates parent dir + file when
/// missing. Returns the absolute path written for caller logging.
pub fn append_turn(project_dir: &Path, bot_role: &str, record: &TurnRecord) -> Result<PathBuf> {
    ensure_dir(project_dir, bot_role)?;
    let path = turns_jsonl_path(project_dir, bot_role);
    let line = serde_json::to_string(record)? + "\n";
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(path)
}

/// Read every parseable record from the bot's turns.jsonl. Returns an
/// empty Vec when the file is absent (V0.6.0 F108 first-turn case).
pub fn read_all_turns(project_dir: &Path, bot_role: &str) -> Result<Vec<TurnRecord>> {
    let path = turns_jsonl_path(project_dir, bot_role);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TurnRecord>(trimmed) {
            Ok(r) => out.push(r),
            // Skip half-flushed / older-shape rows defensively — F118
            // recovery has to work on whatever survived.
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Return the last `n` parseable turns, in chronological order. F118
/// `rebuild_from_turns_jsonl` uses this to bound the conversation
/// history it injects into a fresh tmux session.
pub fn last_n_turns(project_dir: &Path, bot_role: &str, n: usize) -> Result<Vec<TurnRecord>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let all = read_all_turns(project_dir, bot_role)?;
    let start = all.len().saturating_sub(n);
    Ok(all[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mk_turn(id: &str, role: &str, user: &str, assistant: &str) -> TurnRecord {
        TurnRecord {
            turn_id: id.to_string(),
            ts: Utc::now(),
            vendor: "claude".to_string(),
            role: role.to_string(),
            user: user.to_string(),
            assistant: assistant.to_string(),
            usage: Value::Null,
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn path_helpers_produce_expected_layout() {
        let p = Path::new("/p");
        assert_eq!(
            turns_jsonl_path(p, "alice"),
            PathBuf::from("/p/.ccteam/chat/alice/turns.jsonl")
        );
        assert_eq!(chat_dir(p, "alice"), PathBuf::from("/p/.ccteam/chat/alice"));
    }

    #[test]
    fn append_and_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let t1 = mk_turn("t1", "alice", "hi", "hello");
        append_turn(tmp.path(), "alice", &t1).unwrap();
        let t2 = mk_turn("t2", "alice", "again", "yo");
        append_turn(tmp.path(), "alice", &t2).unwrap();

        let read = read_all_turns(tmp.path(), "alice").unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].turn_id, "t1");
        assert_eq!(read[0].assistant, "hello");
        assert_eq!(read[1].user, "again");
    }

    #[test]
    fn last_n_returns_chronological_tail() {
        let tmp = TempDir::new().unwrap();
        for i in 0..5 {
            let r = mk_turn(&format!("t{i}"), "bob", "u", "a");
            append_turn(tmp.path(), "bob", &r).unwrap();
        }
        let tail = last_n_turns(tmp.path(), "bob", 2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].turn_id, "t3");
        assert_eq!(tail[1].turn_id, "t4");
    }

    #[test]
    fn last_n_zero_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let r = mk_turn("t0", "x", "u", "a");
        append_turn(tmp.path(), "x", &r).unwrap();
        let tail = last_n_turns(tmp.path(), "x", 0).unwrap();
        assert!(tail.is_empty());
    }

    #[test]
    fn read_all_missing_file_is_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let out = read_all_turns(tmp.path(), "ghost").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_skips_corrupt_lines() {
        let tmp = TempDir::new().unwrap();
        ensure_dir(tmp.path(), "carol").unwrap();
        let path = turns_jsonl_path(tmp.path(), "carol");
        let good = serde_json::to_string(&mk_turn("g", "carol", "u", "a")).unwrap();
        fs::write(&path, format!("{good}\n{{not-json\n{good}\n   \n")).unwrap();
        let read = read_all_turns(tmp.path(), "carol").unwrap();
        assert_eq!(read.len(), 2);
    }
}
