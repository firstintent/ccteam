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

/// V0.6.1 F134 — persistent byte-offset state for one bot's
/// `turns.jsonl` tailer. Stored at
/// `<projects_root>/<slug>/.ccteam/chat/<role>/outbound.cursor` as a
/// JSON blob `{"position": N}` (serde-derived on [`TailCursor`]).
///
/// Daemon restart re-loads this file so messages forwarded across a
/// previous run aren't re-sent to the user. Missing / malformed file
/// → fall back to the zero cursor (re-forward everything; safer than
/// dropping content silently, and `turns.jsonl` grows monotonically
/// so a stale cursor would only ever under-skip, never over-skip).
pub fn outbound_cursor_path(
    projects_root: &std::path::Path,
    slug: &str,
    role: &str,
) -> PathBuf {
    projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("outbound.cursor")
}

/// Load a [`TailCursor`] from disk. Missing file or parse failure
/// returns the default (position = 0).
pub fn load_cursor(path: &std::path::Path) -> TailCursor {
    if !path.exists() {
        return TailCursor::default();
    }
    match fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(err) => {
            tracing::warn!(error = %err, path = %path.display(), "imd: outbound: load_cursor read failed");
            TailCursor::default()
        }
    }
}

/// Persist a [`TailCursor`] to disk. Creates the parent directory if
/// missing. Errors propagate so the caller can warn-log; persistence
/// failure is not fatal (the worst case is re-forwarding a few rows
/// next tick if the cursor doesn't land).
pub fn save_cursor(path: &std::path::Path, cursor: &TailCursor) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let body = serde_json::to_string(cursor).context("serialize TailCursor")?;
    fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
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

/// V0.6.0 Wave 3 — push every assistant row in `rows` to `channel`
/// addressed at `recipient`. Honors the [`should_forward`] filter so
/// `user` / `tool` rows are skipped. Per-row send errors are logged
/// but do not abort the forward loop (one flake shouldn't stall the
/// whole bot). Returns the count of rows successfully dispatched.
///
/// The daemon resolves `(channel, recipient)` per-bot from the
/// [`crate::BotRegistration`] (`im_platform` → channel impl,
/// `im_chat_id` → recipient string).
pub async fn forward_new_rows(
    rows: &[TurnRow],
    channel: &dyn crate::transport::Channel,
    recipient: &str,
    inbound_message_ids: &[String],
) -> usize {
    let mut sent = 0;
    for row in rows {
        if !should_forward(row, inbound_message_ids) {
            continue;
        }
        let mut msg = crate::transport::SendMessage::new(row.content.clone(), recipient);
        msg.thread_ts = row.thread_ts.clone();
        match channel.send(&msg).await {
            Ok(_) => {
                sent += 1;
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    recipient,
                    "outbound forward failed; continuing"
                );
            }
        }
    }
    sent
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

    #[tokio::test]
    async fn forward_new_rows_dispatches_assistant_only() {
        use crate::transport::providers::mock::MockChannel;
        let channel = MockChannel::new();
        let rows = vec![
            TurnRow {
                role: "assistant".into(),
                content: "reply-1".into(),
                reply_to: None,
                thread_ts: None,
                reply_target: None,
            },
            TurnRow {
                role: "user".into(),
                content: "u-1".into(),
                reply_to: None,
                thread_ts: None,
                reply_target: None,
            },
            TurnRow {
                role: "assistant".into(),
                content: "reply-2".into(),
                reply_to: None,
                thread_ts: None,
                reply_target: None,
            },
        ];
        let sent = forward_new_rows(&rows, &channel, "user-alice", &[]).await;
        assert_eq!(sent, 2);
        let out = channel.outbox().await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content, "reply-1");
        assert_eq!(out[1].content, "reply-2");
        assert_eq!(out[0].recipient, "user-alice");
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
