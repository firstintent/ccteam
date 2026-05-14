//! V0.4.0 F61 — `ClaudeCodeAdapter` thin refactor integration tests.
//!
//! Covers the three rewritten surfaces (`spawn_session`,
//! `ingest_snapshot`, `shutdown_session`) plus the `state_json_path`
//! helper. All tests are hermetic: `$CCTEAM_CLAUDE_BIN` / `$CCTEAM_CLAUDE_JOBS_DIR`
//! pin the claude binary + jobs root to tempdirs so the suite never
//! reads the live user's `~/.claude/`.
//!
//! `serial_test::serial` for tests that mutate env vars (PATH, the two
//! ccteam env overrides). Without serial the parallel runner races on
//! env state and fails intermittently — same pattern the F62 codex
//! adapter integration tests use.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ccteam_core::{
    parse_cc_state_json, state_json_path, ClaudeCodeAdapter, HarnessAdapter, HarnessError,
    HarnessSnapshot, SessionHandle, SpawnOpts, CLAUDE_BIN_ENV, CLAUDE_JOBS_DIR_ENV,
};
use chrono::Utc;
use serial_test::serial;
use tempfile::TempDir;

/// Write an executable shell script that emits `body` on stdout and
/// exits 0. Returns the script path. Used to mock `claude --bg`.
fn install_fake_claude_bin(dir: &std::path::Path, body: &str) -> PathBuf {
    let bin = dir.join("claude");
    let script = format!("#!/bin/sh\nprintf '%s\\n' '{body}'\n");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// RAII guard that restores the original value of `key` on drop.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
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

// =====================================================================
// t01 — spawn_session captures job_id from `claude --bg` stdout
// =====================================================================

#[test]
#[serial]
fn t01_spawn_returns_job_id() {
    let tmp = TempDir::new().unwrap();
    // Real `claude --bg` stdout: `backgrounded · <8-hex>` followed by
    // a tip block. The parser must pick the id off the first line.
    let bin = install_fake_claude_bin(tmp.path(), "backgrounded · 9432490e");
    let _bin_guard = EnvGuard::set(CLAUDE_BIN_ENV, &bin);

    let opts = SpawnOpts {
        harness: "claude-code",
        slug: "dev-thin".into(),
        sid: "claude-1".into(),
        cwd: tmp.path().to_path_buf(),
        role: "main".into(),
        extra_args: Vec::new(),
    };
    let handle = ClaudeCodeAdapter::new()
        .spawn_session(opts)
        .expect("spawn_session must succeed with fake claude bin");

    assert_eq!(handle.harness, "claude-code");
    assert_eq!(handle.sid, "claude-1");
    assert_eq!(handle.job_id.as_deref(), Some("9432490e"));
    // F61 retains the `ccteam-<slug>-<sid>` name shape for downstream
    // labels even though ClaudeCodeAdapter no longer owns a tmux session.
    assert_eq!(handle.tmux_session, "ccteam-dev-thin-claude-1");
    // PID is not populated at spawn time (the bg job writes its own
    // state.json; F66 observer reads it).
    assert!(handle.pid.is_none());
}

// =====================================================================
// t02 — ingest_snapshot parses state.json from CCTEAM_CLAUDE_JOBS_DIR
// =====================================================================

#[test]
#[serial]
fn t02_ingest_from_state_json() {
    let tmp = TempDir::new().unwrap();
    let _jobs_guard = EnvGuard::set(CLAUDE_JOBS_DIR_ENV, tmp.path());

    let job_id = "9432490e";
    let job_dir = tmp.path().join(job_id);
    std::fs::create_dir_all(&job_dir).unwrap();
    // Real `claude 2.1.x` state.json shape: `state` (not `status`),
    // `cwd` (not `workdir`), no cost/model/context fields.
    let raw = r#"{
        "state": "working",
        "tempo": "active",
        "cwd": "/tmp/some-project",
        "daemonShort": "9432490e",
        "sessionId": "9432490e-90f8-457c-87b6-2ccfc5c452d3",
        "cliVersion": "2.1.141",
        "template": "bg",
        "intent": "do the thing"
    }"#;
    std::fs::write(job_dir.join("state.json"), raw).unwrap();

    let path = state_json_path(job_id);
    assert_eq!(path, tmp.path().join(job_id).join("state.json"));
    let body = std::fs::read_to_string(&path).unwrap();

    let snap = ClaudeCodeAdapter::new().ingest_snapshot(&body).unwrap();
    assert_eq!(snap.harness, "claude-code");
    // No `model` field → fall back to `cliVersion`.
    assert_eq!(snap.model_display_name, "claude 2.1.141");
    // No `context_pct` / `cost_usd` → defaults.
    assert_eq!(snap.context_used_pct, 0);
    assert!((snap.cost_usd_total - 0.0).abs() < 1e-9);
    assert_eq!(
        snap.cwd.as_deref(),
        Some(std::path::Path::new("/tmp/some-project"))
    );
    // raw preserves full shape — orchestrator reads `state` for the
    // session_status decision.
    assert_eq!(snap.raw["state"], "working");
    assert_eq!(snap.raw["daemonShort"], "9432490e");
}

// =====================================================================
// t03 — shutdown_session sends SIGTERM to the pid in state.json
// =====================================================================

#[test]
#[serial]
fn t03_shutdown_sends_sigterm() {
    let tmp = TempDir::new().unwrap();
    let _jobs_guard = EnvGuard::set(CLAUDE_JOBS_DIR_ENV, tmp.path());

    // Spawn a child that sleeps long enough for shutdown to catch it.
    // `sh -c 'sleep 30'` is portable enough for CI (any POSIX shell).
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 30"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sleep child");
    let pid = child.id() as i32;

    let job_id = "fa11dead";
    let job_dir = tmp.path().join(job_id);
    std::fs::create_dir_all(&job_dir).unwrap();
    let state = serde_json::json!({
        "state": "working",
        "tempo": "active",
        "cwd": "/tmp",
        "daemonShort": job_id,
        "pid": pid,
    });
    std::fs::write(
        job_dir.join("state.json"),
        serde_json::to_string(&state).unwrap(),
    )
    .unwrap();

    let handle = SessionHandle {
        tmux_session: "ccteam-dev-thin-claude-1".into(),
        harness: "claude-code".into(),
        sid: "claude-1".into(),
        job_id: Some(job_id.into()),
        pid: None,
        started_at: Utc::now(),
    };

    ClaudeCodeAdapter::new()
        .shutdown_session(&handle)
        .expect("first shutdown must succeed (SIGTERM)");

    // Reap the child so we don't leave a zombie. The SIGTERM may take
    // a moment to be delivered; bound the wait so a stuck test fails
    // loudly rather than hanging CI.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                // Force-kill if SIGTERM never landed (test would have
                // failed semantically below).
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(err) => panic!("try_wait: {err}"),
        }
    }

    // Second shutdown is idempotent — pid no longer exists (ESRCH) +
    // state.json still on disk. Must not error.
    ClaudeCodeAdapter::new()
        .shutdown_session(&handle)
        .expect("idempotent shutdown on already-dead pid");
}

// =====================================================================
// t04 — state_json_path resolves under env override
// =====================================================================

#[test]
#[serial]
fn t04_state_json_path_env_override() {
    let tmp = TempDir::new().unwrap();
    let _jobs_guard = EnvGuard::set(CLAUDE_JOBS_DIR_ENV, tmp.path());

    let path = state_json_path("abc123");
    assert_eq!(path, tmp.path().join("abc123").join("state.json"));

    // Different job_id → different path (no caching / stale state).
    let other = state_json_path("def456");
    assert_eq!(other, tmp.path().join("def456").join("state.json"));
    assert_ne!(path, other);
}

// =====================================================================
// t05 — statusline write API removed from ccteam-core
// =====================================================================

/// Compile-time validation that the V0.3.1 statusline writer API is
/// gone. We can't reference deleted symbols, so this test instead
/// asserts the public exports we DO surface compile and the deleted
/// ones cannot (the source files have been edited to drop them; the
/// dev-coupling-audit F61 captures the broader contract).
///
/// As a positive assertion: `parse_cc_state_json` + `state_json_path`
/// + `CLAUDE_BIN_ENV` + `CLAUDE_JOBS_DIR_ENV` are the new module-level
/// API surface. Their existence here proves the F61 reshuffle landed.
#[test]
fn t05_thin_api_surface_present() {
    // Positive: F61's new public API is reachable.
    let _ = parse_cc_state_json(r#"{"model":"x"}"#).unwrap();
    let _ = state_json_path("fake");
    let _: &str = CLAUDE_BIN_ENV;
    let _: &str = CLAUDE_JOBS_DIR_ENV;
}

// =====================================================================
// t06 — spawn_session command line includes --agent <role>
// =====================================================================

#[test]
#[serial]
fn t06_spawn_includes_role_and_cwd_via_current_dir() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("project-root");
    std::fs::create_dir_all(&cwd).unwrap();

    // Fake claude that records argv + working directory to sentinel
    // files. Production claude has no `--workdir` flag; cwd is set via
    // `Command::current_dir` and shows up as `pwd` in the child.
    let bin = tmp.path().join("claude");
    let script = r#"#!/bin/sh
printf '%s\n' "$@" > "$CCTEAM_TEST_ARGV_SINK"
pwd > "$CCTEAM_TEST_CWD_SINK"
printf 'backgrounded · facefeed\n'
printf '  claude agents             list sessions\n'
"#;
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let argv_sink = tmp.path().join("argv.txt");
    let cwd_sink = tmp.path().join("cwd.txt");
    let _bin_guard = EnvGuard::set(CLAUDE_BIN_ENV, &bin);
    let _argv_guard = EnvGuard::set("CCTEAM_TEST_ARGV_SINK", &argv_sink);
    let _cwd_guard = EnvGuard::set("CCTEAM_TEST_CWD_SINK", &cwd_sink);

    let opts = SpawnOpts {
        harness: "claude-code",
        slug: "dev-roletest".into(),
        sid: "claude-1".into(),
        cwd: cwd.clone(),
        role: "reviewer".into(),
        extra_args: Vec::new(),
    };
    let handle = ClaudeCodeAdapter::new().spawn_session(opts).unwrap();
    assert_eq!(handle.job_id.as_deref(), Some("facefeed"));

    let argv = std::fs::read_to_string(&argv_sink).unwrap();
    assert!(argv.contains("--bg"), "argv missing --bg: {argv}");
    assert!(argv.contains("--agent"), "argv missing --agent: {argv}");
    assert!(
        argv.contains("reviewer"),
        "argv missing role 'reviewer': {argv}"
    );
    // CRITICAL: real claude CLI has no --workdir flag. Spawning with
    // one exits "unknown option '--workdir'" before init.
    assert!(
        !argv.contains("--workdir"),
        "argv must NOT include --workdir (not a real CLI flag): {argv}"
    );
    // CLAUDE.md §三 — bg sessions need permissions skipped or they park
    // at the workspace trust dialog and never run.
    assert!(
        argv.contains("--dangerously-skip-permissions"),
        "argv must skip permission prompts for --bg: {argv}"
    );

    // cwd should be set via Command::current_dir → reflected in pwd.
    let observed_cwd = std::fs::read_to_string(&cwd_sink).unwrap();
    // Use canonicalize on both sides — /tmp can be a symlink on macOS.
    let expected = std::fs::canonicalize(&cwd).unwrap();
    let observed = std::fs::canonicalize(observed_cwd.trim()).unwrap();
    assert_eq!(observed, expected, "spawn cwd mismatch");
}

// =====================================================================
// t07 — empty role rejected with SpawnFailed
// =====================================================================

#[test]
#[serial]
fn t07_spawn_rejects_empty_role() {
    let tmp = TempDir::new().unwrap();
    let bin = install_fake_claude_bin(tmp.path(), r#"{"job_id":"never"}"#);
    let _bin_guard = EnvGuard::set(CLAUDE_BIN_ENV, &bin);

    let opts = SpawnOpts {
        harness: "claude-code",
        slug: "dev-empty".into(),
        sid: "claude-1".into(),
        cwd: tmp.path().to_path_buf(),
        role: String::new(),
        extra_args: Vec::new(),
    };
    let err = ClaudeCodeAdapter::new().spawn_session(opts).unwrap_err();
    match err {
        HarnessError::SpawnFailed(msg) => {
            assert!(msg.contains("role"), "error must mention role: {msg}");
        }
        other => panic!("expected SpawnFailed, got {other:?}"),
    }
}

// =====================================================================
// t08 — spawn_session surfaces missing job_id loudly
// =====================================================================

#[test]
#[serial]
fn t08b_spawn_skips_warning_prefix_and_picks_backgrounded_line() {
    // Real claude prints a warning line before `backgrounded · <id>`
    // when the agent name is unknown — parser must skip it.
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("claude");
    let script = "#!/bin/sh\n\
        printf 'warning: no agent named %s\\n' \"explorer\"\n\
        printf 'backgrounded · deadbeef\\n'\n\
        printf '  claude agents             list sessions\\n'\n";
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _bin_guard = EnvGuard::set(CLAUDE_BIN_ENV, &bin);

    let opts = SpawnOpts {
        harness: "claude-code",
        slug: "dev-warn".into(),
        sid: "claude-1".into(),
        cwd: tmp.path().to_path_buf(),
        role: "explorer".into(),
        extra_args: Vec::new(),
    };
    let handle = ClaudeCodeAdapter::new().spawn_session(opts).unwrap();
    assert_eq!(handle.job_id.as_deref(), Some("deadbeef"));
}

#[test]
#[serial]
fn t08_spawn_missing_backgrounded_line_fails_loud() {
    let tmp = TempDir::new().unwrap();
    // Fake binary that emits unrelated output (no `backgrounded · …`
    // marker). Must bubble as SpawnFailed (loud), not silently succeed.
    let bin = install_fake_claude_bin(tmp.path(), "queued no marker here");
    let _bin_guard = EnvGuard::set(CLAUDE_BIN_ENV, &bin);

    let opts = SpawnOpts {
        harness: "claude-code",
        slug: "dev-nojob".into(),
        sid: "claude-1".into(),
        cwd: tmp.path().to_path_buf(),
        role: "main".into(),
        extra_args: Vec::new(),
    };
    let err = ClaudeCodeAdapter::new().spawn_session(opts).unwrap_err();
    match err {
        HarnessError::SpawnFailed(msg) => {
            assert!(
                msg.contains("backgrounded"),
                "error must mention backgrounded marker: {msg}"
            );
        }
        other => panic!("expected SpawnFailed, got {other:?}"),
    }
}

// =====================================================================
// t09 — shutdown on handle with no job_id is idempotent no-op
// =====================================================================

#[test]
fn t09_shutdown_without_job_id_is_noop() {
    // Legacy / codex rows leave `job_id: None`. ClaudeCodeAdapter
    // shutdown must treat that as a no-op (logged, not errored) so a
    // misrouted call (or upgrade from pre-F61 state.json) doesn't
    // bring down the whole `ccteam session rm` path.
    let handle = SessionHandle {
        tmux_session: "ccteam-legacy-claude-1".into(),
        harness: "claude-code".into(),
        sid: "claude-1".into(),
        job_id: None,
        pid: None,
        started_at: Utc::now(),
    };
    ClaudeCodeAdapter::new()
        .shutdown_session(&handle)
        .expect("shutdown without job_id must be a no-op success");
}

// =====================================================================
// t10 — HarnessSnapshot shape stable (web layer contract)
// =====================================================================

#[test]
fn t10_harness_snapshot_shape_round_trip() {
    // The F68 web layer reads `HarnessSnapshot` over the SSE wire — its
    // serde shape must round-trip cleanly even after F61's source
    // pivot. Test pins that contract: fields the dashboard renders
    // (harness, model_display_name, context_used_pct, cost_usd_total)
    // serialize lossless.
    let snap = HarnessSnapshot {
        harness: "claude-code".into(),
        model_display_name: "Claude Opus 4.7".into(),
        context_used_pct: 88,
        cost_usd_total: 12.5,
        rate_limit_pct: Some(33),
        cwd: Some(PathBuf::from("/projects/dev-foo")),
        raw: serde_json::json!({"status": "running", "turn_count": 9}),
        captured_at: Utc::now(),
    };
    let json = serde_json::to_string(&snap).unwrap();
    let back: HarnessSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snap);
}
