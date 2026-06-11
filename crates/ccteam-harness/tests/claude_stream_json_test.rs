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

use ccteam_harness::execution::claude_stream_json::spawn_spec::deterministic_session_uuid;
use ccteam_harness::execution::claude_stream_json::ClaudeStreamJsonAdapter;
use ccteam_harness::execution::transcript_tail::anthropic_project_dir;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, PermissionMode,
    SpawnCtx, ThreadEvent, ThreadItemDetails, TurnInput,
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
def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
cmds = os.environ.get("FAKE_SJ_INIT_COMMANDS", "compact,context,clear").split(",")
emit({"type":"system","subtype":"init","session_id":sid,"model":"fake-model",
      "slash_commands":cmds,"tools":["Bash"]})
if os.environ.get("FAKE_SJ_DIE_AFTER_INIT") == "1":
    sys.exit(0)
reply = os.environ.get("FAKE_SJ_REPLY", "ok")
for line in sys.stdin:
    if not line.strip():
        continue
    emit({"type":"assistant","session_id":sid,
          "message":{"role":"assistant","content":[{"type":"text","text":reply}]}})
    emit({"type":"result","subtype":"success","result":reply,"is_error":False,
          "total_cost_usd":0.001,"usage":{"input_tokens":7,"output_tokens":4},
          "session_id":sid})
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
    fake
}

fn ctx(tmp: &Path, slug: &str, sid: &str) -> SpawnCtx {
    SpawnCtx {
        slug: slug.to_string(),
        sid: sid.to_string(),
        cwd: tmp.to_path_buf(),
        project_dir: tmp.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
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
