//! V0.6.0 F107 — HarnessAdapter trait migration tests (Option C).
//!
//! Replaces the V0.4.0 `harness_thin_test.rs` + `codex_adapter_test.rs`
//! (which exercised the old 4-method `spawn_session` / `shutdown_session`
//! / `ingest_snapshot` / `subagent_states` surface that V0.6.0 F107
//! retires). New coverage:
//!
//! 1. trait signature compile-time guarantee (each impl satisfies all
//!    5 async methods + `name` + `vendor`).
//! 2. `ClaudeBgAdapter::start_thread` → `close_thread` round-trip
//!    behavioural parity with the V0.5.1 `claude --bg` path.
//! 3. `CodexExecAdapter::start_thread` creates a real tmux session
//!    (gated behind `codex-tests` feature + tmux on PATH; same gating
//!    convention as V0.4.0 `codex_adapter_test.rs`).
//! 4. `ClaudeTuiAdapter` all 5 async methods return
//!    `HarnessError::NotImplemented` with a Wave-2 reason string.
//! 5. `AgentVendor` serde round-trip.
//! 6. `ThreadHandle.identity` carries job_id (bg) / tmux session name
//!    (codex) per adapter contract.
//! 7. `SessionHandle::from_thread_handle` translation parity for both
//!    vendors (orchestrator boundary correctness).

use ccteam_harness::{
    execution::{ClaudeTuiAdapter, CodexAppServerAdapter, CodexExecAdapter},
    parse_backgrounded_short_id, AgentSpecBrief, AgentVendor, ClaudeBgAdapter, ExecutionMode,
    HarnessAdapter, HarnessError, SessionHandle, SpawnCtx, ThreadHandle, CLAUDE_BIN_ENV,
    CLAUDE_JOBS_DIR_ENV, CODEX_BIN_ENV,
};

// =====================================================================
// 1. trait signature compile-time guarantee
// =====================================================================

#[test]
fn trait_signature_compile_time_guarantee() {
    // If this compiles, every adapter satisfies the full trait contract:
    // 5 async lifecycle + name + vendor (v0.6) PLUS the v0.8.5 no-default
    // `handle_directive` + `thread_status`. A new vendor that forgets
    // either fails to compile here — the anti-silent-downgrade lock.
    fn assert_trait_impl<T: HarnessAdapter>() {}
    assert_trait_impl::<ClaudeBgAdapter>();
    assert_trait_impl::<ClaudeTuiAdapter>();
    assert_trait_impl::<CodexExecAdapter>();
    assert_trait_impl::<CodexAppServerAdapter>();
}

#[test]
fn adapter_names_are_stable_strings() {
    assert_eq!(ClaudeBgAdapter::new().name(), "claude-bg");
    assert_eq!(ClaudeTuiAdapter::new().name(), "claude-tui");
    assert_eq!(CodexExecAdapter::new().name(), "codex-exec");
}

#[test]
fn adapter_vendor_routing() {
    assert_eq!(ClaudeBgAdapter::new().vendor(), AgentVendor::Claude);
    assert_eq!(ClaudeTuiAdapter::new().vendor(), AgentVendor::Claude);
    assert_eq!(CodexExecAdapter::new().vendor(), AgentVendor::Codex);
}

// =====================================================================
// 2. ClaudeBgAdapter start_thread → close_thread parity with V0.5.1
// =====================================================================

/// Drop guard that restores a `$ENV` var to its pre-test value when
/// dropped. Same convention V0.4.0 `harness_thin_test.rs` used so
/// `serial_test` isn't required to share these env keys across tests.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

// `#[serial]` because this asserts the DEFAULT (`claude --bg`) path,
// which requires `CCTEAM_CLAUDE_BG_VIA_MUX` UNSET. The via_mux tests
// below set that env var process-globally; without serialization a
// concurrent via_mux test flips this test onto the foreground-in-mux
// path and the `tmux_session`/`identity` asserts fail intermittently.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn claude_bg_start_thread_parses_backgrounded_marker() {
    // Build a fake `claude` script that prints the `backgrounded ·
    // <id>` marker on stdout — same fixture shape V0.4.0 F61 used.
    let tmp = tempfile::tempdir().unwrap();
    let fake_claude = tmp.path().join("fake-claude");
    std::fs::write(
        &fake_claude,
        "#!/bin/sh\necho 'backgrounded · deadbeef'\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let _bin = EnvGuard::set(CLAUDE_BIN_ENV, fake_claude.to_str().unwrap());

    let brief = AgentSpecBrief {
        role: "tester".into(),
    };
    let ctx = SpawnCtx {
        mode: None,
        slug: "demo".into(),
        sid: "claude-1".into(),
        owner: "user:web-api".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec!["initial prompt".into()],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    };

    let handle = ClaudeBgAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread succeeds with fake claude on PATH");
    assert_eq!(handle.vendor, AgentVendor::Claude);
    assert_eq!(handle.mode, ExecutionMode::Bg);
    assert_eq!(handle.identity, "deadbeef");
    assert_eq!(
        handle
            .raw_extras
            .get("tmux_session")
            .and_then(|v| v.as_str()),
        Some("ccteam-demo-claude-1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_bg_start_thread_rejects_empty_role() {
    let brief = AgentSpecBrief {
        role: String::new(),
    };
    let ctx = SpawnCtx {
        mode: None,
        slug: "demo".into(),
        sid: "claude-1".into(),
        owner: "user:web-api".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    };
    let err = ClaudeBgAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect_err("empty role must error");
    match err {
        HarnessError::SpawnFailed(msg) => assert!(msg.contains("non-empty role")),
        other => panic!("expected SpawnFailed, got: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn claude_bg_close_thread_is_idempotent_on_missing_state() {
    let tmp = tempfile::tempdir().unwrap();
    let _jobs = EnvGuard::set_path(CLAUDE_JOBS_DIR_ENV, tmp.path());

    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Bg,
        identity: "ghost-job".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    // state.json missing under tempdir — must return Ok (idempotent).
    ClaudeBgAdapter::new()
        .close_thread(&h)
        .await
        .expect("close_thread idempotent on missing state.json");
}

#[tokio::test(flavor = "current_thread")]
async fn claude_bg_resume_thread_is_not_implemented() {
    let err = ClaudeBgAdapter::new()
        .resume_thread("any-sid")
        .await
        .expect_err("bg fresh-context red line: resume unsupported");
    assert!(matches!(err, HarnessError::NotImplemented { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn claude_bg_submit_turn_synthesises_turn_id() {
    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Bg,
        identity: "abc12345".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    let tid = ClaudeBgAdapter::new()
        .submit_turn(&h, ccteam_harness::TurnInput::UserText("hi".into()))
        .await
        .expect("synthetic ok");
    assert_eq!(tid.0, "bg-abc12345");
}

// ─── V0.8 W3 — opt-in foreground-in-mux bg path ──────────────────────
//
// `claude --bg` self-detaches, so mux can't supervise the worker (see
// `docs/versions/v0-8-rmux/w3-mode2-bg-findings.md`). The W3 deliverable
// is an opt-in `claude -p --agent` foreground run inside an Ephemeral
// mux session, gated by `CCTEAM_CLAUDE_BG_VIA_MUX=1`. These tests verify
// the path is reachable + mints a mux-backed handle, and that
// `close_thread` routes teardown through the backend.

fn tmux_on_path_bg() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn claude_bg_via_mux_spawns_ephemeral_session_and_close_reaps_it() {
    let _mux_guard = EnvGuard::set("CCTEAM_MUX_BACKEND", "tmux");
    if !tmux_on_path_bg() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Fake `claude` that records its argv (so we can assert the Path B
    // invocation contract: `-p --agent <role> --dangerously-skip-...`)
    // then sleeps so the mux session sticks around for the exists()
    // assert before close_thread reaps it. (Real `claude -p` runs the
    // turn to completion; a sleeper stands in for "process alive in the
    // pane".)
    let fake_claude = tmp.path().join("fake-claude");
    let argv_capture = tmp.path().join("argv-capture");
    std::fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nsleep 30\n",
            argv_capture.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let _bin = EnvGuard::set(CLAUDE_BIN_ENV, fake_claude.to_str().unwrap());
    let _flag = EnvGuard::set("CCTEAM_CLAUDE_BG_VIA_MUX", "1");

    let brief = AgentSpecBrief {
        role: "tester".into(),
    };
    let ctx = SpawnCtx {
        mode: None,
        slug: "muxbg".into(),
        sid: "claude-9".into(),
        owner: "user:web-api".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec!["do the thing".into()],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    };

    let adapter = ClaudeBgAdapter::new();
    let handle = adapter
        .start_thread(&brief, &ctx)
        .await
        .expect("via-mux start_thread spawns an ephemeral mux session");

    assert_eq!(handle.vendor, AgentVendor::Claude);
    assert_eq!(handle.mode, ExecutionMode::Bg);
    // Mux path uses the session name as identity (NOT the --bg job id).
    assert_eq!(handle.identity, "ccteam-bg-muxbg-claude-9");
    assert_eq!(
        handle.raw_extras.get("via_mux").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        handle
            .raw_extras
            .get("mux_session")
            .and_then(|v| v.as_str()),
        Some("ccteam-bg-muxbg-claude-9")
    );
    // Legacy field preserved for SessionRecord parity.
    assert_eq!(
        handle
            .raw_extras
            .get("tmux_session")
            .and_then(|v| v.as_str()),
        Some("ccteam-bg-muxbg-claude-9")
    );

    // The session is live: the daemon (tmux) owns the child process.
    let backend = ccteam_harness::default_backend();
    let id = ccteam_harness::MuxSessionId::new("ccteam-bg-muxbg-claude-9".to_string());
    assert!(
        backend.exists(&id).await.unwrap(),
        "mux session should exist after via-mux spawn"
    );

    // Verify the Path B invocation contract: the pane child is
    // `claude -p --agent tester --dangerously-skip-permissions <prompt>`,
    // NOT `--bg`. The fake claude writes its argv async inside the pane;
    // poll briefly for the capture file.
    let mut argv_text = String::new();
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&argv_capture) {
            if !s.is_empty() {
                argv_text = s;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        argv_text.lines().any(|l| l == "-p"),
        "via-mux argv must use foreground -p (not --bg); got: {argv_text:?}"
    );
    assert!(
        !argv_text.lines().any(|l| l == "--bg"),
        "via-mux argv must NOT contain --bg; got: {argv_text:?}"
    );
    assert!(
        argv_text.lines().any(|l| l == "--agent"),
        "via-mux argv must pass --agent; got: {argv_text:?}"
    );
    assert!(
        argv_text.lines().any(|l| l == "tester"),
        "via-mux argv must carry the role; got: {argv_text:?}"
    );
    assert!(
        argv_text.lines().any(|l| l == "do the thing"),
        "via-mux argv must carry the extra_args prompt; got: {argv_text:?}"
    );

    // close_thread routes through ProcessBackend::kill (via_mux=true handle).
    adapter
        .close_thread(&handle)
        .await
        .expect("close_thread reaps the mux session");
    assert!(
        !backend.exists(&id).await.unwrap(),
        "mux session should be gone after close_thread"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn claude_bg_via_mux_close_thread_idempotent_on_missing_session() {
    let _mux_guard = EnvGuard::set("CCTEAM_MUX_BACKEND", "tmux");
    // A via_mux handle whose session never existed (or already reaped):
    // ProcessBackend::kill is idempotent, so close_thread is Ok. Does not
    // require tmux on PATH — kill of a non-existent session is a no-op
    // even when the backend can shell out (tmux kill-session on a
    // missing target is treated as success by tmux_ops).
    if !tmux_on_path_bg() {
        eprintln!("skipping: tmux not on PATH");
        return;
    }
    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Bg,
        identity: "ccteam-ghost-mux-0".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({
            "via_mux": true,
            "mux_session": "ccteam-ghost-mux-0",
        }),
    };
    ClaudeBgAdapter::new()
        .close_thread(&h)
        .await
        .expect("via_mux close_thread idempotent on missing mux session");
}

#[test]
fn parse_backgrounded_short_id_extracts_marker() {
    let out = "warning: bla\nbackgrounded · 9432490e\n  attach hint\n";
    assert_eq!(
        parse_backgrounded_short_id(out).as_deref(),
        Some("9432490e")
    );
}

// =====================================================================
// 3. CodexExecAdapter start_thread tmux session (gated)
// =====================================================================

#[cfg(feature = "codex-tests")]
mod codex_tmux {
    use super::*;
    use serial_test::serial;

    fn tmux_on_path() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn codex_exec_start_thread_creates_tmux_session() {
        if !tmux_on_path() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        // codex won't be on PATH in CI; use a sleeper as the
        // session's pane process so the session sticks around for
        // the assert below.
        let fake_codex = tmp.path().join("codex");
        std::fs::write(&fake_codex, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_codex, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prev_path = std::env::var_os("PATH");
        let new_path = format!(
            "{}:{}",
            tmp.path().display(),
            prev_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        std::env::set_var("PATH", &new_path);

        let brief = AgentSpecBrief {
            role: String::new(),
        };
        let ctx = SpawnCtx {
            slug: "codextest".into(),
            sid: "codex-1".into(),
            owner: "user:web-api".into(),
            cwd: tmp.path().to_path_buf(),
            project_dir: tmp.path().to_path_buf(),
            extra_args: vec![],
            model_id: None,
            effort: None,
            permission_mode: ccteam_harness::PermissionMode::Skip,
            secret: String::new(),
            remote: None,
        };

        let adapter = CodexExecAdapter::new();
        let handle = adapter.start_thread(&brief, &ctx).await.expect("spawn ok");
        assert_eq!(handle.vendor, AgentVendor::Codex);
        assert_eq!(handle.identity, "ccteam-codextest-codex-1");

        // Cleanup tmux session.
        adapter.close_thread(&handle).await.unwrap();

        match prev_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn codex_exec_submit_turn_returns_synthetic_turn_id_wave3() {
    // Wave 3: submit_turn spawns `codex exec --json` (or the fake bin
    // pointed at by CCTEAM_CODEX_BIN). The TurnId is synthesised
    // monotonically per adapter instance regardless of whether the
    // subprocess succeeds — `events()` is the channel that reports
    // success/failure. We point CCTEAM_CODEX_BIN at `/bin/true` so the
    // process exits cleanly and the test doesn't depend on a real
    // codex install.
    std::env::set_var(CODEX_BIN_ENV, "/bin/true");
    let h = ThreadHandle {
        vendor: AgentVendor::Codex,
        mode: ExecutionMode::Bg,
        identity: "ccteam-foo-codex-1".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    let tid = CodexExecAdapter::new()
        .submit_turn(&h, ccteam_harness::TurnInput::UserText("hi".into()))
        .await
        .expect("Wave 3 submit_turn returns synthetic TurnId");
    assert!(tid.0.starts_with("codex-exec-"));
    std::env::remove_var(CODEX_BIN_ENV);
}

#[tokio::test(flavor = "current_thread")]
async fn codex_exec_resume_thread_synthesises_handle_wave3() {
    let h = CodexExecAdapter::new()
        .resume_thread("01988f74-0d1f-7e80-a-b-c")
        .await
        .expect("resume returns synthesised handle");
    assert_eq!(h.vendor, AgentVendor::Codex);
    assert_eq!(h.identity, "01988f74-0d1f-7e80-a-b-c");
    assert_eq!(h.raw_extras["resumed"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn codex_exec_resume_thread_rejects_empty_persistent_id() {
    let err = CodexExecAdapter::new()
        .resume_thread("")
        .await
        .expect_err("empty id should error");
    assert!(matches!(err, HarnessError::SpawnFailed(_)));
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn codex_exec_close_thread_idempotent_on_missing_session() {
    let _mux_guard = EnvGuard::set("CCTEAM_MUX_BACKEND", "tmux");
    let h = ThreadHandle {
        vendor: AgentVendor::Codex,
        mode: ExecutionMode::Bg,
        identity: "ccteam-ghost-codex-1".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    // No tmux session created — close must succeed silently.
    CodexExecAdapter::new()
        .close_thread(&h)
        .await
        .expect("idempotent on missing tmux session");
}

// =====================================================================
// 4. ClaudeTuiAdapter Wave 2 surface — name / vendor
// =====================================================================
// v0.8.8 F2: the empty-role guard was REMOVED on purpose — roleless = spawn
// WITHOUT `--agent` (the brain comes from the project's native CLAUDE.md).
// The roleless argv contract (no `--agent` when role is empty; `--name`/sid
// still present) is covered by the `spec_for_new`/`spec_for_resume`
// `*_roleless_omits_agent` unit tests in claude_tui.rs, and create-path
// behavior by `create_session_empty_role_is_ok` in gateway.rs.

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn claude_tui_resume_missing_session_reports_not_implemented() {
    let _mux_guard = EnvGuard::set("CCTEAM_MUX_BACKEND", "tmux");
    // Wave 2: resume_thread w/o a live tmux session falls back to a
    // structured NotImplemented so the caller knows to invoke
    // start_thread + session_recovery::build_recovery_prompt.
    let adapter = ClaudeTuiAdapter::new();
    let err = adapter
        .resume_thread("ccteam-chat-nope-ghost")
        .await
        .unwrap_err();
    match err {
        HarnessError::NotImplemented { reason } => {
            assert!(reason.contains("session_recovery"));
        }
        other => panic!("expected NotImplemented, got {other:?}"),
    }
}

// =====================================================================
// 5. AgentVendor serde round-trip
// =====================================================================

#[test]
fn agent_vendor_serde_round_trip() {
    for v in [AgentVendor::Claude, AgentVendor::Codex] {
        let s = serde_json::to_string(&v).unwrap();
        let back: AgentVendor = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }
}

#[test]
fn agent_vendor_serde_lowercase_wire_format() {
    // Wire format must be `"claude"` / `"codex"` so progress.jsonl
    // executor field (V0.4.5 F80) stays compatible.
    let s = serde_json::to_string(&AgentVendor::Claude).unwrap();
    assert_eq!(s, "\"claude\"");
    let s = serde_json::to_string(&AgentVendor::Codex).unwrap();
    assert_eq!(s, "\"codex\"");
}

// =====================================================================
// 6 + 7. ThreadHandle.identity contract + SessionHandle translation
// =====================================================================

#[test]
fn thread_handle_identity_contract_claude_bg() {
    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Bg,
        identity: "deadbeef".to_string(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({"tmux_session": "ccteam-foo-claude-1"}),
    };
    let sh = SessionHandle::from_thread_handle(&h, "claude-1");
    assert_eq!(sh.sid, "claude-1");
    assert_eq!(sh.harness, "claude-code");
    assert_eq!(sh.tmux_session, "ccteam-foo-claude-1");
    assert_eq!(sh.job_id.as_deref(), Some("deadbeef"));
}

#[test]
fn thread_handle_identity_contract_codex_exec() {
    let h = ThreadHandle {
        vendor: AgentVendor::Codex,
        mode: ExecutionMode::Bg,
        identity: "ccteam-bar-codex-1".to_string(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({
            "tmux_session": "ccteam-bar-codex-1",
            "pid": 9001u64
        }),
    };
    let sh = SessionHandle::from_thread_handle(&h, "codex-1");
    assert_eq!(sh.sid, "codex-1");
    assert_eq!(sh.harness, "codex");
    assert!(
        sh.job_id.is_none(),
        "codex sessions don't carry a job_id (no `--bg` surface yet)"
    );
    assert_eq!(sh.pid, Some(9001));
}

#[test]
fn thread_handle_identity_contract_claude_tui() {
    // TUI adapter encodes the tmux session name in `identity`; the
    // translator must surface that into SessionHandle.harness =
    // "claude-tui" (distinct from "claude-code" bg case).
    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: "ccteam-chat-foo-bot".to_string(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({"tmux_session": "ccteam-chat-foo-bot"}),
    };
    let sh = SessionHandle::from_thread_handle(&h, "chat-1");
    assert_eq!(sh.harness, "claude-tui");
    assert!(
        sh.job_id.is_none(),
        "TUI adapter does not produce a bg job_id"
    );
}
