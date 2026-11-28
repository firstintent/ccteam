//! GitHub #220 — a delegation completion notification reaches a BUSY Claude
//! stream-json parent at once, as a line injected into the turn it is running,
//! exactly the way a human's steer does. It is never parked behind that turn.
//!
//! Parking it made a planner that stayed in one long turn (6h40m in the field
//! report) see none of its hires' completions until the whole turn ended. This
//! drives the REAL `ClaudeStreamJsonAdapter` (via `CCTEAM_CLAUDE_BIN` pointing
//! at a deterministic fake `claude`) through the gateway's notifier door
//! (`Gateway::submit_to_sid_shared`, the call the delegation notifier makes),
//! and watches the fake's stdin: the notification line must arrive while the
//! first turn is still running, joined to that turn, with nothing left in the
//! parked-input mirror.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ccteam_harness::execution::claude_stream_json::ClaudeStreamJsonAdapter;
use ccteam_harness::{AgentVendor, HarnessAdapter, PermissionMode};
use ccteam_im::gateway::{Gateway, GatewayDeadline};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Serialize this file's env-mutating tests (`HOME` / `CCTEAM_CLAUDE_BIN` /
/// `FAKE_SJ_*` are process-global).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex as StdMutex, OnceLock};
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A fake `claude` that reads stdin the way the real CLI does mid-turn: a
/// reader thread takes every line the moment it is written (and logs it to
/// `FAKE_SJ_USER_LOG`), while the main loop answers them one turn at a time.
/// The FIRST turn holds its `result` for `FAKE_SJ_FIRST_TURN_SECS`, so a line
/// written during that window is visibly read while the turn is still running
/// — and a line the daemon parked instead is visibly absent.
const FAKE_PY: &str = r#"#!/usr/bin/env python3
import sys, os, json, threading, queue, time
argv = sys.argv[1:]
if "--no-chrome" not in argv:
    sys.stderr.write("fake-claude-sj: missing --no-chrome\n")
    sys.exit(3)
out_lock = threading.Lock()
def emit(obj):
    with out_lock:
        sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
init_line = sys.stdin.readline()
init_rid = "init"
try:
    init_rid = json.loads(init_line).get("request_id", "init")
except Exception:
    pass
emit({"type":"control_response","response":{"subtype":"success","request_id":init_rid,
      "response":{"commands":[{"name":"compact"}],"models":[{"model":"fake-model"}]}}})
user_log = os.environ.get("FAKE_SJ_USER_LOG")
turns = queue.Queue()
def reader():
    while True:
        line = sys.stdin.readline()
        if not line:
            turns.put(None)
            return
        if not line.strip():
            continue
        try:
            msg = json.loads(line)
        except Exception:
            msg = None
        if isinstance(msg, dict) and msg.get("type") == "control_request":
            emit({"type":"control_response","response":{"subtype":"success",
                  "request_id":msg.get("request_id","ctl"),"response":{}}})
            continue
        try:
            blocks = msg["message"]["content"]
            text = "".join(b.get("text", "") for b in blocks if isinstance(b, dict))
        except Exception:
            text = line.strip()
        if user_log:
            with open(user_log, "a") as f:
                f.write(json.dumps(text) + "\n")
        turns.put(text)
threading.Thread(target=reader, daemon=True).start()
hold = float(os.environ.get("FAKE_SJ_FIRST_TURN_SECS", "0"))
n = 0
while True:
    text = turns.get()
    if text is None:
        break
    n += 1
    emit({"type":"assistant","session_id":"sid",
          "message":{"role":"assistant","content":[{"type":"text","text":"ok " + str(n)}]}})
    if n == 1:
        time.sleep(hold)
    emit({"type":"result","subtype":"success","result":"ok","is_error":False,
          "total_cost_usd":0.001,"usage":{"input_tokens":1,"output_tokens":1},
          "session_id":"sid"})
"#;

fn write_fake(dir: &Path) -> PathBuf {
    let path = dir.join("fake-claude-sj-notify.py");
    std::fs::write(&path, FAKE_PY).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The lines the fake has read off its stdin so far, in order.
fn stdin_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<String>(line).expect("the fake logs JSON strings"))
        .collect()
}

/// Poll the fake's stdin log until `pred` holds or `within` runs out.
async fn await_stdin(log: &Path, within: Duration, pred: impl Fn(&[String]) -> bool) -> bool {
    let until = Instant::now() + within;
    loop {
        if pred(&stdin_lines(log)) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `await_holding_lock`: the env lock is held across the whole test on
/// purpose — it is what keeps another test in this binary from re-pointing
/// `HOME` / `CCTEAM_CLAUDE_BIN` under a live session.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completion_notification_is_injected_into_a_busy_claude_parent_at_once() {
    let _env = env_lock();
    ccteam_core::tool_surface::disable_tool_surface_bootstrap_for_tests();
    let home = TempDir::new().unwrap();
    // CCTEAM_HOME wins over HOME in the root resolvers: pin both.
    std::env::set_var("HOME", home.path());
    std::env::set_var("CCTEAM_HOME", home.path().join(".ccteam"));
    std::env::set_var("CCTEAM_CLAUDE_BIN", write_fake(home.path()));
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "5000");
    let first_turn = Duration::from_secs(6);
    std::env::set_var("FAKE_SJ_FIRST_TURN_SECS", first_turn.as_secs().to_string());
    let log = home.path().join("stdin.log");
    std::env::set_var("FAKE_SJ_USER_LOG", &log);
    let project_dir = home.path().join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();

    let adapter: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(ClaudeStreamJsonAdapter::new());
    let mut gateway = Gateway::new(adapter, "alpha", &project_dir);
    let sid = gateway
        .create_session_api(
            "alpha".into(),
            String::new(),
            AgentVendor::Claude,
            PermissionMode::Skip,
        )
        .await
        .expect("session")
        .sid;
    let gateway = Arc::new(Mutex::new(gateway));

    // The parent is busy: its own long turn is running.
    let running_turn = Gateway::submit_web_sid_shared(
        Arc::clone(&gateway),
        &sid,
        "plan the release".into(),
        "console",
        GatewayDeadline::start(),
    )
    .await
    .expect("the parent's own turn starts");
    let started = Instant::now();
    assert!(
        await_stdin(&log, Duration::from_secs(5), |lines| lines.len() == 1).await,
        "the parent's turn reached the CLI: {:?}",
        stdin_lines(&log)
    );

    // A child finishes: the delegation notifier's own door.
    let notification = "s7 done · claude · turn 1 req-1\nthe verdict";
    let notified_turn = Gateway::submit_to_sid_shared(
        Arc::clone(&gateway),
        &sid,
        notification.into(),
        GatewayDeadline::start(),
    )
    .await
    .expect("the notification is accepted");

    // Read off stdin while the parent's first turn is still running.
    let delivered = await_stdin(&log, Duration::from_secs(3), |lines| {
        lines.iter().any(|line| line == notification)
    })
    .await;
    assert!(
        delivered && started.elapsed() < first_turn,
        "the notification reaches the CLI mid-turn, not after the turn ends \
         ({:?} into a {first_turn:?} turn): {:?}",
        started.elapsed(),
        stdin_lines(&log)
    );
    assert_eq!(
        notified_turn, running_turn,
        "the notification JOINED the running turn (Injected) instead of reserving a turn of its own"
    );
    let mirror = project_dir
        .join(".ccteam")
        .join("chat")
        .join(&sid)
        .join("deferred-input.json");
    assert!(
        !mirror.exists(),
        "nothing was parked: {:?}",
        std::fs::read_to_string(&mirror).ok()
    );

    for key in [
        "FAKE_SJ_FIRST_TURN_SECS",
        "FAKE_SJ_USER_LOG",
        "CCTEAM_CLAUDE_BIN",
        "CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS",
    ] {
        std::env::remove_var(key);
    }
}
