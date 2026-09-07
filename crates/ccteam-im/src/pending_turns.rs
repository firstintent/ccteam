//! v0.8.24 F5 — pending turn queue for the cold-start / resume window.
//!
//! When a user sends a turn while the session child is still starting or
//! resuming, the gateway enqueues the text (FIFO, file-backed under the
//! session's `.ccteam/chat/<sid>/pending_turns.jsonl`) and drains it only
//! after the session is live — still via the sole turns writer path
//! (`submit_resolved` / event pump). No alternate turns writer.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One queued user turn waiting for the session to become live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingTurn {
    /// User text payload (not a directive — directives are not queued).
    pub text: String,
    /// ISO-8601 enqueue time (diagnostic only).
    pub enqueued_at: String,
    /// Optional origin channel tag (`im` / `web` / `mcp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Bypass vendor slash-directive parsing when the row is drained. Scheduled
    /// message bodies are always literal normal user turns, including `/...`.
    #[serde(default)]
    pub literal: bool,
    /// v0.10.1 — was this turn authored by ccteam (a delegation notification,
    /// an internal re-submit) rather than by a human? The queue is the ONLY
    /// place a turn survives its submit call, so without carrying this the
    /// drain has to guess — and it guessed "internal" for everything, which
    /// makes a human's queued question look like nobody asked it.
    #[serde(default)]
    pub internal: bool,
    /// The delegation request this line belongs to, when the submit that
    /// enqueued it had one (issue #197 E). The queue is the only place a
    /// dispatched task survives its submit call, so without the identity here
    /// an explicit stop could only say "N lines are retained for this session"
    /// and never "YOUR task is retained" — and attributing an id-less row to
    /// every unbound request is a claim ccteam cannot prove. Absent on human
    /// turns and on rows written before this field existed: those are counted,
    /// never attributed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// What one session's queue is holding, split by whether ccteam can say WHOSE
/// each line is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetainedPending {
    /// Delegation requests whose task line is still in the queue.
    pub request_ids: Vec<String>,
    /// Rows carrying no request identity (a human's queued message, a row from
    /// before the field existed). Reported as a count, never attributed.
    pub unattributed: usize,
}

impl RetainedPending {
    /// Whether the queue still holds this request's line.
    pub fn holds(&self, request_id: &str) -> bool {
        self.request_ids.iter().any(|id| id == request_id)
    }

    /// Nothing queued at all.
    pub fn is_empty(&self) -> bool {
        self.request_ids.is_empty() && self.unattributed == 0
    }
}

/// The basename of this queue's file. One literal, named, because a caller
/// that must tell a dispatcher WHERE its undelivered task is being held
/// (`agent_stop`'s receipt) should not spell the path a second time.
pub const PENDING_TURNS_FILE: &str = "pending_turns.jsonl";

fn pending_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join(PENDING_TURNS_FILE)
}

/// Append one pending turn (FIFO). Creates parent dirs as needed.
pub fn enqueue_pending_turn(
    project_dir: &Path,
    sid: &str,
    text: impl Into<String>,
    origin: Option<String>,
    literal: bool,
    internal: bool,
    request_id: Option<String>,
) -> Result<()> {
    let path = pending_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let row = PendingTurn {
        text: text.into(),
        enqueued_at: chrono::Utc::now().to_rfc3339(),
        origin,
        literal,
        internal,
        request_id,
    };
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open pending_turns {}", path.display()))?;
    serde_json::to_writer(&mut f, &row)?;
    f.write_all(b"\n")?;
    f.sync_all().ok();
    Ok(())
}

/// Drain all pending turns (FIFO order) and remove the file.
pub fn drain_pending_turns(project_dir: &Path, sid: &str) -> Result<VecDeque<PendingTurn>> {
    let path = pending_path(project_dir, sid);
    if !path.exists() {
        return Ok(VecDeque::new());
    }
    let file = std::fs::File::open(&path)
        .with_context(|| format!("read pending_turns {}", path.display()))?;
    let mut out = VecDeque::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<PendingTurn>(line) {
            Ok(t) => out.push_back(t),
            Err(e) => {
                tracing::warn!(error = %e, %line, "skip corrupt pending_turns row");
            }
        }
    }
    let _ = std::fs::remove_file(&path);
    Ok(out)
}

/// Count pending turns without draining.
pub fn pending_turn_count(project_dir: &Path, sid: &str) -> usize {
    let path = pending_path(project_dir, sid);
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count()
}

/// What the queue holds right now, WITHOUT draining it — the read an explicit
/// stop makes to answer "is my task still retained, and can you prove it".
pub fn retained_pending(project_dir: &Path, sid: &str) -> RetainedPending {
    let path = pending_path(project_dir, sid);
    let Ok(file) = std::fs::File::open(path) else {
        return RetainedPending::default();
    };
    let mut out = RetainedPending::default();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A row this build cannot parse is still a row the queue will try to
        // replay: count it rather than pretend the queue is emptier than it is.
        match serde_json::from_str::<PendingTurn>(line) {
            Ok(row) => match row.request_id {
                Some(id) => out.request_ids.push(id),
                None => out.unattributed += 1,
            },
            Err(_) => out.unattributed += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// GitHub #197 (E) — a queued line is attributed to a request only when it
    /// SAYS which one, and rows that say nothing are counted, never spread over
    /// every unbound request. The queue used to carry no identity at all, so a
    /// stop could either claim per-request retention it could not prove or say
    /// nothing at all.
    #[test]
    fn retention_is_attributed_only_where_the_row_names_its_request() {
        let tmp = TempDir::new().unwrap();
        assert!(retained_pending(tmp.path(), "s1").is_empty());
        enqueue_pending_turn(
            tmp.path(),
            "s1",
            "the delegated task",
            None,
            false,
            true,
            Some("req-1".into()),
        )
        .unwrap();
        // A human's message queued behind the same body names nobody.
        enqueue_pending_turn(tmp.path(), "s1", "hey", None, false, false, None).unwrap();

        let held = retained_pending(tmp.path(), "s1");
        assert_eq!(held.request_ids, vec!["req-1".to_string()]);
        assert_eq!(held.unattributed, 1);
        assert!(held.holds("req-1"));
        assert!(
            !held.holds("req-2"),
            "a request whose line is not in the queue is never claimed as retained"
        );

        // The identity survives the round trip the drain makes.
        let drained = drain_pending_turns(tmp.path(), "s1").unwrap();
        assert_eq!(drained[0].request_id.as_deref(), Some("req-1"));
        assert_eq!(drained[1].request_id, None);
        assert!(retained_pending(tmp.path(), "s1").is_empty());
    }

    /// A row from before the identity existed is unreadable as an attribution,
    /// and is counted rather than dropped: the queue will still replay it.
    #[test]
    fn a_row_without_an_identity_is_counted_not_attributed() {
        let tmp = TempDir::new().unwrap();
        let path = pending_path(tmp.path(), "s1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"text\":\"old shape\",\"enqueued_at\":\"2026-01-01T00:00:00Z\"}\n{ torn\n",
        )
        .unwrap();
        let held = retained_pending(tmp.path(), "s1");
        assert!(held.request_ids.is_empty());
        assert_eq!(held.unattributed, 2, "a torn row is still a row to replay");
    }

    #[test]
    fn enqueue_drain_fifo() {
        let tmp = TempDir::new().unwrap();
        enqueue_pending_turn(
            tmp.path(),
            "s1",
            "first",
            Some("web".into()),
            false,
            false,
            None,
        )
        .unwrap();
        enqueue_pending_turn(
            tmp.path(),
            "s1",
            "second",
            Some("web".into()),
            true,
            true,
            None,
        )
        .unwrap();
        assert_eq!(pending_turn_count(tmp.path(), "s1"), 2);
        let drained = drain_pending_turns(tmp.path(), "s1").unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].text, "first");
        assert_eq!(drained[1].text, "second");
        assert!(!drained[0].literal);
        assert!(drained[1].literal);
        assert_eq!(pending_turn_count(tmp.path(), "s1"), 0);
        // second drain is empty
        assert!(drain_pending_turns(tmp.path(), "s1").unwrap().is_empty());
    }

    #[test]
    fn drain_missing_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(drain_pending_turns(tmp.path(), "nope").unwrap().is_empty());
    }
}
