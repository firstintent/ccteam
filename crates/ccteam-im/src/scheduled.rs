//! Durable one-shot user-message scheduling.
//!
//! Each session owns one atomic `scheduled.json` beside its `turns.jsonl`.
//! This module deliberately contains only time parsing and persistence; the
//! gateway owns ACLs, the wakeable timer, progress events, and dispatching a
//! due item through its ordinary user-turn path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Duration, Local, LocalResult, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Maximum distance between creation and delivery.
pub const MAX_HORIZON: Duration = Duration::days(7);
/// Overdue messages older than this fail instead of firing during restart catch-up.
pub const MAX_CATCH_UP_AGE: Duration = Duration::hours(24);
/// Failed rows remain visible for this long.
pub const FAILED_RETENTION: Duration = Duration::hours(24);
/// Per-session pending-message cap.
pub const MAX_PENDING_PER_SID: usize = 20;
/// Pending-message cap across the sessions visible to one human chat.
pub const MAX_PENDING_VISIBLE: usize = 100;

/// Lifecycle of a stored scheduled message. Successful fires and cancellations
/// are removed; only pending and short-lived failures remain on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledStatus {
    /// Waiting for `send_at`.
    Pending,
    /// Dispatch failed; retained for 24 hours or until cancelled.
    Failed,
}

/// One durable scheduled user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledItem {
    /// Daemon-wide monotonic short id (`d{n}`).
    pub id: String,
    /// Target gateway session.
    pub sid: String,
    /// Catalog project slug owning the session.
    pub project: String,
    /// Full normal-user-turn body.
    pub text: String,
    /// UTC fire time.
    pub send_at: DateTime<Utc>,
    /// UTC creation time.
    pub created_at: DateTime<Utc>,
    /// Human owner tag (`channel:chat_id` / `user:<tenant>`).
    pub created_by: String,
    /// Pending or failed.
    pub status: ScheduledStatus,
    /// Human-readable dispatch failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<String>,
    /// UTC failure timestamp, used for 24-hour GC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<DateTime<Utc>>,
    /// Original human delivery channel. Kept separately from `created_by` so
    /// web owner tags (`user:*`) never masquerade as an IM transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_channel: Option<String>,
    /// Original human recipient/chat id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_chat_id: Option<String>,
}

impl ScheduledItem {
    /// Whether this row has exceeded failed-item retention at `now`.
    pub fn failed_expired(&self, now: DateTime<Utc>) -> bool {
        self.status == ScheduledStatus::Failed
            && self
                .failed_at
                .is_some_and(|failed_at| now.signed_duration_since(failed_at) >= FAILED_RETENTION)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduledFile {
    #[serde(default)]
    items: Vec<ScheduledItem>,
}

/// `<project>/.ccteam/chat/<sid>/scheduled.json`.
pub fn scheduled_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("scheduled.json")
}

/// Read one session's queue. A missing file is an empty queue.
pub fn read_scheduled(project_dir: &Path, sid: &str) -> Result<Vec<ScheduledItem>> {
    let path = scheduled_path(project_dir, sid);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let mut file: ScheduledFile =
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?;
    file.items.sort_by(scheduled_order);
    Ok(file.items)
}

/// Atomically replace one session's full queue.
pub fn write_scheduled(project_dir: &Path, sid: &str, items: &[ScheduledItem]) -> Result<()> {
    let path = scheduled_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&ScheduledFile {
        items: items.to_vec(),
    })?;
    ccteam_harness::atomic_write_durable(&path, &bytes)
}

/// Scan every registered project's session chat directories on daemon start.
/// Corrupt files are returned as errors to the caller one at a time only when
/// read directly; the aggregate rebuild warns and skips them so one bad queue
/// cannot prevent other scheduled messages from firing.
pub fn scan_scheduled(projects: &BTreeMap<String, PathBuf>) -> Vec<(PathBuf, ScheduledItem)> {
    let mut rows = Vec::new();
    for (slug, project_dir) in projects {
        let chat_dir = project_dir.join(".ccteam").join("chat");
        let Ok(entries) = std::fs::read_dir(&chat_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let sid = entry.file_name().to_string_lossy().into_owned();
            match read_scheduled(project_dir, &sid) {
                Ok(items) => {
                    rows.extend(items.into_iter().map(|mut item| {
                        item.sid = sid.clone();
                        item.project = slug.clone();
                        (project_dir.clone(), item)
                    }));
                }
                Err(err) => tracing::warn!(
                    project = %slug,
                    sid = %sid,
                    error = %err,
                    "scheduled queue rebuild skipped corrupt file"
                ),
            }
        }
    }
    rows.sort_by(|a, b| scheduled_order(&a.1, &b.1));
    rows
}

/// Sort by fire time, then monotonic id for deterministic ties.
pub fn scheduled_order(a: &ScheduledItem, b: &ScheduledItem) -> std::cmp::Ordering {
    a.send_at.cmp(&b.send_at).then_with(|| a.id.cmp(&b.id))
}

/// At most 80 Unicode scalar values, with whitespace collapsed for display and
/// progress events. The full body remains only in `scheduled.json`.
pub fn preview(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

/// Human-readable daemon-local timezone label for web pickers.
pub fn daemon_timezone_label() -> String {
    Local::now().format("%Z (UTC%:z)").to_string()
}

/// Parse one owner-confirmed time expression in the daemon's local timezone.
pub fn parse_send_time(input: &str) -> Result<DateTime<Utc>, ScheduleTimeError> {
    parse_send_time_at(input, Local::now())
}

/// Deterministic form used by tests. Absolute wall times are interpreted by
/// the daemon's actual [`Local`] timezone, exactly like production.
pub fn parse_send_time_at(
    input: &str,
    now: DateTime<Local>,
) -> Result<DateTime<Utc>, ScheduleTimeError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(ScheduleTimeError::InvalidFormat);
    }

    let target = if let Some(relative) = parse_relative(raw)? {
        // Relative units are minute/hour granular but measured from the exact
        // receipt instant (`+30m` is never shortened by wall-clock rounding).
        now.checked_add_signed(relative)
            .ok_or(ScheduleTimeError::InvalidFormat)?
    } else {
        let (day_offset, rest, has_day_prefix) = if let Some(rest) = raw.strip_prefix("今天") {
            (0, rest.trim(), true)
        } else if let Some(rest) = raw.strip_prefix("明天") {
            (1, rest.trim(), true)
        } else {
            (0, raw, false)
        };

        let naive = if rest.len() == 5 && rest.as_bytes().get(2) == Some(&b':') {
            let time = chrono::NaiveTime::parse_from_str(rest, "%H:%M")
                .map_err(|_| ScheduleTimeError::InvalidFormat)?;
            let date = now
                .date_naive()
                .checked_add_days(Days::new(day_offset))
                .ok_or(ScheduleTimeError::InvalidFormat)?;
            date.and_time(time)
        } else if !has_day_prefix {
            NaiveDateTime::parse_from_str(rest, "%Y-%m-%d %H:%M")
                .map_err(|_| ScheduleTimeError::InvalidFormat)?
        } else {
            return Err(ScheduleTimeError::InvalidFormat);
        };
        match Local.from_local_datetime(&naive) {
            LocalResult::Single(value) => value,
            LocalResult::Ambiguous(_, _) => return Err(ScheduleTimeError::AmbiguousLocalTime),
            LocalResult::None => return Err(ScheduleTimeError::NonexistentLocalTime),
        }
    };

    if target <= now {
        return Err(ScheduleTimeError::Past);
    }
    if target.signed_duration_since(now) > MAX_HORIZON {
        return Err(ScheduleTimeError::TooFar);
    }
    Ok(target.with_timezone(&Utc))
}

fn parse_relative(raw: &str) -> Result<Option<Duration>, ScheduleTimeError> {
    let Some(rest) = raw.strip_prefix('+') else {
        return Ok(None);
    };
    let (digits, unit) = if let Some(digits) = rest.strip_suffix('m') {
        (digits, "m")
    } else if let Some(digits) = rest.strip_suffix('h') {
        (digits, "h")
    } else {
        return Err(ScheduleTimeError::InvalidFormat);
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ScheduleTimeError::InvalidFormat);
    }
    let value = digits
        .parse::<i64>()
        .map_err(|_| ScheduleTimeError::InvalidFormat)?;
    let duration = match unit {
        "m" => Duration::try_minutes(value).ok_or(ScheduleTimeError::InvalidFormat)?,
        "h" => Duration::try_hours(value).ok_or(ScheduleTimeError::InvalidFormat)?,
        _ => return Err(ScheduleTimeError::InvalidFormat),
    };
    Ok(Some(duration))
}

/// User-facing time-parse failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleTimeError {
    /// Not one of the four accepted forms.
    #[error("invalid time; use HH:MM, YYYY-MM-DD HH:MM, +30m/+2h, or 今天/明天 HH:MM")]
    InvalidFormat,
    /// Absolute time is not in the future (today never rolls to tomorrow).
    #[error("scheduled time must be in the future (past HH:MM does not roll to tomorrow)")]
    Past,
    /// More than seven days away.
    #[error("scheduled time must be within 7 days")]
    TooFar,
    /// DST fall-back wall time names two instants.
    #[error("local time is ambiguous because of a timezone transition; choose another minute")]
    AmbiguousLocalTime,
    /// DST spring-forward wall time does not exist.
    #[error("local time does not exist because of a timezone transition; choose another minute")]
    NonexistentLocalTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDate, TimeZone, Timelike};
    use tempfile::TempDir;

    fn local_now() -> DateTime<Local> {
        let naive = NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(10, 15, 40)
            .unwrap();
        Local.from_local_datetime(&naive).single().unwrap()
    }

    #[test]
    fn parses_supported_local_and_relative_forms() {
        let now = local_now();
        let hhmm = parse_send_time_at("10:16", now)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!((hhmm.hour(), hhmm.minute()), (10, 16));

        let absolute = parse_send_time_at("2026-07-26 09:30", now)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            (absolute.day(), absolute.hour(), absolute.minute()),
            (26, 9, 30)
        );

        let relative = parse_send_time_at("+2h", now)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            (relative.hour(), relative.minute(), relative.second()),
            (12, 15, 40)
        );

        let tomorrow = parse_send_time_at("明天 09:00", now)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            (tomorrow.day(), tomorrow.hour(), tomorrow.minute()),
            (26, 9, 0)
        );
    }

    #[test]
    fn rejects_past_today_without_rollover_and_horizon() {
        let now = local_now();
        assert_eq!(
            parse_send_time_at("09:00", now).unwrap_err(),
            ScheduleTimeError::Past
        );
        assert_eq!(
            parse_send_time_at("今天 09:00", now).unwrap_err(),
            ScheduleTimeError::Past
        );
        assert_eq!(
            parse_send_time_at("+169h", now).unwrap_err(),
            ScheduleTimeError::TooFar
        );
        assert_eq!(
            parse_send_time_at("+0m", now).unwrap_err(),
            ScheduleTimeError::Past
        );
    }

    #[test]
    fn store_round_trip_is_sorted_and_cancel_is_durable() {
        let tmp = TempDir::new().unwrap();
        let now = Utc::now();
        let mk = |id: &str, minute: i64| ScheduledItem {
            id: id.into(),
            sid: "s1".into(),
            project: "demo".into(),
            text: format!("body {id}"),
            send_at: now + Duration::minutes(minute),
            created_at: now,
            created_by: "telegram:c1".into(),
            status: ScheduledStatus::Pending,
            fail_reason: None,
            failed_at: None,
            reply_channel: Some("telegram".into()),
            reply_chat_id: Some("c1".into()),
        };
        write_scheduled(tmp.path(), "s1", &[mk("d2", 2), mk("d1", 1)]).unwrap();
        let mut rows = read_scheduled(tmp.path(), "s1").unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["d1", "d2"]
        );

        rows.retain(|row| row.id != "d1");
        write_scheduled(tmp.path(), "s1", &rows).unwrap();
        assert_eq!(
            read_scheduled(tmp.path(), "s1")
                .unwrap()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["d2"]
        );
        assert!(!scheduled_path(tmp.path(), "s1")
            .with_extension("json.tmp")
            .exists());
    }

    #[test]
    fn preview_is_whitespace_collapsed_and_hard_capped() {
        assert_eq!(preview(" one\n two  three "), "one two three");
        assert_eq!(preview(&"界".repeat(100)).chars().count(), 80);
    }
}
