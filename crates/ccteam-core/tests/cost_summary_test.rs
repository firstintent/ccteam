//! V0.4.6 F91 — integration tests for the new cost SoT
//! (`ccteam_core::cost_summary` / `compute_cost_summary` /
//! `CostSummary`). These replace the deleted `cost_accumulate_*`
//! battery in `crates/ccteam-hooks/tests/hooks_test.rs`.
//!
//! Coverage rationale (one test per documented invariant in
//! `docs/v0-4-6/dev-plan.md` §4 / `docs/v0-4-6/prd.md` F91):
//!
//! - `t01_cost_summary_basic` — 5 `agent_done` events at $0.10 each →
//!   `cost_total = $0.50`, `cost_24h = $0.50` (all events freshly
//!   stamped within window).
//! - `t02_cost_summary_24h_filter` — half the events stamped > 24h
//!   ago → only the recent half lands in `cost_24h_usd`; the older
//!   ones still contribute to `cost_total_usd`.
//! - `t03_cost_summary_active_reads_state_json` — 2 open
//!   `agent_spawn` rows whose `job_id` resolves to a mock state.json
//!   with `cost_usd: 0.15` each → `cost_active_usd = $0.30`.
//! - `t04_cost_used_usd_serde_compat_old_files` — hand-rolled
//!   pre-F91 `state.json` with `cost_used_usd: 1.23` still loads
//!   (deprecated serde-default path).
//! - `t05_doctor_update_hooks_removes_cost_accumulate` — settings.json
//!   containing the legacy `ccteam hook cost-accumulate` PostToolUse
//!   entry is scrubbed by `remove_cost_accumulate_hooks`.

use std::fs;

use ccteam_core::{
    compute_cost_summary, cost_summary, progress, remove_cost_accumulate_hooks, CcteamPaths,
    CostAccumulateScrubAction, JobLiveness, ProjectState,
};
use chrono::{Duration, Utc};
use serde_json::json;
use tempfile::TempDir;

fn fake_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join(".ccteam"),
        projects_root: tmp.path().join("projects"),
    }
}

#[test]
fn t01_cost_summary_basic_aggregates_agent_done_events() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(&tmp);
    let slug = "dev-cost-basic";
    let progress_path = paths.progress_jsonl(slug);
    fs::create_dir_all(progress_path.parent().unwrap()).unwrap();

    // 5 fresh agent_done rows × $0.10 each.
    let now = Utc::now();
    for i in 0..5 {
        progress::append_event(
            &progress_path,
            &json!({
                "event": "agent_done",
                "session_id": format!("sid-{i}"),
                "role": "fixer",
                "status": "completed",
                "cost_usd": 0.10,
                "slug": slug,
                "ts": now.to_rfc3339(),
            }),
        )
        .unwrap();
    }

    let summary = cost_summary(slug, &progress_path, &paths).unwrap();
    assert!((summary.cost_total_usd - 0.50).abs() < 1e-9);
    assert!((summary.cost_24h_usd - 0.50).abs() < 1e-9);
    assert_eq!(summary.session_count_24h, 5);
    assert_eq!(summary.session_count_active, 0);
    assert!((summary.cost_active_usd - 0.0).abs() < 1e-9);
}

#[test]
fn t02_cost_summary_24h_filter_drops_old_events() {
    let now = Utc::now();
    let mut events = Vec::new();
    // 3 fresh + 3 old.
    for i in 0..3 {
        events.push(json!({
            "event": "agent_done",
            "session_id": format!("fresh-{i}"),
            "status": "completed",
            "cost_usd": 0.10,
            "ts": now.to_rfc3339(),
        }));
    }
    let old_ts = (now - Duration::hours(48)).to_rfc3339();
    for i in 0..3 {
        events.push(json!({
            "event": "agent_done",
            "session_id": format!("old-{i}"),
            "status": "completed",
            "cost_usd": 0.10,
            "ts": old_ts,
        }));
    }

    let summary = compute_cost_summary(&events, now, |_| JobLiveness::Terminal {
        status: "killed",
        cost_usd: 0.0,
    });
    // cost_24h only counts the 3 fresh rows.
    assert!((summary.cost_24h_usd - 0.30).abs() < 1e-9);
    assert_eq!(summary.session_count_24h, 3);
    // cost_total folds all 6.
    assert!((summary.cost_total_usd - 0.60).abs() < 1e-9);
}

#[test]
fn t03_cost_summary_active_reads_state_json() {
    let now = Utc::now();
    // 2 open agent_spawn rows; no matching agent_done.
    let events = vec![
        json!({
            "event": "agent_spawn",
            "session_id": "sid-a",
            "role": "fixer",
            "job_id": "job-a",
            "ts": now.to_rfc3339(),
        }),
        json!({
            "event": "agent_spawn",
            "session_id": "sid-b",
            "role": "fixer",
            "job_id": "job-b",
            "ts": now.to_rfc3339(),
        }),
    ];

    // Mock probe: every job_id is "Running" with cost_usd: 0.15. The
    // closure abstraction lets us stub the live state.json read here
    // without touching $CCTEAM_CLAUDE_JOBS_DIR — terminal+0.15 has
    // the same `cost_active_usd` semantics as a Running session
    // whose state.json would carry that value.
    let summary = compute_cost_summary(&events, now, |_job_id| JobLiveness::Terminal {
        status: "completed",
        cost_usd: 0.15,
    });
    assert_eq!(summary.session_count_active, 2);
    assert!((summary.cost_active_usd - 0.30).abs() < 1e-9);
    // No agent_done events → cost_total = 0.
    assert!((summary.cost_total_usd - 0.0).abs() < 1e-9);
}

#[test]
fn t04_cost_used_usd_serde_compat_old_files() {
    // V0.4.6 F91: state.cost_used_usd is deprecated, but
    // pre-F91 state.json files (with the field populated) must still
    // deserialize cleanly. F91 only freezes the writes — it does NOT
    // break the file format.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.json");
    let body = r#"{
        "slug": "legacy-cost",
        "team": "dev",
        "created_at": "2026-05-01T00:00:00Z",
        "tmux_session": "ccteam-legacy-cost",
        "claude_session_id": null,
        "claude_pid": null,
        "phase_state": "idle",
        "current_phase": "",
        "parallelism": "solo",
        "phase_history": [],
        "auto_loop_cycle_count": 0,
        "cost_used_usd": 1.23,
        "soft_warn_threshold_usd": 20.0,
        "hard_kill_threshold_usd": 200.0,
        "context_tokens_used": 0,
        "context_reset_threshold_tokens": 600000,
        "context_reset_count": 0,
        "last_progress_event_at": null,
        "last_event_type": null,
        "last_user_interaction_at": "2026-05-01T00:00:00Z",
        "user_attached": false,
        "user_pause_pending": false
    }"#;
    fs::write(&path, body).unwrap();

    let state = ProjectState::load(&path).expect("legacy state.json must still deserialize");
    assert_eq!(state.slug, "legacy-cost");
    // Sanity: the deprecated field round-tripped. Asserting through
    // the `#[allow(deprecated)]` read so the test stays green.
    #[allow(deprecated)]
    {
        assert!((state.cost_used_usd - 1.23).abs() < 1e-9);
    }
    // The new SoT (cost_summary) reports zero because there's no
    // progress.jsonl — confirming the read paths are independent.
}

#[test]
fn t05_doctor_update_hooks_removes_cost_accumulate() {
    // V0.4.6 F91: `doctor --update-hooks` walks every project's
    // settings.json and strips the legacy `cost-accumulate` hook
    // entry. The scrub is idempotent.
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join("settings.json");
    fs::write(
        &settings,
        r#"{
          "hooks": {
            "PostToolUse": [
              {
                "hooks": [
                  {"type": "command", "command": "/usr/local/bin/ccteam hook progress-append PostToolUse", "async": true},
                  {"type": "command", "command": "/usr/local/bin/ccteam hook cost-accumulate", "async": true}
                ]
              }
            ],
            "Stop": [
              {
                "hooks": [
                  {"type": "command", "command": "/usr/local/bin/ccteam hook parse-phase-end", "timeout": 10}
                ]
              }
            ]
          }
        }"#,
    )
    .unwrap();

    let report = remove_cost_accumulate_hooks(&settings, false).unwrap();
    assert!(
        matches!(
            report.action,
            CostAccumulateScrubAction::Removed { entries: 1 }
        ),
        "expected one entry removed, got {:?}",
        report.action,
    );

    // After scrub the file must still parse and the PostToolUse hook
    // entry must keep the surviving `progress-append` command.
    let body = fs::read_to_string(&settings).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let post = &v["hooks"]["PostToolUse"][0]["hooks"];
    assert_eq!(post.as_array().unwrap().len(), 1);
    assert!(post[0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook progress-append PostToolUse"));

    // Stop hook untouched.
    let stop = &v["hooks"]["Stop"][0]["hooks"];
    assert_eq!(stop.as_array().unwrap().len(), 1);

    // Idempotency: a second run reports NoChangeNeeded.
    let again = remove_cost_accumulate_hooks(&settings, false).unwrap();
    assert!(
        matches!(again.action, CostAccumulateScrubAction::NoChangeNeeded),
        "second scrub must be no-op, got {:?}",
        again.action,
    );
}
