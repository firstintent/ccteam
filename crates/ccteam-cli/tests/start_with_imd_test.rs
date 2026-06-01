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
//!    background against an empty CCTEAM_HOME, observes the IMD
//!    heartbeat file appear (proving the supervisor task spawned),
//!    then `ccteam stop`s gracefully. With `--no-imd` set the
//!    heartbeat must NOT appear.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

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

/// V0.6.1 F130 smoke — `ccteam start --no-web` should bring up the IMD
/// supervisor as a tokio task inside the gateway process and publish
/// `~/.ccteam/state/imd.heartbeat`. With `--no-imd`, no fresh heartbeat
/// is observed.
///
/// The heartbeat is written **once at daemon boot** (the v8.1 gateway
/// daemon has no supervisor tick), so observing it is really measuring
/// "process spawned + booted past the heartbeat write" — a one-shot
/// startup latency that is ~60ms on an idle host. Under heavy parallel
/// test load the cold-start of the (large) debug binary plus tokio
/// runtime init can stretch that out, so [`READY_TIMEOUT`] is
/// deliberately generous and load-insensitive: we only assert the
/// supervisor *eventually* publishes a heartbeat, never that it is fast.
/// A daemon that dies during boot is caught via `try_wait` so the test
/// fails fast with the child's captured stderr instead of burning the
/// whole timeout polling for a heartbeat that will never appear.
///
/// We point HOME at a tempdir so the heartbeat lands in isolation and
/// the test doesn't race with the operator's real daemon (if any).
#[test]
fn start_spawns_imd_supervisor_unless_no_imd_set() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path();
    // Need at least an empty projects dir so `ccteam start` doesn't
    // bail early on missing config; CCTEAM_HOME drives ccteam state,
    // HOME drives where the IMD heartbeat lands (via dirs::home_dir).
    let ccteam_home = fake_home.join(".ccteam");
    std::fs::create_dir_all(ccteam_home.join("phases")).unwrap();
    std::fs::create_dir_all(fake_home.join("projects")).unwrap();
    let heartbeat = ccteam_home.join("state").join("imd.heartbeat");
    let mcp_socket = ccteam_home.join("run").join("mcp.sock");
    assert!(
        !heartbeat.exists(),
        "tempdir-isolated heartbeat must start absent",
    );

    // --- Case 1: default (no --no-imd) — heartbeat MUST appear -----
    // Capture the daemon's stderr to a file (not /dev/null) so a boot
    // failure is diagnosable from the assertion message instead of
    // surfacing as a silent timeout.
    let stderr_log = ccteam_home.join("daemon-case1.stderr.log");
    let started_at = SystemTime::now();
    let mut child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--tick-seconds", "1"])
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

    let (observed, observed_mcp) = await_supervisor_ready(
        &mut child,
        &heartbeat,
        &mcp_socket,
        started_at,
        READY_TIMEOUT,
    );
    // Record whether the daemon exited on its own (crash / early bail)
    // before we issue the teardown trigger — folded into the assertion
    // message so a real boot failure is obvious rather than looking
    // like a slow heartbeat.
    let early_exit = child.try_wait().ok().flatten();

    // Tear down — write the F86 shutdown trigger.
    let trigger = trigger_path();
    let _ = std::fs::write(&trigger, format!("{child_pid}\n"));
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
    let _ = std::fs::remove_file(&trigger);

    let daemon_stderr = std::fs::read_to_string(&stderr_log).unwrap_or_default();
    assert!(
        observed,
        "F130: IMD heartbeat at {} was not published by the in-process supervisor within {}s \
         (observed_mcp={observed_mcp}, daemon early_exit={early_exit:?}).\n\
         The heartbeat is written once at daemon boot, so this means the daemon never reached \
         that point.\n--- daemon stderr ---\n{}",
        heartbeat.display(),
        READY_TIMEOUT.as_secs(),
        daemon_stderr.trim(),
    );
    assert!(
        observed_mcp,
        "v8.1: MCP socket at {} should be served by `ccteam start` \
         (daemon early_exit={early_exit:?}).\n--- daemon stderr ---\n{}",
        mcp_socket.display(),
        daemon_stderr.trim(),
    );

    // --- Case 2: --no-imd — heartbeat must NOT appear --------------
    let _ = std::fs::remove_file(&heartbeat);
    let no_imd_started = SystemTime::now();
    let mut child2 = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--no-imd", "--tick-seconds", "1"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ccteam start --no-imd");
    let child2_pid = child2.id();

    // Wait at least 6s (>3 supervisor ticks) to make sure we'd have
    // seen the heartbeat if it were going to appear.
    std::thread::sleep(Duration::from_secs(6));
    let stale = match std::fs::metadata(&heartbeat) {
        Ok(meta) => meta.modified().map(|m| m < no_imd_started).unwrap_or(true),
        Err(_) => true,
    };

    let _ = std::fs::write(&trigger, format!("{child2_pid}\n"));
    let drain_deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < drain_deadline {
        match child2.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break,
        }
    }
    if child2.try_wait().ok().flatten().is_none() {
        let _ = child2.kill();
        let _ = child2.wait();
    }
    let _ = std::fs::remove_file(&trigger);

    assert!(
        stale,
        "F130: with --no-imd, supervisor task must be skipped — observed fresh heartbeat at {}",
        heartbeat.display()
    );
}

/// Generous, load-insensitive budget for the daemon to publish its
/// first (and only) boot heartbeat + bind the MCP socket. The happy
/// path resolves in tens of milliseconds; this ceiling exists purely so
/// a genuinely-stuck daemon eventually fails the test rather than
/// hanging forever. It must NOT be tightened to "catch slow boots" — a
/// slow-but-successful boot under parallel load is a pass, not a bug.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll until the IMD heartbeat is fresh (`mtime >= since`) **and** the
/// MCP socket is bound, or `timeout` elapses, or `child` exits early.
///
/// Returns `(observed_heartbeat, observed_mcp)`. The early-exit check is
/// the key robustness property: a daemon that crashes during boot is
/// detected within one poll interval, so the caller fails fast with the
/// child's captured stderr instead of polling a never-coming heartbeat
/// for the full timeout and then reporting a misleading "within Ns".
fn await_supervisor_ready(
    child: &mut Child,
    heartbeat: &Path,
    mcp_socket: &Path,
    since: SystemTime,
    timeout: Duration,
) -> (bool, bool) {
    let deadline = Instant::now() + timeout;
    let mut observed = false;
    let mut observed_mcp = false;
    while Instant::now() < deadline {
        if !observed {
            if let Ok(meta) = std::fs::metadata(heartbeat) {
                if let Ok(mtime) = meta.modified() {
                    observed = mtime >= since;
                }
            }
        }
        if !observed_mcp {
            observed_mcp = mcp_socket.exists();
        }
        if observed && observed_mcp {
            break;
        }
        // Bail the instant the daemon dies — no point waiting out the
        // timeout for signals a dead process can no longer emit.
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (observed, observed_mcp)
}

/// Mirror of `crates/ccteam-cli/src/main.rs::shutdown_trigger_path` so
/// the test doesn't need to depend on internal CLI items.
fn trigger_path() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
    PathBuf::from("/tmp").join(format!("ccteam-{user}.shutdown"))
}
