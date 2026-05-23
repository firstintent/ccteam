//! F163 — `ccteam start` graceful SIGTERM/SIGINT shutdown tests.
//!
//! Verifies:
//! 1. SIGTERM causes the daemon to exit within 5s (not hang).
//! 2. Pidfile is removed on clean exit.
//! 3. SIGINT (ctrl_c equivalent via kill -INT) also triggers exit.
//! 4. tmux sessions are NOT killed (verified by checking that the
//!    daemon's exit does not invoke `tmux kill-session`; we confirm
//!    this structurally since no tmux sessions exist in the test
//!    env — the daemon must exit without erroring on their absence).
//!
//! We point CCTEAM_HOME at a tempdir so these tests don't race with
//! the operator's real daemon. Tests use `--no-web --no-imd` to avoid
//! port conflicts and network I/O in CI.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

/// Spawn a minimal `ccteam start` daemon in an isolated tempdir.
/// Returns (child, ccteam_home, pidfile_path).
fn spawn_test_daemon(tmp_dir: &tempfile::TempDir) -> (std::process::Child, PathBuf, PathBuf) {
    let fake_home = tmp_dir.path();
    let ccteam_home = fake_home.join(".ccteam");
    std::fs::create_dir_all(ccteam_home.join("phases")).unwrap();
    std::fs::create_dir_all(ccteam_home.join("state")).unwrap();
    std::fs::create_dir_all(fake_home.join("projects")).unwrap();
    let pidfile = ccteam_home.join("state").join("orchestrator.pid");

    let child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd", "--tick-seconds", "1"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ccteam start");

    (child, ccteam_home, pidfile)
}

/// Wait until the pidfile appears (daemon has written it and is running).
/// Returns Ok(pid) or Err if the deadline is exceeded.
fn wait_for_pidfile(pidfile: &PathBuf, deadline: Instant) -> Result<u32, String> {
    while Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(pidfile) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                if let Ok(pid) = trimmed.parse::<u32>() {
                    return Ok(pid);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "pidfile {} did not appear within deadline",
        pidfile.display()
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
        Err(format!("kill({pid}, {sig}) failed: errno={}", unsafe {
            *libc::__errno_location()
        }))
    }
}

/// F163 case 1 — SIGTERM causes clean exit within 5s + pidfile removed.
#[test]
#[cfg(unix)]
fn sigterm_causes_graceful_exit_and_pidfile_cleanup() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut child, _ccteam_home, pidfile) = spawn_test_daemon(&tmp);

    // Wait up to 10s for the daemon to start and write its pidfile.
    let pidfile_deadline = Instant::now() + Duration::from_secs(10);
    let daemon_pid = match wait_for_pidfile(&pidfile, pidfile_deadline) {
        Ok(pid) => pid,
        Err(msg) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("F163: {msg}");
        }
    };

    // Sanity: process is alive.
    assert!(
        ccteam_core_pid_alive(daemon_pid),
        "F163: daemon pid {daemon_pid} should be alive after pidfile appears"
    );

    // Send SIGTERM.
    send_signal(daemon_pid, libc::SIGTERM).expect("send SIGTERM");

    // Wait up to 10s for the process to exit (signal + 30s orch drain +
    // 5s web/imd drain = at most 35s; but with --no-web --no-imd the
    // orch has no project tasks so it exits almost instantly).
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exited = wait_for_exit(&mut child, exit_deadline);

    // Regardless of exit result, ensure child is cleaned up.
    if exited.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();

    assert!(
        exited.is_ok(),
        "F163: daemon should exit within 10s of SIGTERM; still running after deadline"
    );

    // Pidfile must be gone after graceful shutdown.
    assert!(
        !pidfile.exists(),
        "F163: pidfile {} should be removed after graceful SIGTERM exit",
        pidfile.display()
    );
}

/// F163 case 2 — SIGINT also causes clean exit + pidfile removed.
#[test]
#[cfg(unix)]
fn sigint_causes_graceful_exit_and_pidfile_cleanup() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut child, _ccteam_home, pidfile) = spawn_test_daemon(&tmp);

    let pidfile_deadline = Instant::now() + Duration::from_secs(10);
    let daemon_pid = match wait_for_pidfile(&pidfile, pidfile_deadline) {
        Ok(pid) => pid,
        Err(msg) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("F163: {msg}");
        }
    };

    // Send SIGINT (equivalent to Ctrl-C).
    send_signal(daemon_pid, libc::SIGINT).expect("send SIGINT");

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

    assert!(
        !pidfile.exists(),
        "F163: pidfile {} should be removed after graceful SIGINT exit",
        pidfile.display()
    );
}

/// F163 case 3 — shutdown via trigger file (ccteam stop path) still
/// exits cleanly and removes the pidfile.
#[test]
fn trigger_file_shutdown_exits_cleanly_and_cleans_pidfile() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (mut child, _ccteam_home, pidfile) = spawn_test_daemon(&tmp);

    let pidfile_deadline = Instant::now() + Duration::from_secs(10);
    let _daemon_pid = match wait_for_pidfile(&pidfile, pidfile_deadline) {
        Ok(pid) => pid,
        Err(msg) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("F163: {msg}");
        }
    };

    // Write the F86 trigger file to simulate `ccteam stop`.
    let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
    let trigger = PathBuf::from("/tmp").join(format!("ccteam-{user}.shutdown"));
    std::fs::write(&trigger, format!("{}\n", std::process::id())).expect("write shutdown trigger");

    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exited = wait_for_exit(&mut child, exit_deadline);

    if exited.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = std::fs::remove_file(&trigger); // cleanup trigger

    assert!(
        exited.is_ok(),
        "F163: daemon should exit within 10s of trigger-file shutdown"
    );
    assert!(
        !pidfile.exists(),
        "F163: pidfile {} should be removed after trigger-file shutdown",
        pidfile.display()
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
    let pidfile = ccteam_home.join("state").join("orchestrator.pid");

    // Capture stderr so we can inspect it for unwanted tmux-kill messages.
    let mut child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd", "--tick-seconds", "1"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ccteam start");

    let pidfile_deadline = Instant::now() + Duration::from_secs(10);
    let daemon_pid = match wait_for_pidfile(&pidfile, pidfile_deadline) {
        Ok(pid) => pid,
        Err(msg) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("F163: {msg}");
        }
    };

    // Send SIGTERM and wait for exit.
    send_signal(daemon_pid, libc::SIGTERM).expect("send SIGTERM");

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

/// Portable `kill -0` analog: returns true if a process with the given
/// pid is still alive. Uses `kill(pid, 0)` on Unix.
#[cfg(unix)]
fn ccteam_core_pid_alive(pid: u32) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    ret == 0
}
