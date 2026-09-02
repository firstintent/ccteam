//! v0.8.11 E1 Wave 1 — integration (fake-vendor) e2e for
//! `ClaudeStreamJsonAdapter`. A deterministic python fake speaks the
//! stream-json wire (system:init → per-turn assistant+result) so the tests
//! exercise the real spawn / NDJSON / translate / resume paths without the
//! real `claude` binary.
//!
//! Serial + env-mutating (HOME / CCTEAM_CLAUDE_BIN): one process, so the
//! tests run `#[serial]` and each pins its own tempdir HOME.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::sync::Arc;

use ccteam_harness::execution::claude_stream_json::bridge::{ApprovalDecision, FnResolver};
use ccteam_harness::execution::claude_stream_json::spawn_spec::deterministic_session_uuid;
use ccteam_harness::execution::claude_stream_json::ClaudeStreamJsonAdapter;
use ccteam_harness::execution::transcript_tail::anthropic_project_dir;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ChoiceSelection, Directive, DirectiveOutcome, ExecutionMode,
    HarnessAdapter, HarnessError, PermissionMode, SpawnCtx, ThreadEvent, ThreadItemDetails,
    TurnInput,
};
use futures::StreamExt;
use serial_test::serial;

/// The fake `claude` stream-json vendor. Reads argv to echo the session id
/// and resume/fresh mode, emits `system:init`, then one assistant+result
/// turn per stdin line. Env knobs: `FAKE_SJ_ARGV_LOG` (record mode+id),
/// `FAKE_SJ_DIE_AFTER_INIT=1`, `FAKE_SJ_REPLY`, `FAKE_SJ_INIT_COMMANDS`.
const FAKE_PY: &str = r#"#!/usr/bin/env python3
import sys, os, json
argv = sys.argv[1:]
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
    with open(log, "w") as f:
        f.write(mode + " " + sid + "\n")
# Contract guard: real `claude` only speaks stream-json (and thus only
# emits system:init) under --no-chrome (or --print). The fake mirrors that
# — fail loud if build_argv ever drops the flag again, so this fake can't
# mask the "timed out waiting for system:init" regression the way it did.
if "--no-chrome" not in argv:
    sys.stderr.write("fake-claude-sj: missing --no-chrome (no system:init)\n")
    sys.exit(3)
# Resume-failure fault: die immediately when spawned with --resume (before
# init), so start_thread's resume attempt fails and falls back to fresh.
if mode == "resume" and os.environ.get("FAKE_SJ_DIE_ON_RESUME") == "1":
    sys.exit(1)
def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
cmds = os.environ.get("FAKE_SJ_INIT_COMMANDS", "compact,context,clear").split(",")
# The adapter's spawn handshake sends an `initialize` control_request FIRST
# (real claude does not emit system:init until the first user turn). Read it
# and reply with a control_response carrying the slash-command table + model.
init_line = sys.stdin.readline()
init_rid = "init"
try:
    init_rid = json.loads(init_line).get("request_id", "init")
except Exception:
    pass
emit({"type":"control_response","response":{"subtype":"success","request_id":init_rid,
      "response":{"commands":[{"name":c} for c in cmds],"models":[{"model":"fake-model"}]}}})
if os.environ.get("FAKE_SJ_DIE_AFTER_INIT") == "1":
    sys.exit(0)
reply = os.environ.get("FAKE_SJ_REPLY", "ok")
ask_tool = os.environ.get("FAKE_SJ_ASK_TOOL")
# Use readline (not `for line in sys.stdin`) so we can interleave a
# control_response read after emitting a can_use_tool request.
n = 0
while True:
    line = sys.stdin.readline()
    if not line:
        break
    if not line.strip():
        continue
    # Client→CLI control requests (e.g. /model → set_model) get a
    # control_response, NOT a turn. Mirrors real claude's control channel.
    try:
        ctl = json.loads(line)
    except Exception:
        ctl = None
    if isinstance(ctl, dict) and ctl.get("type") == "control_request":
        rid = ctl.get("request_id", "ctl")
        sub = (ctl.get("request") or {}).get("subtype", "")
        ctl_log = os.environ.get("FAKE_SJ_CTL_LOG")
        if ctl_log:
            with open(ctl_log, "a") as f:
                f.write(sub + "\n")
        if sub == "get_context_usage":
            # Real claude returns the vendor's actual window here. The bare
            # "fake-model" has no [1m] suffix, so the heuristic would give 200k;
            # maxTokens 1000000 can ONLY come from get_context_usage → it both
            # proves the source AND drives the [1m] model-id tag.
            emit({"type":"control_response","response":{"subtype":"success",
                  "request_id":rid,"response":{"totalTokens":12345,"maxTokens":1000000,"percentage":1}}})
        elif os.environ.get("FAKE_SJ_SET_MODEL_FAIL") == "1":
            emit({"type":"control_response","response":{"subtype":"error",
                  "request_id":rid,"error":"unsupported model"}})
        else:
            emit({"type":"control_response","response":{"subtype":"success",
                  "request_id":rid,"response":{}}})
        continue
    n += 1
    user_log = os.environ.get("FAKE_SJ_USER_LOG")
    if user_log:
        try:
            blocks = json.loads(line)["message"]["content"]
            text = "".join(b.get("text", "") for b in blocks if isinstance(b, dict))
        except Exception:
            text = line.strip()
        with open(user_log, "a") as f:
            f.write(text + "\n")
    # Real claude emits `system:init` on the FIRST user turn (not at spawn),
    # and that line is where it reports each MCP server's connection status.
    # `FAKE_SJ_INIT_MCP_FAILED=1` reproduces a child that came up while the
    # daemon's `/mcp` was not listening yet: alive, working, and toolless.
    if n == 1 and os.environ.get("FAKE_SJ_INIT_MCP_FAILED") == "1":
        emit({"type":"system","subtype":"init","session_id":sid,
              "mcp_servers":[{"name":"ccteam","status":"failed"}]})
    if os.environ.get("FAKE_SJ_DIE_MID_TURN") == "1":
        # Emit an assistant block (turn now in flight) then die WITHOUT a
        # result — the in-flight-loss fault.
        emit({"type":"assistant","session_id":sid,
              "message":{"role":"assistant","content":[{"type":"text","text":"thinking..."}]}})
        sys.exit(0)
    if os.environ.get("FAKE_SJ_ERROR_RESULT") == "1":
        # claude API failure proxy ("断网"): an error-subtype result.
        emit({"type":"assistant","session_id":sid,
              "message":{"role":"assistant","content":[{"type":"text","text":"trying"}]}})
        emit({"type":"result","subtype":"error_during_execution","is_error":True,
              "session_id":sid})
        continue
    if os.environ.get("FAKE_SJ_NO_RESULT") == "1":
        # Emit an assistant step but NO result — the turn stays "working...".
        # Loop back to readline so we still answer control_requests
        # (get_context_usage) for the mid-turn status refresh.
        emit({"type":"assistant","session_id":sid,
              "message":{"role":"assistant","content":[{"type":"text","text":"working..."}]}})
        continue
    if ask_tool:
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
        # The turn still COMPLETES even when the tool was denied (deny only
        # blocks the tool call, never the turn).
        emit({"type":"assistant","session_id":sid,
              "message":{"role":"assistant","content":[{"type":"text","text":verdict}]}})
        emit({"type":"result","subtype":"success","result":verdict,"is_error":False,
              "total_cost_usd":0.001,"usage":{"input_tokens":7,"output_tokens":4},
              "session_id":sid})
        continue
    emit({"type":"assistant","session_id":sid,
          "message":{"role":"assistant","content":[{"type":"text","text":reply}]}})
    # A long FIRST turn: the assistant step is out, the result comes later —
    # the window in which a slash line would be read as a steer, not a command.
    slow = os.environ.get("FAKE_SJ_SLOW_FIRST_RESULT_SECS")
    if slow and n == 1:
        import time
        time.sleep(float(slow))
    emit({"type":"result","subtype":"success","result":reply,"is_error":False,
          "total_cost_usd":0.001,"usage":{"input_tokens":7,"output_tokens":4},
          "session_id":sid})
# Real claude keeps working after stdin EOF while a turn is in flight and only
# exits once idle. `FAKE_SJ_LINGER_SECS` models that body: stay alive for N
# seconds after EOF, then exit — the daemon-restart "body outlives the daemon"
# shape the session_body gate exists for.
linger = os.environ.get("FAKE_SJ_LINGER_SECS")
if linger:
    import time
    time.sleep(float(linger))
"#;

fn write_fake(tmp: &Path) -> PathBuf {
    let p = tmp.join("fake-claude-sj.py");
    std::fs::write(&p, FAKE_PY).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Pin HOME + the fake binary + a short init timeout for one test.
fn setup(tmp: &Path) -> PathBuf {
    let fake = write_fake(tmp);
    std::env::set_var("HOME", tmp);
    std::env::set_var("CCTEAM_HOME", tmp.join(".ccteam"));
    std::env::set_var("CCTEAM_CLAUDE_BIN", &fake);
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "5000");
    std::env::set_var("FAKE_SJ_ARGV_LOG", tmp.join("argv.log"));
    std::env::remove_var("FAKE_SJ_DIE_AFTER_INIT");
    std::env::remove_var("FAKE_SJ_NO_RESULT");
    std::env::remove_var("FAKE_SJ_SLOW_FIRST_RESULT_SECS");
    std::env::remove_var("FAKE_SJ_USER_LOG");
    // Clear the fault switch here, not only at the end of the test that sets
    // it: a panicking test never reaches its own cleanup, and the leaked
    // `FAKE_SJ_DIE_ON_RESUME=1` then killed the NEXT serial test's resume spawn
    // — one real failure masqueraded as a second, unrelated "flaky" one.
    // Every fault switch belongs here so each test starts from a clean slate.
    std::env::remove_var("FAKE_SJ_DIE_ON_RESUME");
    std::env::remove_var("FAKE_SJ_INIT_MCP_FAILED");
    std::env::remove_var("FAKE_SJ_CTL_LOG");
    std::env::remove_var("FAKE_SJ_LINGER_SECS");
    fake
}

fn ctx(tmp: &Path, slug: &str, sid: &str) -> SpawnCtx {
    SpawnCtx {
        generation: 0,
        mode: None,
        slug: slug.to_string(),
        sid: sid.to_string(),
        owner: "user:web-api".into(),
        cwd: tmp.to_path_buf(),
        project_dir: tmp.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    }
}

fn argv_mode(tmp: &Path) -> String {
    std::fs::read_to_string(tmp.join("argv.log"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Drain the events stream up to a deadline, returning the agent-message
/// answer + whether a TurnCompleted was seen.
async fn collect_answer(
    adapter: &ClaudeStreamJsonAdapter,
    handle: &ccteam_harness::ThreadHandle,
) -> (Option<String>, bool) {
    let mut stream = adapter.events(handle);
    let mut answer = None;
    let mut completed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, stream.next()).await {
        match ev {
            ThreadEvent::ItemCompleted { item } => {
                if let ThreadItemDetails::AgentMessage(t) = item.details {
                    answer = Some(t);
                }
            }
            ThreadEvent::TurnCompleted { .. } => {
                completed = true;
                break;
            }
            _ => {}
        }
    }
    (answer, completed)
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn spawn_init_and_turn_emits_answer() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();

    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");
    assert_eq!(handle.vendor, AgentVendor::Claude);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    // Identity is the deterministic per-(slug,sid) uuid.
    assert_eq!(handle.identity, deterministic_session_uuid("demo", "s1"));
    assert_eq!(
        handle.raw_extras.get("protocol").and_then(|v| v.as_str()),
        Some("stream-json")
    );
    assert_eq!(
        handle.raw_extras.get("host").and_then(|v| v.as_str()),
        Some("local")
    );
    // Command table captured from system:init.
    let cmds = adapter.session_command_table(&handle.identity).unwrap();
    assert!(cmds.contains(&"compact".to_string()));
    // Identity record (§七 ⑤).
    let id = adapter.session_identity(&handle.identity).unwrap();
    assert_eq!(id.sid, "s1");
    assert_eq!(id.host, "local");

    // Subscribe, then submit a turn → the fake answers "ok".
    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("hi".into()));
    let (answer, completed) = tokio::join!(collect_answer(&adapter, &stream_handle), submit).0;
    let _ = completed;
    assert_eq!(answer.as_deref(), Some("ok"), "expected the fake's answer");

    adapter.close_thread(&handle).await.unwrap();
}

/// A session whose ccteam MCP server failed to connect is alive, answering,
/// and toolless — and no vendor retries a failed MCP server on its own. The
/// adapter must heal it from the session's OWN `system:init` report, using the
/// `mcp_reconnect` control request (the same action the TUI's `/mcp` performs).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn dead_tool_face_at_init_is_reported_not_silently_reconnected() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let ctl_log = tmp.path().join("ctl.log");
    std::env::set_var("FAKE_SJ_CTL_LOG", &ctl_log);
    std::env::set_var("FAKE_SJ_INIT_MCP_FAILED", "1");
    let adapter = ClaudeStreamJsonAdapter::new();

    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("hi".into()));
    let _ = tokio::join!(collect_answer(&adapter, &stream_handle), submit);

    // Give the status tap the same bounded moment the old auto-heal had, then
    // assert nothing was sent. A dead tool face is reported, never "healed" by
    // an in-place reconnect: that reconnect attaches the machine's global
    // `ccteam` entry, so the session would trade "no tools" for "someone
    // else's identity" — wrong parent, wrong project, nothing visibly broken.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !std::fs::read_to_string(&ctl_log)
            .unwrap_or_default()
            .lines()
            .any(|line| line.trim() == "mcp_reconnect"),
        "a dead tool face must NOT be auto-reconnected; control log: {:?}",
        std::fs::read_to_string(&ctl_log).unwrap_or_default()
    );

    std::env::remove_var("FAKE_SJ_INIT_MCP_FAILED");
    std::env::remove_var("FAKE_SJ_CTL_LOG");
    adapter.close_thread(&handle).await.unwrap();
}

/// …and the on-demand path (what IM `/mcp` drives) answers the same way: it
/// states that only a respawn reapplies the curated config, and sends no
/// control request at all.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn rebuild_tool_surface_refuses_in_place_and_sends_no_reconnect() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let ctl_log = tmp.path().join("ctl.log");
    std::env::set_var("FAKE_SJ_CTL_LOG", &ctl_log);
    let adapter = ClaudeStreamJsonAdapter::new();

    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    let outcome = adapter
        .rebuild_tool_surface(&handle)
        .await
        .expect("declaring a limitation is an answer, not an error");
    // "Claims an in-place rebuild" is unrepresentable since the `Rebuilt`
    // variant was deleted (no vendor has a producer), so this destructure is
    // irrefutable and the remaining question is whether the reason is usable.
    let ccteam_harness::ToolSurfaceRebuild::RespawnRequired { reason } = outcome;
    assert!(
        reason.contains("/new"),
        "say what restores the tool face: {reason:?}"
    );
    assert!(
        reason.contains("principal") || reason.contains("credential"),
        "say WHY an in-place reconnect is refused: {reason:?}"
    );
    // The control request must NOT be sent. Measured on a real machine: it makes
    // the vendor re-resolve its server list without honouring
    // `--strict-mcp-config`, attach the global same-named `ccteam` entry, and
    // route every later `tools/call` there — so the session spends the rest of
    // its life calling with the MACHINE's credential instead of its own.
    assert!(
        !std::fs::read_to_string(&ctl_log)
            .unwrap_or_default()
            .lines()
            .any(|line| line.trim() == "mcp_reconnect"),
        "an in-place reconnect swaps this session's identity and must never be sent"
    );

    std::env::remove_var("FAKE_SJ_CTL_LOG");
    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn fresh_spawn_uses_session_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");
    let uuid = deterministic_session_uuid("demo", "s1");
    assert_eq!(argv_mode(tmp.path()), format!("session-id {uuid}"));
    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn resume_spawn_uses_resume_when_transcript_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let uuid = deterministic_session_uuid("demo", "s2");

    // Pre-create the Anthropic transcript jsonl so start_thread chooses
    // --resume (the resume-by-sid path).
    let dir = anthropic_project_dir(tmp.path()).expect("anthropic dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{uuid}.jsonl")), "{}\n").unwrap();

    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s2"),
        )
        .await
        .expect("start_thread");
    assert_eq!(argv_mode(tmp.path()), format!("resume {uuid}"));
    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn resume_thread_without_live_session_is_not_implemented() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    // Never spawned → no live session → NotImplemented (gateway falls back
    // to the resume-aware start_thread).
    let err = adapter
        .resume_thread(&deterministic_session_uuid("demo", "s9"))
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::NotImplemented { .. }));
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn child_death_then_restart_recovers() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();

    // 1) Spawn a child that dies right after init. start_thread still
    //    succeeds (init arrived) but the session is already gone.
    std::env::set_var("FAKE_SJ_DIE_AFTER_INIT", "1");
    let dead = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread (init before death)");
    // The events stream terminates once the child's stdout closes.
    let mut stream = adapter.events(&dead);
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(_ev) = stream.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "events stream must terminate on child death");
    adapter.close_thread(&dead).await.unwrap();

    // 2) Restart with a healthy fake (recovery) — a turn works again.
    std::env::remove_var("FAKE_SJ_DIE_AFTER_INIT");
    let revived = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("restart");
    let stream_handle = revived.clone();
    let submit = adapter.submit_turn(&revived, TurnInput::UserText("again".into()));
    let (answer, _) = tokio::join!(collect_answer(&adapter, &stream_handle), submit).0;
    assert_eq!(
        answer.as_deref(),
        Some("ok"),
        "recovered session must answer"
    );
    adapter.close_thread(&revived).await.unwrap();
}

// ── Wave 2: slash bridge + HITL ─────────────────────────────────────────

fn ctx_hitl(tmp: &Path, slug: &str, sid: &str) -> SpawnCtx {
    SpawnCtx {
        generation: 0,
        permission_mode: PermissionMode::Hitl,
        ..ctx(tmp, slug, sid)
    }
}

/// issue #193 — a vendor slash command sent while a turn is in flight is
/// parked and delivered right after the turn's `result` (Claude executes a
/// slash line only when idle; mid-turn it becomes a steer the model reads as
/// prose). The user gets a receipt saying so instead of silence.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn slash_during_turn_is_deferred_until_the_turn_ends() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let user_log = tmp.path().join("user.log");
    std::env::set_var("FAKE_SJ_USER_LOG", &user_log);
    std::env::set_var("FAKE_SJ_SLOW_FIRST_RESULT_SECS", "1.5");
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s9"),
        )
        .await
        .expect("start");
    let mut events = adapter.events(&handle);

    // Turn 1 in flight (assistant step out, result 1.5s away).
    adapter
        .submit_turn(&handle, TurnInput::UserText("long task".into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let goal = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "goal".into(),
                args: "ship it".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match &goal {
        DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("/goal ship it"),
                "names the command: {receipt}"
            );
            assert!(receipt.contains("排队"), "says it is queued: {receipt}");
        }
        other => panic!("mid-turn slash must be parked with a receipt, got {other:?}"),
    }
    // Not written yet: the CLI would read it as a steer.
    let logged = std::fs::read_to_string(&user_log).unwrap_or_default();
    assert_eq!(logged.lines().collect::<Vec<_>>(), vec!["long task"]);

    // Two vendor turns complete: the long task, then the deferred /goal.
    let mut completed = 0;
    while completed < 2 {
        match tokio::time::timeout(Duration::from_secs(10), events.next()).await {
            Ok(Some(ThreadEvent::TurnCompleted { .. })) => completed += 1,
            Ok(Some(_)) => {}
            other => panic!("expected two completed turns, got {other:?} after {completed}"),
        }
    }
    let logged = std::fs::read_to_string(&user_log).unwrap();
    assert_eq!(
        logged.lines().collect::<Vec<_>>(),
        vec!["long task", "/goal ship it"],
        "the slash line reaches the CLI verbatim, after the turn"
    );
    std::env::remove_var("FAKE_SJ_SLOW_FIRST_RESULT_SECS");
    std::env::remove_var("FAKE_SJ_USER_LOG");
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn slash_bridge_passes_through_safe_rejects_dialog() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start");

    // /compact is a red-line passthrough (and in the init table) → Turn.
    let compact = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "compact".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(compact, DirectiveOutcome::Turn(_)),
        "/compact must pass through as a turn, got {compact:?}"
    );
    // Let the /compact turn finish: a slash sent while it is still in flight
    // is parked (issue #193), which is not what this test is about.
    let mut events = adapter.events(&handle);
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.next()).await {
            Ok(Some(ThreadEvent::TurnCompleted { .. })) => break,
            Ok(Some(_)) => {}
            other => panic!("expected the /compact turn to complete, got {other:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bare /model (no arg, no choice) → a NeedsChoice picker built strictly
    // from the REAL captured model list (the `initialize` `response.models[]`;
    // the fake ships one model `fake-model` with no effort axis → a single
    // bare-id option). `/model <id>` instead drives `set_model` → Done, and a
    // picker selection re-enters this same arm with `d.choice` set — see
    // `model_directive_drives_set_model` / `bare_model_picker_*` below.
    let model = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match model {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert!(
                prompt.title.contains("model"),
                "title should name the choice: {}",
                prompt.title
            );
            assert_eq!(
                prompt.options.len(),
                1,
                "fake ships one no-effort model → one bare-id option"
            );
            assert_eq!(prompt.options[0].id, "fake-model");
        }
        other => panic!("bare /model must offer a picker, got {other:?}"),
    }

    // An unknown command becomes text (never "Unknown skill") → Turn.
    let unknown = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "frobnicate".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(unknown, DirectiveOutcome::Turn(_)));

    adapter.close_thread(&handle).await.unwrap();
}

/// Task 1 — `thread_status` reports the live model + context-window usage
/// (the IM `/sessions` suffix + web statusline source). Model is seeded from
/// the `initialize` handshake; context lands once the first turn's
/// `result.usage` is folded in by the per-session status tap.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn thread_status_reports_model_and_context_after_turn() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    // Before any turn: model seeded from init ("fake-model"), context unknown.
    let pre = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(pre.model.as_deref(), Some("fake-model"));
    assert!(pre.context.is_none(), "no context before the first turn");

    // Drive one turn (the fake's result carries usage{input_tokens:7}).
    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("hi".into()));
    let _ = tokio::join!(collect_answer(&adapter, &stream_handle), submit);

    // The status tap queries get_context_usage on each turn and folds it into
    // the live status asynchronously (transport broadcast) — poll until it lands.
    let mut got = None;
    for _ in 0..40 {
        let st = adapter.thread_status(&handle).await.unwrap();
        if let Some(c) = st.context {
            // The 1M window (from get_context_usage) tags the model id `[1m]`
            // even though message.model / init had no suffix.
            assert_eq!(st.model.as_deref(), Some("fake-model[1m]"));
            got = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let c = got.expect("context populated after a turn");
    // Numbers come from the vendor's `get_context_usage` (totalTokens 12345,
    // maxTokens 1000000) — NOT the result.usage sum (7); the bare "fake-model"
    // heuristic would give 200k, so a 1M window can only come from get_context_usage.
    assert_eq!(
        c.used_tokens,
        Some(12_345),
        "totalTokens from get_context_usage"
    );
    assert_eq!(
        c.window_tokens, 1_000_000,
        "maxTokens from get_context_usage (real window, not the heuristic)"
    );

    adapter.close_thread(&handle).await.unwrap();
}

/// v0.8.20 (owner bug #2) — during a LONG working turn the context window keeps
/// growing as tools run; `/sessions` must track it, not freeze at the previous
/// TurnResult. The status tap now re-queries `get_context_usage` (authoritative)
/// on each assistant step (throttled). Here the fake emits an assistant step but
/// NEVER a result, so context can ONLY land via the mid-turn path.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn thread_status_refreshes_context_mid_turn_before_result() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    std::env::set_var("FAKE_SJ_NO_RESULT", "1");
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    // No turn yet → no context.
    assert!(adapter
        .thread_status(&handle)
        .await
        .unwrap()
        .context
        .is_none());

    // Submit a turn that streams an assistant step but never a result. submit_turn
    // returns the TurnId once submitted (the turn then streams async via the read
    // loop + status tap); context must populate from the mid-turn refresh WITHOUT
    // any TurnResult ever arriving.
    adapter
        .submit_turn(&handle, TurnInput::UserText("hi".into()))
        .await
        .expect("submit_turn");
    let mut got = None;
    for _ in 0..160 {
        if let Some(c) = adapter.thread_status(&handle).await.unwrap().context {
            got = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let c = got.expect("context populated mid-turn (no result was emitted)");
    assert_eq!(
        c.used_tokens,
        Some(12_345),
        "totalTokens from get_context_usage mid-turn"
    );
    assert_eq!(
        c.window_tokens, 1_000_000,
        "maxTokens from get_context_usage mid-turn"
    );

    adapter.close_thread(&handle).await.unwrap();
    std::env::remove_var("FAKE_SJ_NO_RESULT");
}

/// Task 1 (durability) — stream-json status is in-memory only, so without
/// persistence it would vanish on idle-release / daemon restart (the TUI gets
/// durability free from its on-disk transcript). The tap mirrors each turn's
/// status to `<project>/.ccteam/chat/<sid>/status.json`; after the live session
/// is released, `thread_status` falls back to that file — so the statusline
/// survives, matching the rmux/terminal path.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn stream_json_status_survives_release_via_persisted_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    // Drive a turn so the tap persists status to disk.
    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("hi".into()));
    let _ = tokio::join!(collect_answer(&adapter, &stream_handle), submit);

    // Poll the persisted file directly (the tap writes it just after folding
    // the turn's usage) so the test never races the in-memory update.
    let status_file = tmp.path().join(".ccteam/chat/s1/status.json");
    for _ in 0..40 {
        if status_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(status_file.exists(), "tap persisted status.json");

    // Release the live session (idle-release / restart proxy) — in-memory
    // status + tap are gone, but thread_status now reads the persisted file.
    adapter.close_thread(&handle).await.unwrap();
    let after = adapter.thread_status(&handle).await.unwrap();
    let c = after
        .context
        .expect("persisted context survives session release");
    assert_eq!(c.used_tokens, Some(12_345));
    assert_eq!(c.window_tokens, 1_000_000);
}

/// `docs-local/issues/#14②` — claude writer family: the status tap persists a
/// clone of the spawn-seeded shared `ThreadStatus`, so that seed carries this
/// thread's generation and every write inherits it. A sid outlives its threads
/// and a re-spawn reads this file back to choose a model; an unstamped
/// observation cannot say which thread produced it.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn a_persisted_claude_observation_carries_the_spawn_generation() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let mut spawn = ctx(tmp.path(), "demo", "s1");
    spawn.generation = 5;
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &spawn,
        )
        .await
        .expect("start_thread");

    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("hi".into()));
    let _ = tokio::join!(collect_answer(&adapter, &stream_handle), submit);

    let status_file = tmp.path().join(".ccteam/chat/s1/status.json");
    for _ in 0..40 {
        if status_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let persisted = ccteam_harness::execution::session_status::read_status_file(tmp.path(), "s1")
        .expect("tap persisted status.json");
    assert_eq!(
        persisted.generation,
        Some(5),
        "the observation names the thread that made it"
    );
    adapter.close_thread(&handle).await.unwrap();
}

/// Task 2 — `/model <id>` is driveable in stream-json: it issues a
/// `set_model` control_request and, on success, completes (`Done`) + updates
/// the live status model. A bare `/model` (no id) is a usage-hint `Rejected`
/// (no interactive picker in this channel).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn model_directive_drives_set_model() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    let out = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: "claude-opus-4-8[1m]".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => assert!(
            receipt.contains("claude-opus-4-8[1m]"),
            "receipt names the model: {receipt}"
        ),
        other => panic!("/model <id> must complete (Done), got {other:?}"),
    }
    // The switch is reflected in thread_status immediately (model only).
    let st = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(st.model.as_deref(), Some("claude-opus-4-8[1m]"));

    adapter.close_thread(&handle).await.unwrap();
}

/// A bare-`/model` picker SELECTION re-enters `handle_directive` with the
/// ORIGINAL directive (name=model, args="") + `d.choice` carrying the picked
/// option id, and applies it through the SAME `set_model` path as an explicit
/// `/model <id>` — symmetric to codex's picker round-trip. The fake ships one
/// no-effort model whose option id is the bare value `fake-model`; selecting
/// it must drive set_model → Done + update the live status.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn bare_model_picker_selection_applies_via_set_model() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    // Re-entry: bare args, but `choice` carries the picked option id.
    let out = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: String::new(),
                choice: Some(ChoiceSelection {
                    token: "cm0".into(),
                    ids: vec!["fake-model".into()],
                    free_text: None,
                }),
            },
        )
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => assert!(
            receipt.contains("fake-model"),
            "receipt names the selected model: {receipt}"
        ),
        other => panic!("picker selection must apply via set_model (Done), got {other:?}"),
    }
    // The selection is reflected in thread_status immediately.
    let st = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(st.model.as_deref(), Some("fake-model"));

    adapter.close_thread(&handle).await.unwrap();
}

/// Task 2 — when the vendor refuses `set_model` (error subtype), `/model`
/// degrades to an honest `Rejected` (never a silent success, never a kill).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn model_directive_rejects_on_vendor_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    std::env::set_var("FAKE_SJ_SET_MODEL_FAIL", "1");
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    let out = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: "bogus-model".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    std::env::remove_var("FAKE_SJ_SET_MODEL_FAIL");
    match out {
        DirectiveOutcome::Rejected { reason } => {
            assert!(reason.contains("切换失败"), "honest refusal: {reason}")
        }
        other => panic!("a vendor set_model error must Reject, got {other:?}"),
    }

    adapter.close_thread(&handle).await.unwrap();
}

/// `/interrupt` — `interrupt_turn` sends an `interrupt` control_request (the
/// fake records the subtype it received) and, on the vendor's `success`
/// control_response, returns `Ok(())`. CRUCIALLY it does NOT destroy the
/// session: the same live session still answers a subsequent `set_model`
/// directive + `thread_status` (the context is kept). This is the contrast
/// with `/stop` (which closes the thread) — interrupt stops only the turn.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn interrupt_turn_sends_control_request_and_keeps_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let ctl_log = tmp.path().join("ctl.log");
    std::env::set_var("FAKE_SJ_CTL_LOG", &ctl_log);
    let adapter = ClaudeStreamJsonAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread");

    // Interrupt the (notional) running turn → success.
    adapter
        .interrupt_turn(&handle)
        .await
        .expect("interrupt_turn must succeed on a live session");

    // The fake recorded an `interrupt` control_request subtype — proves the
    // out-of-band control line was actually sent (not a no-op).
    let recorded = std::fs::read_to_string(&ctl_log).unwrap_or_default();
    assert!(
        recorded.lines().any(|l| l.trim() == "interrupt"),
        "fake must have received an `interrupt` control_request; saw: {recorded:?}"
    );

    // Session NOT destroyed: a following directive still drives the SAME live
    // session (set_model → Done) and thread_status answers — the whole point of
    // interrupt-vs-stop (context preserved, /model still works).
    let out = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: "claude-opus-4-8[1m]".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(out, DirectiveOutcome::Done { .. }),
        "the session is still live after interrupt → /model applies, got {out:?}"
    );
    let st = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(st.model.as_deref(), Some("claude-opus-4-8[1m]"));

    std::env::remove_var("FAKE_SJ_CTL_LOG");
    adapter.close_thread(&handle).await.unwrap();
}

/// `interrupt_turn` on a handle with no live session (never spawned / already
/// closed) is an honest error — never a silent success — so the gateway can
/// surface it.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn interrupt_turn_without_live_session_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let phantom = ccteam_harness::ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: "never-spawned-uuid".into(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    };
    let err = adapter
        .interrupt_turn(&phantom)
        .await
        .expect_err("interrupt on a dead handle must error");
    assert!(
        matches!(err, HarnessError::SubmitFailed(_)),
        "non-live interrupt is a SubmitFailed, got {err:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn hitl_deny_blocks_tool_but_turn_completes() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    std::env::set_var("FAKE_SJ_ASK_TOOL", "Bash");
    // Resolver allows everything EXCEPT Bash.
    let adapter = ClaudeStreamJsonAdapter::new().with_resolver(Arc::new(FnResolver(
        |_sid: &str, req: &ccteam_harness::execution::claude_stream_json::bridge::CanUseToolReq| {
            if req.tool_name == "Bash" {
                ApprovalDecision::deny("denied by policy")
            } else {
                ApprovalDecision::allow()
            }
        },
    )));

    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx_hitl(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start hitl");

    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("run a command".into()));
    let ((answer, completed), _) = tokio::join!(collect_answer(&adapter, &stream_handle), submit);
    // Deny round-tripped: the fake saw behavior=deny → "blocked:Bash".
    assert_eq!(answer.as_deref(), Some("blocked:Bash"));
    // The turn still COMPLETED — deny blocks only the tool, never the turn.
    assert!(completed, "turn must complete despite the tool denial");

    std::env::remove_var("FAKE_SJ_ASK_TOOL");
    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn hitl_allow_lets_tool_run() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    std::env::set_var("FAKE_SJ_ASK_TOOL", "Read");
    let adapter = ClaudeStreamJsonAdapter::new().with_resolver(Arc::new(FnResolver(
        |_sid: &str,
         _req: &ccteam_harness::execution::claude_stream_json::bridge::CanUseToolReq| {
            ApprovalDecision::allow()
        },
    )));

    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "alice".into(),
            },
            &ctx_hitl(tmp.path(), "demo", "s2"),
        )
        .await
        .expect("start hitl");

    let stream_handle = handle.clone();
    let submit = adapter.submit_turn(&handle, TurnInput::UserText("read a file".into()));
    let (answer, _) = tokio::join!(collect_answer(&adapter, &stream_handle), submit).0;
    assert_eq!(answer.as_deref(), Some("ran:Read"));

    std::env::remove_var("FAKE_SJ_ASK_TOOL");
    adapter.close_thread(&handle).await.unwrap();
}

// ── Wave 4 (E3): fault × channel matrix ─────────────────────────────────
//
// Axis-parameterized fault fixture (PRD §七 ③). The matrix is
// {channel} × {fault}; a future host axis (×{local, satellite, k8s}) is an
// added parameter, not a rewrite. The `terminal` (tmux) channel's faults
// are covered by the `claude_tui` soak tests; this fixture exercises the
// NEW `stream-json` channel. Invariants: outbound no-loss-no-dup (exactly
// one answer per turn), failures carry a human signal, in-flight loss is
// never silent, resume continues the session.

#[derive(Clone, Copy, Debug)]
enum Channel {
    StreamJson,
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    IdleClose,
    ChildDeathMidTurn,
    ErrorResult,
    DaemonRestartResume,
}

/// Drain to a terminal event: (answer_count, last_answer, completed, failure).
async fn collect_outcome(
    adapter: &ClaudeStreamJsonAdapter,
    handle: &ccteam_harness::ThreadHandle,
) -> (usize, Option<String>, bool, Option<String>) {
    let mut stream = adapter.events(handle);
    let (mut answers, mut last, mut completed, mut failure) = (0usize, None, false, None);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, stream.next()).await {
        match ev {
            ThreadEvent::ItemCompleted { item } => {
                if let ThreadItemDetails::AgentMessage(t) = item.details {
                    answers += 1;
                    last = Some(t);
                }
            }
            ThreadEvent::TurnCompleted { .. } => {
                completed = true;
                break;
            }
            ThreadEvent::TurnFailed { err, .. } => {
                failure = Some(err.message);
                break;
            }
            _ => {}
        }
    }
    (answers, last, completed, failure)
}

async fn run_fault_case(tmp: &Path, channel: Channel, fault: Fault) {
    let Channel::StreamJson = channel;
    let adapter = ClaudeStreamJsonAdapter::new();
    match fault {
        Fault::IdleClose => {
            let h = adapter
                .start_thread(
                    &AgentSpecBrief { role: "a".into() },
                    &ctx(tmp, "demo", "s1"),
                )
                .await
                .expect("start");
            let sh = h.clone();
            let submit = adapter.submit_turn(&h, TurnInput::UserText("hi".into()));
            let (answers, last, completed, failure) =
                tokio::join!(collect_outcome(&adapter, &sh), submit).0;
            assert_eq!(answers, 1, "exactly one answer per turn (no-dup)");
            assert_eq!(last.as_deref(), Some("ok"));
            assert!(completed && failure.is_none());
            adapter.close_thread(&h).await.unwrap();
            let (_, _, _, post) = collect_outcome(&adapter, &h).await;
            assert!(post.is_none(), "idle close must NOT signal a failure");
        }
        Fault::ChildDeathMidTurn => {
            std::env::set_var("FAKE_SJ_DIE_MID_TURN", "1");
            let h = adapter
                .start_thread(
                    &AgentSpecBrief { role: "a".into() },
                    &ctx(tmp, "demo", "s1"),
                )
                .await
                .expect("start");
            let sh = h.clone();
            let submit = adapter.submit_turn(&h, TurnInput::UserText("go".into()));
            let (_, _, completed, failure) = tokio::join!(collect_outcome(&adapter, &sh), submit).0;
            std::env::remove_var("FAKE_SJ_DIE_MID_TURN");
            assert!(!completed, "in-flight death must not complete the turn");
            let msg = failure.expect("in-flight loss MUST emit a human signal");
            assert!(msg.contains("stream-json"), "human signal: {msg}");
            adapter.close_thread(&h).await.unwrap();
        }
        Fault::ErrorResult => {
            std::env::set_var("FAKE_SJ_ERROR_RESULT", "1");
            let h = adapter
                .start_thread(
                    &AgentSpecBrief { role: "a".into() },
                    &ctx(tmp, "demo", "s1"),
                )
                .await
                .expect("start");
            let sh = h.clone();
            let submit = adapter.submit_turn(&h, TurnInput::UserText("go".into()));
            let (_, _, _, failure) = tokio::join!(collect_outcome(&adapter, &sh), submit).0;
            std::env::remove_var("FAKE_SJ_ERROR_RESULT");
            let msg = failure.expect("an error result MUST surface a failure signal");
            assert!(msg.contains("error_during_execution"), "error kind: {msg}");
            adapter.close_thread(&h).await.unwrap();
        }
        Fault::DaemonRestartResume => {
            let h1 = adapter
                .start_thread(
                    &AgentSpecBrief { role: "a".into() },
                    &ctx(tmp, "demo", "s1"),
                )
                .await
                .expect("start");
            adapter.close_thread(&h1).await.unwrap();
            // A FRESH adapter = the restarted daemon (empty live map). The
            // transcript exists → start_thread re-spawns via --resume.
            let uuid = deterministic_session_uuid("demo", "s1");
            let dir = anthropic_project_dir(tmp).expect("anthropic dir");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{uuid}.jsonl")), "{}\n").unwrap();
            let restarted = ClaudeStreamJsonAdapter::new();
            let h2 = restarted
                .start_thread(
                    &AgentSpecBrief { role: "a".into() },
                    &ctx(tmp, "demo", "s1"),
                )
                .await
                .expect("restart");
            assert_eq!(argv_mode(tmp), format!("resume {uuid}"), "must --resume");
            let sh = h2.clone();
            let submit = restarted.submit_turn(&h2, TurnInput::UserText("again".into()));
            let (answers, last, completed, _) =
                tokio::join!(collect_outcome(&restarted, &sh), submit).0;
            assert_eq!(answers, 1, "resumed session answers exactly once");
            assert_eq!(last.as_deref(), Some("ok"));
            assert!(completed);
            restarted.close_thread(&h2).await.unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn fault_matrix_stream_json() {
    for fault in [
        Fault::IdleClose,
        Fault::ChildDeathMidTurn,
        Fault::ErrorResult,
        Fault::DaemonRestartResume,
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        setup(tmp.path());
        run_fault_case(tmp.path(), Channel::StreamJson, fault).await;
    }
}

/// E3 — the resume→fresh fallback emits a `chat_session_reset` event
/// carrying the sid + a reason (the honest "context was lost" signal),
/// never silently. We force the resume spawn to die (FAKE_SJ_DIE_ON_RESUME)
/// so start_thread falls back to a fresh `--session-id` spawn + the reset.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn resume_failure_emits_reset_event_with_sid_and_reason() {
    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    // Pre-create the transcript so start_thread chooses --resume first.
    let uuid = deterministic_session_uuid("demo", "s1");
    let dir = anthropic_project_dir(tmp.path()).expect("anthropic dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{uuid}.jsonl")), "{}\n").unwrap();
    // Resume spawn dies; fresh spawn lives.
    std::env::set_var("FAKE_SJ_DIE_ON_RESUME", "1");
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "1200");

    let adapter = ClaudeStreamJsonAdapter::new();
    let h = adapter
        .start_thread(
            &AgentSpecBrief { role: "a".into() },
            &ctx(tmp.path(), "demo", "s1"),
        )
        .await
        .expect("start_thread should fall back to fresh after resume death");
    // The fresh fallback ran (the recorded mode is session-id, not resume).
    assert_eq!(argv_mode(tmp.path()), format!("session-id {uuid}"));

    // The reset event landed in the progress jsonl with sid + reason.
    // The path is `<ccteam root>/state/progress/<slug>.jsonl` — this test read
    // it WITHOUT the `state/` segment, so it asserted on an always-empty file
    // and failed deterministically wherever the emit path was correct. (The
    // sibling `claude_tui_resume_test` spells the same path right.)
    let progress = tmp
        .path()
        .join(".ccteam")
        .join("state")
        .join("progress")
        .join("demo.jsonl");
    let body = std::fs::read_to_string(&progress).unwrap_or_default();
    assert!(
        body.contains("\"s1\"") && body.contains("resume_failed_fallback_to_fresh"),
        "reset event must carry sid + reason; got: {body}"
    );

    std::env::remove_var("FAKE_SJ_DIE_ON_RESUME");
    std::env::remove_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS");
    adapter.close_thread(&h).await.unwrap();
}

// ─────────────── one sid, one body (2026-08-19) ───────────────

/// A spawn records its body (`<project>/.ccteam/chat/<sid>/body.json`, pid +
/// start fingerprint) BEFORE the handshake; an explicit `close_thread` ends
/// the process and clears the record; a `detach_thread` (daemon shutdown)
/// leaves the process running AND keeps the record — the facts the next
/// daemon's gate needs to wait for this body instead of spawning a twin.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn body_record_follows_spawn_close_and_detach() {
    use ccteam_harness::execution::session_body::{self, BodyProbe};
    use ccteam_harness::DetachOutcome;

    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    // Linger 30s after stdin EOF: this body "outlives the daemon".
    std::env::set_var("FAKE_SJ_LINGER_SECS", "30");
    let adapter = ClaudeStreamJsonAdapter::new();
    let spec = AgentSpecBrief {
        role: String::new(),
    };

    // spawn → recorded + alive
    let h = adapter
        .start_thread(&spec, &ctx(tmp.path(), "proj", "s41"))
        .await
        .unwrap();
    let body = session_body::read(tmp.path(), "s41").expect("body.json written at spawn");
    assert_eq!(body.adapter, "claude-stream-json");
    assert!(matches!(
        session_body::probe(tmp.path(), "s41"),
        BodyProbe::Alive(_)
    ));

    // detach → process still alive, record kept, session no longer live here
    match adapter.detach_thread(&h).await.unwrap() {
        DetachOutcome::Detached { pid, in_flight } => {
            assert_eq!(pid, Some(body.pid));
            assert!(!in_flight, "no turn was in flight");
        }
        other => panic!("expected Detached, got {other:?}"),
    }
    assert!(!adapter.thread_is_live(&h));
    assert!(
        matches!(session_body::probe(tmp.path(), "s41"), BodyProbe::Alive(_)),
        "the detached body keeps running and keeps its record"
    );
    // A second detach is a no-op (nothing held any more).
    assert_eq!(
        adapter.detach_thread(&h).await.unwrap(),
        DetachOutcome::NotApplicable
    );
    // Tidy: end the lingering fake so it does not outlive the test.
    session_body::terminate(&body, "s41", Duration::from_secs(3)).unwrap();
    assert!(matches!(
        session_body::probe(tmp.path(), "s41"),
        BodyProbe::Gone(_)
    ));
    session_body::clear(tmp.path(), "s41");

    // spawn again → close → process gone AND record cleared
    std::env::remove_var("FAKE_SJ_LINGER_SECS");
    let h2 = adapter
        .start_thread(&spec, &ctx(tmp.path(), "proj", "s41"))
        .await
        .unwrap();
    let body2 = session_body::read(tmp.path(), "s41").expect("re-spawn re-records");
    assert_ne!(body2.pid, body.pid);
    adapter.close_thread(&h2).await.unwrap();
    // close = explicit kill + clear (the clear-on-exit task may race; wait briefly)
    for _ in 0..50 {
        if matches!(session_body::probe(tmp.path(), "s41"), BodyProbe::Absent) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(session_body::probe(tmp.path(), "s41"), BodyProbe::Absent);
    assert!(
        !session_body::process_exists(body2.pid) || {
            // killed but maybe not yet reaped by tokio: a zombie is not a body
            !session_body::body_is_alive(&body2, "s41")
        }
    );
}

/// The body's own exit (no detach, no close — it just ended) clears the record
/// too: the clear-on-close task watches the transport, so a crash / idle exit
/// never leaves a stale record behind while THIS daemon is alive.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn body_record_clears_when_the_child_exits_on_its_own() {
    use ccteam_harness::execution::session_body::{self, BodyProbe};

    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let spec = AgentSpecBrief {
        role: String::new(),
    };
    let h = adapter
        .start_thread(&spec, &ctx(tmp.path(), "proj", "s42"))
        .await
        .unwrap();
    let body = session_body::read(tmp.path(), "s42").unwrap();
    assert!(matches!(
        session_body::probe(tmp.path(), "s42"),
        BodyProbe::Alive(_)
    ));
    // End the child from outside (a crash): the transport sees EOF.
    session_body::terminate(&body, "s42", Duration::from_secs(3)).unwrap();
    for _ in 0..100 {
        if matches!(session_body::probe(tmp.path(), "s42"), BodyProbe::Absent) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        session_body::probe(tmp.path(), "s42"),
        BodyProbe::Absent,
        "observed exit clears the record"
    );
    assert!(!adapter.thread_is_live(&h));
}

/// `recover_unobserved_turn` reads claude's own transcript jsonl for the
/// session's deterministic uuid under the pinned HOME: end_turn text newer
/// than `observed_until` comes back as the recovered answer (+ usage).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn recover_unobserved_turn_reads_the_vendor_transcript() {
    use ccteam_harness::UnobservedTurnCtx;

    let tmp = tempfile::TempDir::new().unwrap();
    setup(tmp.path());
    let adapter = ClaudeStreamJsonAdapter::new();
    let uuid = deterministic_session_uuid("proj", "s43");
    let dir = anthropic_project_dir(tmp.path()).expect("HOME is pinned");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        serde_json::json!({"type":"assistant","timestamp":"2026-08-19T02:00:00.000Z",
            "message":{"role":"assistant","stop_reason":"end_turn",
                "content":[{"type":"text","text":"observed"}],
                "usage":{"input_tokens":1,"output_tokens":1}}}),
        serde_json::json!({"type":"assistant","timestamp":"2026-08-19T02:30:00.000Z",
            "message":{"role":"assistant","stop_reason":"tool_use",
                "content":[{"type":"text","text":"working on it"},{"type":"tool_use","name":"Bash","input":{}}],
                "usage":{"input_tokens":10,"output_tokens":20}}}),
        serde_json::json!({"type":"assistant","timestamp":"2026-08-19T02:40:00.000Z",
            "message":{"role":"assistant","stop_reason":"end_turn",
                "content":[{"type":"text","text":"DONE after restart"}],
                "usage":{"input_tokens":5,"output_tokens":6}}}),
    ];
    let body = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.join(format!("{uuid}.jsonl")), body).unwrap();

    let observed_until = chrono::DateTime::parse_from_rfc3339("2026-08-19T02:09:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let recovered = adapter
        .recover_unobserved_turn(&UnobservedTurnCtx {
            sid: "s43".into(),
            slug: "proj".into(),
            cwd: tmp.path().to_path_buf(),
            vendor_uuid: uuid.clone(),
            observed_until,
            last_observed_assistant: Some("observed".into()),
        })
        .await
        .expect("the unobserved end_turn is recovered");
    // issue #192 — the mid-turn text block is part of the turn's answer.
    assert_eq!(recovered.assistant, "working on it\n\nDONE after restart");
    assert_eq!(recovered.usage["input_tokens"], 15);
    assert_eq!(recovered.usage["output_tokens"], 26);

    // Nothing newer than the cut → None (an idle body has nothing to report).
    let later = chrono::DateTime::parse_from_rfc3339("2026-08-19T03:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(adapter
        .recover_unobserved_turn(&UnobservedTurnCtx {
            sid: "s43".into(),
            slug: "proj".into(),
            cwd: tmp.path().to_path_buf(),
            vendor_uuid: uuid,
            observed_until: later,
            last_observed_assistant: None,
        })
        .await
        .is_none());
}
