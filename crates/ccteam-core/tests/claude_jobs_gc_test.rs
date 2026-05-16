//! V0.4.6 F85 — integration tests for `~/.claude/jobs/` GC.
//!
//! Covers the four PRD-mandated rows (dev-plan 阶段 7):
//!
//! 1. terminated dirs older than retention → removed
//! 2. `state == "working"` → never touched
//! 3. unparseable `state.json` → preserved (+ WARN, not asserted here)
//! 4. `retention_days == 0` → noop
//!
//! Every test builds its own jobs/ tempdir and calls
//! `gc_terminated_jobs(&jobs_dir, ...)` directly so we don't need to
//! mutate `$CCTEAM_CLAUDE_JOBS_DIR` (race-prone across test threads).

use std::fs;
use std::path::Path;

use ccteam_core::{gc_terminated_jobs, GcDisposition};
use chrono::{Duration, Utc};
use tempfile::TempDir;

/// Write a `state.json` mocking the Claude Code `--bg` shape.
fn write_state_json(jobs_dir: &Path, id: &str, state: &str, first_terminal_at: Option<&str>) {
    let dir = jobs_dir.join(id);
    fs::create_dir_all(&dir).unwrap();
    let body = match first_terminal_at {
        Some(ts) => format!(r#"{{"state":"{state}","firstTerminalAt":"{ts}","cost_usd":0.42}}"#,),
        None => format!(r#"{{"state":"{state}","cost_usd":0.42}}"#),
    };
    fs::write(dir.join("state.json"), body).unwrap();
}

#[test]
fn t01_gc_removes_terminated_old() {
    // 3 mock entries: 1 working / 1 completed 8d ago / 1 completed 3d ago.
    // retention_days = 7 → only the 8d-old one is eligible.
    let tmp = TempDir::new().unwrap();
    let jobs_dir = tmp.path().join("jobs");
    fs::create_dir_all(&jobs_dir).unwrap();

    let eight_d_ago = (Utc::now() - Duration::days(8)).to_rfc3339();
    let three_d_ago = (Utc::now() - Duration::days(3)).to_rfc3339();

    write_state_json(&jobs_dir, "active", "working", None);
    write_state_json(&jobs_dir, "old", "completed", Some(&eight_d_ago));
    write_state_json(&jobs_dir, "recent", "completed", Some(&three_d_ago));

    let report = gc_terminated_jobs(&jobs_dir, 7, false).unwrap();

    assert_eq!(report.dir_count_before, 3, "should observe 3 entries");
    assert_eq!(report.removed, 1, "exactly one entry should be reclaimed");
    assert_eq!(report.kept_working, 1);
    assert_eq!(report.kept_recent, 1);
    assert_eq!(
        report.dir_count_after, 2,
        "after real removal, 2 dirs remain on disk"
    );
    assert!(!report.dry_run);

    // Verify on-disk reality matches the report.
    assert!(jobs_dir.join("active").is_dir(), "working dir must stay");
    assert!(jobs_dir.join("recent").is_dir(), "in-window dir must stay");
    assert!(
        !jobs_dir.join("old").exists(),
        "8d-old terminated dir must be removed"
    );

    // Per-entry detail matches.
    let old = report
        .entries
        .iter()
        .find(|e| e.job_id == "old")
        .expect("missing old entry");
    assert_eq!(old.disposition, GcDisposition::Removed);
}

#[test]
fn t02_gc_preserves_working() {
    // Even though "working" is `firstTerminalAt: null` and the dir's
    // mtime is old, `state == "working"` is an absolute hard-stop per
    // CLAUDE.md §三 "永不主动 kill 长 session". GC must never reclaim
    // it regardless of retention setting.
    let tmp = TempDir::new().unwrap();
    let jobs_dir = tmp.path().join("jobs");
    fs::create_dir_all(&jobs_dir).unwrap();

    write_state_json(&jobs_dir, "long-running", "working", None);
    // Backdate the mtime — irrelevant for state=="working" but a
    // realistic stress: a session running for >7 days.
    let dir = jobs_dir.join("long-running");
    let old = filetime_for_path_old(&dir);
    set_file_mtime(&dir, old);

    let report = gc_terminated_jobs(&jobs_dir, 7, false).unwrap();

    assert_eq!(report.dir_count_before, 1);
    assert_eq!(report.removed, 0, "working dir must NEVER be removed");
    assert_eq!(report.kept_working, 1);
    assert!(jobs_dir.join("long-running").is_dir());
}

#[test]
fn t03_gc_preserves_corrupt_state_json() {
    // Unparseable JSON → preserved + WARN. Same for missing
    // state.json (a job dir without one is corrupt in our schema).
    let tmp = TempDir::new().unwrap();
    let jobs_dir = tmp.path().join("jobs");
    fs::create_dir_all(&jobs_dir).unwrap();

    // 1. Invalid JSON
    let bad = jobs_dir.join("garbled");
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join("state.json"), b"{ not valid json").unwrap();

    // 2. Missing state.json entirely
    fs::create_dir_all(jobs_dir.join("orphan")).unwrap();

    let report = gc_terminated_jobs(&jobs_dir, 7, false).unwrap();

    assert_eq!(report.dir_count_before, 2);
    assert_eq!(report.removed, 0, "corrupt dirs must NEVER be removed");
    assert_eq!(report.kept_corrupt, 2);
    assert!(jobs_dir.join("garbled").is_dir());
    assert!(jobs_dir.join("orphan").is_dir());
}

#[test]
fn t04_gc_zero_retention_noop() {
    // `claude_jobs_retention_days: 0` → GC disabled. Even an
    // ancient terminated dir must be preserved untouched.
    let tmp = TempDir::new().unwrap();
    let jobs_dir = tmp.path().join("jobs");
    fs::create_dir_all(&jobs_dir).unwrap();

    let ancient = (Utc::now() - Duration::days(365)).to_rfc3339();
    write_state_json(&jobs_dir, "ancient", "completed", Some(&ancient));

    let report = gc_terminated_jobs(&jobs_dir, 0, false).unwrap();

    // F85 contract: retention=0 short-circuits before even reading
    // `jobs_dir`, so dir_count_before stays 0.
    assert_eq!(report.dir_count_before, 0);
    assert_eq!(report.removed, 0);
    assert!(
        report.entries.is_empty(),
        "retention=0 must not enumerate entries"
    );
    assert!(
        jobs_dir.join("ancient").is_dir(),
        "GC must be a no-op when retention is 0"
    );
}

// ----- file mtime helpers (no extra deps; stdlib metadata + a tiny shim) -----

fn filetime_for_path_old(_p: &Path) -> std::time::SystemTime {
    std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 24 * 3600)
}

/// Best-effort `utimensat` via the `filetime`-style stdlib hop. We
/// avoid pulling a new dep just for this; `std::os::unix::fs::FileExt`
/// doesn't expose mtime writes, so we shell out to `touch -d`.
fn set_file_mtime(path: &Path, when: std::time::SystemTime) {
    // Convert SystemTime → "@<epoch>" so `touch -d` parses without TZ
    // headaches.
    let epoch = when
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = std::process::Command::new("touch")
        .arg("-d")
        .arg(format!("@{}", epoch))
        .arg(path)
        .status();
}
