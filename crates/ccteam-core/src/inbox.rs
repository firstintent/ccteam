//! Per-session inbox/outbox file protocol (M1.1).
//!
//! Schema, semantics, and routing rules: `docs/interfaces.md` §3.4.
//!
//! Each ccteam-managed long session (meta-agent and project sessions)
//! has its own `<session>/.ccteam/inbox/` and `<session>/.ccteam/outbox/`.
//! Channel adapters (M2+, e.g. Telegram bot) write into inbox; the
//! session's claude writes replies into outbox; the orchestrator
//! consumes inbox files and injects body into the session via
//! tmux send-keys (idle-aware, see `progress::idle_aware_message`).
//!
//! **Atomic write**: every writer creates `<name>.tmp` then renames to
//! `<name>` so a partial flush is never visible to a reader.
//! **Idempotent ack**: the consumer (orchestrator for inbox, channel
//! adapter for outbox) deletes the file after processing; both sides
//! must treat "file missing" as a successful prior consumption.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// Current schema version emitted by ccteam writers.
///
/// A reader that finds `schema_version > LATEST_SCHEMA_VERSION` should
/// log a warning and best-effort parse — never refuse the file outright,
/// because the channel adapter shipping the future schema is presumed
/// to be at least as careful.
pub const LATEST_SCHEMA_VERSION: u32 = 1;

/// Inbox front matter — fields a channel adapter writes to describe an
/// incoming external message. Body lives outside the front matter.
///
/// Required: `schema_version` / `source` / `source_user` /
/// `created_at` / `ingested_at` / `content_type`.
/// Everything else is optional — adapters fill what they have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxFrontMatter {
    pub schema_version: u32,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_msg_id: Option<String>,
    pub source_user: String,
    pub created_at: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<InboxAttachment>,
}

/// Inbox attachment descriptor (M2+ multimedia path; M1 schema parses
/// it but production writers don't emit any).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxAttachment {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Outbox front matter — fields the session writes to describe an
/// outgoing reply. Adapter routes off `target_channels` /
/// `in_reply_to_source_msg_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxFrontMatter {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to_source_msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_channels: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default = "default_priority")]
    pub priority: OutboxPriority,
    pub event_kind: OutboxEventKind,
}

fn default_priority() -> OutboxPriority {
    OutboxPriority::Normal
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxPriority {
    Normal,
    High,
}

/// Outbox event kind — adapter routes off this for ack semantics
/// (silent vs. visible vs. threaded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxEventKind {
    /// Routine NL conversation reply.
    Reply,
    /// Phase boundary milestone — adapter MAY downgrade to silent push.
    Progress,
    /// User decision required — adapter MUST send a visible alert.
    /// Reserved for M1.7 once the L3 NL channel lands; M1 schema
    /// nonetheless accepts it so meta-agents can write them today.
    Escalation,
    /// Project terminal state.
    Shipped,
    /// Phase-internal CLARIFY question (Seed M2+).
    Clarify,
}

/// Parsed inbox message: front matter + body.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxMessage {
    pub front: InboxFrontMatter,
    pub body: String,
}

/// Parsed outbox message: front matter + body.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxMessage {
    pub front: OutboxFrontMatter,
    pub body: String,
}

impl InboxMessage {
    pub fn parse(source: &str) -> Result<Self> {
        let (front_yaml, body) = split_front_matter(source)?;
        let front: InboxFrontMatter = serde_yaml::from_str(front_yaml)
            .context("inbox front matter does not match schema")?;
        Ok(Self {
            front,
            body: body.to_string(),
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read inbox {}", path.display()))?;
        Self::parse(&body)
            .with_context(|| format!("parse inbox {}", path.display()))
    }

    /// Render to the canonical `--- yaml --- body` form.
    pub fn to_string(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(&self.front)
            .context("serialize inbox front matter")?;
        Ok(format!("---\n{yaml}---\n\n{}", self.body))
    }

    /// Atomic write to `path` (.tmp + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let body = self.to_string()?;
        atomic_write(path, body.as_bytes())
    }
}

impl OutboxMessage {
    pub fn parse(source: &str) -> Result<Self> {
        let (front_yaml, body) = split_front_matter(source)?;
        let front: OutboxFrontMatter = serde_yaml::from_str(front_yaml)
            .context("outbox front matter does not match schema")?;
        Ok(Self {
            front,
            body: body.to_string(),
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read outbox {}", path.display()))?;
        Self::parse(&body)
            .with_context(|| format!("parse outbox {}", path.display()))
    }

    pub fn to_string(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(&self.front)
            .context("serialize outbox front matter")?;
        Ok(format!("---\n{yaml}---\n\n{}", self.body))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = self.to_string()?;
        atomic_write(path, body.as_bytes())
    }
}

/// Build the canonical inbox filename for `now` and a 1-based sequence
/// number: `msg-<YYYYMMDDTHHMMSSZ>-<NNN>.md`. Compact ISO timestamp
/// (no colons) so the filename is portable across platforms / shells.
pub fn inbox_filename(now: DateTime<Utc>, seq: u32) -> String {
    format!("msg-{}-{:03}.md", compact_ts(now), seq)
}

/// Build the canonical outbox filename: `reply-<ts>-<NNN>.md`.
pub fn outbox_filename(now: DateTime<Utc>, seq: u32) -> String {
    format!("reply-{}-{:03}.md", compact_ts(now), seq)
}

fn compact_ts(now: DateTime<Utc>) -> String {
    // RFC3339 looks like 2026-05-06T10:30:00Z; we strip the colons so
    // the filename works on Windows / FAT and is shell-friendly.
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
        .replace(':', "")
}

/// Inbox/outbox directory pair for a ccteam-managed session.
#[derive(Debug, Clone)]
pub struct SessionMailbox {
    pub inbox: PathBuf,
    pub outbox: PathBuf,
}

impl SessionMailbox {
    /// Resolve from the session's `.ccteam/` directory.
    pub fn for_ccteam_dir(ccteam_dir: &Path) -> Self {
        Self {
            inbox: ccteam_dir.join("inbox"),
            outbox: ccteam_dir.join("outbox"),
        }
    }

    /// `mkdir -p` both directories. Idempotent.
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.inbox)
            .with_context(|| format!("create {}", self.inbox.display()))?;
        std::fs::create_dir_all(&self.outbox)
            .with_context(|| format!("create {}", self.outbox.display()))?;
        Ok(())
    }

    /// List inbox files in lexicographic (== chronological) order.
    /// Skips `.tmp` and dotfiles. Missing dir returns empty vec.
    pub fn list_inbox(&self) -> Result<Vec<PathBuf>> {
        list_messages(&self.inbox, "msg-")
    }

    /// List outbox files in lexicographic order.
    pub fn list_outbox(&self) -> Result<Vec<PathBuf>> {
        list_messages(&self.outbox, "reply-")
    }
}

fn list_messages(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| {
                    n.starts_with(prefix) && n.ends_with(".md") && !n.starts_with('.')
                })
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Atomic file write: write `<path>.tmp` then rename. Creates parent
/// dir if missing.
fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);
    std::fs::write(&tmp, body)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Strip the YAML front matter delimited by `---` lines.
fn split_front_matter(source: &str) -> Result<(&str, &str)> {
    let after_first = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("inbox/outbox file must start with `---`"))?;
    let end = after_first
        .find("\n---\n")
        .or_else(|| after_first.find("\n---\r\n"))
        .ok_or_else(|| anyhow!("inbox/outbox file missing closing `---` line"))?;
    let yaml = &after_first[..end];
    // Skip the closing fence line (`\n---\n` is 5 chars; `\r\n` 6).
    let body_start_offset = if after_first[end..].starts_with("\n---\r\n") {
        end + 6
    } else {
        end + 5
    };
    let body = after_first.get(body_start_offset..).unwrap_or("").trim_start_matches('\n');
    Ok((yaml, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_inbox() -> InboxMessage {
        InboxMessage {
            front: InboxFrontMatter {
                schema_version: 1,
                source: "telegram".into(),
                source_chat_id: Some("@rob".into()),
                source_msg_id: Some("tg-1".into()),
                source_user: "rob".into(),
                created_at: Utc.with_ymd_and_hms(2026, 5, 6, 10, 30, 0).unwrap(),
                ingested_at: Utc.with_ymd_and_hms(2026, 5, 6, 10, 30, 1).unwrap(),
                content_type: "text".into(),
                attachments: Vec::new(),
            },
            body: "做一个 todo cli\n".into(),
        }
    }

    #[test]
    fn round_trip_inbox_message() {
        let msg = sample_inbox();
        let body = msg.to_string().unwrap();
        let parsed = InboxMessage::parse(&body).unwrap();
        assert_eq!(parsed.front, msg.front);
        assert_eq!(parsed.body, msg.body);
    }

    #[test]
    fn parse_inbox_rejects_missing_required_field() {
        // No `source_user` field — required.
        let src = concat!(
            "---\n",
            "schema_version: 1\n",
            "source: telegram\n",
            "created_at: 2026-05-06T10:30:00Z\n",
            "ingested_at: 2026-05-06T10:30:01Z\n",
            "content_type: text\n",
            "---\n",
            "\n",
            "hello\n",
        );
        let err = InboxMessage::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("source_user"));
    }

    #[test]
    fn parse_inbox_accepts_minimal_required_fields() {
        let src = concat!(
            "---\n",
            "schema_version: 1\n",
            "source: cli\n",
            "source_user: rob\n",
            "created_at: 2026-05-06T10:30:00Z\n",
            "ingested_at: 2026-05-06T10:30:00Z\n",
            "content_type: text\n",
            "---\n",
            "\n",
            "body text\n",
        );
        let m = InboxMessage::parse(src).unwrap();
        assert_eq!(m.front.source_user, "rob");
        assert!(m.front.source_chat_id.is_none());
        assert_eq!(m.body.trim(), "body text");
    }

    #[test]
    fn round_trip_outbox_message() {
        let msg = OutboxMessage {
            front: OutboxFrontMatter {
                schema_version: 1,
                in_reply_to: Some("msg-2026-05-06T103000Z-001.md".into()),
                in_reply_to_source_msg_id: None,
                target_channels: vec!["telegram".into()],
                created_at: Utc.with_ymd_and_hms(2026, 5, 6, 10, 30, 45).unwrap(),
                priority: OutboxPriority::Normal,
                event_kind: OutboxEventKind::Reply,
            },
            body: "收到了\n".into(),
        };
        let body = msg.to_string().unwrap();
        let parsed = OutboxMessage::parse(&body).unwrap();
        assert_eq!(parsed.front.event_kind, OutboxEventKind::Reply);
        assert_eq!(parsed.front.priority, OutboxPriority::Normal);
        assert_eq!(parsed.body.trim(), "收到了");
    }

    #[test]
    fn outbox_event_kind_escalation_parses() {
        // M1 must accept escalation outboxes even though M1.7 doesn't
        // implement the full L3 NL flow yet — adapters writing today
        // shouldn't get rejected.
        let src = concat!(
            "---\n",
            "schema_version: 1\n",
            "created_at: 2026-05-06T10:30:00Z\n",
            "priority: high\n",
            "event_kind: escalation\n",
            "---\n",
            "stuck\n",
        );
        let m = OutboxMessage::parse(src).unwrap();
        assert_eq!(m.front.event_kind, OutboxEventKind::Escalation);
        assert_eq!(m.front.priority, OutboxPriority::High);
    }

    #[test]
    fn save_uses_atomic_rename() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("inbox/msg-1.md");
        let msg = sample_inbox();
        msg.save(&path).unwrap();
        let parsed = InboxMessage::load(&path).unwrap();
        assert_eq!(parsed.body, msg.body);
        // The .tmp should not linger.
        assert!(!path.with_extension("md.tmp").exists());
    }

    #[test]
    fn inbox_filename_is_portable() {
        // interfaces.md §3.4.1 specifies "compact ISO timestamp, colon-stripped"
        // — dashes in the date portion stay so the slug is human-readable.
        let ts = Utc.with_ymd_and_hms(2026, 5, 6, 10, 30, 0).unwrap();
        let name = inbox_filename(ts, 1);
        assert_eq!(name, "msg-2026-05-06T103000Z-001.md");
        assert!(!name.contains(':'), "filename must not contain colons");
    }

    #[test]
    fn outbox_filename_is_portable() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 6, 10, 30, 45).unwrap();
        let name = outbox_filename(ts, 7);
        assert_eq!(name, "reply-2026-05-06T103045Z-007.md");
        assert!(!name.contains(':'));
    }

    #[test]
    fn list_inbox_returns_chronological_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mb = SessionMailbox::for_ccteam_dir(tmp.path());
        mb.ensure_dirs().unwrap();
        let names = ["msg-20260506T103000Z-002.md", "msg-20260506T103000Z-001.md"];
        for name in names {
            let p = mb.inbox.join(name);
            std::fs::write(&p, "---\nschema_version: 1\nsource: cli\nsource_user: rob\ncreated_at: 2026-05-06T10:30:00Z\ningested_at: 2026-05-06T10:30:00Z\ncontent_type: text\n---\n\nx\n").unwrap();
        }
        // Foreign files should be skipped.
        std::fs::write(mb.inbox.join("notes.txt"), "noise").unwrap();
        std::fs::write(mb.inbox.join("msg-pending.md.tmp"), "x").unwrap();

        let list = mb.list_inbox().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].file_name().unwrap().to_str().unwrap().ends_with("-001.md"));
        assert!(list[1].file_name().unwrap().to_str().unwrap().ends_with("-002.md"));
    }

    #[test]
    fn list_inbox_handles_missing_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mb = SessionMailbox::for_ccteam_dir(tmp.path());
        // No ensure_dirs — list should return empty rather than erroring.
        assert!(mb.list_inbox().unwrap().is_empty());
    }
}
