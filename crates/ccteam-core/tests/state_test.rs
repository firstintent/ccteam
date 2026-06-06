//! Integration tests for `ccteam_core::state` — `ProjectState` round-trip,
//! atomic save semantics, and corruption recovery via `.bak`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use tempfile::TempDir;

use ccteam_core::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState, TeamKind};

fn sample_state() -> ProjectState {
    let t0 = Utc.with_ymd_and_hms(2026, 5, 4, 10, 23, 0).unwrap();
    // V0.4.6 F91 — `cost_used_usd` deprecated; we still set it on the
    // struct literal because the field stays for serde compat. The
    // single allow scopes the warning to the construction site.
    #[allow(deprecated)]
    ProjectState {
        slug: "bookmark-mgr-a3f9".into(),
        team: "dev".into(),
        team_kind: TeamKind::Workflow,
        created_at: t0,
        tmux_session: "ccteam-bookmark-mgr-a3f9".into(),
        claude_session_id: Some("abc123-def-456".into()),
        claude_pid: Some(12345),
        phase_state: PhaseState::Idle,
        parallelism: Parallelism::Solo,
        current_phase: "implement".into(),
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
        last_event_type: Some("Stop".into()),
        auto_loop_cycle_count: 0,
        cost_used_usd: 1.23,
        soft_warn_threshold_usd: 20.0,
        hard_kill_threshold_usd: 200.0,
        context_tokens_used: 142_000,
        context_reset_threshold_tokens: 600_000,
        context_reset_count: 0,
        last_progress_event_at: Some(t0),
        last_user_interaction_at: t0,
        user_attached: false,
        user_pause_pending: false,
        detached: false,
        schedule_last_fire: BTreeMap::new(),
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
    // V0.4.6 F91 — field deprecated; allow mutation on this F91-compat test.
    #[allow(deprecated)]
    {
        v2.cost_used_usd = 9.99;
    }
    v2.save(&main).unwrap();

    assert!(
        !tmp.exists(),
        ".tmp must not linger after a successful save"
    );
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
    v2.auto_loop_cycle_count = 2;
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
    // V0.4.6 F91 — field deprecated; allow access on this compat test.
    #[allow(deprecated)]
    {
        v2.cost_used_usd = 7.5;
    }
    v2.save(&main).unwrap();

    std::fs::remove_file(&main).unwrap();

    let loaded = ProjectState::load(&main).unwrap();
    #[allow(deprecated)]
    {
        assert_eq!(loaded.cost_used_usd, sample_state().cost_used_usd);
    }
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

#[test]
fn legacy_state_without_f49_fields_loads_as_workflow_with_empty_sessions() {
    let dir = TempDir::new().unwrap();
    let (main, _, _) = paths(&dir);
    let body = r#"{
        "slug": "legacy",
        "team": "dev",
        "created_at": "2026-05-01T00:00:00Z",
        "tmux_session": "ccteam-legacy",
        "claude_session_id": null,
        "claude_pid": null,
        "phase_state": "idle",
        "current_phase": "",
        "parallelism": "solo",
        "phase_history": [],
        "auto_loop_cycle_count": 0,
        "cost_used_usd": 0.0,
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
    std::fs::write(&main, body).unwrap();

    let loaded = ProjectState::load(&main).unwrap();
    assert_eq!(loaded.team_kind, TeamKind::Workflow);

    loaded.save(&main).unwrap();
    let saved = std::fs::read_to_string(&main).unwrap();
    assert!(!saved.contains("team_kind"));
}
