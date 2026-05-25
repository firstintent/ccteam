//! V0.6.0 F108 — chat-progress hook handler tests.
//!
//! Tests `ccteam internal hook chat-progress <event>` plumbing without
//! actually shelling out to ccteam. Each test fakes a stdin payload,
//! invokes [`ccteam_hooks::handle_chat_progress`] directly, and
//! asserts the resulting progress.jsonl emission shape.

use std::path::Path;

use ccteam_core::{CcteamPaths, ProjectState};
use ccteam_hooks::handle_chat_progress;
use serde_json::{json, Value};
use serial_test::serial;
use tempfile::TempDir;

/// Bootstrap a hermetic `CcteamPaths` rooted at `tmp` + register a
/// project for `cwd` so `session_context_from_cwd` resolves.
fn setup_project(tmp: &TempDir, slug: &str) -> (CcteamPaths, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let ccteam = home.join(".ccteam");
    std::fs::create_dir_all(&ccteam).unwrap();
    std::env::set_var("CCTEAM_HOME", &ccteam);
    let paths = CcteamPaths::from_env().unwrap();
    let projects_root = tmp.path().join("projects");
    let project_dir = projects_root.join(slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    // `session_context_from_cwd` walks up looking for `.ccteam/state.json`.
    let state = ProjectState::initial(slug.to_string());
    std::fs::write(
        project_dir.join(".ccteam/state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
    (paths, project_dir)
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    if !path.exists() {
        return vec![];
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
#[serial]
fn session_start_emits_chat_session_started() {
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-start";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "alice");

    let stdin = json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-abc",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": project_dir.to_string_lossy(),
        "source": "startup",
    });
    handle_chat_progress(&paths, "session-start", &stdin).unwrap();

    let progress = paths.progress_jsonl_for_context(
        &ccteam_core::session_context_from_cwd(&project_dir, &paths).unwrap(),
    );
    let rows = read_jsonl(&progress);
    assert!(!rows.is_empty());
    assert_eq!(rows.last().unwrap()["event"], "chat_session_started");
    assert_eq!(rows.last().unwrap()["role"], "alice");
    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn user_prompt_emits_truncated_excerpt() {
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-prompt";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "bob");

    let long = "x".repeat(1000);
    let stdin = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-2",
        "cwd": project_dir.to_string_lossy(),
        "prompt": long,
    });
    handle_chat_progress(&paths, "user-prompt", &stdin).unwrap();

    let progress = paths.progress_jsonl_for_context(
        &ccteam_core::session_context_from_cwd(&project_dir, &paths).unwrap(),
    );
    let rows = read_jsonl(&progress);
    let row = rows.last().unwrap();
    assert_eq!(row["event"], "chat_turn_user_prompt");
    assert_eq!(row["role"], "bob");
    assert_eq!(row["turn_id"], "sess-2");
    // Excerpt should be truncated to 256 chars.
    assert_eq!(row["prompt_excerpt"].as_str().unwrap().chars().count(), 256);
    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn stop_emits_chat_turn_completed_with_usage() {
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-stop";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "carol");

    let stdin = json!({
        "hook_event_name": "Stop",
        "session_id": "sess-3",
        "cwd": project_dir.to_string_lossy(),
        "last_assistant_message": "all done",
    });
    handle_chat_progress(&paths, "stop", &stdin).unwrap();

    let progress = paths.progress_jsonl_for_context(
        &ccteam_core::session_context_from_cwd(&project_dir, &paths).unwrap(),
    );
    let rows = read_jsonl(&progress);
    let row = rows.last().unwrap();
    assert_eq!(row["event"], "chat_turn_completed");
    assert!(row["usage"].is_object());
    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn session_end_with_clear_reason_emits_chat_session_reset() {
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-reset";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "dora");

    let stdin = json!({
        "hook_event_name": "SessionEnd",
        "session_id": "sess-4",
        "cwd": project_dir.to_string_lossy(),
        "reason": "clear",
    });
    handle_chat_progress(&paths, "session-end", &stdin).unwrap();

    let progress = paths.progress_jsonl_for_context(
        &ccteam_core::session_context_from_cwd(&project_dir, &paths).unwrap(),
    );
    let rows = read_jsonl(&progress);
    assert_eq!(rows.last().unwrap()["event"], "chat_session_reset");
    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn session_start_writes_active_session_id_marker() {
    // F176 — the hook persists Anthropic's real session_id (carried
    // in stdin) to `<project>/.ccteam/chat/<role>/active-session-id`
    // so the chat-mode tail loop can target the correct jsonl
    // deterministically. This is the only reliable source for the sid
    // because `--name` is just an internal label, not the filename.
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-marker-write";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "alice");

    let sid = "11111111-2222-3333-4444-555555555555";
    let stdin = json!({
        "hook_event_name": "SessionStart",
        "session_id": sid,
        "cwd": project_dir.to_string_lossy(),
        "source": "startup",
    });
    handle_chat_progress(&paths, "session-start", &stdin).unwrap();

    let marker = project_dir.join(".ccteam/chat/alice/active-session-id");
    assert!(
        marker.exists(),
        "active-session-id marker must be written on session-start; expected at {}",
        marker.display()
    );
    let body = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(body, sid, "marker body must be the raw sid (no JSON wrap)");

    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn session_start_overwrites_marker_after_rotation() {
    // /clear emits SessionEnd(reason=clear) then SessionStart with a
    // fresh sid. The marker must point at the NEW sid; the old one
    // shouldn't survive the rotation.
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-marker-rotate";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "bob");

    let sid_old = "aaaa-old";
    handle_chat_progress(
        &paths,
        "session-start",
        &json!({
            "hook_event_name": "SessionStart",
            "session_id": sid_old,
            "cwd": project_dir.to_string_lossy(),
            "source": "startup",
        }),
    )
    .unwrap();

    let sid_new = "bbbb-new";
    handle_chat_progress(
        &paths,
        "session-start",
        &json!({
            "hook_event_name": "SessionStart",
            "session_id": sid_new,
            "cwd": project_dir.to_string_lossy(),
            "source": "clear",
        }),
    )
    .unwrap();

    let marker = project_dir.join(".ccteam/chat/bob/active-session-id");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), sid_new);

    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn session_end_clear_removes_active_session_id_marker() {
    // SessionEnd { reason: "clear" } means the user invoked /clear —
    // the sid is about to rotate. Drop the marker so the tail loop
    // waits for the next SessionStart instead of pointing at a now-
    // stale jsonl.
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-marker-clear";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "carol");

    // Plant the marker first.
    let sid = "doomed-sid";
    handle_chat_progress(
        &paths,
        "session-start",
        &json!({
            "hook_event_name": "SessionStart",
            "session_id": sid,
            "cwd": project_dir.to_string_lossy(),
            "source": "startup",
        }),
    )
    .unwrap();
    let marker = project_dir.join(".ccteam/chat/carol/active-session-id");
    assert!(marker.exists());

    // Now signal /clear.
    handle_chat_progress(
        &paths,
        "session-end",
        &json!({
            "hook_event_name": "SessionEnd",
            "session_id": sid,
            "cwd": project_dir.to_string_lossy(),
            "reason": "clear",
        }),
    )
    .unwrap();
    assert!(
        !marker.exists(),
        "session-end (clear) must remove the marker; still present at {}",
        marker.display()
    );

    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn session_end_non_clear_leaves_marker_intact() {
    // Process exit / daemon kill / network drop send SessionEnd with
    // a non-`clear` reason. The sid hasn't rotated; the marker must
    // survive so a daemon restart can pick up where it left off.
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-marker-exit";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "dora");

    let sid = "surviving-sid";
    handle_chat_progress(
        &paths,
        "session-start",
        &json!({
            "hook_event_name": "SessionStart",
            "session_id": sid,
            "cwd": project_dir.to_string_lossy(),
            "source": "startup",
        }),
    )
    .unwrap();
    let marker = project_dir.join(".ccteam/chat/dora/active-session-id");
    assert!(marker.exists());

    handle_chat_progress(
        &paths,
        "session-end",
        &json!({
            "hook_event_name": "SessionEnd",
            "session_id": sid,
            "cwd": project_dir.to_string_lossy(),
            "reason": "exit",
        }),
    )
    .unwrap();
    assert!(
        marker.exists(),
        "session-end with reason != 'clear' must NOT remove the marker"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), sid);

    std::env::remove_var("CCTEAM_CHAT_ROLE");
}

#[test]
#[serial]
fn unknown_event_arg_falls_back_to_chat_prefix() {
    let tmp = TempDir::new().unwrap();
    let slug = "tcp-future";
    let (paths, project_dir) = setup_project(&tmp, slug);
    std::env::set_var("CCTEAM_CHAT_ROLE", "ed");

    let stdin = json!({
        "hook_event_name": "FuturisticHook",
        "session_id": "sess-5",
        "cwd": project_dir.to_string_lossy(),
    });
    handle_chat_progress(&paths, "future-event", &stdin).unwrap();

    let progress = paths.progress_jsonl_for_context(
        &ccteam_core::session_context_from_cwd(&project_dir, &paths).unwrap(),
    );
    let rows = read_jsonl(&progress);
    assert_eq!(rows.last().unwrap()["event"], "chat_future_event");
    std::env::remove_var("CCTEAM_CHAT_ROLE");
}
