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

/// `GET http://<addr>/health`, parsed. Hand-rolled over std so the test
/// needs no HTTP client: one loopback request, one JSON body.
fn http_get_json(addr: &str) -> serde_json::Value {
    use std::io::{Read, Write};
    let mut stream =
        std::net::TcpStream::connect(addr).unwrap_or_else(|err| panic!("connect {addr}: {err}"));
    stream
        .write_all(format!("GET /health HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes())
        .expect("write request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");
    let body = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no header/body split in {raw:?}"))
        .1;
    serde_json::from_str(body).unwrap_or_else(|err| panic!("parse {body:?}: {err}"))
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
    assert!(
        status["web_bind"]
            .as_str()
            .is_some_and(|b| b.starts_with("127.0.0.1:") && !b.ends_with(":0")),
        "the bind is the one the daemon SERVES, resolved from `:0`: {status}"
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
fn status_reports_the_bind_the_daemon_serves_not_the_one_that_was_requested() {
    // `--web-bind 127.0.0.1:0` asks for "any free port". The launcher's
    // pid record can only ever say `:0`; the daemon serves a concrete
    // port. `daemon status` must report the served one — a client that
    // dials what status reported has to reach the daemon.
    let sb = Sandbox::new();
    let started = sb.json(&[
        "start",
        "--json",
        "--web-bind",
        "127.0.0.1:0",
        "--dsh-web-bind",
        "off",
        "--no-imd",
    ]);
    assert_eq!(started["status"], "started", "{started}");

    let status = sb.json(&["daemon", "status", "--json"]);
    let reported = status["web_bind"]
        .as_str()
        .unwrap_or_else(|| panic!("web_bind must be reported: {status}"));
    assert_ne!(
        reported, "127.0.0.1:0",
        "status must not echo the REQUESTED bind: {status}"
    );
    let addr: std::net::SocketAddr = reported
        .parse()
        .unwrap_or_else(|err| panic!("web_bind {reported:?} must parse ({err}): {status}"));
    assert_ne!(
        addr.port(),
        0,
        "an ephemeral request resolves to a real port"
    );
    assert!(
        status["uptime_secs"].is_u64(),
        "uptime comes from the running daemon: {status}"
    );

    // The reported address is the one /health actually answers on, and it
    // is the same daemon (same pid, same home).
    let health = http_get_json(reported);
    assert_eq!(health["pid"], status["pid"], "{health} vs {status}");
    assert_eq!(health["home"], status["home"], "{health} vs {status}");
    assert_eq!(
        health["web_bind"], status["web_bind"],
        "{health} vs {status}"
    );
    assert_eq!(health["build"], status["build"], "{health} vs {status}");
    assert!(
        status["dsh_web_bind"].is_null(),
        "`off` stays absent, never the literal string: {status}"
    );

    sb.json(&["daemon", "stop", "--json"]);
    // Once it is down there is nothing to ask, so the served-only fields
    // go null rather than falling back to the recorded request.
    let after = sb.json(&["daemon", "status", "--json"]);
    assert_eq!(after["status"], "down", "{after}");
    assert!(after["web_bind"].is_null(), "{after}");
    assert!(after["uptime_secs"].is_null(), "{after}");
}

#[test]
#[cfg(unix)]
fn update_refuses_a_replay_restart_it_cannot_reconstruct() {
    // A pid record with no `args` (written by a launcher older than the
    // identity surface) used to fall through to the compiled-in defaults:
    // a daemon on 127.0.0.1:<port> came back on 0.0.0.0:7331 and /health
    // on the original port died. It must refuse instead — and refuse
    // WITHOUT stopping the running daemon.
    let sb = Sandbox::new();
    let started = sb.json(&[
        "start",
        "--json",
        "--web-bind",
        "127.0.0.1:0",
        // No web => no /health => nothing to reconstruct from either.
        "--dsh-web-bind",
        "off",
        "--no-web",
        "--no-imd",
    ]);
    assert_eq!(started["status"], "started", "{started}");
    let pid = started["pid"].as_u64().expect("pid");

    // Strip `args` exactly as a pre-v0.10.5 launcher would have left it.
    let pidfile = sb.ccteam_home.join("state").join("orchestrator.pid");
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pidfile).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("args");
    std::fs::write(&pidfile, serde_json::to_vec(&record).unwrap()).unwrap();

    let out = sb.run(&[
        "update",
        "--channel",
        "npm",
        "--binary",
        ccteam_bin(),
        "--json",
        "--now",
    ]);
    assert!(!out.status.success(), "a refusal must exit non-zero");
    let verdict: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json verdict");
    assert_eq!(verdict["status"], "error", "{verdict}");
    assert_eq!(verdict["code"], "notManaged", "{verdict}");
    let message = verdict["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("ccteam start --web-bind"),
        "the refusal must name the remedy: {verdict}"
    );
    assert!(
        !message.contains("0.0.0.0:7331"),
        "it must not offer a guessed bind: {verdict}"
    );

    // Still running, still the same process, still on its own bind.
    let status = sb.json(&["daemon", "status", "--json"]);
    assert_eq!(status["ready"], true, "the daemon must survive: {status}");
    assert_eq!(status["pid"].as_u64(), Some(pid), "{status}");
    sb.json(&["daemon", "stop", "--json"]);
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
