//! v0.9.0 W2 (F2/F7) — durable delegation watch: the crash-safe record of a
//! parent's interest in a child's completion.
//!
//! Lives at `<project>/.ccteam/chat/<child_sid>/delegation.json`, written with
//! the same `atomic_write_durable` (tmp+fsync+rename) discipline as `meta.json`
//! so a power-loss can never leave a half-written watch. There is at most ONE
//! watch per child (a later dispatch to the same child overwrites/updates it —
//! "one child has one pending parent" per the steer semantics). The gateway
//! keeps an in-memory mirror for the hot path; this file is the SoT that a
//! daemon-restart reconcile reads to deliver any completion notifications that
//! were missed while the daemon was down (at-least-once, deduped by
//! `notified_turns`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::fs_atomic::atomic_write_durable;
use super::turns_mirror::chat_dir;

/// One child's completion watch — who to notify, and what has already been
/// notified (dedup key = `(child_sid, turn_id)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationWatch {
    /// The sid of the session to notify when a watched turn completes (the
    /// dispatcher's principal — usually, but not necessarily, the spawner).
    pub parent_sid: String,
    /// Whether completion delivers a notification turn to `parent_sid`. `false`
    /// = ledger-only (the parent polls `session_collect` itself).
    pub notify: bool,
    /// Optional short label carried into the notification / visualization
    /// (ledger-only — NEVER concatenated into any dispatched prompt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The turn id of the most recent dispatch (diagnostic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_turn: Option<String>,
    /// ISO-8601 dispatch time (diagnostic).
    pub dispatched_at: String,
    /// Completed child turns already notified — the at-least-once dedup set. A
    /// reconcile after a daemon restart delivers only the completed turns NOT
    /// already in here.
    #[serde(default)]
    pub notified_turns: Vec<String>,
}

impl DelegationWatch {
    /// A fresh watch armed by a dispatch (no turns notified yet).
    pub fn armed(
        parent_sid: impl Into<String>,
        notify: bool,
        title: Option<String>,
        dispatched_turn: Option<String>,
    ) -> Self {
        Self {
            parent_sid: parent_sid.into(),
            notify,
            title,
            dispatched_turn,
            dispatched_at: chrono::Utc::now().to_rfc3339(),
            notified_turns: Vec::new(),
        }
    }
}

/// `<project>/.ccteam/chat/<child_sid>/delegation.json`.
pub fn delegation_path(project_dir: &Path, child_sid: &str) -> PathBuf {
    chat_dir(project_dir, child_sid).join("delegation.json")
}

/// Read the watch for `child_sid` (best-effort: missing / unparseable → `None`).
pub fn read_delegation_watch(project_dir: &Path, child_sid: &str) -> Option<DelegationWatch> {
    let raw = std::fs::read_to_string(delegation_path(project_dir, child_sid)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Durably write the watch for `child_sid` (tmp+fsync+rename — same discipline
/// as `meta.json`).
pub fn write_delegation_watch(
    project_dir: &Path,
    child_sid: &str,
    watch: &DelegationWatch,
) -> Result<()> {
    let path = delegation_path(project_dir, child_sid);
    std::fs::create_dir_all(path.parent().expect("path has parent"))?;
    atomic_write_durable(&path, serde_json::to_string_pretty(watch)?.as_bytes())
}

/// Drop the watch for `child_sid` (best-effort — used when the parent session
/// no longer exists so the watch can never fire).
pub fn remove_delegation_watch(project_dir: &Path, child_sid: &str) {
    let _ = std::fs::remove_file(delegation_path(project_dir, child_sid));
}

/// Scan `<project>/.ccteam/chat/*/delegation.json` and return every parseable
/// `(child_sid, watch)` pair (child_sid = the chat dir name). Used by the
/// daemon-startup reconcile.
pub fn scan_delegation_watches(project_dir: &Path) -> Vec<(String, DelegationWatch)> {
    let chat_base = project_dir.join(".ccteam").join("chat");
    let Ok(entries) = std::fs::read_dir(&chat_base) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in entries.flatten() {
        let Some(child_sid) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let path = entry.path().join("delegation.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(watch) = serde_json::from_str::<DelegationWatch>(&raw) {
            out.push((child_sid, watch));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let watch =
            DelegationWatch::armed("s1", true, Some("research".into()), Some("s2-3".into()));
        write_delegation_watch(tmp.path(), "s2", &watch).unwrap();
        let back = read_delegation_watch(tmp.path(), "s2").expect("watch reads back");
        assert_eq!(back.parent_sid, "s1");
        assert!(back.notify);
        assert_eq!(back.title.as_deref(), Some("research"));
        assert!(back.notified_turns.is_empty());
    }

    #[test]
    fn overwrite_updates_watch() {
        let tmp = TempDir::new().unwrap();
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s1", true, None, None),
        )
        .unwrap();
        // A later dispatch from a different parent overwrites (one pending parent).
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s9", false, None, None),
        )
        .unwrap();
        let back = read_delegation_watch(tmp.path(), "s2").unwrap();
        assert_eq!(back.parent_sid, "s9");
        assert!(!back.notify);
    }

    #[test]
    fn scan_finds_all_watches() {
        let tmp = TempDir::new().unwrap();
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s1", true, None, None),
        )
        .unwrap();
        write_delegation_watch(
            tmp.path(),
            "s3",
            &DelegationWatch::armed("s1", true, None, None),
        )
        .unwrap();
        let mut found = scan_delegation_watches(tmp.path());
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "s2");
        assert_eq!(found[1].0, "s3");
    }

    #[test]
    fn read_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        assert!(read_delegation_watch(tmp.path(), "nope").is_none());
    }

    #[test]
    fn remove_drops_watch() {
        let tmp = TempDir::new().unwrap();
        write_delegation_watch(
            tmp.path(),
            "s2",
            &DelegationWatch::armed("s1", true, None, None),
        )
        .unwrap();
        remove_delegation_watch(tmp.path(), "s2");
        assert!(read_delegation_watch(tmp.path(), "s2").is_none());
    }
}
