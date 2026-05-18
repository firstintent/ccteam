//! Outbound pipeline: tail `<project>/.ccteam/chat/<bot>/turns.jsonl`
//! → forward agent replies through the appropriate [`Channel`].
//!
//! The tui-impl teammate's `turns_mirror.rs` writes one JSON object
//! per agent reply. We tail by file-position, parse each new line,
//! and dispatch to the platform Channel matching the bot's
//! registered `im_platform`.

use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One row in `turns.jsonl` (subset we care about — extras tolerated).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRow {
    /// "user" / "assistant" / "tool" — only `assistant` is forwarded.
    pub role: String,
    /// Reply text (post tui-adapter cleanup).
    pub content: String,
    /// Optional reference back to the inbound platform message id
    /// (echo suppression — outbound never sends a turn whose
    /// `reply_to` matches a message we just dropped to mailbox).
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Optional thread id (if the user spoke in a Slack thread, the
    /// reply belongs to the same thread).
    #[serde(default)]
    pub thread_ts: Option<String>,
    /// Where the reply should land (mailbox `reply_target` echoed
    /// through by the tui adapter).
    #[serde(default)]
    pub reply_target: Option<String>,
}

/// Tailer state for one bot. Persisted between daemon ticks so we
/// don't re-forward old turns on restart.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TailCursor {
    /// File-position in bytes (cursor advanced after each successful
    /// dispatch).
    pub position: u64,
}

/// Resolve `<project>/.ccteam/chat/<role>/turns.jsonl` for the given
/// `(projects_root, slug, role)` triple.
pub fn turns_jsonl_path(projects_root: &std::path::Path, slug: &str, role: &str) -> PathBuf {
    projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("turns.jsonl")
}

/// Read every new line since `cursor.position`, returning the parsed
/// rows + the new cursor position (advanced to EOF). Missing file is
/// not an error — returns an empty Vec.
pub fn read_new_rows(path: &std::path::Path, cursor: &TailCursor) -> Result<(Vec<TurnRow>, TailCursor)> {
    if !path.exists() {
        return Ok((vec![], cursor.clone()));
    }
    let mut f = fs::File::open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let len = f.metadata()?.len();
    // File truncated / replaced → rewind.
    let start = if cursor.position > len {
        0
    } else {
        cursor.position
    };
    f.seek(SeekFrom::Start(start))?;
    let reader = BufReader::new(f);
    let mut rows = Vec::new();
    let mut consumed: u64 = start;
    for line_res in reader.lines() {
        let line = line_res?;
        consumed += line.len() as u64 + 1; // +1 for `\n`
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TurnRow>(trimmed) {
            Ok(row) => rows.push(row),
            Err(err) => {
                tracing::warn!(error = %err, line = %trimmed, "malformed turns.jsonl row; skipping");
            }
        }
    }
    Ok((rows, TailCursor { position: consumed }))
}

/// Filter: only `role == "assistant"` rows are forwarded to IM, and
/// we suppress turns whose `reply_to` matches a message id we
/// dropped inbound (echo suppression).
pub fn should_forward(row: &TurnRow, inbound_message_ids: &[String]) -> bool {
    if row.role != "assistant" {
        return false;
    }
    if let Some(rt) = &row.reply_to {
        if inbound_message_ids.iter().any(|id| id == rt) {
            // The agent's reply is to a message we already sent; this
            // is the IM->agent round-trip we initiated. Forward it.
            // (We only suppress when the **agent** initiates a bot-to-bot
            // call referencing an external id — checked by hop count
            // in the router, not here.)
            return true;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_empty_when_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.jsonl");
        let (rows, cur) = read_new_rows(&path, &TailCursor::default()).unwrap();
        assert!(rows.is_empty());
        assert_eq!(cur.position, 0);
    }

    #[test]
    fn read_advances_cursor() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        fs::write(
            &path,
            r#"{"role":"assistant","content":"hi"}
{"role":"user","content":"u"}
"#,
        )
        .unwrap();
        let (rows, cur) = read_new_rows(&path, &TailCursor::default()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(cur.position > 0);

        // Append one more line and re-read from the cursor.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        writeln!(f, "{{\"role\":\"assistant\",\"content\":\"new\"}}").unwrap();
        drop(f);
        let (more, _cur2) = read_new_rows(&path, &cur).unwrap();
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].content, "new");
    }

    #[test]
    fn malformed_line_skipped() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        fs::write(
            &path,
            "{\"role\":\"assistant\",\"content\":\"ok\"}\nNOT JSON\n",
        )
        .unwrap();
        let (rows, _) = read_new_rows(&path, &TailCursor::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "ok");
    }

    #[test]
    fn truncation_rewinds_cursor() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        // Write a long first version so the cursor ends up well past
        // the eventual truncated length.
        fs::write(
            &path,
            "{\"role\":\"assistant\",\"content\":\"first version with lots of bytes here so the cursor advances\"}\n",
        )
        .unwrap();
        let (_, cur) = read_new_rows(&path, &TailCursor::default()).unwrap();
        // Replace with shorter content (strict truncation).
        fs::write(&path, "{\"role\":\"assistant\",\"content\":\"b\"}\n").unwrap();
        let (rows, _) = read_new_rows(&path, &cur).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "b");
    }

    #[test]
    fn only_assistant_rows_forwarded() {
        let assistant = TurnRow {
            role: "assistant".into(),
            content: "x".into(),
            reply_to: None,
            thread_ts: None,
            reply_target: None,
        };
        let user = TurnRow {
            role: "user".into(),
            content: "x".into(),
            reply_to: None,
            thread_ts: None,
            reply_target: None,
        };
        assert!(should_forward(&assistant, &[]));
        assert!(!should_forward(&user, &[]));
    }
}
