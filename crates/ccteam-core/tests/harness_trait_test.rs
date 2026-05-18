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

use ccteam_core::execution::{ClaudeBgAdapter, ClaudeTuiAdapter, CodexExecAdapter};
use ccteam_core::harness::{
    parse_backgrounded_short_id, AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter,
    HarnessError, SessionHandle, SpawnCtx, ThreadHandle, CLAUDE_BIN_ENV, CLAUDE_JOBS_DIR_ENV,
};

// =====================================================================
// 1. trait signature compile-time guarantee
// =====================================================================

#[test]
fn trait_signature_compile_time_guarantee() {
    // If this compiles, every adapter satisfies the locked Wave 1
    // contract: 5 async methods + name + vendor.
    fn assert_trait_impl<T: HarnessAdapter>() {}
    assert_trait_impl::<ClaudeBgAdapter>();
    assert_trait_impl::<ClaudeTuiAdapter>();
    assert_trait_impl::<CodexExecAdapter>();
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

#[tokio::test(flavor = "current_thread")]
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
        std::fs::set_permissions(
            &fake_claude,
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    let _bin = EnvGuard::set(CLAUDE_BIN_ENV, fake_claude.to_str().unwrap());

    let brief = AgentSpecBrief {
        role: "tester".into(),
    };
    let ctx = SpawnCtx {
        slug: "demo".into(),
        sid: "claude-1".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec!["initial prompt".into()],
    };

    let handle = ClaudeBgAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread succeeds with fake claude on PATH");
    assert_eq!(handle.vendor, AgentVendor::Claude);
    assert_eq!(handle.mode, ExecutionMode::Bg);
    assert_eq!(handle.identity, "deadbeef");
    assert_eq!(
        handle.raw_extras.get("tmux_session").and_then(|v| v.as_str()),
        Some("ccteam-demo-claude-1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn claude_bg_start_thread_rejects_empty_role() {
    let brief = AgentSpecBrief {
        role: String::new(),
    };
    let ctx = SpawnCtx {
        slug: "demo".into(),
        sid: "claude-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
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
        .submit_turn(&h, ccteam_core::harness::TurnInput::UserText("hi".into()))
        .await
        .expect("synthetic ok");
    assert_eq!(tid.0, "bg-abc12345");
}

#[test]
fn parse_backgrounded_short_id_extracts_marker() {
    let out = "warning: bla\nbackgrounded · 9432490e\n  attach hint\n";
    assert_eq!(parse_backgrounded_short_id(out).as_deref(), Some("9432490e"));
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
            std::fs::set_permissions(
                &fake_codex,
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let prev_path = std::env::var_os("PATH");
        let new_path = format!(
            "{}:{}",
            tmp.path().display(),
            prev_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
        );
        std::env::set_var("PATH", &new_path);

        let brief = AgentSpecBrief {
            role: String::new(),
        };
        let ctx = SpawnCtx {
            slug: "codextest".into(),
            sid: "codex-1".into(),
            cwd: tmp.path().to_path_buf(),
            project_dir: tmp.path().to_path_buf(),
            extra_args: vec![],
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
async fn codex_exec_submit_turn_not_implemented_wave1() {
    let h = ThreadHandle {
        vendor: AgentVendor::Codex,
        mode: ExecutionMode::Bg,
        identity: "ccteam-foo-codex-1".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    let err = CodexExecAdapter::new()
        .submit_turn(&h, ccteam_core::harness::TurnInput::UserText("hi".into()))
        .await
        .expect_err("Wave 3 fills");
    assert!(matches!(err, HarnessError::NotImplemented { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn codex_exec_close_thread_idempotent_on_missing_session() {
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
// 4. ClaudeTuiAdapter all 5 methods return NotImplemented
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn claude_tui_stub_all_methods_not_implemented() {
    let brief = AgentSpecBrief {
        role: "helpful-bot".into(),
    };
    let ctx = SpawnCtx {
        slug: "demo".into(),
        sid: "chat-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
    };
    let h = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: "tmux-session".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };

    let adapter = ClaudeTuiAdapter::new();
    assert!(matches!(
        adapter.start_thread(&brief, &ctx).await,
        Err(HarnessError::NotImplemented { .. })
    ));
    assert!(matches!(
        adapter
            .submit_turn(&h, ccteam_core::harness::TurnInput::UserText("x".into()))
            .await,
        Err(HarnessError::NotImplemented { .. })
    ));
    assert!(matches!(
        adapter.resume_thread("any-sid").await,
        Err(HarnessError::NotImplemented { .. })
    ));
    assert!(matches!(
        adapter.close_thread(&h).await,
        Err(HarnessError::NotImplemented { .. })
    ));
    // events() is non-fallible — must return an empty stream (not
    // panic, not loop).
    let _stream = adapter.events(&h);
}

#[tokio::test(flavor = "current_thread")]
async fn claude_tui_not_implemented_carries_wave2_marker() {
    let adapter = ClaudeTuiAdapter::new();
    let err = adapter.resume_thread("ignored").await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Wave 2") || msg.contains("F108"),
        "stub reason must reference Wave 2 / F108: got {msg}"
    );
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
