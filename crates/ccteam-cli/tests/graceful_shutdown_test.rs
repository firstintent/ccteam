//! F163 (+ v0.9.7) — `ccteam start` graceful SIGTERM/SIGINT shutdown tests.
//!
//! Verifies:
//! 1. SIGTERM causes the daemon to exit within the deadline (not hang).
//! 2. SIGINT (ctrl_c equivalent via kill -INT) also triggers exit.
//! 3. The foreground daemon NEVER writes `state/orchestrator.pid` —
//!    since v0.9.7 the pid record is launcher-written only
//!    (`ccteam daemon start`), so "managed" stays an honest signal.
//! 4. tmux sessions are NOT killed on shutdown.
//!
//! Daemon readiness is observed via the MCP socket (`run/mcp.sock`
//! accepting connections) — the same liveness signal production uses.
//! The v0.4.6 trigger-file stop channel is retired (SIGTERM only), so
//! its test case is gone with it.
//!
//! We point HOME + CCTEAM_HOME at a tempdir so these tests don't race
//! with the operator's real daemon. Tests use `--no-web --no-imd` to
//! avoid port conflicts and network I/O in CI.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

/// Spawn a minimal `ccteam start` daemon in an isolated tempdir.
/// Returns (child, ccteam_home, mcp_socket_path).
fn spawn_test_daemon(tmp_dir: &tempfile::TempDir) -> (std::process::Child, PathBuf, PathBuf) {
    spawn_test_daemon_with(
        tmp_dir,
        /* with_agent_teams_root = */ false,
        Stdio::null,
    )
}

/// Spawn helper with explicit toggles. `with_agent_teams_root=true`
/// pre-creates `<HOME>/.claude/teams/` so the F95 AgentTeamsWatcher
/// installs its inotify watch + runs the full discovery loop — this is
/// the production code path that V0.6.5 F163 retro fixes (`spawn_blocking`
/// task held BlockingPool::shutdown for the full TEAMS_DISCOVERY_INTERVAL
/// at process exit).
fn spawn_test_daemon_with(
    tmp_dir: &tempfile::TempDir,
    with_agent_teams_root: bool,
    stderr_factory: fn() -> Stdio,
) -> (std::process::Child, PathBuf, PathBuf) {
    let fake_home = tmp_dir.path();
    let ccteam_home = fake_home.join(".ccteam");
    std::fs::create_dir_all(ccteam_home.join("phases")).unwrap();
    std::fs::create_dir_all(ccteam_home.join("state")).unwrap();
    std::fs::create_dir_all(fake_home.join("projects")).unwrap();
    if with_agent_teams_root {
        std::fs::create_dir_all(fake_home.join(".claude").join("teams")).unwrap();
    }
    let mcp_socket = ccteam_home.join("run").join("mcp.sock");

    let child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(stderr_factory())
        .spawn()
        .expect("spawn ccteam start");

    (child, ccteam_home, mcp_socket)
}

#[cfg(unix)]
fn mcp_socket_reachable(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Wait until the daemon's MCP socket accepts a connection (daemon is
/// serving). Fails fast if the child exits during boot.
fn wait_for_ready(
    child: &mut std::process::Child,
    socket: &Path,
    deadline: Instant,
) -> Result<(), String> {
    while Instant::now() < deadline {
        if mcp_socket_reachable(socket) {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("daemon exited during boot: {status}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "MCP socket {} did not accept connections within deadline",
        socket.display()
    ))
}

/// Wait until the child process exits. Returns Ok(status) or Err on
/// timeout.
fn wait_for_exit(
    child: &mut std::process::Child,
    deadline: Instant,
) -> Result<std::process::ExitStatus, String> {
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(err) => return Err(format!("try_wait failed: {err}")),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("process did not exit within deadline".to_string())
}

/// Send a Unix signal to a pid. Returns Ok(()) if the syscall succeeded.
#[cfg(unix)]
fn send_signal(pid: u32, sig: libc::c_int) -> Result<(), String> {
    let ret = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if ret == 0 {
        Ok(())
    } else {
        Err(format!(
            "kill({pid}, {sig}) failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// F163 case 1 — SIGTERM causes clean exit; foreground never wrote a
/// pid record.
#[test]
#[cfg(unix)]
fn sigterm_causes_graceful_exit_without_pidfile() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut child, ccteam_home, socket) = spawn_test_daemon(&tmp);
    let pidfile = ccteam_home.join("state").join("orchestrator.pid");

    let ready = wait_for_ready(
        &mut child,
        &socket,
        Instant::now() + Duration::from_secs(15),
    );
    if let Err(msg) = ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("F163: {msg}");
    }

    // v0.9.7: the FOREGROUND daemon must not write the managed pid
    // record — that file is launcher-written only.
    assert!(
        !pidfile.exists(),
        "foreground `ccteam start` must not write {}",
        pidfile.display()
    );

    send_signal(child.id(), libc::SIGTERM).expect("send SIGTERM");

    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exited = wait_for_exit(&mut child, exit_deadline);
    if exited.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();

    assert!(
        exited.is_ok(),
        "F163: daemon should exit within 10s of SIGTERM; still running after deadline"
    );
    assert!(
        !pidfile.exists(),
        "no pid record may appear at any point in a foreground run"
    );
}

/// F163 case 2 — SIGINT also causes clean exit.
#[test]
#[cfg(unix)]
fn sigint_causes_graceful_exit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut child, _ccteam_home, socket) = spawn_test_daemon(&tmp);

    let ready = wait_for_ready(
        &mut child,
        &socket,
        Instant::now() + Duration::from_secs(15),
    );
    if let Err(msg) = ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("F163: {msg}");
    }

    // Send SIGINT (equivalent to Ctrl-C).
    send_signal(child.id(), libc::SIGINT).expect("send SIGINT");

    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exited = wait_for_exit(&mut child, exit_deadline);
    if exited.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();

    assert!(
        exited.is_ok(),
        "F163: daemon should exit within 10s of SIGINT; still running after deadline"
    );
}

/// F163 case 4 — daemon does NOT kill tmux sessions on shutdown.
///
/// Structural verification: we run the daemon with no active projects
/// and no tmux sessions. After SIGTERM the daemon exits cleanly without
/// any tmux-kill side effects. We verify the log doesn't contain
/// "tmux kill" and that the daemon exited with code 0.
#[test]
#[cfg(unix)]
fn shutdown_does_not_kill_tmux_sessions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path();
    let ccteam_home = fake_home.join(".ccteam");
    std::fs::create_dir_all(ccteam_home.join("phases")).unwrap();
    std::fs::create_dir_all(ccteam_home.join("state")).unwrap();
    std::fs::create_dir_all(fake_home.join("projects")).unwrap();
    let socket = ccteam_home.join("run").join("mcp.sock");

    // Capture stderr so we can inspect it for unwanted tmux-kill messages.
    let mut child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ccteam start");

    let ready = wait_for_ready(
        &mut child,
        &socket,
        Instant::now() + Duration::from_secs(15),
    );
    if let Err(msg) = ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("F163: {msg}");
    }

    send_signal(child.id(), libc::SIGTERM).expect("send SIGTERM");

    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exited = wait_for_exit(&mut child, exit_deadline);

    if exited.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output().ok();
    let stderr_text = output
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .unwrap_or_default();

    assert!(
        exited.is_ok(),
        "F163: daemon should exit within 10s of SIGTERM; still running after deadline"
    );

    // The daemon must NOT emit any "tmux kill-session" command in the log.
    // This would indicate the daemon is killing bot sessions on shutdown,
    // which violates CLAUDE.md §三 red line: 永不主动 kill 长 session.
    assert!(
        !stderr_text.contains("tmux kill-session"),
        "F163: daemon must not kill tmux sessions on shutdown; found 'tmux kill-session' in log:\n{stderr_text}"
    );
}

/// F163 retro case 5 — SIGTERM exits within 6s **even when the
/// AgentTeamsWatcher path is fully wired** (`~/.claude/teams/`
/// present, so the F95 watcher installs its inotify watch + spawns
/// the blocking discovery loop).
///
/// Before the retro fix, this configuration triggered the
/// `BlockingPool::shutdown(None)` hang: the AgentTeamsWatcher's
/// `spawn_blocking` thread blocked inside `recv_timeout(60s)` and
/// never noticed runtime tear-down, so `Runtime::drop` waited a full
/// TEAMS_DISCOVERY_INTERVAL (>60s) and the process required SIGKILL.
#[test]
#[cfg(unix)]
fn sigterm_exits_within_5s_with_agent_teams_watcher_active() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut child, _ccteam_home, socket) =
        spawn_test_daemon_with(&tmp, /* with_agent_teams_root = */ true, Stdio::null);

    let ready = wait_for_ready(
        &mut child,
        &socket,
        Instant::now() + Duration::from_secs(15),
    );
    if let Err(msg) = ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("F163 retro: {msg}");
    }

    let signal_sent_at = Instant::now();
    send_signal(child.id(), libc::SIGTERM).expect("send SIGTERM");

    // 6s ceiling — tighter than the 10s user-facing contract to catch
    // any regression that re-introduces a partial BlockingPool hang.
    let exit_deadline = signal_sent_at + Duration::from_secs(6);
    let exited = wait_for_exit(&mut child, exit_deadline);
    let exit_elapsed = signal_sent_at.elapsed();

    if exited.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();

    assert!(
        exited.is_ok(),
        "F163 retro: daemon with active AgentTeamsWatcher should exit within 6s of SIGTERM; \
         elapsed={:?} (was the cancel-handle wiring removed?)",
        exit_elapsed,
    );
}

/// F163 retro case 6 — five consecutive `start → SIGTERM` cycles all
/// exit within 6s, with the AgentTeamsWatcher wired. Catches any
/// non-deterministic shutdown flake (e.g. a race where the cancel
/// flag flip lands AFTER the discovery thread already entered
/// recv_timeout for the next 60s window).
#[test]
#[cfg(unix)]
fn sigterm_exits_fast_across_five_consecutive_runs() {
    for run in 1..=5 {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut child, _ccteam_home, socket) =
            spawn_test_daemon_with(&tmp, /* with_agent_teams_root = */ true, Stdio::null);

        let ready = wait_for_ready(
            &mut child,
            &socket,
            Instant::now() + Duration::from_secs(15),
        );
        if let Err(msg) = ready {
            let _ = child.kill();
            let _ = child.wait();
            panic!("F163 retro run #{run}: {msg}");
        }

        let signal_sent_at = Instant::now();
        send_signal(child.id(), libc::SIGTERM).expect("send SIGTERM");

        let exit_deadline = signal_sent_at + Duration::from_secs(6);
        let exited = wait_for_exit(&mut child, exit_deadline);
        let exit_elapsed = signal_sent_at.elapsed();

        if exited.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();

        assert!(
            exited.is_ok(),
            "F163 retro run #{run}: daemon should exit within 6s of SIGTERM; elapsed={:?}",
            exit_elapsed,
        );
    }
}

/// F163 retro case 7 — daemon logs the expected shutdown
/// signal-handling lines BEFORE exit. Asserts the in-process telemetry
/// chain (signal handler observed → graceful shutdown begin → graceful
/// shutdown complete) actually executes, not just "process exited".
/// A future regression that bypasses the orchestrator drain (e.g. by
/// short-circuiting on a race or hard-aborting) would still satisfy
/// "exited within 6s" but break this assertion.
///
/// Captures both stdout and stderr because the tracing subscriber's
/// default writer (stdout) is asymmetric with the bin's banner
/// `eprintln!` (stderr); merging both keeps the assertion robust to a
/// future writer swap.
#[test]
#[cfg(unix)]
fn sigterm_emits_full_graceful_shutdown_telemetry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path();
    let ccteam_home = fake_home.join(".ccteam");
    std::fs::create_dir_all(ccteam_home.join("phases")).unwrap();
    std::fs::create_dir_all(ccteam_home.join("state")).unwrap();
    std::fs::create_dir_all(fake_home.join("projects")).unwrap();
    std::fs::create_dir_all(fake_home.join(".claude").join("teams")).unwrap();
    let socket = ccteam_home.join("run").join("mcp.sock");

    let mut child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ccteam start");

    let ready = wait_for_ready(
        &mut child,
        &socket,
        Instant::now() + Duration::from_secs(15),
    );
    if let Err(msg) = ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("F163 retro telemetry: {msg}");
    }

    send_signal(child.id(), libc::SIGTERM).expect("send SIGTERM");

    let exit_deadline = Instant::now() + Duration::from_secs(6);
    let exited = wait_for_exit(&mut child, exit_deadline);
    let output = child.wait_with_output().expect("wait_with_output");
    let stdout_text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();
    let merged = format!("STDOUT:\n{stdout_text}\nSTDERR:\n{stderr_text}");

    assert!(
        exited.is_ok(),
        "F163 retro telemetry: daemon must exit within 6s of SIGTERM; \
         output-so-far:\n{merged}",
    );

    // The orchestrator graceful drain must execute. Without it we'd be
    // tearing down state non-cooperatively — a CLAUDE.md §三 violation
    // adjacent to "永不主动 kill 长 session".
    assert!(
        merged.contains("SIGTERM received"),
        "F163 retro telemetry: expected 'SIGTERM received' line; got:\n{merged}",
    );
    assert!(
        merged.contains("graceful shutdown complete"),
        "F163 retro telemetry: expected 'graceful shutdown complete' line; got:\n{merged}",
    );
}

/// F163 retro case 8 — `AgentTeamsWatcher::cancel_handle()` flipping
/// drives the blocking discovery loop to exit within the 500ms
/// `WATCHER_SHUTDOWN_POLL` floor (well under the 60s
/// `TEAMS_DISCOVERY_INTERVAL`). Direct unit test of the
/// crate-internal contract the daemon path relies on — a regression
/// that loses the cancel wiring inside `AgentTeamsWatcher::start` would
/// pass the SIGTERM tests above only via the defense-in-depth
/// `runtime.shutdown_timeout(5s)` fallback, but fail this one within
/// 600ms (no pool teardown involved).
#[tokio::test(flavor = "current_thread")]
async fn agent_teams_watcher_cancel_handle_stops_blocking_loop_fast() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let teams_root = tmp.path().join("teams");
    let tasks_root = tmp.path().join("tasks");
    let progress_path = tmp.path().join("progress.jsonl");
    std::fs::create_dir_all(&teams_root).unwrap();

    let cfg = ccteam_flow::AgentTeamsWatcherConfig {
        teams_root,
        tasks_root,
        progress_path,
        discovery_interval: Duration::from_secs(60),
    };
    let watcher = ccteam_flow::AgentTeamsWatcher::new(cfg).expect("new watcher");
    let cancel = watcher.cancel_handle();
    let handle = watcher.start();

    // Let the discovery loop reach its recv_timeout sleep.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let cancel_at = Instant::now();
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    // Join via blocking with a tight deadline. The loop polls cancel
    // each `WATCHER_SHUTDOWN_POLL` (500ms), so a generous 1500ms
    // ceiling catches any regression that bumps the poll interval or
    // moves the cancel check below recv_timeout.
    let join_res = tokio::time::timeout(Duration::from_millis(1500), handle).await;
    let elapsed = cancel_at.elapsed();
    assert!(
        join_res.is_ok(),
        "F163 retro: AgentTeamsWatcher blocking loop did not exit within 1500ms of cancel; \
         elapsed={:?}",
        elapsed,
    );
}
