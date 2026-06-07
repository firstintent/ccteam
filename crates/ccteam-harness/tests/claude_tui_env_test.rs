//! F175 — verify chat-mode tmux spawn injects `CCTEAM_CHAT_ROLE` /
//! `CCTEAM_CHAT_SLUG` into the new session's environment so the Claude
//! Code hook subprocess can derive the bot role correctly.
//!
//! Without these env vars, `derive_role_from_payload` in
//! `ccteam-hooks::chat_progress` falls back to `None`, and every
//! `chat_*` progress event ships with `role=""` — breaking per-bot
//! turns.jsonl routing and the F176 active-session-id marker.
//!
//! The test models on `claude_tui_resume_test.rs`: fake-claude shell
//! script + `CCTEAM_CLAUDE_BIN` redirect + `tmux show-environment` to
//! inspect the session's env table.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ccteam_harness::execution::claude_tui::{chat_session_name, ClaudeTuiAdapter};
use ccteam_harness::{AgentSpecBrief, HarnessAdapter, SpawnCtx, CLAUDE_BIN_ENV};
use serial_test::serial;

fn kill_session_quiet(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();
}

fn fake_claude_sleep_script(tmp: &tempfile::TempDir) -> PathBuf {
    let p = tmp.path().join("fake-claude");
    std::fs::write(&p, "#!/bin/sh\nsleep 999\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// v0.8.8 F1 — the ccteam session sid the spawn must inject as
/// `CCTEAM_CHAT_SID` and use to name the pane (`ccteam-chat-<slug>-<sid>`).
const F175_SID: &str = "s1";

fn make_ctx(slug: &str, tmp: &tempfile::TempDir) -> SpawnCtx {
    SpawnCtx {
        slug: slug.to_string(),
        sid: F175_SID.into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
    }
}

fn show_env(session: &str, key: &str) -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(["show-environment", "-t", session, key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn fresh_spawn_injects_chat_role_and_slug_env() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_sleep_script(&tmp);

    let slug = format!("f175-env-{}", std::process::id());
    let role = "alpha";
    // v0.8.8 F1 — the pane name is keyed by the ccteam session sid, NOT the
    // role (`--agent {role}` still binds the persona). `show_env` must query
    // the sid-named session or it would never find the env table.
    let session_name = chat_session_name(&slug, F175_SID);
    kill_session_quiet(&session_name);

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());
    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);

    ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("fresh spawn must succeed");

    // tmux `show-environment -t <session> KEY` prints `KEY=VAL` to stdout.
    let role_line = show_env(&session_name, "CCTEAM_CHAT_ROLE")
        .expect("CCTEAM_CHAT_ROLE must be set on the tmux session");
    assert_eq!(role_line, format!("CCTEAM_CHAT_ROLE={role}"));

    let slug_line = show_env(&session_name, "CCTEAM_CHAT_SLUG")
        .expect("CCTEAM_CHAT_SLUG must be set on the tmux session");
    assert_eq!(slug_line, format!("CCTEAM_CHAT_SLUG={slug}"));

    // v0.8.8 F1 — the spawn must also inject the ccteam session sid so the
    // hook (PermissionRequest / chat-progress marker) + the in-pane session_*
    // forwarder can learn the EXACT firing session's identity. Without this
    // every downstream sid-keying degrades to (slug, role) inference.
    let sid_line = show_env(&session_name, "CCTEAM_CHAT_SID")
        .expect("CCTEAM_CHAT_SID must be set on the tmux session");
    assert_eq!(sid_line, format!("CCTEAM_CHAT_SID={F175_SID}"));

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}
