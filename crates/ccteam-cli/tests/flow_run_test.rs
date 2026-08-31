//! Card F0b — `ccteam flow run` end to end against a REAL daemon.
//!
//! Nothing here is mocked below the CLI: a live `ccteam` daemon serves
//! `POST /mcp`, the workflow's `agent()` calls are real `agent` /
//! `agent_read` / `agent_stop` tool calls authenticated with the machine's
//! enrollment credential, and the sessions they hire are real ccteam sessions
//! whose vendor is a deterministic fake `claude` (`CCTEAM_CLAUDE_BIN`).
//!
//! Two things are being proved:
//!
//! 1. **happy path** — a two-hire script produces a report with two ok agents,
//!    each carrying the sid of a session the daemon actually created.
//! 2. **resume** — a run poisoned after its first hire, re-run with
//!    `--resume`, replays that first call from the journal instead of hiring
//!    again. The fake vendor appends one line per process it is started as, so
//!    "no re-hire" is a count, not an inference.
//!
//! ## Isolation
//!
//! Every child process gets `HOME` **and** `CCTEAM_HOME` pointed into a
//! tempdir, and the test asserts the pinned `HOME` is not the operator's:
//! `CCTEAM_HOME` outranks `HOME` in ccteam's own resolver, so pinning only
//! `HOME` would have let a stray export write into the real `~/.ccteam`. This
//! file never calls `std::env::set_var` — cwd and env are process-global, so
//! isolation lives on the child (`Command::env` / `current_dir`) instead.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

/// A deterministic `claude` stream-json vendor, trimmed from the fixture in
/// `ccteam-harness/tests/claude_stream_json_test.rs`. Two differences that
/// matter here:
///
/// * `--version` is answered without logging, so the daemon's readiness probe
///   does not count as a session spawn;
/// * the spawn log is APPENDED to (the harness fixture truncates), which is
///   what makes it a count of vendor processes rather than a record of the
///   last one.
const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import sys, os, json
argv = sys.argv[1:]
if "--version" in argv or "-v" in argv:
    print("1.2.3 (fake claude)")
    sys.exit(0)
if "--no-chrome" not in argv:
    sys.stderr.write("fake-claude: missing --no-chrome\n")
    sys.exit(3)
mode = "session-id"; sid = ""
i = 0
while i < len(argv):
    a = argv[i]
    if a == "--session-id" and i + 1 < len(argv):
        sid = argv[i+1]; mode = "session-id"; i += 2
    elif a == "--resume" and i + 1 < len(argv):
        sid = argv[i+1]; mode = "resume"; i += 2
    else:
        i += 1
log = os.environ.get("FAKE_SJ_ARGV_LOG")
if log:
    with open(log, "a") as f:
        f.write(mode + " " + sid + "\n")
def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
init_line = sys.stdin.readline()
init_rid = "init"
try:
    init_rid = json.loads(init_line).get("request_id", "init")
except Exception:
    pass
emit({"type":"control_response","response":{"subtype":"success","request_id":init_rid,
      "response":{"commands":[],"models":[{"model":"fake-model"}]}}})
while True:
    line = sys.stdin.readline()
    if not line:
        break
    if not line.strip():
        continue
    try:
        ctl = json.loads(line)
    except Exception:
        ctl = None
    if isinstance(ctl, dict) and ctl.get("type") == "control_request":
        rid = ctl.get("request_id", "ctl")
        sub = (ctl.get("request") or {}).get("subtype", "")
        if sub == "get_context_usage":
            emit({"type":"control_response","response":{"subtype":"success","request_id":rid,
                  "response":{"totalTokens":2000,"maxTokens":200000,"percentage":1}}})
        else:
            emit({"type":"control_response","response":{"subtype":"success",
                  "request_id":rid,"response":{}}})
        continue
    # The reply quotes the task so the report can be checked against what was
    # actually asked, not just against "something came back".
    try:
        asked = json.loads(line).get("message", {}).get("content", "")
    except Exception:
        asked = ""
    if not isinstance(asked, str):
        asked = json.dumps(asked)
    reply = "answered: " + asked.strip()
    emit({"type":"assistant","session_id":sid,
          "message":{"role":"assistant","content":[{"type":"text","text":reply}]}})
    emit({"type":"result","subtype":"success","result":reply,"is_error":False,
          "total_cost_usd":0.001,"usage":{"input_tokens":7,"output_tokens":4},
          "session_id":sid})
"#;

/// Two hires, with a poison pill in between that only fires when `args.boom`
/// is set. That is what lets one script cover both the happy path and the
/// "died mid-run, resume" path without the two diverging.
const SCRIPT: &str = r#"export const meta = { name: 'flow-e2e', description: 'two hires through a live daemon' };

const first = await agent('say one', { vendor: 'claude', label: 'one' });
if (args && args.boom) { throw new Error('poisoned after the first hire'); }
const second = await agent('say two', { vendor: 'claude', label: 'two' });
return [first, second];
"#;

/// An isolated ccteam installation plus the project a workflow runs in.
struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    ccteam_home: PathBuf,
    project_dir: PathBuf,
    script: PathBuf,
    spawn_log: PathBuf,
    fake_claude: PathBuf,
    port: u16,
    daemon_started: bool,
}

impl Sandbox {
    fn new(slug: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let ccteam_home = home.join(".ccteam");
        let project_dir = home.join("projects").join(slug);
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        // The guard the CLAUDE.md post-mortem exists for: `CCTEAM_HOME` wins
        // over `HOME`, so an isolated `HOME` alone is not isolation.
        let real_home = dirs::home_dir().expect("a real home to compare against");
        assert_ne!(
            home, real_home,
            "the sandbox HOME must not be the operator's real home",
        );
        assert!(
            !ccteam_home.starts_with(&real_home),
            "the sandbox CCTEAM_HOME ({}) must not live under the real home ({})",
            ccteam_home.display(),
            real_home.display(),
        );

        let fake_claude = tmp.path().join("fake-claude.py");
        std::fs::write(&fake_claude, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let script = tmp.path().join("two-hires.js");
        std::fs::write(&script, SCRIPT).unwrap();

        Self {
            home,
            ccteam_home,
            project_dir,
            script,
            spawn_log: tmp.path().join("vendor-spawns.log"),
            fake_claude,
            // An ephemeral `:0` bind is not usable here: the daemon records
            // the MCP URL from the bind it was ASKED for, so port 0 would be
            // written into `run/mcp-url` verbatim. Reserve a real one.
            port: free_port(),
            daemon_started: false,
            _tmp: tmp,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(ccteam_bin());
        cmd.args(args)
            .env("HOME", &self.home)
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", self.home.join("projects"))
            .env("CCTEAM_CLAUDE_BIN", &self.fake_claude)
            .env("FAKE_SJ_ARGV_LOG", &self.spawn_log)
            .env("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "15000")
            .env("RUST_LOG", "warn");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run ccteam")
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
            panic!(
                "`ccteam {}` did not print JSON ({err}); stdout={stdout} stderr={}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr),
            )
        })
    }

    /// `ccteam init` in the project dir, then a daemon serving `/mcp`.
    fn start(&mut self, slug: &str) {
        let init = self
            .command(&["init", "--slug", slug])
            .current_dir(&self.project_dir)
            .output()
            .expect("run ccteam init");
        assert!(
            init.status.success(),
            "ccteam init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );

        let bind = format!("127.0.0.1:{}", self.port);
        // `--dsh-web-bind off`: the companion bind is otherwise derived as
        // web_port + 1, which is somebody else's port on a shared CI box.
        // The IM supervisor is deliberately NOT disabled: `--no-imd` also
        // means "no gateway", and the gateway IS the session ledger every
        // `agent` call needs ("no live gateway (standalone web has no session
        // ledger)"). With no bot credentials in the sandbox it just idles.
        // Flag BEFORE the spawn: `daemon stop` on a never-started daemon is
        // harmless, but a panic between spawn and a late flag leaks a daemon
        // into every later test on the box.
        self.daemon_started = true;
        let started = self.json(&[
            "start",
            "--json",
            "--web-bind",
            &bind,
            "--dsh-web-bind",
            "off",
            "--no-clipboard",
        ]);
        assert!(
            started["status"] == "started" || started["status"] == "alreadyRunning",
            "daemon did not start: {started}",
        );
    }

    /// Vendor processes started so far. One line per spawn, appended by the
    /// fake — the direct evidence that a resumed call did not re-hire.
    fn vendor_spawns(&self) -> usize {
        std::fs::read_to_string(&self.spawn_log)
            .map(|body| body.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    fn flow_run(&self, extra: &[&str], slug: &str, run_dir: &Path) -> (Output, Value) {
        let run_dir = run_dir.to_string_lossy().to_string();
        let mut args = vec![
            "flow",
            "run",
            self.script.to_str().unwrap(),
            "--project",
            slug,
        ];
        // `--resume` and `--run-dir` name the same directory but mean
        // different things; the caller picks which one it is passing.
        if extra.contains(&"--resume") {
            args.push("--resume");
            args.push(&run_dir);
        } else {
            args.push("--run-dir");
            args.push(&run_dir);
        }
        args.extend(extra.iter().filter(|a| **a != "--resume").copied());
        let out = self.run(&args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let report = serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
            panic!(
                "flow run printed no report ({err}); stdout={stdout} stderr={}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out, report)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A panicking assertion must not leak a daemon into the next test.
        if self.daemon_started {
            let _ = self.command(&["daemon", "stop", "--json"]).output();
        }
    }
}

/// Reserve a port by binding and releasing it. Racy in principle; in practice
/// the kernel does not hand the same ephemeral port out twice in a row, and
/// the alternative (`:0`) is unusable — see `Sandbox::new`.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
    listener.local_addr().expect("port").port()
}

/// (a) Happy path: two hires through the daemon, two ok agents in the report,
/// each with a real sid.
#[test]
fn flow_run_hires_two_agents_through_the_daemon() {
    let slug = "flowdemo";
    let mut sb = Sandbox::new(slug);
    sb.start(slug);

    let run_dir = sb.home.join("run-a");
    let (out, report) = sb.flow_run(&[], slug, &run_dir);

    assert!(
        out.status.success(),
        "flow run should exit 0 on a clean run; report={report} stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(report["ok"], Value::Bool(true), "{report}");
    assert_eq!(report["name"], "flow-e2e", "{report}");
    assert_eq!(report["totals"]["agents"], 2, "{report}");

    let agents = report["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 2, "{report}");
    for agent in agents {
        assert_eq!(agent["ok"], Value::Bool(true), "{agent}");
        assert_eq!(agent["vendor"], "claude", "{agent}");
        assert_eq!(agent["cached"], Value::Bool(false), "{agent}");
        let sid = agent["sid"].as_str().unwrap_or_default();
        assert!(
            sid.starts_with('s') && sid.len() > 1,
            "every hire must carry the daemon's own sid: {agent}",
        );
    }
    assert_ne!(
        agents[0]["sid"], agents[1]["sid"],
        "two hires are two sessions: {report}",
    );

    // The script returns the two answers; the fake echoes the task, so this
    // also proves the task text survived the whole round trip.
    let returned = report["returned"].as_array().expect("returned array");
    assert!(
        returned[0].as_str().unwrap_or_default().contains("say one"),
        "{report}",
    );
    assert!(
        returned[1].as_str().unwrap_or_default().contains("say two"),
        "{report}",
    );

    // Progress is the operator's half of the answer and goes to stderr, so a
    // `> report.json` redirect still shows the run happening.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("agent #0 start"), "stderr={stderr}");
    assert!(stderr.contains("run finished"), "stderr={stderr}");
}

/// (b) Resume: a run poisoned after its first hire replays that call from the
/// journal instead of paying for it twice.
#[test]
fn a_resumed_run_replays_the_first_call_instead_of_re_hiring() {
    let slug = "flowresume";
    let mut sb = Sandbox::new(slug);
    sb.start(slug);

    let run_dir = sb.home.join("run-b");
    let (first, report) = sb.flow_run(&["--args", r#"{"boom":true}"#], slug, &run_dir);
    assert!(
        !first.status.success(),
        "a script that throws must exit non-zero: {report}",
    );
    assert_eq!(report["ok"], Value::Bool(false), "{report}");
    assert!(
        report["script_error"]
            .as_str()
            .unwrap_or_default()
            .contains("poisoned"),
        "the report must say what killed the run: {report}",
    );
    assert_eq!(report["totals"]["agents"], 1, "{report}");
    let after_first = sb.vendor_spawns();
    assert_eq!(after_first, 1, "the first run hires exactly one vendor");

    // Same directory, no `--args`: the poison is gone, so the script gets past
    // the first call — which the journal must answer for free.
    let (second, resumed) = sb.flow_run(&["--resume"], slug, &run_dir);
    assert!(
        second.status.success(),
        "the resumed run should complete; report={resumed} stderr={}",
        String::from_utf8_lossy(&second.stderr),
    );
    assert_eq!(resumed["ok"], Value::Bool(true), "{resumed}");
    assert_eq!(resumed["cache"]["hits"], 1, "{resumed}");
    assert_eq!(resumed["totals"]["agents"], 2, "{resumed}");

    let agents = resumed["agents"].as_array().expect("agents array");
    assert_eq!(
        agents[0]["cached"],
        Value::Bool(true),
        "call 0 must come from the journal: {resumed}",
    );
    assert_eq!(
        agents[1]["cached"],
        Value::Bool(false),
        "call 1 was never made before: {resumed}",
    );

    // The count is the proof: one more vendor process, not two.
    assert_eq!(
        sb.vendor_spawns(),
        after_first + 1,
        "a resumed call must not start a second vendor for work already paid for",
    );

    // A replayed call spends nothing THIS run, even though its record keeps the
    // original cost.
    assert_eq!(
        resumed["totals"]["cost_usd"].as_f64().unwrap_or(-1.0),
        agents[1]["cost_usd"].as_f64().unwrap_or(-1.0),
        "a replay's money was spent by the run that first made the call: {resumed}",
    );
}
