//! V0.6.1 F130 — `ccteam start` folds the IMD supervisor in-process.
//!
//! Two surface-level guarantees we check here without spinning up
//! the full daemon (that would require a registered project, tmux,
//! etc.):
//!
//! 1. `ccteam start --help` advertises `--no-imd` (mirror of the
//!    pre-existing `--no-web`) so the operator switch survives any
//!    accidental rename of the clap arg.
//! 2. The standalone `ccteam-imd` binary no longer exists in the
//!    cargo workspace — F130 removed the `[[bin]]` from
//!    `ccteam-imd/Cargo.toml` and deleted `src/main.rs`. Cargo's
//!    `CARGO_BIN_EXE_<name>` env var is only emitted for binaries
//!    declared in the workspace; absence is the cleanest proof.
//! 3. An end-to-end smoke that boots `ccteam start --no-web` in the
//!    background against an empty CCTEAM_HOME, observes the IMD
//!    heartbeat file appear (proving the supervisor task spawned),
//!    then `ccteam stop`s gracefully. With `--no-imd` set the
//!    heartbeat must NOT appear.

use std::path::PathBuf;
use std::process::{Command, Stdio};
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
fn standalone_ccteam_imd_binary_no_longer_exists() {
    // F130: `[[bin]]` was removed from crates/ccteam-imd/Cargo.toml.
    // Cargo only injects CARGO_BIN_EXE_ccteam-imd at compile time
    // when the workspace declares such a binary; we should NOT find
    // an executable at any historical release path either.
    let candidates = [
        PathBuf::from("target/debug/ccteam-imd"),
        PathBuf::from("target/release/ccteam-imd"),
    ];
    // CARGO_TARGET_DIR redirection (e.g. shared-target workspaces) is
    // optional; if set, also check there.
    let mut search: Vec<PathBuf> = candidates.to_vec();
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        search.push(PathBuf::from(&td).join("debug/ccteam-imd"));
        search.push(PathBuf::from(&td).join("release/ccteam-imd"));
    }
    for p in &search {
        assert!(
            !p.is_file(),
            "F130: standalone ccteam-imd binary should be gone, but found {}",
            p.display()
        );
    }
}

/// V0.6.1 F130 smoke — `ccteam start --no-web` should bring up the IMD
/// supervisor as a tokio task inside the orchestrator process and
/// refresh `~/.ccteam/state/imd.heartbeat` within ~5s. With `--no-imd`,
/// no fresh heartbeat is observed.
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
    assert!(
        !heartbeat.exists(),
        "tempdir-isolated heartbeat must start absent",
    );

    // --- Case 1: default (no --no-imd) — heartbeat MUST appear -----
    let started_at = SystemTime::now();
    let mut child = Command::new(ccteam_bin())
        .args(["start", "--no-web", "--tick-seconds", "1"])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ccteam start");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut observed = false;
    while Instant::now() < deadline {
        if let Ok(meta) = std::fs::metadata(&heartbeat) {
            if let Ok(mtime) = meta.modified() {
                if mtime >= started_at {
                    observed = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Tear down — write the F86 shutdown trigger.
    let trigger = trigger_path();
    let _ = std::fs::write(&trigger, format!("{}\n", std::process::id()));
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

    assert!(
        observed,
        "F130: IMD heartbeat at {} should have been refreshed by the in-process supervisor within 15s",
        heartbeat.display()
    );

    // --- Case 2: --no-imd — heartbeat must NOT appear --------------
    let _ = std::fs::remove_file(&heartbeat);
    let no_imd_started = SystemTime::now();
    let mut child2 = Command::new(ccteam_bin())
        .args([
            "start",
            "--no-web",
            "--no-imd",
            "--tick-seconds",
            "1",
        ])
        .env("HOME", fake_home)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", fake_home.join("projects"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ccteam start --no-imd");

    // Wait at least 6s (>3 supervisor ticks) to make sure we'd have
    // seen the heartbeat if it were going to appear.
    std::thread::sleep(Duration::from_secs(6));
    let stale = match std::fs::metadata(&heartbeat) {
        Ok(meta) => meta
            .modified()
            .map(|m| m < no_imd_started)
            .unwrap_or(true),
        Err(_) => true,
    };

    let _ = std::fs::write(&trigger, format!("{}\n", std::process::id()));
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

/// Mirror of `crates/ccteam-cli/src/main.rs::shutdown_trigger_path` so
/// the test doesn't need to depend on internal CLI items.
fn trigger_path() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
    PathBuf::from("/tmp").join(format!("ccteam-{user}.shutdown"))
}
