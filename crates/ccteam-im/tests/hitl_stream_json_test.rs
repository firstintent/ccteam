//! v0.8.22 P0-2 — end-to-end proof that `run_daemon_with_shutdown` actually
//! wires the production HITL resolver onto the stream-json Claude adapter
//! singleton (review §3.1-1).
//!
//! Before this fix, `default_adapter_factory` built the stream-json adapter
//! with NO `CanUseToolResolver`, so a `hitl` stream-json session's
//! non-allowlist `can_use_tool` reverse-RPCs silently denied with **no
//! IM/web prompt ever rendered** — the `/new … hitl` receipt promised
//! "non-allowlist tools need IM approval" but nobody was ever asked.
//!
//! This drives the REAL `ClaudeStreamJsonAdapter` (via `CCTEAM_CLAUDE_BIN`
//! pointing at a deterministic fake `claude` — same fixture shape as
//! `ccteam-harness/tests/claude_stream_json_test.rs`) through the actual
//! composition-root wiring path (`DaemonArgs::gateway` +
//! `DaemonArgs::claude_stream_json_adapter`, exactly what `ccteam start`
//! builds via `build_gateway_for_daemon`): `/new claude helper hitl` spawns
//! a real hitl stream-json session, a turn triggers a `can_use_tool`
//! reverse-RPC for a non-allowlist tool, and the test asserts the resulting
//! approval prompt actually reaches the gateway's pending-approval registry
//! (observable via the rendered Approve/Deny buttons in the MockChannel
//! outbox) — then resolves it through [`ccteam_im::gateway::Gateway::resolve_web_selection`],
//! the SAME machinery an IM inline-button click or a web `POST …/resolve`
//! use, and asserts allow → tool runs / deny → tool blocked but the TURN
//! STILL COMPLETES (never killed).

#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ccteam_im::daemon::{
    default_adapter_factory_with_stream_json_handle, run_daemon_with_shutdown, DaemonArgs,
};
use ccteam_im::gateway::Gateway;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::{Channel, SendMessage};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Serialize this file's env-mutating tests (mirrors every other
/// `ccteam-im` integration test file — `HOME` / `CCTEAM_CLAUDE_BIN` /
/// `FAKE_SJ_*` are process-global).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex as StdMutex, OnceLock};
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn isolate_home() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("HOME", tmp.path());
    tmp
}

/// A deterministic fake `claude` stream-json vendor (trimmed copy of the
/// fixture `ccteam-harness/tests/claude_stream_json_test.rs` uses): performs
/// the `initialize` handshake, then on the first user turn fires a
/// `can_use_tool` reverse-RPC for `FAKE_SJ_ASK_TOOL` and replies
/// `ran:<tool>` / `blocked:<tool>` per the control_response's `behavior` —
/// the turn ALWAYS completes (deny blocks only the tool, never the turn).
const FAKE_PY: &str = r#"#!/usr/bin/env python3
import sys, json
argv = sys.argv[1:]
if "--no-chrome" not in argv:
    sys.stderr.write("fake-claude-sj: missing --no-chrome\n")
    sys.exit(3)
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
ask_tool = "Bash"
n = 0
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
            emit({"type":"control_response","response":{"subtype":"success",
                  "request_id":rid,"response":{"totalTokens":1,"maxTokens":200000,"percentage":0}}})
        else:
            emit({"type":"control_response","response":{"subtype":"success",
                  "request_id":rid,"response":{}}})
        continue
    n += 1
    rid = "req-%d" % n
    emit({"type":"control_request","request_id":rid,
          "request":{"subtype":"can_use_tool","tool_name":ask_tool,
                     "input":{"command":"ls -la"},"tool_use_id":"tu-%d" % n}})
    resp = sys.stdin.readline()
    behavior = "deny"
    try:
        behavior = json.loads(resp)["response"]["response"]["behavior"]
    except Exception:
        pass
    verdict = ("ran:" + ask_tool) if behavior == "allow" else ("blocked:" + ask_tool)
    emit({"type":"assistant","session_id":"sid",
          "message":{"role":"assistant","content":[{"type":"text","text":verdict}]}})
    emit({"type":"result","subtype":"success","result":verdict,"is_error":False,
          "total_cost_usd":0.001,"usage":{"input_tokens":1,"output_tokens":1},
          "session_id":"sid"})
"#;

fn write_fake(tmp: &Path) -> PathBuf {
    let p = tmp.join("fake-claude-sj-hitl.py");
    std::fs::write(&p, FAKE_PY).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Pin `HOME` + the fake binary + a short init timeout for one test.
fn setup(tmp: &Path) -> PathBuf {
    let fake = write_fake(tmp);
    std::env::set_var("HOME", tmp);
    std::env::set_var("CCTEAM_HOME", tmp.join(".ccteam"));
    std::env::set_var("CCTEAM_CLAUDE_BIN", &fake);
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "5000");
    fake
}

/// Poll `mock`'s outbox until `pred` matches some entry, or panic after
/// `deadline`. Local poll loop (no external notification path exists for a
/// `MockChannel` send) — bounded generously so it never hangs a CI run.
async fn wait_for_outbox<F: Fn(&SendMessage) -> bool>(
    mock: &MockChannel,
    deadline: Duration,
    what: &str,
    pred: F,
) -> SendMessage {
    let start = tokio::time::Instant::now();
    loop {
        if let Some(m) = mock.outbox().await.into_iter().rev().find(|m| pred(m)) {
            return m;
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for {what}; outbox so far: {:?}",
                mock.outbox().await
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Drive one full hitl stream-json round trip: spawn a real hitl session
/// (talking to the fake vendor), submit a turn that triggers a
/// `can_use_tool` ask for `Bash`, resolve it via
/// `Gateway::resolve_web_selection(token, decision)` — the EXACT machinery
/// an IM inline-button click / a web `[Approve]`/`[Deny]` use — and assert
/// the fake vendor saw the matching `behavior` and the turn completed with
/// `expect_answer`.
///
/// Held lint: `await_holding_lock` doesn't apply on the `current_thread`
/// runtime this test uses (the task cannot migrate to another OS thread),
/// matching the same allow already used across this crate's other
/// env-mutating integration tests (e.g. `tests/inbound_wiring_test.rs`).
#[allow(clippy::await_holding_lock)]
async fn run_round_trip(decision: &str, expect_answer: &str) {
    let _g = env_lock();
    let home = isolate_home();
    setup(home.path());
    let projects_root = home.path().join("projects");
    let default_dir = projects_root.join("default");
    std::fs::create_dir_all(&default_dir).unwrap();

    let mock = Arc::new(MockChannel::new());
    let mut channels: HashMap<String, Arc<dyn Channel + Send + Sync>> = HashMap::new();
    channels.insert(
        "mock".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    // Build the gateway EXACTLY the way the composition root
    // (`ccteam-cli`'s `ccteam start`) does via `build_gateway_for_daemon`:
    // the factory + its stream-json adapter handle are paired, and BOTH are
    // threaded into `DaemonArgs` (`gateway` / `claude_stream_json_adapter`)
    // so `run_daemon_with_shutdown`'s post-build wiring reaches the SAME
    // adapter singleton this gateway spawns sessions through.
    let (factory, stream_json_handle) = default_adapter_factory_with_stream_json_handle();
    let gateway = Arc::new(Mutex::new(Gateway::new_with_factory(
        factory,
        "default",
        default_dir,
    )));

    let args = DaemonArgs {
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_secs(15)),
        channels_override: Some(channels),
        gateway: Some(gateway.clone()),
        claude_stream_json_adapter: Some(stream_json_handle),
        ..Default::default()
    };

    let daemon = tokio::spawn(async move {
        run_daemon_with_shutdown(args, futures::future::pending::<()>()).await
    });

    // Let the daemon's synchronous startup (channel build, `set_pending` /
    // `set_event_sink` / `set_resolver` wiring) run before driving it.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Spawn the hitl session directly on the shared gateway handle — the
    // in-process peer of an IM `/new claude helper hitl` message.
    let spawn_receipt = gateway
        .lock()
        .await
        .handle_text("mock", "chat-1", "alice", "/new claude helper hitl")
        .await
        .expect("spawn hitl session");
    assert!(
        spawn_receipt.iter().any(|r| r.contains("hitl")),
        "spawn receipt should flag hitl: {spawn_receipt:?}"
    );

    // Submit a turn — the fake vendor answers with a `can_use_tool` ask for
    // `Bash` before it will complete the turn.
    gateway
        .lock()
        .await
        .handle_text("mock", "chat-1", "alice", "please run a command")
        .await
        .expect("submit turn");

    // THE PROOF: the `can_use_tool` reverse-RPC reached the gateway's
    // pending-approval machinery and rendered Approve/Deny buttons — this is
    // exactly what review §3.1-1 says never happened before this fix (a
    // silent deny with no prompt).
    let prompt = wait_for_outbox(&mock, Duration::from_secs(10), "the approval prompt", |m| {
        m.options.len() == 2 && m.options.iter().any(|o| o.id == "allow")
    })
    .await;
    assert!(
        prompt.content.contains("Bash"),
        "prompt should name the tool: {}",
        prompt.content
    );
    let token = prompt.options[0]
        .data
        .split(':')
        .next()
        .expect("token prefix")
        .to_string();

    // Resolve it through the SAME machinery an IM click / web
    // `POST …/resolve` use — proving approve/deny round-trips, not just that
    // a prompt was rendered.
    gateway
        .lock()
        .await
        .resolve_web_selection(&token, decision)
        .await
        .expect("resolve the pending approval");

    // The turn completed (never killed) with the vendor's verdict for this
    // decision — deny blocks ONLY the tool call. The focused answer now also
    // carries the v0.8.23 review §3.2-5 context echo (`→ slug/sid (role)`),
    // so match on a prefix rather than full equality.
    let answer = wait_for_outbox(
        &mock,
        Duration::from_secs(10),
        "the turn's final answer",
        |m| m.content.starts_with(expect_answer),
    )
    .await;
    assert!(
        answer.content.starts_with(expect_answer),
        "got: {}",
        answer.content
    );
    assert!(
        answer.content.contains("→ default/s1 (helper)"),
        "carries the context echo: {}",
        answer.content
    );

    // The session is still tracked/live — denial never killed it.
    assert_eq!(gateway.lock().await.session_views().len(), 1);

    daemon.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn hitl_stream_json_can_use_tool_approve_allows_the_tool() {
    run_round_trip("allow", "ran:Bash").await;
}

#[tokio::test(flavor = "current_thread")]
async fn hitl_stream_json_can_use_tool_deny_blocks_tool_without_killing_turn() {
    run_round_trip("deny", "blocked:Bash").await;
}
