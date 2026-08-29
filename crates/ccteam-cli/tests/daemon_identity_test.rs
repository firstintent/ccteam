//! v0.10.5 PLUG-1 — the daemon's identity surface, end to end.
//!
//! A DSH plugin that finds a ccteam already running has to answer one
//! question before it attaches: *is this MY engine* — same
//! `$CCTEAM_HOME`, same version? These tests pin the two places it can
//! ask: the `alreadyRunning` verdict of `ccteam daemon start --json` and
//! `ccteam daemon status --json`.
//!
//! They boot a REAL daemon, so every knob is pinned to a tempdir (`HOME`,
//! `CCTEAM_HOME`, `CCTEAM_PROJECTS_ROOT`) and the web bind is
//! `127.0.0.1:0` — a fixed port would fight the developer's own daemon.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    ccteam_home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let ccteam_home = home.join(".ccteam");
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(home.join("projects")).unwrap();
        Self {
            _tmp: tmp,
            home,
            ccteam_home,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(ccteam_bin())
            .args(args)
            .env("HOME", &self.home)
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", self.home.join("projects"))
            .env("RUST_LOG", "warn")
            .output()
            .expect("run ccteam")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.run(args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
            panic!(
                "`ccteam {}` did not print one JSON line ({err}).\n--- stdout ---\n{stdout}\n\
                 --- stderr ---\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}

/// The canonical form of a path, matching what the daemon reports.
fn canonical(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

#[test]
#[cfg(unix)]
fn second_start_reports_already_running_with_pid_and_home() {
    let sb = Sandbox::new();
    let start_args = [
        "start",
        "--json",
        "--web-bind",
        "127.0.0.1:0",
        "--dsh-web-bind",
        "off",
        "--no-imd",
    ];

    let first = sb.json(&start_args);
    assert_eq!(first["status"], "started", "first start: {first}");
    let started_pid = first["pid"].as_u64().expect("started carries a pid");
    assert_eq!(
        first["home"],
        canonical(&sb.ccteam_home),
        "both verdicts carry home so a plugin needs only one code path: {first}"
    );

    // Second start = the plugin's "somebody beat me to it" path.
    let second = sb.json(&start_args);
    assert_eq!(second["status"], "alreadyRunning", "second start: {second}");
    assert_eq!(
        second["pid"].as_u64(),
        Some(started_pid),
        "alreadyRunning must name the RUNNING pid, not this launcher's: {second}"
    );
    assert_eq!(second["home"], canonical(&sb.ccteam_home), "{second}");

    // `daemon status --json` answers the same identity locally, plus the
    // absolute path of the binary that serves it.
    let status = sb.json(&["daemon", "status", "--json"]);
    assert_eq!(status["status"], "ok", "{status}");
    assert_eq!(status["pid"].as_u64(), Some(started_pid), "{status}");
    assert_eq!(status["home"], canonical(&sb.ccteam_home), "{status}");
    assert!(status["version"].as_str().is_some(), "{status}");
    assert!(status["uptime_secs"].is_u64(), "{status}");
    assert_eq!(
        status["web_bind"].as_str(),
        Some("127.0.0.1:0"),
        "the bind comes from the launcher's recorded argv: {status}"
    );
    assert!(
        status["dsh_web_bind"].is_null(),
        "`off` is reported as absent, not as the literal string: {status}"
    );
    let binary = status["binary"].as_str().expect("binary path");
    assert!(
        Path::new(binary).is_absolute(),
        "binary must be absolute: {status}"
    );

    // `daemon stop` semantics are unchanged: it stops the managed one.
    let stopped = sb.json(&["daemon", "stop", "--json"]);
    assert_eq!(stopped["status"], "stopped", "{stopped}");
    assert_eq!(stopped["pid"].as_u64(), Some(started_pid), "{stopped}");
}

#[test]
#[cfg(unix)]
fn start_is_the_launcher_and_returns_without_leaving_a_foreground_process() {
    // v0.10.5 D7 — `nohup ccteam start &` used to leave an UNMANAGED
    // daemon: `daemon stop` refused it and no plugin could manage it.
    // Now `start` is the launcher, so the invoked process exits on its
    // own once the detached daemon is ready, and `ccteam stop` works.
    let sb = Sandbox::new();
    let started = sb.json(&[
        "start",
        "--json",
        "--web-bind",
        "127.0.0.1:0",
        "--dsh-web-bind",
        "off",
        "--no-imd",
        "--no-clipboard",
    ]);
    assert_eq!(started["status"], "started", "{started}");
    let pid = started["pid"].as_u64().expect("pid");

    // The daemon is MANAGED — the whole point of deleting the foreground
    // start — so the plain `ccteam stop` alias can end it.
    let status = sb.json(&["daemon", "status", "--json"]);
    assert_eq!(status["managed"], true, "{status}");
    assert_eq!(status["pid"].as_u64(), Some(pid), "{status}");

    let stop = sb.run(&["stop"]);
    assert!(
        stop.status.success(),
        "`ccteam stop` must stop a launcher-started daemon: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let after = sb.json(&["daemon", "status", "--json"]);
    assert_eq!(after["status"], "down", "{after}");
}
