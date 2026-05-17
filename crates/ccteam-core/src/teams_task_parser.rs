//! V0.5.0 F95 — parser + differ for `~/.claude/tasks/<team>/*.json`.
//!
//! Anthropic stores per-team task lists as a directory of
//! `<id>.json` files plus two siblings (`.lock`, `.highwatermark`)
//! that ccteam **ignores**. Each task transitions through a
//! `status` field; F95 emits `team_task_created` when a new file
//! lands with `status: pending` and `team_task_completed` on the
//! transition to `status: completed`.
//!
//! ## Status transitions tracked (PRD F95 §需求 .2)
//!
//! ```text
//! (missing)           → pending     ⇒ team_task_created
//! pending             → in_progress ⇒ ignored (no F95 event)
//! in_progress/pending → completed   ⇒ team_task_completed
//! ```
//!
//! Intermediate states (`in_progress`, `failed`, etc.) do NOT emit
//! their own F95 event — F94's `TaskCompleted` hook fills the gap
//! when wired (advanced path); the watcher-only fallback handles
//! the `(missing → pending → completed)` happy path.
//!
//! ## Red lines
//!
//! - Read-only against `~/.claude/tasks/`.
//! - `.lock` / `.highwatermark` siblings explicitly skipped.
//! - Schema-failure tolerance — parsing errors degrade to "no
//!   event for this file"; watcher continues other files in dir.

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

/// Possible terminal / intermediate statuses observed on Anthropic's
/// task files. We treat anything we don't recognise as "unknown" and
/// fall back to suppressing F95 emit for that transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Other(String),
}

impl TaskStatus {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// One task file. Only the fields F95 needs for events are extracted;
/// extra Anthropic fields are silently ignored so future schema adds
/// don't break parsing.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TaskFile {
    /// Numeric ID; the file name carries the same value but we read
    /// from the body for safety.
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Free-form result text recorded by the lead when status flips
    /// to `completed`. Optional — F94 hooks may emit a richer summary.
    #[serde(default)]
    pub result: Option<String>,
    /// Anthropic writes the canonical status as `status: pending |
    /// in_progress | completed`. Missing field treated as "unknown" so
    /// half-written files don't emit spurious events.
    #[serde(default)]
    pub status: String,
    /// Member name (`<role>`, not `<role>@<team>`). Optional — some
    /// tasks are team-wide.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Task IDs this task waits on. Defaults to empty.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// RFC3339 — Anthropic writes ISO-8601 with millisecond precision.
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// Helper deserializer so we accept either `id: "1"` or `id: 1`.
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "task id must be string or number, got {other}"
        ))),
    }
}

/// Parse a single task file's body.
pub fn parse_task(bytes: &[u8]) -> Result<TaskFile> {
    serde_json::from_slice(bytes).context("teams_task_parser: deserialize task JSON")
}

/// True when `file_name` is one of the ignored sibling files
/// (`.lock`, `.highwatermark`) — see PRD F95 §需求 .2.
pub fn is_sibling_file(file_name: &str) -> bool {
    matches!(file_name, ".lock" | ".highwatermark")
}

/// Decide what events to emit for a single task file's transition,
/// given the **previous** parsed body (or `None` if the file is new
/// to the watcher) and the **next** body. Pure — no IO.
///
/// Edge cases:
///
/// - `None → pending` ⇒ `team_task_created`.
/// - `pending → completed` (file modify, no `pending` boundary
///   stored in snapshot) ⇒ both `team_task_created` and
///   `team_task_completed`, in that order, so consumers see a
///   coherent lifecycle even when the watcher missed the
///   intermediate write.
/// - `_ → completed` (prev not completed) ⇒ `team_task_completed`.
/// - `_ → completed → completed` ⇒ no event (idempotent).
/// - Other transitions (`pending → in_progress`, etc.) ⇒ no event.
pub fn diff_task(prev: Option<&TaskFile>, next: &TaskFile, team_name: &str) -> Vec<Value> {
    let next_status = TaskStatus::parse(&next.status);
    let prev_status = prev.map(|p| TaskStatus::parse(&p.status));

    let mut events = Vec::new();
    let now_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    match (prev_status.as_ref(), &next_status) {
        // Fresh task we've never seen + currently pending → created.
        (None, TaskStatus::Pending) => {
            events.push(task_created_event(team_name, next, &now_ts));
        }
        // Fresh task observed already completed (we missed the
        // intermediate write). Emit created + completed so the SoT
        // stream stays self-consistent.
        (None, TaskStatus::Completed) => {
            events.push(task_created_event(team_name, next, &now_ts));
            events.push(task_completed_event(team_name, next, &now_ts));
        }
        // Modified task that just flipped to completed.
        (Some(p), TaskStatus::Completed) if !p.is_completed() => {
            events.push(task_completed_event(team_name, next, &now_ts));
        }
        // Everything else (already-completed seen again, pending →
        // in_progress, unknown intermediate) → no event.
        _ => {}
    }
    events
}

fn task_created_event(team_name: &str, t: &TaskFile, ts: &str) -> Value {
    json!({
        "event": "team_task_created",
        "ts": ts,
        "team_name": team_name,
        "task_id": t.id,
        "title": t.title,
        "assignee": t.assignee,
        "dependencies": t.dependencies,
    })
}

fn task_completed_event(team_name: &str, t: &TaskFile, ts: &str) -> Value {
    let completed_at = t
        .completed_at
        .clone()
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    json!({
        "event": "team_task_completed",
        "ts": ts,
        "team_name": team_name,
        "task_id": t.id,
        "result_summary": t.result,
        "completed_at": completed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: &str) -> TaskFile {
        TaskFile {
            id: id.into(),
            title: format!("task {id}"),
            result: None,
            status: status.into(),
            assignee: Some("frontend-dev".into()),
            dependencies: vec![],
            completed_at: None,
        }
    }

    #[test]
    fn sibling_skipped() {
        assert!(is_sibling_file(".lock"));
        assert!(is_sibling_file(".highwatermark"));
        assert!(!is_sibling_file("1.json"));
        assert!(!is_sibling_file("foo.json"));
    }

    #[test]
    fn cold_pending_emits_created() {
        let t = task("1", "pending");
        let events = diff_task(None, &t, "roblog");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "team_task_created");
        assert_eq!(events[0]["task_id"], "1");
    }

    #[test]
    fn pending_to_completed_emits_completed() {
        let prev = task("1", "pending");
        let next = task("1", "completed");
        let events = diff_task(Some(&prev), &next, "roblog");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "team_task_completed");
    }

    #[test]
    fn pending_to_in_progress_emits_nothing() {
        let prev = task("1", "pending");
        let next = task("1", "in_progress");
        assert!(diff_task(Some(&prev), &next, "roblog").is_empty());
    }

    #[test]
    fn in_progress_to_completed_emits_completed_only() {
        let prev = task("1", "in_progress");
        let next = task("1", "completed");
        let events = diff_task(Some(&prev), &next, "roblog");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "team_task_completed");
    }

    #[test]
    fn cold_completed_emits_both_events() {
        let t = task("1", "completed");
        let events = diff_task(None, &t, "roblog");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"], "team_task_created");
        assert_eq!(events[1]["event"], "team_task_completed");
    }

    #[test]
    fn already_completed_no_event() {
        let prev = task("1", "completed");
        let next = task("1", "completed");
        assert!(diff_task(Some(&prev), &next, "roblog").is_empty());
    }

    #[test]
    fn pending_to_pending_no_event() {
        // Defensive: an idempotent rewrite of the same status (which
        // would touch mtime) must not double-emit.
        let prev = task("1", "pending");
        let next = task("1", "pending");
        assert!(diff_task(Some(&prev), &next, "roblog").is_empty());
    }

    #[test]
    fn parse_minimal_pending_task() {
        let bytes = br#"{"id":"7","title":"x","status":"pending"}"#;
        let t = parse_task(bytes).unwrap();
        assert_eq!(t.id, "7");
        assert_eq!(t.status, "pending");
        assert!(t.dependencies.is_empty());
    }

    #[test]
    fn parse_numeric_id_coerces_to_string() {
        let bytes = br#"{"id":42,"title":"x","status":"pending"}"#;
        let t = parse_task(bytes).unwrap();
        assert_eq!(t.id, "42");
    }

    #[test]
    fn parse_broken_json_returns_err() {
        assert!(parse_task(b"not json").is_err());
    }

    #[test]
    fn completed_event_uses_file_completed_at_when_present() {
        let mut t = task("1", "completed");
        t.completed_at = Some("2026-05-17T12:00:00Z".into());
        let events = diff_task(None, &t, "roblog");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["completed_at"], "2026-05-17T12:00:00Z");
    }
}
