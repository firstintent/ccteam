//! V0.4.5 F80 — Liveness probe for `claude --bg --agent` background jobs.
//!
//! Every claude background session writes its lifecycle into
//! `~/.claude/jobs/<job_id>/state.json` (or `$CCTEAM_CLAUDE_JOBS_DIR`
//! when set — same env override `harness::state_json_path` reads).
//! This module is the SHARED helper both `queries::workflow_summary`
//! (read-side: phantom-running detection) and
//! `orchestrator::poll_completions` (write-side: stale-spawn cleanup)
//! call when they need to decide "is the bg job still alive, or did
//! its host process die without writing a matching `agent_done`?"
//!
//! ## Background
//!
//! Pre-F80 the only signal the orchestrator emitted for a finished
//! agent was the `agent_done` line in `progress.jsonl`. That line is
//! written inside `poll_completions` after observing `state.json::state ∈
//! {done, failed, crashed}`. When the daemon itself is SIGKILLed
//! (V0.4.5 still has the shutdown-deadlock force-kill path), in-flight
//! `claude --bg` sessions die without anything writing the matching
//! `agent_done`. The stale `agent_spawn` line lingers forever; the
//! web UI counts it as "running" until manually cleaned.
//!
//! F80 fix: every consumer that needs "is this spawn really still
//! running?" cross-references the spawn's recorded `job_id` against
//! `state.json`. Three terminal signals win:
//!
//! 1. `state.json` is missing entirely (job dir vanished).
//! 2. `firstTerminalAt` field is non-null (Claude Code's own
//!    end-of-session timestamp).
//! 3. `state` field is in the terminal set (`done`, `failed`,
//!    `crashed`, `stopped`, legacy `completed` / `error`).
//!
//! Any of those → [`probe_job`] returns [`JobLiveness::Terminal`] with
//! best-effort `cost_usd` (sourced from state.json when present, else
//! 0.0) and a coarse `status` string the orchestrator can stamp onto a
//! synthetic `agent_done` event.
//!
//! ## Red lines
//!
//! - **No mutation here.** The module only reads `state.json`; emitting
//!   any `agent_done` event is the caller's responsibility (matches the
//!   "progress.jsonl is SoT" red line — only the orchestrator writes
//!   workflow events).
//! - **`job_id = None` always counts as terminal.** Old agent_spawn
//!   lines written before F80 do not carry `job_id`; they all surface
//!   as `Terminal { status: "killed", cost_usd: 0.0 }` so the stale
//!   rows clear once `poll_completions` next runs. There is no
//!   migration path for pre-F80 `progress.jsonl` history beyond this
//!   one-shot drain.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use serde_json::Value;

/// Outcome of a single liveness probe.
#[derive(Debug, Clone, PartialEq)]
pub enum JobLiveness {
    /// `state.json` parsed cleanly and the job is still working.
    /// Treat the matching `agent_spawn` as legitimately running.
    Running,
    /// The job is gone (state.json missing / job_id unset) OR has
    /// finished (`firstTerminalAt` non-null OR `state` is terminal).
    /// Caller should emit a synthetic `agent_done` to retire the
    /// outstanding `agent_spawn`.
    Terminal {
        /// Coarse status string the orchestrator stamps onto the
        /// `agent_done` event. Values: `"completed"` (Claude reported
        /// `done`), `"error"` (`failed` / `crashed`), `"killed"`
        /// (state.json missing or job_id absent — daemon SIGKILL
        /// casualty).
        status: &'static str,
        /// Best-effort cumulative cost the orchestrator should append
        /// to its accumulator + state.cost_used_usd. Sourced from
        /// `state.json::cost_usd` / `cost_usd_total` when present,
        /// else 0.0.
        cost_usd: f64,
    },
}

/// Probe a `claude --bg` background job's liveness via
/// `harness::state_json_path(job_id)`.
///
/// Returns [`JobLiveness::Terminal`] with `status: "killed"` when:
/// - `job_id` is `None` (legacy agent_spawn row without F80 plumbing),
/// - the file does not exist (host job dir wiped),
/// - the file exists but is unparseable JSON (treat as gone — safer
///   than leaving a phantom running),
/// - `firstTerminalAt` is non-null,
/// - `state ∈ {failed, crashed, stopped, error}` (mapped to `"error"`)
/// - `state ∈ {done, completed}` (mapped to `"completed"`).
///
/// Returns [`JobLiveness::Running`] only when `state.json` parses and
/// none of the terminal signals fire — i.e. an active session whose
/// host process is still attached.
pub fn probe_job(job_id: Option<&str>) -> JobLiveness {
    let Some(id) = job_id else {
        return JobLiveness::Terminal {
            status: "killed",
            cost_usd: 0.0,
        };
    };
    let path = ccteam_harness::state_json_path(id);
    probe_state_json(&path)
}

/// Lower-level helper for tests — bypasses the `state_json_path`
/// resolver so unit tests can pass a `tempdir()` path directly without
/// fiddling with `$CCTEAM_CLAUDE_JOBS_DIR`.
pub fn probe_state_json(path: &std::path::Path) -> JobLiveness {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            return JobLiveness::Terminal {
                status: "killed",
                cost_usd: 0.0,
            }
        }
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            return JobLiveness::Terminal {
                status: "killed",
                cost_usd: 0.0,
            }
        }
    };
    classify(&value)
}

/// Pure classifier — useful for unit tests that already have the
/// parsed `Value` in hand (no IO).
///
/// V0.5.0 F92: `cost_usd` is derived from the transcript JSONL
/// (`linkScanPath` or cwd+sessionId fallback) via
/// [`crate::transcript_scanner::session_cost_from_jsonl`] when the
/// state.json's own `cost_usd` / `cost_usd_total` field is `0.0` or
/// missing — that field reads `0` on the host even for sessions that
/// burned real dollars. We log a WARN once per session id on
/// `linkScanPath` miss and then surface the state.json value (typically
/// `0.0`) rather than fabricating a number.
pub fn classify(value: &Value) -> JobLiveness {
    let cost_usd = resolve_cost_usd(value);

    // F80 — Claude Code 2.1.x writes `firstTerminalAt` once the
    // session enters a terminal state. Non-null → finished, even if
    // `state` still reads `"working"` for a tick (race window).
    let first_terminal_at_present = value
        .get("firstTerminalAt")
        .map(|v| !v.is_null())
        .unwrap_or(false);

    let state_str = value
        .get("state")
        .and_then(|s| s.as_str())
        .or_else(|| value.get("status").and_then(|s| s.as_str()))
        .unwrap_or("working");
    let terminal_status = match state_str {
        "done" | "completed" => Some("completed"),
        "failed" | "crashed" | "error" => Some("error"),
        "stopped" => Some("stopped"),
        _ => None,
    };

    if let Some(status) = terminal_status {
        return JobLiveness::Terminal { status, cost_usd };
    }
    if first_terminal_at_present {
        return JobLiveness::Terminal {
            status: "completed",
            cost_usd,
        };
    }
    // V0.6.3 F144 — forward-compat: a `state` string we don't recognise
    // (and that isn't the canonical `working`) is treated as
    // **non-terminal** — we keep probing on the next tick rather than
    // synthesising a premature `agent_done` that would strand a phantom
    // job. Warn once so a Claude Code state-vocabulary drift surfaces in
    // the logs without flooding every poll.
    if state_str != "working" {
        crate::vendor_compat::warn_unknown_vendor_token(
            "claude_job_state",
            state_str,
            "treating as non-terminal (job stays Running); will keep probing",
        );
    }
    JobLiveness::Running
}

/// V0.5.0 F92 — derive `cost_usd` for a parsed `state.json` Value.
///
/// Resolution order:
/// 1. Try the transcript JSONL via `linkScanPath` (or cwd+sessionId
///    fallback) — sums every `message.usage` block, prices via
///    `pricing.json`. This is the **authoritative** path: state.json's
///    own `cost_usd_total` reads `0` in production even when real
///    dollars have accrued (V0.4.6 dex-ui probe).
/// 2. Fall back to state.json's `cost_usd` / `cost_usd_total` field
///    when the transcript can't be located. Emit a WARN-once-per-session
///    so operators can spot misconfigurations.
///
/// Returns `0.0` when neither source produces a number.
pub(crate) fn resolve_cost_usd(value: &Value) -> f64 {
    let state_cost = value
        .get("cost_usd")
        .or_else(|| value.get("cost_usd_total"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let model = model_from_state(value).unwrap_or_else(|| "claude-sonnet-4-6".to_string());
    match crate::transcript_scanner::session_cost_from_jsonl(value, &model) {
        Some(t_cost) if t_cost > 0.0 => t_cost,
        Some(_zero) => {
            // Transcript present but zero usage so far (fresh session,
            // no assistant turn yet). Honor state.json's value — for
            // some workflows the orchestrator finalizes there before
            // the transcript catches up.
            state_cost
        }
        None => {
            // No transcript path resolvable → WARN-once + state.json
            // fallback. On the host the fallback is usually 0.0; for
            // unit tests with a hand-crafted state.json::cost_usd it
            // surfaces that value (still WARN, since the WARN is about
            // the missing transcript link, not the cost number).
            warn_link_scan_miss_once(value);
            state_cost
        }
    }
}

/// Extract the model id following `--model` in
/// `state.json::respawnFlags`. Mirrors `queries::model_from_respawn_flags`
/// but is duplicated here because making it `pub` in queries would leak
/// an internal helper to the public surface; the snippet is six lines.
fn model_from_state(state: &Value) -> Option<String> {
    let flags = state.get("respawnFlags")?.as_array()?;
    let mut it = flags.iter();
    while let Some(item) = it.next() {
        if item.as_str() == Some("--model") {
            return it.next().and_then(|v| v.as_str()).map(String::from);
        }
    }
    None
}

/// Dedup set for the WARN-once-per-session "linkScanPath missing" log.
/// Lives at module scope so tests can clear it deterministically via
/// [`reset_link_scan_warn_for_tests`].
static LINK_SCAN_SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn warn_link_scan_miss_once(value: &Value) {
    let key = value
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            value
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| "<unknown>".to_string())
        });
    let lock = LINK_SCAN_SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = lock.lock().expect("warn-once mutex poisoned");
    if set.insert(key.clone()) {
        let path = value
            .get("linkScanPath")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        tracing::warn!(
            session_id = %key,
            link_scan_path = %path,
            "linkScanPath missing or jsonl unresolvable; falling back to state.json cost (likely 0)",
        );
        #[cfg(any(test, feature = "test-util"))]
        LINK_SCAN_WARN_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Test-only counter — increments each time `warn_link_scan_miss_once`
/// actually emits a WARN (i.e. the dedup set was extended). Lets tests
/// assert "WARN emitted exactly once" without a tracing subscriber.
#[cfg(any(test, feature = "test-util"))]
static LINK_SCAN_WARN_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Read the WARN counter (test-only).
#[cfg(any(test, feature = "test-util"))]
pub fn link_scan_warn_count() -> usize {
    LINK_SCAN_WARN_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Reset both the WARN counter and the dedup set so multi-test
/// interleaving stays deterministic. Test-only.
#[cfg(any(test, feature = "test-util"))]
pub fn reset_link_scan_warn_for_tests() {
    LINK_SCAN_WARN_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
    let lock = LINK_SCAN_SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut s) = lock.lock() {
        s.clear();
    }
}

/// Resolve the absolute state.json path for a `(job_id)`. Thin
/// re-export of `harness::state_json_path` so call sites that want to
/// log the path don't need to import `harness` directly.
pub fn job_state_path(job_id: &str) -> PathBuf {
    ccteam_harness::state_json_path(job_id)
}

// V0.4.6 F85 — `~/.claude/jobs/` GC.
//
// Long-lived hosts accumulate one subdirectory per finished `claude --bg`
// session. F80 phantom-cleanup retired stale `agent_spawn` rows in
// `progress.jsonl`, but it never touches `~/.claude/jobs/<id>/`. As a
// result host inventories drift into the hundreds (289 entries observed
// on the dev box on 2026-05-16) and `state.json` reads grow slower over
// time. F85 reclaims that space by deleting `<jobs_dir>/<job_id>/` once
// the job has been terminal for `retention_days`.
//
// Rules (PRD §F85 + dev-plan 阶段 7):
//
// 1. Walk every immediate child of `jobs_dir` (one dir per job id).
// 2. Read `<entry>/state.json`. If absent, count it `corrupt` and skip.
// 3. Parse JSON. On parse error, count it `corrupt` and skip.
// 4. Classify state:
//    - `working` → keep (active session; never kill long sessions, §三)
//    - terminal (`completed` / `done` / `stopped` / `error` / `failed`
//      / `crashed` / `killed`) AND `firstTerminalAt` >= retention cutoff
//      → keep
//    - terminal AND `firstTerminalAt` < retention cutoff → eligible
//    - terminal without `firstTerminalAt` (legacy state.json) →
//      fall back to the dir mtime
// 5. Eligible entries are `rm -rf`'d unless `dry_run`.

/// Terminal `state` strings the GC treats as "may be reclaimed once
/// retention elapses". Mirrors the classifier above; kept private so the
/// caller can't accidentally widen the set. `working` and unknown
/// states are always preserved.
const TERMINAL_STATES: &[&str] = &[
    "completed",
    "done",
    "stopped",
    "error",
    "failed",
    "crashed",
    "killed",
];

/// Per-directory disposition for the GC report. `Removed` and
/// `WouldRemove` carry the same payload — the only difference is whether
/// `dry_run` was set.
#[derive(Debug, Clone, PartialEq)]
pub enum GcDisposition {
    /// Job dir was `rm -rf`'d (dry_run=false).
    Removed,
    /// Job dir would be removed but dry_run=true.
    WouldRemove,
    /// `state.json::state == "working"` — never touched.
    KeptWorking,
    /// Terminal but inside the retention window.
    KeptRecent,
    /// `state.json` missing or unparseable — preserved + WARN.
    KeptCorrupt,
    /// State is neither terminal nor `working` — preserved
    /// defensively (e.g. forward-compat Claude states we don't know).
    KeptUnknown,
}

/// One row of the GC report.
#[derive(Debug, Clone, PartialEq)]
pub struct GcEntry {
    pub job_id: String,
    pub disposition: GcDisposition,
}

/// Summary of one GC sweep. `dir_count_before` counts the entries
/// directly under `jobs_dir` at scan time (real or simulated removals
/// don't double-count). `removed` matches `entries.iter().filter(|e|
/// e.disposition == Removed || WouldRemove).count()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GcReport {
    /// Number of `<job_id>/` directories observed at scan time.
    pub dir_count_before: usize,
    /// Number of dirs after the sweep. In dry-run mode this equals
    /// `dir_count_before` (we only count what *would* be removed).
    pub dir_count_after: usize,
    /// Count of dirs flagged for removal (or actually removed when
    /// `!dry_run`).
    pub removed: usize,
    /// `dir_count_before - removed - (kept_corrupt + kept_unknown)`
    /// equivalent; preserved separately for human-readable reports.
    pub kept_working: usize,
    pub kept_recent: usize,
    pub kept_corrupt: usize,
    pub kept_unknown: usize,
    /// Whether the sweep was a no-op preview (no fs mutation).
    pub dry_run: bool,
    /// Per-entry detail; ordered as walked (no stable sort — caller
    /// sorts if needed for golden tests).
    pub entries: Vec<GcEntry>,
}

/// Walk `jobs_dir` and reclaim every `<id>/` whose `state.json` reports
/// a terminal state older than `retention_days`. See module-level
/// "V0.4.6 F85" doc for the full ruleset.
///
/// `retention_days == 0` short-circuits the entire sweep — GC is opt-in
/// at every level, so `Config::claude_jobs_retention_days: 0` disables
/// the feature without needing a separate flag.
///
/// Missing `jobs_dir` is not an error (fresh hosts haven't created it
/// yet); the function returns an empty report. Real IO failures
/// (`read_dir`, `remove_dir_all`) bubble.
pub fn gc_terminated_jobs(jobs_dir: &Path, retention_days: u32, dry_run: bool) -> Result<GcReport> {
    let mut report = GcReport {
        dry_run,
        ..GcReport::default()
    };
    if retention_days == 0 {
        // Disabled — return an explicit zero-action report so callers
        // can log "GC disabled" without inferring it from empty entries.
        return Ok(report);
    }
    if !jobs_dir.exists() {
        return Ok(report);
    }

    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);

    let read_dir =
        std::fs::read_dir(jobs_dir).with_context(|| format!("read_dir {}", jobs_dir.display()))?;

    for entry in read_dir {
        let entry = entry.with_context(|| format!("iter {}", jobs_dir.display()))?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "claude_jobs gc: file_type failed; skipping");
                continue;
            }
        };
        if !file_type.is_dir() {
            // `~/.claude/jobs/` is a directory of directories; any
            // stray file (e.g. a leftover `.tmp` or operator artifact)
            // is preserved untouched.
            continue;
        }
        let job_id = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| path.display().to_string());

        report.dir_count_before += 1;

        let disposition = classify_dir_for_gc(&path, cutoff);
        match &disposition {
            GcDisposition::WouldRemove => {
                report.removed += 1;
                // dry_run — don't touch disk; dir_count_after stays
                // equal to before for this entry.
                report.dir_count_after += 1;
            }
            GcDisposition::Removed => {
                // Unreachable here — classify_dir_for_gc never returns
                // Removed; that conversion happens below.
                unreachable!("classify_dir_for_gc never returns Removed");
            }
            GcDisposition::KeptWorking => {
                report.kept_working += 1;
                report.dir_count_after += 1;
            }
            GcDisposition::KeptRecent => {
                report.kept_recent += 1;
                report.dir_count_after += 1;
            }
            GcDisposition::KeptCorrupt => {
                report.kept_corrupt += 1;
                report.dir_count_after += 1;
                tracing::warn!(
                    job_id = %job_id,
                    path = %path.display(),
                    "claude_jobs gc: state.json missing/corrupt; preserving for manual inspection"
                );
            }
            GcDisposition::KeptUnknown => {
                report.kept_unknown += 1;
                report.dir_count_after += 1;
            }
        }

        let final_disposition = match disposition {
            GcDisposition::WouldRemove if !dry_run => {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => {
                        // Successful real remove: subtract back from the
                        // after-count we provisionally bumped.
                        report.dir_count_after -= 1;
                        GcDisposition::Removed
                    }
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            job_id = %job_id,
                            path = %path.display(),
                            "claude_jobs gc: remove_dir_all failed; preserving entry"
                        );
                        // Failed real remove: keep counting it as
                        // present, decrement `removed` because it
                        // wasn't actually reclaimed.
                        report.removed -= 1;
                        report.kept_corrupt += 1;
                        GcDisposition::KeptCorrupt
                    }
                }
            }
            other => other,
        };

        report.entries.push(GcEntry {
            job_id,
            disposition: final_disposition,
        });
    }

    Ok(report)
}

/// Decide whether a `<jobs_dir>/<id>/` should be reclaimed.
///
/// Pure-ish (filesystem reads only; no mutation) so unit tests can
/// drive every branch via a tempdir tree.
fn classify_dir_for_gc(path: &Path, cutoff: chrono::DateTime<chrono::Utc>) -> GcDisposition {
    let state_path = path.join("state.json");
    let raw = match std::fs::read_to_string(&state_path) {
        Ok(s) => s,
        Err(_) => return GcDisposition::KeptCorrupt,
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return GcDisposition::KeptCorrupt,
    };

    let state_str = value
        .get("state")
        .and_then(|s| s.as_str())
        .or_else(|| value.get("status").and_then(|s| s.as_str()))
        .unwrap_or("working");

    if state_str == "working" {
        return GcDisposition::KeptWorking;
    }
    if !TERMINAL_STATES.contains(&state_str) {
        return GcDisposition::KeptUnknown;
    }

    let terminal_at = value
        .get("firstTerminalAt")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let effective_terminal_at = terminal_at.or_else(|| dir_mtime_utc(path));

    match effective_terminal_at {
        Some(t) if t < cutoff => GcDisposition::WouldRemove,
        Some(_) => GcDisposition::KeptRecent,
        // No timestamp at all (corrupt fs metadata) — be conservative.
        None => GcDisposition::KeptCorrupt,
    }
}

fn dir_mtime_utc(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = mtime.into();
    Some(dt)
}

/// Convenience wrapper: GC the user's real `~/.claude/jobs/` (honoring
/// `$CCTEAM_CLAUDE_JOBS_DIR` for tests). Daemon startup + the `ccteam
/// doctor --gc-claude-jobs` CLI path both call this; the lower-level
/// [`gc_terminated_jobs`] is exposed so test code (and any future GC
/// caller wanting a custom path) can target a tempdir directly.
pub fn gc_user_claude_jobs(retention_days: u32, dry_run: bool) -> Result<GcReport> {
    let base = std::env::var_os(ccteam_harness::CLAUDE_JOBS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        });
    gc_terminated_jobs(&base, retention_days, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn probe_returns_killed_for_none_job_id() {
        match probe_job(None) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "killed");
                assert_eq!(cost_usd, 0.0);
            }
            other => panic!("expected killed, got {other:?}"),
        }
    }

    #[test]
    fn probe_state_json_returns_killed_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope/state.json");
        match probe_state_json(&missing) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "killed");
                assert_eq!(cost_usd, 0.0);
            }
            other => panic!("expected killed, got {other:?}"),
        }
    }

    #[test]
    fn probe_state_json_returns_killed_when_unparseable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        std::fs::write(&path, b"{ broken json").unwrap();
        match probe_state_json(&path) {
            JobLiveness::Terminal { status, .. } => assert_eq!(status, "killed"),
            other => panic!("expected killed, got {other:?}"),
        }
    }

    #[test]
    fn classify_running_when_state_working_and_no_first_terminal_at() {
        let v = json!({
            "state": "working",
            "firstTerminalAt": null,
            "cost_usd": 0.42,
        });
        assert_eq!(classify(&v), JobLiveness::Running);
    }

    #[test]
    fn classify_terminal_when_state_done() {
        let v = json!({
            "state": "done",
            "firstTerminalAt": "2026-05-15T12:00:00Z",
            "cost_usd": 1.25,
        });
        match classify(&v) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "completed");
                assert!((cost_usd - 1.25).abs() < 1e-9);
            }
            other => panic!("expected completed, got {other:?}"),
        }
    }

    #[test]
    fn classify_terminal_when_state_failed() {
        let v = json!({
            "state": "failed",
            "cost_usd": 0.10,
        });
        match classify(&v) {
            JobLiveness::Terminal { status, cost_usd } => {
                assert_eq!(status, "error");
                assert!((cost_usd - 0.10).abs() < 1e-9);
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn classify_terminal_via_first_terminal_at_even_when_state_working() {
        // Race window: Claude Code wrote firstTerminalAt but state
        // field hasn't flipped yet. F80 treats this as terminal.
        let v = json!({
            "state": "working",
            "firstTerminalAt": "2026-05-15T12:00:00Z",
        });
        match classify(&v) {
            JobLiveness::Terminal { status, .. } => assert_eq!(status, "completed"),
            other => panic!("expected completed, got {other:?}"),
        }
    }

    #[test]
    fn classify_missing_cost_defaults_to_zero() {
        let v = json!({"state": "done"});
        match classify(&v) {
            JobLiveness::Terminal { cost_usd, .. } => assert_eq!(cost_usd, 0.0),
            other => panic!("expected terminal, got {other:?}"),
        }
    }

    // V0.6.3 F144 — forward-compat regression tests. Anthropic may ship
    // a `claude` CLI that writes a `state.json` with an unknown `state`
    // value and/or extra fields; ccteam must not panic and must NOT
    // mistake the unknown state for "done" (that would strand a phantom
    // job in `progress.jsonl`).

    #[test]
    fn classify_unknown_state_stays_non_terminal() {
        // A future Claude Code state vocabulary value.
        let v = json!({
            "state": "suspended_for_review",
            "firstTerminalAt": null,
            "cost_usd": 0.5,
        });
        assert_eq!(
            classify(&v),
            JobLiveness::Running,
            "unknown job state must stay non-terminal so the orchestrator keeps probing"
        );
    }

    #[test]
    fn classify_unknown_state_with_future_fields_does_not_panic() {
        // Synthetic "future" state.json: unknown state + extra fields
        // ccteam has never seen.
        let v = json!({
            "state": "hibernating",
            "firstTerminalAt": null,
            "cost_usd": 1.0,
            "future_field_a": {"nested": [1, 2, 3]},
            "schema_version": 99,
            "respawnPolicy": "lunar",
        });
        // The assertion is simply "does not panic" + correct degradation.
        assert_eq!(classify(&v), JobLiveness::Running);
    }

    #[test]
    fn classify_unknown_state_via_first_terminal_at_still_terminal() {
        // Even with an unknown `state`, a non-null `firstTerminalAt`
        // remains an authoritative terminal signal (vendor's own
        // end-of-session stamp). Unknown != "ignore every signal".
        let v = json!({
            "state": "winding_down",
            "firstTerminalAt": "2026-05-22T12:00:00Z",
        });
        match classify(&v) {
            JobLiveness::Terminal { status, .. } => assert_eq!(status, "completed"),
            other => panic!("expected terminal via firstTerminalAt, got {other:?}"),
        }
    }
}
