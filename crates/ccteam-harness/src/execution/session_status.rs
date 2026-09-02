//! Per-session status snapshot — `<project>/.ccteam/chat/<sid>/status.json`.
//!
//! A long-lived session's statusline (model · effort · context usage) is
//! assembled from events that only arrive at turn boundaries. Held purely in
//! the adapter's live map, it evaporates the moment the session leaves it —
//! idle-release, `sessions.max_live` eviction, daemon restart — and the
//! session comes back reporting nothing, or worse, reporting the zero its
//! counters were reinitialised to.
//!
//! So the snapshot is written next to the turns mirror and read back when the
//! live map has nothing better. This is the durability the TUI adapter gets
//! for free (it re-derives status from the on-disk transcript on every call);
//! every long-stdio adapter has to persist it deliberately.
//!
//! ccteam-owned file, no vendor-internal dependency. Introduced by
//! stream-json, shared with the ACP vendors so "where does a session's status
//! live" has one answer instead of one per protocol.

use std::path::{Path, PathBuf};

use crate::ThreadStatus;

use super::fs_atomic::atomic_write_durable;

/// `<project_dir>/.ccteam/chat/<sid>/status.json`.
pub fn status_json_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("status.json")
}

/// Persist the latest status atomically. Best-effort: a write failure only
/// means a released session shows no statusline until its next turn — never
/// worth failing a turn over.
pub fn write_status_file(project_dir: &Path, sid: &str, status: &ThreadStatus) {
    let path = status_json_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(body) = serde_json::to_vec(status) else {
        return;
    };
    let _ = atomic_write_durable(&path, &body);
}

/// Read the persisted snapshot, or `None` if absent / unreadable / stale-shaped.
///
/// A caller that must not confuse "no snapshot" with "could not look" wants
/// [`read_status_file_reporting`] instead.
pub fn read_status_file(project_dir: &Path, sid: &str) -> Option<ThreadStatus> {
    read_status_file_reporting(project_dir, sid).ok().flatten()
}

/// [`read_status_file`] with the failure it otherwise collapses into `None`.
///
/// `docs-local/issues/#14` — the thread-generation floor is computed from these
/// snapshots, so a session whose `status.json` could not be READ is not a
/// session with no stamp: its generation is still on disk, and treating it as
/// absent would let a recovered counter re-issue it. `Ok(None)` = genuinely no
/// snapshot yet; `Err` = a read/permission failure, or [`std::io::ErrorKind::
/// InvalidData`] for a file that exists but does not parse.
pub fn read_status_file_reporting(
    project_dir: &Path,
    sid: &str,
) -> Result<Option<ThreadStatus>, std::io::Error> {
    match std::fs::read_to_string(status_json_path(project_dir, sid)) {
        Ok(body) => serde_json::from_str(&body)
            .map(Some)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextSource, ContextUsage};

    #[test]
    fn roundtrips_a_status_with_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let status = ThreadStatus {
            generation: None,
            model: Some("grok-4.5".into()),
            context: Some(ContextUsage::known(17_580, 500_000, ContextSource::Derived)),
            effort: Some("high".into()),
            goal: None,
        };
        write_status_file(dir.path(), "s7", &status);
        assert_eq!(read_status_file(dir.path(), "s7"), Some(status));
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_status_file(dir.path(), "s404"), None);
    }

    /// A snapshot written before `ContextUsage` grew its `Option`/provenance
    /// shape must still load — it is live daemon state, not a fixture.
    #[test]
    fn legacy_snapshot_without_source_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = status_json_path(dir.path(), "s8");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"model":"claude-opus-4-8[1m]","context":{"used_tokens":188000,"window_tokens":1000000}}"#,
        )
        .unwrap();
        let got = read_status_file(dir.path(), "s8").expect("legacy shape must load");
        let ctx = got.context.expect("context survives");
        assert_eq!(ctx.used_tokens, Some(188_000));
        assert_eq!(ctx.source, ContextSource::Unknown);
        assert_eq!(
            got.generation, None,
            "a file written before the stamp existed claims no thread"
        );
    }

    /// `docs-local/issues/#14②` — the file layer every writer family goes
    /// through. A sid outlives its threads, so the reader decides which
    /// observation to trust by its generation; the stamp has to survive the
    /// round trip or the whole rule is inert.
    #[test]
    fn a_persisted_status_keeps_its_generation_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let status = ThreadStatus {
            generation: Some(42),
            model: Some("claude-fable-5-1[1m]".into()),
            context: None,
            effort: Some("max".into()),
            goal: None,
        };
        write_status_file(dir.path(), "s9", &status);
        assert_eq!(
            read_status_file(dir.path(), "s9").and_then(|s| s.generation),
            Some(42)
        );
    }
}
