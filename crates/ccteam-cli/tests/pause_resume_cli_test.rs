//! CLI surface tests for `ccteam session pause <slug>` / `ccteam session
//! resume <slug>`.
//!
//! These are the documented `ccteam-control` control surface
//! (`skills/ccteam-control/SKILL.md`), mirroring the
//! `mcp__ccteam__workflow_{pause,resume}` tools. Both front doors call
//! the same `ccteam_core::actions::{pause,resume}` body the MCP and web
//! action routes call. We assert the CLI resolves (exit 0) and produces
//! the `actions::pause` / `actions::resume` side effect on
//! `state.json::user_pause_pending` — without depending on a real
//! `claude` binary (pause/resume only rewrite project state).

use std::path::Path;
use std::process::Command;

/// Minimal `state.json` `actions::pause` / `actions::resume` need;
/// serde defaults fill every optional field. The timestamp is a fixed
/// literal — its value is irrelevant to the pause/resume side effect.
fn minimal_state_json(slug: &str) -> String {
    let now = "2026-06-01T00:00:00Z";
    format!(
        r#"{{
  "slug": "{slug}",
  "team": "dev",
  "created_at": "{now}",
  "tmux_session": "ccteam-{slug}",
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 0,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": null,
  "last_user_interaction_at": "{now}",
  "user_attached": false,
  "user_pause_pending": false
}}"#
    )
}

/// Write `projects/<slug>/.ccteam/state.json`. Returns the state path so
/// the test can read back the `user_pause_pending` flag.
fn bootstrap(projects: &Path, slug: &str) -> std::path::PathBuf {
    let ccteam_dir = projects.join(slug).join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir).unwrap();
    let state_path = ccteam_dir.join("state.json");
    std::fs::write(&state_path, minimal_state_json(slug)).unwrap();
    state_path
}

fn pause_pending(state_path: &Path) -> bool {
    let raw = std::fs::read_to_string(state_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v["user_pause_pending"].as_bool().unwrap()
}

/// `ccteam session pause <slug>` exits 0 and invokes `actions::pause`
/// (sets `user_pause_pending=true`); `ccteam session resume <slug>` exits
/// 0 and clears it. Round-tripping both exercises the symmetric front
/// doors.
#[test]
fn pause_then_resume_round_trips_through_actions() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&projects).unwrap();

    let slug = "dev-pauseme";
    let state_path = bootstrap(&projects, slug);
    assert!(!pause_pending(&state_path), "fixture starts unpaused");

    // pause — exit 0 + user_pause_pending flips true.
    let out = Command::new(bin)
        .args(["session", "pause", slug])
        .env("CCTEAM_HOME", &home)
        .env("CCTEAM_PROJECTS_ROOT", &projects)
        .output()
        .expect("spawn ccteam pause");
    assert!(
        out.status.success(),
        "ccteam pause should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        pause_pending(&state_path),
        "ccteam pause must set user_pause_pending=true (actions::pause)",
    );

    // resume — exit 0 + user_pause_pending flips back false.
    let out = Command::new(bin)
        .args(["session", "resume", slug])
        .env("CCTEAM_HOME", &home)
        .env("CCTEAM_PROJECTS_ROOT", &projects)
        .output()
        .expect("spawn ccteam resume");
    assert!(
        out.status.success(),
        "ccteam resume should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !pause_pending(&state_path),
        "ccteam resume must clear user_pause_pending (actions::resume)",
    );
}
