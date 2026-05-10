//! V0.3 M5.1 — read-only query helpers private to the web layer.
//!
//! These wrap `ccteam_core::collect_recent_events` /
//! `SessionMailbox::list_outbox` with web-specific massaging
//! (best-effort skip of unparseable rows; tail 200 events; preview
//! truncation; newest-first ordering for outbox). Pulled into a
//! dedicated module so dashboard / project handlers stay narrow.

use chrono::{DateTime, Utc};
use serde_json::Value;

use ccteam_core::{collect_recent_events, CcteamPaths, OutboxMessage, SessionMailbox};

use crate::views::{EventRow, OutboxRow};

const OUTBOX_PREVIEW_CHARS: usize = 200;
pub const DEFAULT_OUTBOX_LIMIT: usize = 20;
pub const STATUS_EVENT_LIMIT: usize = 200;
pub const PROJECT_EVENT_DISPLAY_LIMIT: usize = 10;

/// Tail `n` recent events. Any I/O failure is logged + swallowed (the
/// dashboard prefers an empty list to a 500 page on a single broken
/// project).
pub fn slug_recent_events(paths: &CcteamPaths, slug: &str, n: usize) -> Vec<Value> {
    match collect_recent_events(paths, slug, n) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(slug, error = %err, "collect_recent_events failed");
            Vec::new()
        }
    }
}

/// Tail `n` events from one flex session's progress stream.
pub fn session_recent_events(paths: &CcteamPaths, slug: &str, sid: &str, n: usize) -> Vec<Value> {
    let path = paths.progress_jsonl_for_session(slug, sid);
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(file = %path.display(), error = %err, "session progress read failed");
            return Vec::new();
        }
    };
    let mut all: Vec<Value> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if all.len() > n {
        let drop = all.len() - n;
        all.drain(..drop);
    }
    all
}

pub fn event_ts_label(event: &Value) -> Option<String> {
    event.get("ts").and_then(|s| s.as_str()).map(str::to_string)
}

/// Convert raw progress events to `EventRow` for the project page.
pub fn events_to_rows(events: &[Value]) -> Vec<EventRow> {
    events
        .iter()
        .map(|e| EventRow {
            ts: e
                .get("ts")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            event: e
                .get("event")
                .and_then(|s| s.as_str())
                .unwrap_or("(unknown)")
                .to_string(),
            detail: short_detail(e),
        })
        .collect()
}

/// Build a single-line "detail" hint from a progress event without
/// dumping the entire JSON body. Picks the most-likely-useful field
/// (tool / phase / kind / count).
fn short_detail(event: &Value) -> String {
    if let Some(tool) = event.get("tool").and_then(|s| s.as_str()) {
        return format!("tool={tool}");
    }
    if let Some(phase) = event.get("phase").and_then(|s| s.as_str()) {
        return format!("phase={phase}");
    }
    if let Some(kind) = event.get("kind").and_then(|s| s.as_str()) {
        return format!("kind={kind}");
    }
    if let Some(c) = event.get("count").and_then(|s| s.as_u64()) {
        return format!("count={c}");
    }
    String::new()
}

/// Scan a project's outbox dir, return up to `limit` newest-first
/// rows. Front-matter parse failures fall back to an `(unparseable)`
/// kind+preview placeholder so a single corrupt file does not take
/// the page down.
pub fn outbox_rows(paths: &CcteamPaths, slug: &str, limit: usize) -> Vec<OutboxRow> {
    let mailbox = SessionMailbox::for_ccteam_dir(&paths.project_ccteam_dir(slug));
    mailbox_outbox_rows(mailbox, slug, limit)
}

pub fn session_outbox_rows(
    paths: &CcteamPaths,
    slug: &str,
    sid: &str,
    limit: usize,
) -> Vec<OutboxRow> {
    let mailbox = SessionMailbox::for_ccteam_dir(&paths.project_session_dir(slug, sid));
    mailbox_outbox_rows(mailbox, &format!("{slug}/{sid}"), limit)
}

fn mailbox_outbox_rows(mailbox: SessionMailbox, label: &str, limit: usize) -> Vec<OutboxRow> {
    let mut paths_vec = match mailbox.list_outbox() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(target = %label, error = %err, "list_outbox failed");
            return Vec::new();
        }
    };
    // `list_outbox` returns lexically sorted (== oldest first because
    // filenames embed UTC timestamps). Reverse for newest-first.
    paths_vec.reverse();
    paths_vec.truncate(limit);

    paths_vec
        .into_iter()
        .map(|p| {
            let filename = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            match OutboxMessage::load(&p) {
                Ok(msg) => OutboxRow {
                    filename,
                    kind: format!("{:?}", msg.front.event_kind).to_lowercase(),
                    created_at: msg.front.created_at.to_rfc3339(),
                    preview: truncate(msg.body.trim(), OUTBOX_PREVIEW_CHARS),
                },
                Err(err) => {
                    tracing::warn!(file = %p.display(), error = %err, "outbox parse failed");
                    OutboxRow {
                        filename,
                        kind: "(unparseable)".to_string(),
                        created_at: String::new(),
                        preview: "(could not parse front matter)".to_string(),
                    }
                }
            }
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Render a "last event was X seconds ago" summary for the dashboard.
pub fn recent_event_summary(_when: DateTime<Utc>, silent_seconds: u64) -> String {
    if silent_seconds < 60 {
        format!("{}s ago", silent_seconds)
    } else if silent_seconds < 3600 {
        format!("{}m ago", silent_seconds / 60)
    } else if silent_seconds < 86_400 {
        format!("{}h ago", silent_seconds / 3600)
    } else {
        format!("{}d ago", silent_seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 200), "hello");
    }

    #[test]
    fn truncate_long_string_appended_with_ellipsis() {
        let long = "x".repeat(250);
        let t = truncate(&long, 200);
        assert!(t.ends_with("…"));
        // 200 chars + 1 ellipsis char.
        assert_eq!(t.chars().count(), 201);
    }

    #[test]
    fn short_detail_picks_tool_first() {
        let e = json!({"event": "PreToolUse", "tool": "Read", "phase": "implement"});
        assert_eq!(short_detail(&e), "tool=Read");
    }

    #[test]
    fn short_detail_falls_back_to_phase() {
        let e = json!({"event": "phase_done", "phase": "ship"});
        assert_eq!(short_detail(&e), "phase=ship");
    }

    #[test]
    fn short_detail_empty_when_no_known_fields() {
        let e = json!({"event": "something"});
        assert_eq!(short_detail(&e), "");
    }

    #[test]
    fn recent_event_summary_buckets() {
        assert_eq!(recent_event_summary(Utc::now(), 30), "30s ago");
        assert_eq!(recent_event_summary(Utc::now(), 90), "1m ago");
        assert_eq!(recent_event_summary(Utc::now(), 3600), "1h ago");
        assert_eq!(recent_event_summary(Utc::now(), 90_000), "1d ago");
    }

    #[test]
    fn events_to_rows_handles_missing_fields() {
        let events = vec![json!({"event": "Stop"})];
        let rows = events_to_rows(&events);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event, "Stop");
        assert_eq!(rows[0].ts, "");
    }
}
