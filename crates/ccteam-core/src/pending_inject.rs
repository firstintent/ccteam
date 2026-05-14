//! V0.2.2 F36 — pending phase-inject record.
//!
//! When `Orchestrator::dispatch_phase_with_state` detects an active
//! sub-agent (`progress::subagent_active(events) == true`) it skips the
//! tmux send-keys and instead persists a [`PendingInject`] to
//! `<project>/.ccteam/pending-inject.json`. The orchestrator daemon
//! tick later sees the file, re-checks `subagent_active`, and either:
//!
//! - drains it (subagent has stopped) → real `dispatch_phase_with_state`
//!   call + `delete(path)`,
//! - or, if the enqueue timestamp is older than `max_defer_minutes`,
//!   surfaces an enriched `needs_attention.outbox.json` payload with
//!   `ccteam_classification: "inject_defer_timeout"` and deletes the
//!   pending record so the project does not loop forever in defer
//!   limbo.
//!
//! **Single-file overwrite, not a queue**: each new defer for the
//! project replaces the previous record. The orchestrator dispatches
//! at most one phase prompt at a time per project, so queuing has no
//! semantics; the latest pending phase always wins.
//!
//! **Red lines** (CLAUDE.md §三):
//!
//! - Pure file I/O — no LLM, no terminal-output parsing.
//! - Atomic write (`<file>.json.tmp` + rename) so a crash mid-write
//!   never leaves a half-written record on disk.
//! - Caller must skip evergreen / meta-agent projects upstream.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Filename under `<project>/.ccteam/` carrying the deferred phase
/// inject record.
pub const PENDING_INJECT_FILE: &str = "pending-inject.json";

/// Default ceiling on how long a phase inject can sit in defer limbo
/// before the orchestrator gives up and surfaces an enriched escalate
/// instead. PRD §5.2 — "10 minutes is generous; sub-agent runs ≥10
/// min are flagged by F35 as `SubagentRunaway` independently".
pub const DEFAULT_MAX_DEFER_MINUTES: u64 = 10;

/// On-disk shape of `<project>/.ccteam/pending-inject.json`. Schema
/// versioned so a V0.3 change (e.g. queue semantics) can read V0.2.2
/// records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingInject {
    pub schema_version: u32,
    pub slug: String,
    pub phase: String,
    /// `@`-attachment paths the deferred inject should reference once
    /// drained — kept verbatim so the drain rebuilds the same prompt
    /// the original dispatch would have produced (sub-skill outputs
    /// from the prior phase).
    #[serde(default)]
    pub attachments: Vec<String>,
    pub enqueued_at: DateTime<Utc>,
    pub max_defer_minutes: u64,
}

impl PendingInject {
    /// Construct a fresh record stamped with `now`.
    pub fn new(
        slug: impl Into<String>,
        phase: impl Into<String>,
        attachments: Vec<String>,
        now: DateTime<Utc>,
        max_defer_minutes: u64,
    ) -> Self {
        Self {
            schema_version: 1,
            slug: slug.into(),
            phase: phase.into(),
            attachments,
            enqueued_at: now,
            max_defer_minutes,
        }
    }

    /// `true` once `now - enqueued_at >= max_defer_minutes`. Used by
    /// the orchestrator drain to switch from "wait for SubagentStop"
    /// to "fail-loud escalate".
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        let budget = Duration::minutes(self.max_defer_minutes as i64);
        now.signed_duration_since(self.enqueued_at) >= budget
    }
}

/// `<project>/.ccteam/pending-inject.json` for a slug-rooted project
/// directory. Mirrors `paths::project_state_in` shape so tests don't
/// need a `CcteamPaths` instance.
pub fn pending_inject_path_in(project_dir: &Path) -> PathBuf {
    project_dir.join(".ccteam").join(PENDING_INJECT_FILE)
}

/// Persist `record` atomically to `path`. Single-file overwrite — any
/// existing pending inject is replaced.
pub fn save(path: &Path, record: &PendingInject) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(record).context("serialize pending-inject record")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// `Ok(None)` when the file is absent (the common case). Parse
/// failures fail-loud so a corrupt record is visible at the next tick
/// instead of silently swallowing a deferred inject.
pub fn load(path: &Path) -> Result<Option<PendingInject>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if body.trim().is_empty() {
        return Ok(None);
    }
    let record: PendingInject =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(record))
}

/// Best-effort delete — missing file is not an error (the drain path
/// races the tick: a successful real dispatch leaves the file deleted).
pub fn delete(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().unwrap()
    }

    #[test]
    fn pending_inject_path_lives_under_dot_ccteam() {
        let p = pending_inject_path_in(Path::new("/tmp/proj"));
        assert!(
            p.ends_with(".ccteam/pending-inject.json"),
            "got {}",
            p.display()
        );
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pending-inject.json");
        let record = PendingInject::new(
            "dev-x",
            "implement",
            vec![".ccteam/code-review.md".into()],
            ts(1_700_000_000),
            10,
        );
        save(&path, &record).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded, record);
    }

    #[test]
    fn save_overwrites_previous_record() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pending-inject.json");
        let first = PendingInject::new("dev-x", "plan-eng", vec![], ts(1), 10);
        save(&path, &first).unwrap();
        let second = PendingInject::new("dev-x", "implement", vec![], ts(2), 10);
        save(&path, &second).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.phase, "implement");
        assert_eq!(loaded.enqueued_at, ts(2));
    }

    #[test]
    fn load_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.json");
        assert!(load(&path).unwrap().is_none());
    }

    #[test]
    fn load_empty_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty.json");
        std::fs::write(&path, "").unwrap();
        assert!(load(&path).unwrap().is_none());
    }

    #[test]
    fn delete_removes_file_and_tolerates_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pending-inject.json");
        let record = PendingInject::new("dev-x", "x", vec![], ts(1), 10);
        save(&path, &record).unwrap();
        assert!(path.exists());
        delete(&path).unwrap();
        assert!(!path.exists());
        // second delete is a no-op
        delete(&path).unwrap();
    }

    #[test]
    fn is_expired_within_budget_is_false() {
        let enqueued = ts(1_700_000_000);
        let record = PendingInject::new("dev-x", "x", vec![], enqueued, 10);
        let still_inside = enqueued + Duration::minutes(9);
        assert!(!record.is_expired(still_inside));
    }

    #[test]
    fn is_expired_at_or_past_budget_is_true() {
        let enqueued = ts(1_700_000_000);
        let record = PendingInject::new("dev-x", "x", vec![], enqueued, 10);
        let at_cap = enqueued + Duration::minutes(10);
        assert!(record.is_expired(at_cap));
        let past = enqueued + Duration::minutes(11);
        assert!(record.is_expired(past));
    }

    #[test]
    fn schema_version_is_one_for_fresh_records() {
        let r = PendingInject::new("dev-x", "x", vec![], ts(0), 10);
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn default_max_defer_minutes_constant() {
        assert_eq!(DEFAULT_MAX_DEFER_MINUTES, 10);
    }
}
