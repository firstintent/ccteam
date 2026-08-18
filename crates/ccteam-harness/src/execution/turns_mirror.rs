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

use super::journal;

/// A browser-safe reference to one project-scoped outbound attachment.
///
/// The file bytes stay under `<project>/.ccteam/uploads/`; transcript and
/// web wire shapes carry only this metadata. `id` is the stored basename and
/// is resolved by the project-ACL'd upload read route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub id: String,
    pub name: String,
    pub kind: AttachmentRefKind,
    pub size: u64,
}

/// Display class for an outbound attachment. This is deliberately separate
/// from HTTP inline-safety: the download route independently allowlists safe
/// raster extensions and serves every other type as an attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRefKind {
    Image,
    File,
}

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
    /// slash directive (e.g. `/compact`).
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
    /// Project-scoped outbound attachment references. Never contains file
    /// bytes, base64, daemon paths, or browser-provided URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    /// Terminal outcome when this row represents a failed vendor turn.
    /// Successful/interim rows omit the field so existing JSONL stays compact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Stable canonical/vendor error kind (`server_overloaded`, `transport`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// Human-readable terminal error returned by the vendor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TurnRecord {
    /// Whether this row is the terminal failure boundary for its vendor turn.
    pub fn failed(&self) -> bool {
        self.outcome.as_deref() == Some("failed")
    }
}

/// Compact tool-call entry: `name`, optional file-path / arg excerpt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Default subdirectory under `<project>/.ccteam/chat/<sid>/`.
///
/// v0.8.8 F1 — 这一层的键由 bot_role 改为 **sid**(`s<N>`):同
/// `(project, role)` 可有多个独立会话,turns / cursor / marker 一律按
/// sid 隔离,所以两个同 role 会话的历史不互相污染。函数签名(`&str`)不变
/// —— 调用方负责传 sid 而非 role。
const CHAT_BASE: &str = ".ccteam/chat";

/// Resolve `<project>/.ccteam/chat/<sid>/turns.jsonl`.
pub fn turns_jsonl_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir.join(CHAT_BASE).join(sid).join("turns.jsonl")
}

/// Resolve `<project>/.ccteam/chat/<sid>/`. Created by [`ensure_dir`].
pub fn chat_dir(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir.join(CHAT_BASE).join(sid)
}

/// `mkdir -p <project>/.ccteam/chat/<sid>/`. Idempotent.
pub fn ensure_dir(project_dir: &Path, sid: &str) -> Result<()> {
    let p = chat_dir(project_dir, sid);
    fs::create_dir_all(&p).with_context(|| format!("create {}", p.display()))?;
    Ok(())
}

/// Append `record` as one JSONL line. Creates parent dir + file when
/// missing. Returns the absolute path written for caller logging.
pub fn append_turn(project_dir: &Path, sid: &str, record: &TurnRecord) -> Result<PathBuf> {
    ensure_dir(project_dir, sid)?;
    let path = turns_jsonl_path(project_dir, sid);
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

/// Read every parseable record from the session's turns.jsonl. Returns an
/// empty Vec when the file is absent (V0.6.0 F108 first-turn case).
pub fn read_all_turns(project_dir: &Path, sid: &str) -> Result<Vec<TurnRecord>> {
    let path = turns_jsonl_path(project_dir, sid);
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Skip half-flushed / older-shape / torn rows defensively — F118 recovery
    // has to work on whatever survived, so damage must cost one LINE, not the
    // whole transcript ([`super::fs_atomic::read_jsonl`]).
    super::fs_atomic::read_jsonl(&path)
}

/// Return the last `n` parseable turns, in chronological order. F118
/// `rebuild_from_turns_jsonl` uses this to bound the conversation
/// history it injects into a fresh tmux session.
pub fn last_n_turns(project_dir: &Path, sid: &str, n: usize) -> Result<Vec<TurnRecord>> {
    let path = turns_jsonl_path(project_dir, sid);
    Ok(journal::tail_filter_map(&path, n, None, |line| {
        serde_json::from_slice::<TurnRecord>(line).ok()
    })?
    .events)
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
            attachments: Vec::new(),
            outcome: None,
            error_kind: None,
            error: None,
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
    fn last_n_skips_corrupt_rows_while_reading_backwards() {
        let tmp = TempDir::new().unwrap();
        for i in 0..2 {
            append_turn(
                tmp.path(),
                "bob",
                &mk_turn(&format!("t{i}"), "bob", "u", "a"),
            )
            .unwrap();
        }
        let path = turns_jsonl_path(tmp.path(), "bob");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "not-json").unwrap();
        drop(file);
        for i in 2..4 {
            append_turn(
                tmp.path(),
                "bob",
                &mk_turn(&format!("t{i}"), "bob", "u", "a"),
            )
            .unwrap();
        }
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        write!(file, "{{torn").unwrap();

        let tail = last_n_turns(tmp.path(), "bob", 2).unwrap();
        assert_eq!(
            tail.iter()
                .map(|turn| turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["t2", "t3"]
        );
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

    #[test]
    fn failure_outcome_survives_turn_record_round_trip() {
        let raw = serde_json::json!({
            "turn_id": "t-failed",
            "ts": "2026-07-28T08:11:01Z",
            "vendor": "codex",
            "role": "reviewer",
            "assistant": "Selected model is at capacity.",
            "outcome": "failed",
            "error_kind": "server_overloaded",
            "error": "Selected model is at capacity."
        });
        let record: TurnRecord = serde_json::from_value(raw).unwrap();
        let encoded = serde_json::to_value(record).unwrap();
        assert_eq!(encoded["outcome"], "failed");
        assert_eq!(encoded["error_kind"], "server_overloaded");
        assert_eq!(encoded["error"], "Selected model is at capacity.");
    }

    #[test]
    fn attachment_references_survive_turn_record_round_trip() {
        let mut record = mk_turn("t-file", "reviewer", "", "chart attached");
        record.attachments.push(AttachmentRef {
            id: "1780000000000-chart.png".into(),
            name: "chart.png".into(),
            kind: AttachmentRefKind::Image,
            size: 12_345,
        });

        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("base64"));
        let decoded: TurnRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn pre_outcome_turn_record_remains_compatible() {
        let raw = serde_json::json!({
            "turn_id": "t-old",
            "ts": "2026-07-28T08:00:00Z",
            "vendor": "claude",
            "role": "",
            "assistant": "done"
        });
        let record: TurnRecord = serde_json::from_value(raw).unwrap();
        assert!(record.attachments.is_empty());
        let encoded = serde_json::to_value(record).unwrap();
        assert!(encoded.get("attachments").is_none());
        assert!(encoded.get("outcome").is_none());
        assert!(encoded.get("error_kind").is_none());
        assert!(encoded.get("error").is_none());
    }
}
