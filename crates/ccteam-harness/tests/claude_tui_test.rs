//! V0.6.0 F108 Wave 2 — integration tests for `ClaudeTuiAdapter`.
//!
//! Uses a fake `claude` binary (shell script) + real `tmux`. Tests are
//! `serial_test::serial` because they touch the shared tmux server.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ccteam_harness::execution::claude_tui::{
    chat_session_name, ensure_chat_hooks_installed, ClaudeTuiAdapter,
};
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    SpawnCtx, ThreadHandle, TurnInput, CLAUDE_BIN_ENV,
};
use serial_test::serial;

fn fake_claude_script(tmp: &tempfile::TempDir) -> PathBuf {
    let p = tmp.path().join("fake-claude");
    std::fs::write(&p, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn kill_session_quiet(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn start_thread_spawns_tmux_and_returns_handle() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_script(&tmp);
    // Use unique slug so parallel runs don't collide
    let slug = format!("tui-start-{}", std::process::id());
    let role = "alice".to_string();
    let session_name = chat_session_name(&slug, &role);
    kill_session_quiet(&session_name);

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief { role: role.clone() };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: "chat-1".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
    };
    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread should succeed with fake claude + real tmux");
    assert_eq!(handle.vendor, AgentVendor::Claude);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    assert_eq!(handle.identity, session_name);
    assert_eq!(
        handle.raw_extras.get("role").and_then(|v| v.as_str()),
        Some(role.as_str())
    );
    // Heartbeat file should have been written.
    let hb = tmp
        .path()
        .join(".ccteam/chat")
        .join(&role)
        .join("heartbeat");
    assert!(hb.exists(), "heartbeat file should be created");

    // Cleanup.
    let _ = ClaudeTuiAdapter::new().close_thread(&handle).await;
    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn submit_turn_sends_literal_text_to_tmux_pane() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_script(&tmp);
    let slug = format!("tui-submit-{}", std::process::id());
    let role = "bob".to_string();
    let session_name = chat_session_name(&slug, &role);
    kill_session_quiet(&session_name);
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief { role: role.clone() };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: "chat-1".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
    };
    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start");

    let t1 = ClaudeTuiAdapter::new()
        .submit_turn(&handle, TurnInput::UserText("hello world".into()))
        .await
        .expect("submit UserText");
    assert!(t1.0.starts_with("turn-"));

    let outcome = ClaudeTuiAdapter::new()
        .handle_directive(
            &handle,
            Directive {
                name: "compact".to_string(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .expect("handle_directive");
    // Claude passes slash commands straight through as a turn.
    let t2 = match outcome {
        DirectiveOutcome::Turn(id) => id,
        other => panic!("expected DirectiveOutcome::Turn, got {other:?}"),
    };
    assert!(t2.0.starts_with("turn-"));
    assert_ne!(t1.0, t2.0, "turn ids must be unique per submit");

    let _ = ClaudeTuiAdapter::new().close_thread(&handle).await;
    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn submit_turn_artifact_uses_read_protocol() {
    // Artifact path must trigger the "Look at the file I just placed at
    // <p>" sentinel — Wave 2 design sidesteps stdin escape entirely.
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_script(&tmp);
    let slug = format!("tui-artifact-{}", std::process::id());
    let role = "carol".to_string();
    let session_name = chat_session_name(&slug, &role);
    kill_session_quiet(&session_name);
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief { role: role.clone() };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: "chat-1".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
    };
    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .unwrap();

    let art = tmp.path().join("art.txt");
    std::fs::write(&art, b"hello").unwrap();
    let r = ClaudeTuiAdapter::new()
        .submit_turn(&handle, TurnInput::Artifact(art))
        .await;
    assert!(r.is_ok());

    let _ = ClaudeTuiAdapter::new().close_thread(&handle).await;
    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn close_thread_is_idempotent_on_missing_session() {
    // Closing a handle whose tmux session is already gone must not error.
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: "ccteam-chat-ghost-xyz".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    let r = ClaudeTuiAdapter::new().close_thread(&h).await;
    assert!(r.is_ok());
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn resume_thread_on_live_session_returns_handle() {
    // Pre-create a tmux session by hand, then resume it.
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    let tmp = tempfile::TempDir::new().unwrap();
    let session_name = format!("ccteam-chat-resume-{}", std::process::id());
    kill_session_quiet(&session_name);
    let _ = std::process::Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-c",
            tmp.path().to_str().unwrap(),
            "sleep",
            "30",
        ])
        .output()
        .expect("tmux new-session");

    let handle = ClaudeTuiAdapter::new()
        .resume_thread(&session_name)
        .await
        .expect("resume on live session");
    assert_eq!(handle.identity, session_name);
    assert_eq!(handle.mode, ExecutionMode::Chat);

    kill_session_quiet(&session_name);
}

#[test]
fn chat_session_name_format_is_stable() {
    assert_eq!(
        chat_session_name("dev-foo", "alice"),
        "ccteam-chat-dev-foo-alice"
    );
}

#[test]
fn ensure_chat_hooks_creates_settings_with_all_events() {
    let tmp = tempfile::TempDir::new().unwrap();
    ensure_chat_hooks_installed(tmp.path(), "/home/u/.ccteam/hooks/hook.sh").unwrap();
    // v0.8.6 W2b — ccteam writes its managed hooks to the local settings
    // layer (settings.local.json), never the user-committed settings.json.
    let body = std::fs::read_to_string(tmp.path().join(".claude/settings.local.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let hooks = v.get("hooks").expect("hooks key");
    for ev in [
        "SessionStart",
        "UserPromptSubmit",
        "Stop",
        "SubagentStop",
        "PostToolUse",
        "SessionEnd",
        "PreCompact",
        "PostCompact",
    ] {
        assert!(hooks.get(ev).is_some(), "missing hook for {ev}");
    }
    // Hook command must invoke `ccteam internal hook chat-progress ...`.
    let s = body.as_str();
    assert!(s.contains("chat-progress session-start"));
    assert!(s.contains("chat-progress stop"));

    // v0.8.5 D6 — PreToolUse must carry BOTH the `"*"` chat-progress entry and
    // a SECOND `AskUserQuestion` matcher routing to the intercept-ask hook.
    let pre = hooks
        .get("PreToolUse")
        .and_then(|v| v.as_array())
        .expect("PreToolUse is an array");
    assert!(
        pre.iter()
            .any(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("*")),
        "PreToolUse keeps the chat-progress `*` entry: {pre:?}"
    );
    let ask = pre
        .iter()
        .find(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("AskUserQuestion"))
        .expect("PreToolUse has an AskUserQuestion matcher (D6)");
    let cmd = ask
        .pointer("/hooks/0/command")
        .and_then(|c| c.as_str())
        .expect("AskUserQuestion entry has a command");
    assert!(
        cmd.ends_with("intercept-ask") && cmd.contains("hook.sh"),
        "AskUserQuestion routes to the intercept-ask wrapper, got: {cmd}"
    );
}

#[test]
fn ensure_chat_hooks_preserves_unrelated_top_level_keys() {
    // v0.8.6 W2b — the installer merges into settings.local.json, so the
    // pre-existing keys (and the merge result) live in the local layer.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
    std::fs::write(
        tmp.path().join(".claude/settings.local.json"),
        r#"{"someOtherKey": {"x": 1}}"#,
    )
    .unwrap();
    ensure_chat_hooks_installed(tmp.path(), "/home/u/.ccteam/hooks/hook.sh").unwrap();
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["someOtherKey"]["x"], 1);
    assert!(v["hooks"]["Stop"].is_array());
}

#[test]
fn adapter_metadata_advertises_claude_tui() {
    let a = ClaudeTuiAdapter::new();
    assert_eq!(a.name(), "claude-tui");
    assert_eq!(a.vendor(), AgentVendor::Claude);
}

// ── D5 four-channel gate classification (NeedsChoice / Rejected return
// before any backend call, so these need no tmux). ─────────────────────

fn dummy_chat_handle() -> ThreadHandle {
    ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: "ccteam-chat-demo-bot".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    }
}

#[tokio::test]
async fn claude_directive_bare_model_offers_choice() {
    // D5: a bare arg-applicable popup must NeedsChoice (never blind-send).
    let outcome = ClaudeTuiAdapter::new()
        .handle_directive(
            &dummy_chat_handle(),
            Directive {
                name: "model".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .expect("handle_directive");
    match outcome {
        DirectiveOutcome::NeedsChoice(p) => {
            assert!(
                p.token.starts_with("cj") && !p.token.contains(':') && p.token.len() <= 16,
                "token must be a unique, colon-free, ≤16B id: {:?}",
                p.token
            );
            assert!(!p.multi);
            let ids: Vec<&str> = p.options.iter().map(|o| o.id.as_str()).collect();
            assert!(
                ids.contains(&"opus") && ids.contains(&"sonnet"),
                "model aliases expected, got {ids:?}"
            );
        }
        other => panic!("bare /model must NeedsChoice, got {other:?}"),
    }
}

#[tokio::test]
async fn claude_directive_bare_effort_offers_choice() {
    let outcome = ClaudeTuiAdapter::new()
        .handle_directive(
            &dummy_chat_handle(),
            Directive {
                name: "effort".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .expect("handle_directive");
    match outcome {
        DirectiveOutcome::NeedsChoice(p) => {
            let ids: Vec<&str> = p.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["low", "medium", "high"]);
        }
        other => panic!("bare /effort must NeedsChoice, got {other:?}"),
    }
}

#[tokio::test]
async fn claude_directive_panel_command_rejected() {
    // D5: panel-only popups have no chat-drivable arg form → Rejected
    // (never blind-send a bare popup that would stick the modal).
    for name in ["config", "agents", "permissions"] {
        let outcome = ClaudeTuiAdapter::new()
            .handle_directive(
                &dummy_chat_handle(),
                Directive {
                    name: name.into(),
                    args: String::new(),
                    choice: None,
                },
            )
            .await
            .expect("handle_directive");
        assert!(
            matches!(outcome, DirectiveOutcome::Rejected { .. }),
            "/{name} (panel popup) must Rejected, got {outcome:?}"
        );
    }
}
