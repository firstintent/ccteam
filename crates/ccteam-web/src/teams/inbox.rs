//! V0.5.0 F96 — Agent Teams mailbox parser.
//!
//! `<claude_home>/teams/<team>/inboxes/<teammate>.json` is a JSON
//! array of messages. Live schema (per `inbox-team-lead.json` host
//! fixture):
//!
//! ```json
//! { "from": "frontend-dev",
//!   "text": "...",
//!   "summary": "...optional...",
//!   "timestamp": "2026-05-16T13:45:19.594Z",
//!   "color": "green",
//!   "read": true }
//! ```
//!
//! - `text` may be a JSON-stringified system message: `{"type":
//!   "idle_notification", "from":"...", "timestamp":"...",
//!   "idleReason":"available"}`. We flag those with
//!   `is_idle_notification=true` so the Mailbox UI filters them out
//!   (they go to the Topology panel idle badge instead).
//! - `read: bool` is preserved verbatim — never written back (PRD red
//!   line). The SPA renders `read: false` rows with a highlight.
//!
//! `GET /api/v1/teams/<name>/inbox?teammate=<n>&since=<ts>` lists one
//! teammate's box; `?teammate` omitted → merge sort across every
//! `inboxes/*.json` so the Mailbox tab can show all traffic. `since`
//! is an RFC3339 cursor (post-strict, exclusive of equal).

use std::cmp::Reverse;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::teams_root;

/// One message, ready for the wire. The schema is intentionally
/// tolerant: every field except `from` / `timestamp` is optional so
/// host-side schema drift doesn't 500 the endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxMessage {
    pub from: String,
    /// The mailbox owner this message landed in. Populated by
    /// `load_inbox` from the filename so the merged-list view can
    /// route per-teammate. `#[serde(default)]` because the on-disk
    /// schema doesn't carry it — the value is filled server-side.
    #[serde(default)]
    pub to: String,
    /// Raw text. May be a JSON-stringified system message (see
    /// `is_idle_notification`). The SPA renders this as-is for plain
    /// text and skips it from the Mailbox stream for idle msgs.
    pub text: String,
    pub timestamp: String,
    #[serde(default)]
    pub color: Option<String>,
    /// Anthropic's per-user read flag. Read-only — UI just renders
    /// the highlight; never persisted back.
    #[serde(default)]
    pub read: bool,
    /// Sender's optional one-line summary (host fixture shows this on
    /// agent-to-lead checkpoint messages).
    #[serde(default)]
    pub summary: Option<String>,
    /// True iff `text` parses as `{"type": "idle_notification", ...}`.
    /// Server-side derivation so the SPA stays dumb.
    #[serde(default)]
    pub is_idle_notification: bool,
}

/// Load one teammate's inbox file. Missing file → empty vec (no
/// messages yet is a valid state on a fresh team).
pub fn load_inbox(claude_home: &Path, team: &str, teammate: &str) -> Result<Vec<InboxMessage>> {
    let path = teams_root(claude_home)
        .join(team)
        .join("inboxes")
        .join(format!("{teammate}.json"));
    load_inbox_from(&path, teammate)
}

pub(crate) fn load_inbox_from(path: &Path, owner: &str) -> Result<Vec<InboxMessage>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    // We deliberately deserialize into a value first so a single bad
    // entry doesn't trash the whole array.
    let arr: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    let items = match arr {
        serde_json::Value::Array(v) => v,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(items.len());
    for v in items {
        match serde_json::from_value::<InboxMessage>(v.clone()) {
            Ok(mut msg) => {
                msg.to = owner.to_string();
                msg.is_idle_notification = looks_like_idle_notification(&msg.text);
                out.push(msg);
            }
            Err(err) => {
                tracing::warn!(error = %err, owner, "skipping malformed inbox entry");
            }
        }
    }
    Ok(out)
}

/// Merge every `inboxes/*.json` for a team. Used by the
/// `?teammate=<missing>` form of the inbox endpoint and the recent-
/// messages snippet on `/api/v1/teams/<name>`.
pub fn load_all_inboxes(claude_home: &Path, team: &str) -> Result<Vec<InboxMessage>> {
    let dir = teams_root(claude_home).join(team).join("inboxes");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let owner = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let mut msgs = load_inbox_from(&path, &owner)?;
        out.append(&mut msgs);
    }
    // Sort ascending (oldest first) so the Mailbox stream renders
    // top-down; the SPA reverses for time-desc when requested.
    out.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(out)
}

/// Filter messages strictly after `since` (RFC3339). `None` → return
/// everything.
pub fn filter_since(msgs: Vec<InboxMessage>, since: Option<&str>) -> Vec<InboxMessage> {
    let Some(cursor) = since else { return msgs };
    msgs.into_iter()
        .filter(|m| m.timestamp.as_str() > cursor)
        .collect()
}

/// Newest-first preview slice used by `GET /api/v1/teams/<name>` —
/// the SPA cards show the last 5 lines so users can pick the live
/// team without paging.
pub fn recent_preview(msgs: &[InboxMessage], n: usize) -> Vec<InboxMessage> {
    let mut copy: Vec<InboxMessage> = msgs.to_vec();
    copy.sort_by_key(|m| Reverse(m.timestamp.clone()));
    copy.into_iter()
        .filter(|m| !m.is_idle_notification)
        .take(n)
        .collect()
}

/// True iff `text` is a JSON object with `"type": "idle_notification"`.
/// Anything that's not a JSON object falls through as false.
fn looks_like_idle_notification(text: &str) -> bool {
    // Cheap pre-check: a heavy `serde_json::from_str` is wasted on
    // typical plain prose so bail unless the first non-space char is
    // a brace.
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s == "idle_notification")
        })
        .unwrap_or(false)
}
