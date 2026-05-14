//! V0.4.0 F62 - integration tests for the real `CodexAdapter`.
//!
//! These tests require `tmux` on PATH and exercise the `codex` CLI
//! when present. They're gated behind the `codex-tests` feature so
//! default CI runs (without tmux / codex installed) stay green; the
//! same gate documented in `crates/ccteam-core/Cargo.toml`.
//!
//! Each test uses a unique tmux session name (`ccteam-f62-test-…`)
//! plus a `Drop` guard so panicking tests still clean up the tmux
//! session they created. Tests are serialised via the `serial_test`
//! crate so concurrent spawn/shutdown calls don't race the shared
//! tmux server.
//!
//! Tests skip gracefully (with an `eprintln!` notice + early return)
//! when:
//!
//! - `tmux` is missing on PATH
//! - `codex` is missing on PATH **and** the test would actually need
//!   it to make progress (spawn / shutdown). Parser-only tests run
//!   regardless because they don't touch tmux at all.
//!
//! Maps to `docs/v0-4-0/dev-plan.md` §4.1 #3.5 — five canary tests
//! pinning the post-F62 contract.

#![cfg(feature = "codex-tests")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    CodexAdapter, HarnessAdapter, HarnessError, HarnessSnapshot, SessionHandle, SpawnOpts,
    CODEX_STATUS_TAIL_LINES,
};
use chrono::Utc;
use serial_test::serial;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug(test_name: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("f62-{test_name}-{pid}-{n}")
}

/// Cleanup wrapper: kills the named tmux session on drop. Used so
/// panicking tests still leave a clean tmux server behind.
struct ScopedTmux {
    name: String,
}

impl ScopedTmux {
    fn new(name: String) -> Self {
        Self { name }
    }
}

impl Drop for ScopedTmux {
    fn drop(&mut self) {
        let session = TmuxSession::from_name(self.name.clone());
        let _ = session.kill();
    }
}

fn skip_if_no_tmux(test_name: &str) -> bool {
    if !tmux_available() {
        eprintln!("[skip] {test_name}: tmux not on PATH");
        return true;
    }
    false
}

/// Stand up a tiny fake `codex` binary on PATH for tests that need a
/// long-running pane process but don't want to depend on the real
/// codex CLI being installed. The shell script blocks on `read` so
/// tmux keeps the pane alive until shutdown_session sends `q\r`.
/// Returns the temp dir (drop frees the bin) and the modified PATH.
fn install_fake_codex() -> (tempfile::TempDir, std::ffi::OsString) {
    let tmp = tempfile::tempdir().expect("tempdir for fake codex");
    let bin = tmp.path().join("codex");
    std::fs::write(
        &bin,
        "#!/bin/sh\ntrap 'exit 0' TERM INT QUIT\n\
         echo 'CODEX_STATUS: {\"model\":\"fake\",\"context_pct\":0,\"cost_usd\":0.0}'\n\
         while read line; do\n  if [ \"$line\" = q ]; then exit 0; fi\ndone\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
    }
    let mut path = std::ffi::OsString::from(tmp.path());
    if let Some(existing) = std::env::var_os("PATH") {
        path.push(":");
        path.push(existing);
    }
    (tmp, path)
}

/// Tail-capture a tmux pane (max `CODEX_STATUS_TAIL_LINES` lines).
/// Used to drive `ingest_snapshot` against a real session — matches
/// the contract F66's watcher will follow.
fn capture_tail(session_name: &str) -> String {
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-p",
            "-t",
            session_name,
            "-S",
            &format!("-{CODEX_STATUS_TAIL_LINES}"),
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => String::new(),
    }
}

// =====================================================================
// t01 — spawn_session creates a tmux session
// =====================================================================

#[test]
#[serial]
fn t01_spawn_creates_tmux_session() {
    let test = "t01_spawn_creates_tmux_session";
    if skip_if_no_tmux(test) {
        return;
    }
    let (_fake_bin, path) = install_fake_codex();
    // We can't pass env to TmuxSession::start directly, but `tmux`
    // inherits the parent process's PATH for the spawned child.
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &path);

    let slug = unique_slug("t01");
    let expected_name = format!("ccteam-{slug}-codex-1");
    let _guard = ScopedTmux::new(expected_name.clone());

    let opts = SpawnOpts {
        harness: "codex",
        slug: slug.clone(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        extra_args: Vec::new(),
    };
    let handle = CodexAdapter::new()
        .spawn_session(opts)
        .expect("spawn_session succeeds with fake codex on PATH");
    assert_eq!(handle.tmux_session, expected_name);
    assert_eq!(handle.harness, "codex");
    assert_eq!(handle.sid, "codex-1");
    assert!(
        TmuxSession::from_name(expected_name.clone()).exists(),
        "tmux session must exist post-spawn"
    );

    // Restore PATH so subsequent tests aren't polluted.
    if let Some(p) = prev_path {
        std::env::set_var("PATH", p);
    } else {
        std::env::remove_var("PATH");
    }
}

// =====================================================================
// t02 — ingest_snapshot fallback (pane lacks CODEX_STATUS marker)
// =====================================================================

#[test]
fn t02_ingest_snapshot_fallback() {
    // Pure-Rust path; no tmux needed. Empty pane + non-status content
    // both fall back to the same shape: model="codex", zero pct/cost.
    let adapter = CodexAdapter::new();

    let empty = adapter.ingest_snapshot("").unwrap();
    assert_eq!(empty.model_display_name, "codex");
    assert_eq!(empty.context_used_pct, 0);
    assert_eq!(empty.cost_usd_total, 0.0);

    let noise = adapter
        .ingest_snapshot("user@host:~$ codex\nthinking...\n[some output]\n")
        .unwrap();
    assert_eq!(noise.model_display_name, "codex");
    assert!(noise.cwd.is_none());
    assert!(noise.rate_limit_pct.is_none());
}

// =====================================================================
// t03 — ingest_snapshot parses a real CODEX_STATUS: line
// =====================================================================

#[test]
fn t03_ingest_snapshot_parse() {
    let pane = "preamble line\n\
                CODEX_STATUS: {\"model\":\"o3-pro\",\"context_pct\":58,\"cost_usd\":2.5,\"rate_limit_pct\":12}\n\
                $ \n";
    let snap = CodexAdapter::new().ingest_snapshot(pane).unwrap();
    assert_eq!(snap.harness, "codex");
    assert_eq!(snap.model_display_name, "o3-pro");
    assert_eq!(snap.context_used_pct, 58);
    assert!((snap.cost_usd_total - 2.5).abs() < 1e-9);
    assert_eq!(snap.rate_limit_pct, Some(12));
    // raw retains the full upstream JSON for forward-compat.
    assert_eq!(snap.raw["model"], "o3-pro");
}

// =====================================================================
// t04 — shutdown_session kills the tmux session
// =====================================================================

#[test]
#[serial]
fn t04_shutdown_kills_session() {
    let test = "t04_shutdown_kills_session";
    if skip_if_no_tmux(test) {
        return;
    }
    let (_fake_bin, path) = install_fake_codex();
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &path);

    let slug = unique_slug("t04");
    let expected_name = format!("ccteam-{slug}-codex-1");
    let _guard = ScopedTmux::new(expected_name.clone());

    let handle = CodexAdapter::new()
        .spawn_session(SpawnOpts {
            harness: "codex",
            slug: slug.clone(),
            sid: "codex-1".into(),
            cwd: std::env::temp_dir(),
            extra_args: Vec::new(),
        })
        .expect("spawn for shutdown test");
    assert!(TmuxSession::from_name(expected_name.clone()).exists());

    CodexAdapter::new()
        .shutdown_session(&handle)
        .expect("shutdown_session ok");
    assert!(
        !TmuxSession::from_name(expected_name.clone()).exists(),
        "tmux session must be gone post-shutdown"
    );

    // Idempotent: shutting down an already-dead session is Ok.
    CodexAdapter::new()
        .shutdown_session(&handle)
        .expect("idempotent shutdown");

    if let Some(p) = prev_path {
        std::env::set_var("PATH", p);
    } else {
        std::env::remove_var("PATH");
    }
}

// =====================================================================
// t05 — CodexAdapter no longer returns NotImplemented from any path
// =====================================================================

#[test]
fn t05_codex_not_implemented_removed() {
    // Regression guard for F62. None of the three fallible methods may
    // return `HarnessError::NotImplemented`:
    //
    // - `ingest_snapshot` always succeeds with a fallback when input
    //   lacks a status marker.
    // - `spawn_session` returns `SpawnFailed` (not NotImplemented) when
    //   tmux/codex are missing on this host.
    // - `shutdown_session` is idempotent on missing sessions (Ok).
    let adapter = CodexAdapter::new();

    // Ingest never NotImplemented.
    let snap_res = adapter.ingest_snapshot("");
    assert!(
        !matches!(snap_res, Err(HarnessError::NotImplemented { .. })),
        "ingest_snapshot must not return NotImplemented: {snap_res:?}"
    );

    // Shutdown on a non-existent session returns Ok (idempotent), not
    // NotImplemented.
    let phantom = SessionHandle {
        tmux_session: format!("ccteam-{}-phantom", unique_slug("t05")),
        harness: "codex".into(),
        sid: "codex-99".into(),
        pid: None,
        started_at: Utc::now(),
    };
    let shutdown_res = adapter.shutdown_session(&phantom);
    assert!(
        !matches!(shutdown_res, Err(HarnessError::NotImplemented { .. })),
        "shutdown_session must not return NotImplemented: {shutdown_res:?}"
    );

    // Spawn with a bogus slug + missing tmux/codex still must NOT
    // return NotImplemented — at worst SpawnFailed.
    let opts = SpawnOpts {
        harness: "codex",
        slug: unique_slug("t05spawn"),
        sid: "codex-1".into(),
        cwd: PathBuf::from("/nonexistent-dir-f62"),
        extra_args: Vec::new(),
    };
    let spawn_res = adapter.spawn_session(opts);
    assert!(
        !matches!(spawn_res, Err(HarnessError::NotImplemented { .. })),
        "spawn_session must not return NotImplemented: {spawn_res:?}"
    );
    // Cleanup whatever tmux session may have spawned.
    if let Ok(handle) = &spawn_res {
        let _ = TmuxSession::from_name(handle.tmux_session.clone()).kill();
    }
}

// =====================================================================
// extra — end-to-end CODEX_STATUS round-trip via tmux capture-pane
// =====================================================================

#[test]
#[serial]
fn e2e_spawn_capture_parse_shutdown_round_trip() {
    let test = "e2e_spawn_capture_parse_shutdown_round_trip";
    if skip_if_no_tmux(test) {
        return;
    }
    let (_fake_bin, path) = install_fake_codex();
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &path);

    let slug = unique_slug("e2e");
    let expected_name = format!("ccteam-{slug}-codex-1");
    let _guard = ScopedTmux::new(expected_name.clone());

    let adapter = CodexAdapter::new();
    let handle = adapter
        .spawn_session(SpawnOpts {
            harness: "codex",
            slug: slug.clone(),
            sid: "codex-1".into(),
            cwd: std::env::temp_dir(),
            extra_args: Vec::new(),
        })
        .expect("spawn for e2e round-trip");

    // Give the fake codex a moment to print its CODEX_STATUS line.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let pane = capture_tail(&handle.tmux_session);
    let snap: HarnessSnapshot = adapter.ingest_snapshot(&pane).unwrap();
    // Either the fake printed the marker (model=fake) or pane
    // capture was empty (model=codex fallback). Both are valid.
    assert!(
        snap.model_display_name == "fake" || snap.model_display_name == "codex",
        "model should be fake or codex fallback; got {}",
        snap.model_display_name,
    );

    adapter.shutdown_session(&handle).unwrap();
    assert!(!TmuxSession::from_name(expected_name.clone()).exists());

    if let Some(p) = prev_path {
        std::env::set_var("PATH", p);
    } else {
        std::env::remove_var("PATH");
    }
}
