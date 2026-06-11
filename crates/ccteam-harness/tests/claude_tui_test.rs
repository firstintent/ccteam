//! V0.6.0 F108 Wave 2 — integration tests for `ClaudeTuiAdapter`.
//!
//! Uses a fake `claude` binary (shell script) + real `tmux`. Tests are
//! `serial_test::serial` because they touch the shared tmux server.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ccteam_harness::execution::claude_tui::{
    chat_session_name, ensure_chat_hooks_installed, ensure_telegram_plugin_disabled,
    ClaudeTuiAdapter, TELEGRAM_PLUGIN_ID,
};
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    PermissionMode, SpawnCtx, ThreadHandle, TurnInput, CLAUDE_BIN_ENV,
};
use serial_test::serial;

fn fake_claude_script(tmp: &tempfile::TempDir) -> PathBuf {
    let p = tmp.path().join("fake-claude");
    // v0.8.8 F1 — the fresh-spawn path now runs a death/liveness probe
    // (`pane_runs_claude` → `ps -o comm=` must contain "claude"). Sleep
    // WITHOUT `exec` so the live process keeps comm = "fake-claude" (which
    // contains "claude"); `exec sleep` would replace the shell with `sleep`
    // and the probe would (correctly) report the pane as not-a-claude → spawn
    // failure. Mirrors `claude_tui_resume_test.rs`'s `sleep 999` pattern.
    std::fs::write(&p, "#!/bin/sh\nsleep 30\n").unwrap();
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
    // v0.8.8 F1 — the pane name is keyed by the ccteam session sid, not the
    // role (`--agent {role}` still binds the persona).
    let sid = "s1".to_string();
    let session_name = chat_session_name(&slug, &sid);
    kill_session_quiet(&session_name);

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief { role: role.clone() };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: sid.clone(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
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
    // Heartbeat file should have been written. v0.8.8 F1 — keyed by sid (same
    // dimension as turns / cursor / marker), not role.
    let hb = tmp.path().join(".ccteam/chat").join(&sid).join("heartbeat");
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
    // v0.8.8 F1 — pane name is sid-keyed.
    let sid = "s1".to_string();
    let session_name = chat_session_name(&slug, &sid);
    kill_session_quiet(&session_name);
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief { role: role.clone() };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: sid.clone(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
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
    // v0.8.8 F1 — pane name is sid-keyed.
    let sid = "s1".to_string();
    let session_name = chat_session_name(&slug, &sid);
    kill_session_quiet(&session_name);
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief { role: role.clone() };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: sid.clone(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
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
    // v0.8.8 F1 — the second arg is the ccteam session sid (`s<N>`), not the
    // role; the `ccteam-chat-<slug>-<sid>` shape is otherwise unchanged.
    assert_eq!(chat_session_name("dev-foo", "s1"), "ccteam-chat-dev-foo-s1");
}

#[test]
fn ensure_chat_hooks_creates_settings_with_all_events() {
    let tmp = tempfile::TempDir::new().unwrap();
    ensure_chat_hooks_installed(
        tmp.path(),
        "/home/u/.ccteam/hooks/hook.sh",
        PermissionMode::Skip,
    )
    .unwrap();
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

    // v0.8.7 W2 — a SKIP session must NOT install the PermissionRequest hook
    // (the spawn keeps --dangerously-skip-permissions, so the ask-path never
    // fires; an entry here would be dead + confusing).
    assert!(
        hooks.get("PermissionRequest").is_none(),
        "skip session must not carry a PermissionRequest hook: {hooks:?}"
    );
}

#[test]
fn ensure_chat_hooks_hitl_installs_permission_request_hook() {
    // v0.8.7 W2 (DB.2) — a HITL session installs a PermissionRequest hook
    // routing to `{hook_sh} permission-request`, with NO `timeout` field (a
    // long human approval must not be killed by Claude Code's hook budget)
    // and NO `matcher` (all tools reach the ask-path).
    let tmp = tempfile::TempDir::new().unwrap();
    ensure_chat_hooks_installed(
        tmp.path(),
        "/home/u/.ccteam/hooks/hook.sh",
        PermissionMode::Hitl,
    )
    .unwrap();
    let body = std::fs::read_to_string(tmp.path().join(".claude/settings.local.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let pr = v
        .pointer("/hooks/PermissionRequest")
        .and_then(|p| p.as_array())
        .expect("hitl session has a PermissionRequest hook array");
    assert_eq!(pr.len(), 1, "one PermissionRequest entry");
    let entry = &pr[0];
    // No matcher = all tools.
    assert!(
        entry.get("matcher").is_none(),
        "PermissionRequest entry must omit matcher (all tools): {entry:?}"
    );
    let hook0 = entry.pointer("/hooks/0").expect("entry has a hook");
    let cmd = hook0
        .get("command")
        .and_then(|c| c.as_str())
        .expect("hook has a command");
    assert!(
        cmd.ends_with("permission-request") && cmd.contains("hook.sh"),
        "PermissionRequest routes to the permission-request wrapper, got: {cmd}"
    );
    // CRITICAL: no `timeout` — a 600s human approval must not be killed.
    assert!(
        hook0.get("timeout").is_none(),
        "PermissionRequest hook must NOT set a timeout (human approval ~600s): {hook0:?}"
    );
}

#[test]
fn ensure_chat_hooks_skip_removes_stale_permission_request_entry() {
    // v0.8.7 W2 — re-installing as skip after a prior hitl install must REMOVE
    // the PermissionRequest entry so the hook set matches the spawn flag.
    let tmp = tempfile::TempDir::new().unwrap();
    ensure_chat_hooks_installed(
        tmp.path(),
        "/home/u/.ccteam/hooks/hook.sh",
        PermissionMode::Hitl,
    )
    .unwrap();
    ensure_chat_hooks_installed(
        tmp.path(),
        "/home/u/.ccteam/hooks/hook.sh",
        PermissionMode::Skip,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    assert!(
        v.pointer("/hooks/PermissionRequest").is_none(),
        "re-installing as skip must drop the stale PermissionRequest entry"
    );
    // The ordinary chat-progress hooks are still present.
    assert!(v["hooks"]["Stop"].is_array());
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
    ensure_chat_hooks_installed(
        tmp.path(),
        "/home/u/.ccteam/hooks/hook.sh",
        PermissionMode::Skip,
    )
    .unwrap();
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

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn claude_directive_bare_model_passes_through_to_native_picker() {
    // v0.8.10 (commit b18aade) — `/model` no longer offers a ccteam-hardcoded
    // choice list (which drifts from claude's evolving model set); it ALWAYS
    // passes straight through to claude's native picker / "Switch model?"
    // confirmation and returns `Done` with a receipt pointing the user at
    // `/screen`. This needs a live pane because the arm submits a turn.
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_script(&tmp);
    let slug = format!("tui-model-{}", std::process::id());
    let sid = "s1".to_string();
    let session_name = chat_session_name(&slug, &sid);
    kill_session_quiet(&session_name);
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief {
        role: "alice".to_string(),
    };
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: sid.clone(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
    };
    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start");

    let outcome = ClaudeTuiAdapter::new()
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .expect("handle_directive");
    match outcome {
        DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("/screen"),
                "the /model receipt must point the user at /screen, got {receipt:?}"
            );
        }
        other => panic!("bare /model must pass through (Done), got {other:?}"),
    }

    let _ = ClaudeTuiAdapter::new().close_thread(&handle).await;
    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
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

// ── v0.8.11 E2 — Telegram plugin pin-point isolation ────────────────────

#[test]
fn telegram_plugin_disabled_merges_without_clobbering() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let settings = project.join(".claude/settings.local.json");

    // Pre-seed settings with an unrelated key + another enabled plugin.
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(
        &settings,
        serde_json::to_string_pretty(&serde_json::json!({
            "someUserKey": {"keep": "me"},
            "enabledPlugins": {"other@vendor": true},
        }))
        .unwrap(),
    )
    .unwrap();

    ensure_telegram_plugin_disabled(project).unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    // The telegram plugin is pinned false…
    assert_eq!(
        v["enabledPlugins"][TELEGRAM_PLUGIN_ID],
        serde_json::json!(false)
    );
    // …every other plugin + unrelated key is preserved verbatim.
    assert_eq!(v["enabledPlugins"]["other@vendor"], serde_json::json!(true));
    assert_eq!(v["someUserKey"]["keep"], serde_json::json!("me"));

    // Idempotent: a second call is a no-op on the rest.
    ensure_telegram_plugin_disabled(project).unwrap();
    let v2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        v2["enabledPlugins"]["other@vendor"],
        serde_json::json!(true)
    );
    assert_eq!(
        v2["enabledPlugins"][TELEGRAM_PLUGIN_ID],
        serde_json::json!(false)
    );
}

#[test]
fn telegram_plugin_disabled_creates_file_when_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    ensure_telegram_plugin_disabled(tmp.path()).unwrap();
    let settings = tmp.path().join(".claude/settings.local.json");
    assert!(settings.exists());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        v["enabledPlugins"][TELEGRAM_PLUGIN_ID],
        serde_json::json!(false)
    );
}
