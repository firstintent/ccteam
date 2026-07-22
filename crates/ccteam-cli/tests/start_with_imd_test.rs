//! V0.6.1 F130 — `ccteam start` folds the IM supervisor in-process.
//!
//! Two surface-level guarantees we check here without spinning up
//! the full daemon (that would require a registered project, tmux,
//! etc.):
//!
//! 1. `ccteam start --help` advertises `--no-imd` (mirror of the
//!    pre-existing `--no-web`) so the operator switch survives any
//!    accidental rename of the clap arg.
//! 2. The standalone `ccteam-im` binary no longer exists in the
//!    cargo workspace — F130 removed the `[[bin]]` from
//!    `ccteam-im/Cargo.toml` and deleted `src/main.rs`. Cargo's
//!    `CARGO_BIN_EXE_<name>` env var is only emitted for binaries
//!    declared in the workspace; absence is the cleanest proof.
//! 3. An end-to-end smoke that boots `ccteam start --no-web` in the
//!    background against an empty CCTEAM_HOME, observes the daemon MCP
//!    socket accept connections, then `ccteam stop`s gracefully. With
//!    `--no-imd` set the MCP socket must still accept connections.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

#[test]
fn start_help_advertises_no_imd_flag() {
    let out = Command::new(ccteam_bin())
        .args(["start", "--help"])
        .output()
        .expect("spawn ccteam start --help");
    assert!(out.status.success(), "ccteam start --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--no-imd"),
        "help text should mention --no-imd; got: {stdout}",
    );
    assert!(
        stdout.contains("--no-web"),
        "help text should still mention --no-web; got: {stdout}",
    );
}

#[test]
fn standalone_ccteam_im_binary_no_longer_exists() {
    // F130: `[[bin]]` was removed from crates/ccteam-im/Cargo.toml.
    // Cargo only injects CARGO_BIN_EXE_ccteam-im at compile time
    // when the workspace declares such a binary; we should NOT find
    // an executable at any historical release path either.
    let candidates = [
        PathBuf::from("target/debug/ccteam-im"),
        PathBuf::from("target/release/ccteam-im"),
    ];
    // CARGO_TARGET_DIR redirection (e.g. shared-target workspaces) is
    // optional; if set, also check there.
    let mut search: Vec<PathBuf> = candidates.to_vec();
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        search.push(PathBuf::from(&td).join("debug/ccteam-im"));
        search.push(PathBuf::from(&td).join("release/ccteam-im"));
    }
    for p in &search {
        assert!(
            !p.is_file(),
            "F130: standalone ccteam-im binary should be gone, but found {}",
            p.display()
        );
    }
}

/// V0.6.1 F130 smoke, updated for gateway liveness: `ccteam start
/// --no-web` must bind the MCP socket and accept connections. The MCP
/// socket is the daemon liveness signal; stale socket files are not
/// enough. A daemon that dies during boot is caught via `try_wait` so
/// the test fails fast with the child's captured stderr instead of
/// burning the whole timeout polling for a signal a dead process can no
/// longer emit.
#[test]
fn start_spawns_imd_supervisor_unless_no_imd_set() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path();
    // Need at least an empty projects dir so `ccteam start` doesn't
    // bail early on missing config.
    let ccteam_home = fake_home.join(".ccteam");
    std::fs::create_dir_all(ccteam_home.join("phases")).unwrap();
    std::fs::create_dir_all(fake_home.join("projects")).unwrap();
    let heartbeat = ccteam_home.join("state").join("imd.heartbeat");
    let mcp_socket = ccteam_home.join("run").join("mcp.sock");
    assert!(
        !heartbeat.exists(),
        "tempdir-isolated heartbeat must start absent",
    );

    // --- Case 1: default (no --no-imd) — MCP socket MUST accept -----
    // Capture the daemon's stderr to a file (not /dev/null) so a boot
    // failure is diagnosable from the assertion message instead of
    // surfacing as a silent timeout.
    let stderr_log = ccteam_home.join("daemon-case1.stderr.log");
    let mut child = Command::new(ccteam_bin())
        .args(["start", "--no-web"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(&stderr_log).expect("create daemon stderr log"),
        ))
        .spawn()
        .expect("spawn ccteam start");
    let child_pid = child.id();

    let observed_mcp = await_mcp_socket_reachable(&mut child, &mcp_socket, READY_TIMEOUT);
    // Record whether the daemon exited on its own (crash / early bail)
    // before teardown — folded into the assertion message so a real
    // boot failure is obvious rather than looking like a slow bind.
    let early_exit = child.try_wait().ok().flatten();

    // Tear down — SIGTERM, the (only) graceful stop signal since v0.9.7.
    sigterm_and_drain(&mut child, child_pid);

    let daemon_stderr = std::fs::read_to_string(&stderr_log).unwrap_or_default();
    assert!(
        observed_mcp,
        "MCP socket at {} should accept connections from `ccteam start` \
         (daemon early_exit={early_exit:?}).\n--- daemon stderr ---\n{}",
        mcp_socket.display(),
        daemon_stderr.trim(),
    );
    assert!(
        !heartbeat.exists(),
        "global IMD heartbeat must not be written; liveness is MCP socket reachability"
    );

    // --- Case 2: --no-imd — MCP socket still accepts --------------
    let _ = std::fs::remove_file(&heartbeat);
    let _ = std::fs::remove_file(&mcp_socket);
    let mut child2 = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ccteam start --no-imd");
    let child2_pid = child2.id();

    let observed_no_imd_mcp = await_mcp_socket_reachable(&mut child2, &mcp_socket, READY_TIMEOUT);

    sigterm_and_drain(&mut child2, child2_pid);

    assert!(
        observed_no_imd_mcp,
        "--no-imd must still serve the MCP socket"
    );
    assert!(
        !heartbeat.exists(),
        "--no-imd must not create the retired global IMD heartbeat"
    );
}

/// SIGTERM the daemon and wait for it to drain (≤35s), escalating to a
/// hard kill only as test cleanup of last resort.
#[cfg(unix)]
fn sigterm_and_drain(child: &mut Child, pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    let drain_deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < drain_deadline {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break,
        }
    }
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Generous, load-insensitive budget for the daemon to publish its MCP
/// socket. The happy path resolves in tens of milliseconds; this
/// ceiling exists purely so a genuinely-stuck daemon eventually fails
/// the test rather than hanging forever. It must NOT be tightened to
/// "catch slow boots" — a slow-but-successful boot under parallel load
/// is a pass, not a bug.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll until the MCP socket accepts a real connection, or `timeout`
/// elapses, or `child` exits early.
///
/// The early-exit check is the key robustness property: a daemon that
/// crashes during boot is detected within one poll interval, so the
/// caller fails fast with the child's captured stderr.
fn await_mcp_socket_reachable(child: &mut Child, mcp_socket: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if mcp_socket_reachable(mcp_socket) {
            return true;
        }
        // Bail the instant the daemon dies — no point waiting out the
        // timeout for signals a dead process can no longer emit.
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(unix)]
fn mcp_socket_reachable(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn mcp_socket_reachable(_path: &Path) -> bool {
    false
}
