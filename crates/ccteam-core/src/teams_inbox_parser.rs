//! V0.5.0 F95 — parser + differ for
//! `~/.claude/teams/<team>/inboxes/<teammate>.json`.
//!
//! Per the real-world host probe (see
//! `crates/ccteam-core/tests/fixtures/agent_teams/inbox-team-lead.json`),
//! each inbox is a **single JSON file holding an array of messages**
//! (not a directory of per-message files). New messages are appended
//! by Anthropic; ccteam tails the file diff by timestamp and emits
//! `team_message_sent` events.
//!
//! ## Idle-notification routing (PRD F95 §需求 .6)
//!
//! `text` may be a JSON-stringified system message of the form
//! `{"type":"idle_notification", ...}`. F95 **excludes** those from
//! `team_message_sent`; they're meant for F94's `team_teammate_idle`
//! hook (Wave 2) which uses an in-process signal. Filtering them here
//! prevents F95 from polluting the human-message stream.
//!
//! ## Red lines
//!
//! - Read-only against `~/.claude/teams/`.
//! - Schema-failure tolerance — `parse_inbox` returns `Result`;
//!   caller WARNs once and degrades.
//! - Text truncated to `MAX_TEXT_LEN` chars on emit (PRD F95 §需求 .2).

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

/// Max length of the `text_truncated` field on `team_message_sent`
/// events. Picked per PRD F95 §需求 .2 to match the web SPA mailbox
/// row preview budget.
pub const MAX_TEXT_LEN: usize = 200;

/// One inbox entry. Mirrors the Anthropic schema exactly; optional
/// fields default to empty so the differ can index by `timestamp`
/// even when the producer omits `color` / `read`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InboxEntry {
    pub from: String,
    pub text: String,
    /// RFC3339 / ISO-8601 (the host writes `2026-05-16T13:45:19.594Z`).
    /// Used as the diff key, so it must be present + unique per
    /// `(inbox, timestamp)` pair.
    pub timestamp: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub read: bool,
}

/// Parsed snapshot of one inbox file. The diff key is `timestamp` —
/// we keep the original ordering since the array is append-only from
/// Anthropic's side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxSnapshot {
    pub entries: Vec<InboxEntry>,
}

impl InboxSnapshot {
    /// Set of `timestamp` strings already observed. Used by the diff
    /// pass to short-circuit "this message was already emitted".
    pub fn timestamps(&self) -> std::collections::HashSet<&str> {
        self.entries.iter().map(|e| e.timestamp.as_str()).collect()
    }
}

/// Parse an inbox `<teammate>.json` byte buffer. The file is a
/// top-level JSON array; structural failure returns `Err` and the
/// caller WARNs once.
pub fn parse_inbox(bytes: &[u8]) -> Result<InboxSnapshot> {
    let entries: Vec<InboxEntry> =
        serde_json::from_slice(bytes).context("teams_inbox_parser: deserialize inbox JSON")?;
    Ok(InboxSnapshot { entries })
}

/// True when `text` is a JSON-stringified
/// `{"type":"idle_notification", ...}`. F95 callers exclude these
/// from `team_message_sent`. Cheap check: parse as JSON only if the
/// string starts with `{`.
pub fn is_idle_notification(text: &str) -> bool {
    let t = text.trim_start();
    if !t.starts_with('{') {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(t) else {
        return false;
    };
    v.get("type")
        .and_then(|s| s.as_str())
        .map(|s| s == "idle_notification")
        .unwrap_or(false)
}

/// Truncate `text` to at most `MAX_TEXT_LEN` chars (Unicode-safe — we
/// count chars, not bytes, so emoji / CJK don't surprise the field).
pub fn truncate_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT_LEN * 4));
    for (i, ch) in text.chars().enumerate() {
        if i >= MAX_TEXT_LEN {
            break;
        }
        out.push(ch);
    }
    out
}

/// Diff `prev` against `next` for one inbox owned by `to`. Returns
/// the list of `team_message_sent` events the watcher should append.
///
/// Filtering rules (PRD F95 §需求):
///
/// 1. Entries whose `timestamp` was already in `prev` are skipped
///    (replay protection for file rewrites).
/// 2. Entries whose `text` is an `idle_notification` payload are
///    skipped — F94 hook owns the `team_teammate_idle` channel.
///
/// The caller is responsible for cold-start handling: when first
/// observing an inbox, the desired behaviour is "do NOT emit historical
/// messages" (the team predates ccteam discovery). Pass `prev` as the
/// freshly-parsed cold snapshot in that case; an event burst would
/// flood `progress.jsonl` with stale chat. For replay testing, pass
/// `prev = Default::default()` to see every entry.
pub fn diff_inbox(
    prev: &InboxSnapshot,
    next: &InboxSnapshot,
    team_name: &str,
    to: &str,
) -> Vec<Value> {
    let mut events = Vec::new();
    let seen: std::collections::HashSet<&str> = prev.timestamps();
    let now_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    for entry in &next.entries {
        if seen.contains(entry.timestamp.as_str()) {
            continue;
        }
        if is_idle_notification(&entry.text) {
            continue;
        }
        events.push(json!({
            "event": "team_message_sent",
            "ts": now_ts,
            "team_name": team_name,
            "from": entry.from,
            "to": to,
            "text_truncated": truncate_text(&entry.text),
            "msg_ts": entry.timestamp,
            "color": entry.color,
            "read": entry.read,
        }));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(from: &str, text: &str, ts: &str) -> InboxEntry {
        InboxEntry {
            from: from.into(),
            text: text.into(),
            timestamp: ts.into(),
            color: Some("blue".into()),
            read: false,
        }
    }

    #[test]
    fn parse_empty_array() {
        let s = parse_inbox(b"[]").unwrap();
        assert!(s.entries.is_empty());
    }

    #[test]
    fn parse_broken_json_returns_err() {
        assert!(parse_inbox(b"not json").is_err());
    }

    #[test]
    fn is_idle_notification_detects_system_payload() {
        let text =
            r#"{"type":"idle_notification","from":"x","timestamp":"...","idleReason":"available"}"#;
        assert!(is_idle_notification(text));
    }

    #[test]
    fn is_idle_notification_rejects_plain_text() {
        assert!(!is_idle_notification("Hello team"));
    }

    #[test]
    fn is_idle_notification_rejects_unrelated_json() {
        assert!(!is_idle_notification(r#"{"type":"other","payload":1}"#));
    }

    #[test]
    fn truncate_short_text_unchanged() {
        let s = "hello";
        assert_eq!(truncate_text(s), s);
    }

    #[test]
    fn truncate_long_ascii_capped_at_max_text_len() {
        let s = "x".repeat(MAX_TEXT_LEN + 100);
        let out = truncate_text(&s);
        assert_eq!(out.chars().count(), MAX_TEXT_LEN);
    }

    #[test]
    fn truncate_handles_multibyte_chars_by_char_not_byte() {
        // 250 CJK chars × 3 bytes each = 750 bytes; truncate to 200 chars.
        let s = "中".repeat(MAX_TEXT_LEN + 50);
        let out = truncate_text(&s);
        assert_eq!(out.chars().count(), MAX_TEXT_LEN);
    }

    #[test]
    fn diff_filters_replayed_timestamps() {
        let prev = InboxSnapshot {
            entries: vec![entry("a", "old", "2026-01-01T00:00:00Z")],
        };
        let next = InboxSnapshot {
            entries: vec![
                entry("a", "old", "2026-01-01T00:00:00Z"),
                entry("a", "new", "2026-01-02T00:00:00Z"),
            ],
        };
        let events = diff_inbox(&prev, &next, "roblog", "team-lead");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["text_truncated"], "new");
    }

    #[test]
    fn diff_filters_idle_notifications() {
        let prev = InboxSnapshot::default();
        let next = InboxSnapshot {
            entries: vec![
                entry("a", "Hello", "2026-01-01T00:00:00Z"),
                entry(
                    "b",
                    r#"{"type":"idle_notification","from":"b","timestamp":"...","idleReason":"available"}"#,
                    "2026-01-02T00:00:00Z",
                ),
            ],
        };
        let events = diff_inbox(&prev, &next, "roblog", "team-lead");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["from"], "a");
    }

    #[test]
    fn diff_populates_text_truncated_and_metadata() {
        let prev = InboxSnapshot::default();
        let next = InboxSnapshot {
            entries: vec![entry("a", "msg", "2026-01-01T00:00:00Z")],
        };
        let events = diff_inbox(&prev, &next, "roblog", "team-lead");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "team_message_sent");
        assert_eq!(events[0]["team_name"], "roblog");
        assert_eq!(events[0]["from"], "a");
        assert_eq!(events[0]["to"], "team-lead");
        assert_eq!(events[0]["text_truncated"], "msg");
        assert_eq!(events[0]["msg_ts"], "2026-01-01T00:00:00Z");
        assert_eq!(events[0]["color"], "blue");
        assert_eq!(events[0]["read"], false);
    }

    #[test]
    fn roblog_inbox_fixture_idle_notifications_skipped() {
        // PRD F95 §需求 .6 — the fixture has 39 messages with a
        // mix of plain text + idle_notification system messages.
        // Cold-diff should drop every idle_notification.
        let bytes = include_bytes!("../tests/fixtures/agent_teams/inbox-team-lead.json");
        let snap = parse_inbox(bytes).unwrap();
        let prev = InboxSnapshot::default();
        let events = diff_inbox(&prev, &snap, "roblog", "team-lead");
        // No event should carry an idle_notification text.
        for e in &events {
            let text = e["text_truncated"].as_str().unwrap();
            assert!(
                !is_idle_notification(text),
                "leaked idle_notification: {text}",
            );
        }
        // Sanity: at least some events emitted (there are non-idle
        // messages in the fixture) and at least one idle filtered.
        let idle_count = snap
            .entries
            .iter()
            .filter(|e| is_idle_notification(&e.text))
            .count();
        assert!(idle_count > 0, "fixture should contain idle notifications");
        assert_eq!(events.len(), snap.entries.len() - idle_count);
    }
}
