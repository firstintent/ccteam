//! V0.4.6 F91 — integration tests for the new cost SoT
//! (`ccteam_core` `cost_summary` / `compute_cost_summary` /
//! `CostSummary`). These replace the deleted `cost_accumulate_*`
//! battery in `crates/ccteam-hooks/tests/hooks_test.rs`.
//!
//! Coverage rationale (one test per documented invariant in
//! `docs/versions/v0-4-6/dev-plan.md` §4 / `docs/versions/v0-4-6/prd.md` F91):
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
//!
//! V0.5.0 F92 — six new tests covering the transcript-jsonl cost path:
//!
//! - `t06_linkScanPath_present_sums_usage` — happy path: state.json has
//!   linkScanPath + respawnFlags --model; transcript has multi-turn
//!   usage; computed cost within ±5% of hand-calculated dollars.
//! - `t07_state_json_field_zero_falls_back_to_transcript` — pure
//!   pricing path through `classify` when state.json's cost field is 0.
//! - `t08_linkScanPath_missing_falls_back_to_state_json` — no
//!   linkScanPath, no sessionId, only state.json::cost_usd_total = 0.42
//!   → cost = 0.42.
//! - `t09_memoize_second_call_no_reread` — second `sum_usage` call on
//!   same fixture file doesn't re-open the file (`file_read_count`).
//! - `t10_multi_model_pricing` — per-1M-token dollar values for
//!   sonnet-4-6 / opus-4-7 / haiku-4-5 within $0.01 of hand-calculated.
//! - `t11_budget_cap_triggers_with_transcript_cost` — transcript-derived
//!   cost flows into `compute_cost_summary().cost_active_usd` so the F84
//!   budget cap path can compare against threshold.

use std::fs;

use ccteam_core::transcript_scanner::{file_read_count, reset_cache_for_tests};
use ccteam_core::{
    compute_cost_summary, cost_summary, estimate_cost, progress, remove_cost_accumulate_hooks,
    session_cost_from_jsonl, CcteamPaths, CostAccumulateScrubAction, JobLiveness, ProjectState,
    Usage, Vendor,
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

// ============================================================
// V0.5.0 F92 — transcript-jsonl cost source tests (t06..t11).
// One test per acceptance bullet in `docs/versions/v0-5-0/prd.md` §F92 §验收.
// ============================================================

/// One turn's token counters: `(input, cache_create, cache_read, output)`.
type TurnUsage = (u64, u64, u64, u64);
/// One transcript row: its canonical `message.model` + that turn's usage.
type ModeledTurn<'a> = (&'a str, TurnUsage);

/// Build a transcript JSONL with `turns` rows on a single canonical model
/// (`message.model = model`); each row carries one `message.usage` block
/// with the supplied counters. The model id is what the per-turn cost
/// scanner now prices against (the deterministic source).
fn write_transcript_model(path: &std::path::Path, model: &str, turns: &[TurnUsage]) {
    let rows: Vec<ModeledTurn> = turns.iter().map(|&t| (model, t)).collect();
    write_transcript_mixed(path, &rows);
}

/// Convenience: a single-model transcript on `claude-sonnet-4-6` (the
/// model most pricing tests assert against).
fn write_transcript(path: &std::path::Path, turns: &[TurnUsage]) {
    write_transcript_model(path, "claude-sonnet-4-6", turns);
}

/// Build a transcript whose turns mix models — each row is
/// `(canonical_model, (input, cache_create, cache_read, output))`. Proves
/// the scanner prices EACH turn by its own `message.model`.
fn write_transcript_mixed(path: &std::path::Path, rows: &[ModeledTurn]) {
    let mut body = String::new();
    for &(model, (input, cache_create, cache_read, output)) in rows {
        let line = json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "model": model,
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": cache_create,
                    "cache_read_input_tokens": cache_read,
                    "output_tokens": output,
                }
            }
        });
        body.push_str(&line.to_string());
        body.push('\n');
    }
    fs::write(path, body).unwrap();
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn t06_link_scan_path_present_sums_usage() {
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("sess.jsonl");
    // Three assistant turns on sonnet-4-6 (input $3 / cache-create $3.75
    // / cache-read $0.30 / output $15 per 1M).
    write_transcript(
        &transcript,
        &[
            (10_000, 0, 0, 5_000),
            (5_000, 1_000, 2_000, 3_000),
            (0, 0, 8_000, 1_000),
        ],
    );

    // Hand-calc expected dollars for sonnet-4-6:
    //   input  = (10_000 + 5_000 + 0)        * 3.00  / 1M = 0.045
    //   create = (0 + 1_000 + 0)             * 3.75  / 1M = 0.00375
    //   read   = (0 + 2_000 + 8_000)         * 0.30  / 1M = 0.003
    //   output = (5_000 + 3_000 + 1_000)     * 15.00 / 1M = 0.135
    //   total  ≈ 0.18675
    let expected = 0.045 + 0.00375 + 0.003 + 0.135;

    let state = json!({
        "linkScanPath": transcript.to_str().unwrap(),
        "respawnFlags": ["--model", "claude-sonnet-4-6"],
        "state": "working",
    });
    let got = session_cost_from_jsonl(&state).unwrap();
    let drift = (got - expected).abs() / expected;
    assert!(
        drift < 0.05,
        "expected ${expected:.6} ± 5%, got ${got:.6} (drift={drift})",
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn t07_state_json_field_zero_falls_back_to_transcript() {
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("sess.jsonl");
    // The canonical per-turn model (transcript `message.model`) is the
    // deterministic cost source — opus-4-7 here ($5/1M input).
    write_transcript_model(&transcript, "claude-opus-4-7", &[(1_000_000, 0, 0, 0)]);
    // input_tokens × $5 / 1M on opus-4-7 = $5.00.
    let state = json!({
        "linkScanPath": transcript.to_str().unwrap(),
        "respawnFlags": ["--model", "claude-opus-4-7"],
        "state": "working",
        // Per F92 PRD: state.json's own cost reads 0 in production.
        "cost_usd_total": 0,
        "firstTerminalAt": "2026-05-15T12:00:00Z",
    });
    let liveness = ccteam_core::classify_job_state(&state);
    let cost = match liveness {
        JobLiveness::Terminal { cost_usd, .. } => cost_usd,
        JobLiveness::Running => panic!("expected terminal verdict from firstTerminalAt"),
    };
    // Within 1 cent of $5.00.
    assert!(
        (cost - 5.0).abs() < 0.01,
        "expected ~$5.00 from transcript fallback, got ${cost}",
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn t08_link_scan_path_missing_falls_back_to_state_json() {
    reset_cache_for_tests();
    ccteam_core::reset_link_scan_warn_for_tests();
    // No linkScanPath, no cwd / sessionId, only state.json's cost field.
    let state = json!({
        "sessionId": "t08-unique-sid",
        "state": "done",
        "cost_usd_total": 0.42,
    });
    let before = ccteam_core::link_scan_warn_count();
    let liveness = ccteam_core::classify_job_state(&state);
    let after = ccteam_core::link_scan_warn_count();
    match liveness {
        JobLiveness::Terminal { cost_usd, status } => {
            assert_eq!(status, "completed");
            assert!(
                (cost_usd - 0.42).abs() < 1e-9,
                "expected 0.42 fallback, got {cost_usd}",
            );
        }
        other => panic!("expected terminal, got {other:?}"),
    }
    assert_eq!(
        after - before,
        1,
        "expected exactly one WARN for the linkScanPath miss; got delta {}",
        after - before,
    );
    // Re-classify with the same session id — WARN dedup must suppress
    // a second emit.
    let liveness2 = ccteam_core::classify_job_state(&state);
    let after2 = ccteam_core::link_scan_warn_count();
    assert!(matches!(liveness2, JobLiveness::Terminal { .. }));
    assert_eq!(
        after2 - after,
        0,
        "second classify of same sid must not re-WARN; got delta {}",
        after2 - after,
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn t09_memoize_second_call_no_reread() {
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("sess.jsonl");
    write_transcript(&transcript, &[(1_000, 0, 0, 500)]);
    let state = json!({
        "linkScanPath": transcript.to_str().unwrap(),
        "respawnFlags": ["--model", "claude-sonnet-4-6"],
    });
    let first = session_cost_from_jsonl(&state).unwrap();
    let reads_after_first = file_read_count();
    assert!(reads_after_first >= 1, "first call must read disk");
    let second = session_cost_from_jsonl(&state).unwrap();
    let reads_after_second = file_read_count();
    assert!(
        (first - second).abs() < 1e-12,
        "memoized result must equal first ({first} vs {second})",
    );
    assert_eq!(
        reads_after_first, reads_after_second,
        "second call must NOT re-read the file (cache miss leaked through)",
    );
}

#[test]
fn t10_multi_model_pricing() {
    // Pure pricing.rs — no IO. Each row = 1M input tokens × per-1M
    // input rate from anthropic.com/pricing on 2026-05-17.
    let one_m_input = Usage {
        input_tokens: 1_000_000,
        ..Default::default()
    };
    assert!(
        (estimate_cost(&one_m_input, Vendor::Claude, "claude-sonnet-4-6").unwrap() - 3.0).abs()
            < 0.01,
        "sonnet-4-6 input != $3 / 1M",
    );
    assert!(
        (estimate_cost(&one_m_input, Vendor::Claude, "claude-opus-4-7").unwrap() - 5.0).abs()
            < 0.01,
        "opus-4-7 input != $5 / 1M",
    );
    assert!(
        (estimate_cost(&one_m_input, Vendor::Claude, "claude-haiku-4-5").unwrap() - 1.0).abs()
            < 0.01,
        "haiku-4-5 input != $1 / 1M",
    );

    // Output side too — opus-4-7 output is $25 / 1M.
    let one_m_output = Usage {
        output_tokens: 1_000_000,
        ..Default::default()
    };
    assert!(
        (estimate_cost(&one_m_output, Vendor::Claude, "claude-opus-4-7").unwrap() - 25.0).abs()
            < 0.01,
        "opus-4-7 output != $25 / 1M",
    );
    // 1M-context suffix passes through.
    assert!(
        (estimate_cost(&one_m_output, Vendor::Claude, "claude-opus-4-7[1m]").unwrap() - 25.0).abs()
            < 0.01,
        "opus-4-7[1m] suffix must resolve to opus-4-7",
    );
}

#[test]
fn per_vendor_model_specific_pricing() {
    // The concrete model id is priced per-model. This test pins the
    // *per-model* rates so a regression that quietly collapses to one
    // rate is caught. Determinism: an unknown / empty model now prices to
    // `None` (exposed) — there is NO silent fallback.
    //
    // Each assertion uses 1M of a single token kind to make the
    // expected dollars trivially readable against the rate sheet.
    let one_m_input = Usage {
        input_tokens: 1_000_000,
        ..Default::default()
    };
    let one_m_output = Usage {
        output_tokens: 1_000_000,
        ..Default::default()
    };

    // Claude: model-specific differences must not collapse.
    let opus_in = estimate_cost(&one_m_input, Vendor::Claude, "claude-opus-4-7").unwrap();
    let haiku_in = estimate_cost(&one_m_input, Vendor::Claude, "claude-haiku-4-5").unwrap();
    assert!(
        opus_in > haiku_in,
        "opus-4-7 input (${opus_in}) should be 5× haiku-4-5 input (${haiku_in})",
    );
    assert!(
        (opus_in - 5.0).abs() < 0.01,
        "opus-4-7 input != $5/1M: ${opus_in}"
    );
    assert!(
        (haiku_in - 1.0).abs() < 0.01,
        "haiku-4-5 input != $1/1M: ${haiku_in}"
    );

    // Codex: o3 vs gpt-4o-mini differ by ~13× on input.
    let o3_in = estimate_cost(&one_m_input, Vendor::Codex, "o3").unwrap();
    let mini_in = estimate_cost(&one_m_input, Vendor::Codex, "gpt-4o-mini").unwrap();
    assert!(
        o3_in > mini_in,
        "o3 input (${o3_in}) > gpt-4o-mini input (${mini_in})",
    );
    assert!((o3_in - 2.0).abs() < 0.01, "o3 input != $2/1M: ${o3_in}");
    assert!(
        (mini_in - 0.15).abs() < 0.01,
        "gpt-4o-mini input != $0.15/1M: ${mini_in}",
    );

    // Empty model string -> NONE (no fallback). The legacy `""` escape
    // hatch is gone: an absent model is exposed, never billed at a rate.
    assert!(
        estimate_cost(&one_m_output, Vendor::Codex, "").is_none(),
        "empty model must price to None (no silent fallback)",
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn t11_budget_cap_triggers_with_transcript_cost() {
    // Drive `compute_cost_summary` end-to-end: one open agent_spawn
    // whose `probe` returns `Terminal { cost_usd: <transcript-derived> }`
    // — that's the same value F84 watchdog reads to compare against
    // `max_cost_usd_per_24h`.
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("sess.jsonl");
    // sonnet-4-6, output_tokens = 14_000 → 14_000 × $15 / 1M = $0.21.
    write_transcript(&transcript, &[(0, 0, 0, 14_000)]);
    let state = json!({
        "linkScanPath": transcript.to_str().unwrap(),
        "respawnFlags": ["--model", "claude-sonnet-4-6"],
        "state": "working",
    });
    // The Running closure path in compute_cost_summary reads the
    // state.json via job_state_path(job_id) on the host; in tests we
    // route through the `probe` closure with the same transcript-derived
    // cost — that's how `claude_job::resolve_cost_usd` would yield it on
    // a fresh state.json::cost_usd_total == 0.
    let transcript_cost = session_cost_from_jsonl(&state).expect("transcript cost");
    let threshold = 0.10_f64;

    let now = Utc::now();
    let events = vec![json!({
        "event": "agent_spawn",
        "session_id": "sid-budget",
        "role": "fixer",
        "job_id": "job-budget",
        "ts": now.to_rfc3339(),
    })];
    let summary = compute_cost_summary(&events, now, |_job_id| JobLiveness::Terminal {
        status: "completed",
        cost_usd: transcript_cost,
    });
    // F84 watchdog compares `cost_total_usd` (running 24h budget) and
    // `cost_active_usd`. With one open spawn, the live cost lands in
    // `cost_active_usd`; the budget cap kicks at that level.
    assert!(
        summary.cost_active_usd > threshold,
        "expected cost_active_usd > ${threshold}, got ${}",
        summary.cost_active_usd,
    );
    // Hand-check: $14_000 × $15 / 1M = $0.21 ± 2¢ (drift for f64 ops).
    assert!(
        (summary.cost_active_usd - 0.21).abs() < 0.02,
        "expected ~$0.21 from transcript flow, got ${}",
        summary.cost_active_usd,
    );
}

// ============================================================
// COST DETERMINISM — per-turn canonical pricing + no fallback.
// ============================================================

#[test]
#[serial_test::serial(cost_summary_cache)]
fn per_turn_pricing_uses_each_messages_own_canonical_model() {
    // The deterministic win: ONE transcript that mixes models (the real
    // box does — opus for the user's turns, sonnet for sub-task/title
    // turns). Summing usage then pricing once would be wrong; we price
    // EACH turn by its OWN message.model.
    //   turn 1: opus-4-8,   1M output → $25
    //   turn 2: sonnet-4-6, 1M output → $15
    //   total = $40 (NOT 2× one model's rate).
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("mixed.jsonl");
    write_transcript_mixed(
        &transcript,
        &[
            ("claude-opus-4-8", (0, 0, 0, 1_000_000)),
            ("claude-sonnet-4-6", (0, 0, 0, 1_000_000)),
        ],
    );
    let state = json!({ "linkScanPath": transcript.to_str().unwrap() });
    let got = session_cost_from_jsonl(&state).expect("mixed-model transcript prices");
    assert!(
        (got - 40.0).abs() < 0.01,
        "per-turn canonical pricing: opus $25 + sonnet $15 = $40, got ${got}",
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn unpriced_turns_are_skipped_not_billed_at_a_fallback() {
    // A transcript mixing a real model with `<synthetic>` (not in the
    // table). The synthetic turn contributes NOTHING — it is exposed, not
    // billed at the real model's rate.
    //   turn 1: opus-4-8,    1M output → $25
    //   turn 2: <synthetic>, 1M output → unpriced (skipped)
    //   total = $25 (the synthetic 1M output does NOT add another $25).
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("synthetic.jsonl");
    write_transcript_mixed(
        &transcript,
        &[
            ("claude-opus-4-8", (0, 0, 0, 1_000_000)),
            ("<synthetic>", (0, 0, 0, 1_000_000)),
        ],
    );
    let state = json!({ "linkScanPath": transcript.to_str().unwrap() });
    let got = session_cost_from_jsonl(&state).expect("the priced turn yields a cost");
    assert!(
        (got - 25.0).abs() < 0.01,
        "only the opus turn prices ($25); <synthetic> must NOT add a fallback $25, got ${got}",
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn transcript_with_no_priceable_turn_is_none() {
    // Every turn is an unknown model → genuinely unknown cost → None
    // (rendered "—" by the UI), never a fabricated 0.0.
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("all-synthetic.jsonl");
    write_transcript_mixed(
        &transcript,
        &[
            ("<synthetic>", (0, 0, 0, 1_000_000)),
            ("model-not-in-table", (1_000_000, 0, 0, 0)),
        ],
    );
    let state = json!({ "linkScanPath": transcript.to_str().unwrap() });
    assert!(
        session_cost_from_jsonl(&state).is_none(),
        "zero priceable turns must be None (unknown), not 0.0",
    );
}

#[test]
#[serial_test::serial(cost_summary_cache)]
fn transcript_without_model_field_is_unpriced() {
    // A legacy transcript whose assistant turns carry usage but NO
    // message.model → unpriceable (the deterministic source is absent).
    // Honest: None, not a guessed price.
    reset_cache_for_tests();
    let tmp = TempDir::new().unwrap();
    let transcript = tmp.path().join("no-model.jsonl");
    // Hand-write rows with usage but no `model` key.
    let mut body = String::new();
    for _ in 0..2 {
        body.push_str(
            &json!({
                "type": "assistant",
                "message": { "role": "assistant", "usage": { "output_tokens": 1_000_000 } }
            })
            .to_string(),
        );
        body.push('\n');
    }
    fs::write(&transcript, body).unwrap();
    let state = json!({ "linkScanPath": transcript.to_str().unwrap() });
    assert!(
        session_cost_from_jsonl(&state).is_none(),
        "no message.model anywhere ⇒ unpriced ⇒ None",
    );
}
