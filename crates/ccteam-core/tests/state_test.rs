//! Integration tests for `ccteam_core::state` — `ProjectState` round-trip,
//! atomic save semantics, and corruption recovery via `.bak`.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use tempfile::TempDir;

use ccteam_core::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};

fn sample_state() -> ProjectState {
    let t0 = Utc.with_ymd_and_hms(2026, 5, 4, 10, 23, 0).unwrap();
    ProjectState {
        slug: "bookmark-mgr-a3f9".into(),
        team: "dev".into(),
        created_at: t0,
        tmux_session: "ccteam-bookmark-mgr-a3f9".into(),
        claude_session_id: Some("abc123-def-456".into()),
        claude_pid: Some(12345),
        phase_state: PhaseState::InFlight,
        current_phase: "implement".into(),
        parallelism: Parallelism::Solo,
        phase_history: vec![
            PhaseHistoryEntry {
                phase: "seed".into(),
                status: "passed".into(),
                duration_s: 90,
                cost_usd: 0.12,
            },
            PhaseHistoryEntry {
                phase: "plan-eng".into(),
                status: "passed".into(),
                duration_s: 60,
                cost_usd: 0.15,
            },
        ],
        fix_cycle_count: 0,
        cost_used_usd: 1.23,
        soft_warn_threshold_usd: 20.0,
        hard_kill_threshold_usd: 200.0,
        context_tokens_used: 142_000,
        context_reset_threshold_tokens: 600_000,
        context_reset_count: 0,
        last_progress_event_at: Some(t0),
        last_event_type: Some("Stop".into()),
        last_user_interaction_at: t0,
        user_attached: false,
        user_pause_pending: false,
    }
}

fn paths(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let main = dir.path().join("state.json");
    let bak = dir.path().join("state.json.bak");
    let tmp = dir.path().join("state.json.tmp");
    (main, bak, tmp)
}

#[test]
fn save_then_load_roundtrip_is_lossless() {
    let dir = TempDir::new().unwrap();
    let (main, _, _) = paths(&dir);

    let state = sample_state();
    state.save(&main).unwrap();

    let loaded = ProjectState::load(&main).unwrap();
    assert_eq!(loaded, state);
}

#[test]
fn save_creates_parent_directories() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("a/b/c/state.json");

    sample_state().save(&nested).unwrap();
    assert!(nested.exists());
}

#[test]
fn second_save_rotates_previous_to_bak_then_writes_new() {
    let dir = TempDir::new().unwrap();
    let (main, bak, tmp) = paths(&dir);

    let v1 = sample_state();
    v1.save(&main).unwrap();
    assert!(!bak.exists(), "first save must not produce a .bak");

    let mut v2 = v1.clone();
    v2.cost_used_usd = 9.99;
    v2.current_phase = "test-run".into();
    v2.save(&main).unwrap();

    assert!(!tmp.exists(), ".tmp must not linger after a successful save");
    let loaded_main = ProjectState::load(&main).unwrap();
    assert_eq!(loaded_main, v2, "main must hold the latest write");

    let loaded_bak = ProjectState::load(&bak).unwrap();
    assert_eq!(loaded_bak, v1, ".bak must hold exactly the prior version");
}

#[test]
fn load_falls_back_to_bak_when_main_is_corrupt() {
    let dir = TempDir::new().unwrap();
    let (main, _, _) = paths(&dir);

    let v1 = sample_state();
    v1.save(&main).unwrap();
    let mut v2 = v1.clone();
    v2.fix_cycle_count = 2;
    v2.save(&main).unwrap();

    std::fs::write(&main, b"{ not valid json").unwrap();

    let loaded = ProjectState::load(&main).unwrap();
    assert_eq!(loaded, v1, "must recover the previous version from .bak");
}

#[test]
fn load_falls_back_to_bak_when_main_missing() {
    let dir = TempDir::new().unwrap();
    let (main, _, _) = paths(&dir);

    sample_state().save(&main).unwrap();
    let mut v2 = sample_state();
    v2.cost_used_usd = 7.5;
    v2.save(&main).unwrap();

    std::fs::remove_file(&main).unwrap();

    let loaded = ProjectState::load(&main).unwrap();
    assert_eq!(loaded.cost_used_usd, sample_state().cost_used_usd);
}

#[test]
fn load_errors_when_main_and_bak_both_absent() {
    let dir = TempDir::new().unwrap();
    let (main, _, _) = paths(&dir);

    let err = ProjectState::load(&main).unwrap_err();
    assert!(
        err.to_string().contains("read"),
        "expected primary read error, got: {err}"
    );
}

#[test]
fn load_rejects_unknown_enum_value() {
    let dir = TempDir::new().unwrap();
    let (main, _, _) = paths(&dir);

    let mut json = serde_json::to_value(sample_state()).unwrap();
    json["parallelism"] = serde_json::Value::String("totally-wrong".into());
    std::fs::write(&main, serde_json::to_vec(&json).unwrap()).unwrap();

    let err = ProjectState::load(&main).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parallelism") || msg.contains("variant"),
        "expected enum-rejection error, got: {msg}"
    );
}
