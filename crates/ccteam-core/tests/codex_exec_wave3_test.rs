//! V0.6.0 Wave 3 F112 — `CodexExecAdapter` `submit_turn` /
//! `resume_thread` / events stream tests. All tests use a fake codex
//! script via `CCTEAM_CODEX_BIN` env override so they don't require
//! the real codex binary on PATH.

use ccteam_core::execution::codex_app_server::CODEX_BIN_ENV;
use ccteam_core::execution::codex_exec::{build_exec_argv, render_prompt, translate_jsonl_event};
use ccteam_core::execution::CodexExecAdapter;
use ccteam_harness::{
    AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, ThreadEvent, ThreadHandle, TurnId,
    TurnInput,
};
use futures::StreamExt;
use serde_json::json;
use std::io::Write;
use std::time::Duration;

/// Build a fake codex script that emits the supplied JSONL lines on
/// stdout and exits 0. Returns the tempdir guard whose `path().join("codex.sh")`
/// is the script. We use a tempdir (closing the fd before exec) so
/// Linux's "Text file busy" guard doesn't trip when the test spawns
/// the script.
fn fake_codex_emitting(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("codex.sh");
    let mut body = String::from("#!/usr/bin/env bash\n");
    body.push_str("# fake codex CLI for ccteam wave-3 tests\n");
    body.push_str("cat <<'EOF'\n");
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    body.push_str("EOF\n");
    body.push_str("exit 0\n");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    (dir, path)
}

fn handle() -> ThreadHandle {
    ThreadHandle {
        vendor: AgentVendor::Codex,
        mode: ExecutionMode::Bg,
        identity: "ccteam-test-codex-1".into(),
        started_at: chrono::Utc::now(),
        raw_extras: json!({}),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn submit_turn_returns_monotonic_turn_ids() {
    std::env::set_var(CODEX_BIN_ENV, "/bin/true");
    let adapter = CodexExecAdapter::new();
    let h = handle();
    let a = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .unwrap();
    let b = adapter
        .submit_turn(&h, TurnInput::UserText("there".into()))
        .await
        .unwrap();
    assert_ne!(a.0, b.0);
    assert!(a.0.starts_with("codex-exec-"));
    std::env::remove_var(CODEX_BIN_ENV);
}

#[tokio::test(flavor = "current_thread")]
async fn submit_turn_rejects_system_directive() {
    let adapter = CodexExecAdapter::new();
    let h = handle();
    let err = adapter
        .submit_turn(&h, TurnInput::SystemDirective("/compact".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::SubmitFailed(_)), "got {err:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn events_stream_translates_jsonl_to_thread_events() {
    let (_dir, script_path) = fake_codex_emitting(&[
        r#"{"type":"thread.started","thread_id":"t-1"}"#,
        r#"{"type":"turn.started"}"#,
        r#"{"type":"item.completed","item":{"id":"i-1","type":"agent_message","text":"hello"}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
    ]);
    std::env::set_var(CODEX_BIN_ENV, &script_path);

    let adapter = CodexExecAdapter::new();
    let h = handle();
    let stream = adapter.events(&h);
    // Subscribe BEFORE submit_turn so the broadcast picks up the first
    // events from the fake codex subprocess.
    let _tid = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .unwrap();

    let collected: Vec<ThreadEvent> =
        tokio::time::timeout(Duration::from_secs(3), stream.take(4).collect::<Vec<_>>())
            .await
            .expect("events stream timed out");

    assert!(matches!(collected[0], ThreadEvent::ThreadStarted { .. }));
    assert!(matches!(collected[1], ThreadEvent::TurnStarted { .. }));
    assert!(matches!(collected[2], ThreadEvent::ItemCompleted { .. }));
    match &collected[3] {
        ThreadEvent::TurnCompleted { usage, .. } => {
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 5);
        }
        other => panic!("expected TurnCompleted, got {other:?}"),
    }

    std::env::remove_var(CODEX_BIN_ENV);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_thread_synthesises_handle_with_resumed_extras() {
    let h = CodexExecAdapter::new()
        .resume_thread("01abc-feed-face")
        .await
        .unwrap();
    assert_eq!(h.identity, "01abc-feed-face");
    assert_eq!(h.raw_extras["resumed"], true);
    assert_eq!(h.raw_extras["thread_id"], "01abc-feed-face");
    assert_eq!(h.vendor, AgentVendor::Codex);
}

#[tokio::test(flavor = "current_thread")]
async fn build_exec_argv_resume_branch() {
    let exec = build_exec_argv(None);
    assert_eq!(exec, vec!["exec", "--json", "--skip-git-repo-check", "-"]);
    let resume = build_exec_argv(Some("UUID"));
    assert_eq!(
        resume,
        vec!["resume", "UUID", "--json", "--skip-git-repo-check", "-"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn render_prompt_artifact_wraps_in_tag() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    tmp.write_all(b"the body").unwrap();
    let p = tmp.path().to_path_buf();
    let s = render_prompt(&TurnInput::Artifact(p)).unwrap();
    assert!(s.contains("<artifact"));
    assert!(s.contains("the body"));
    assert!(s.contains("</artifact>"));
}

#[test]
fn translate_jsonl_event_thread_started() {
    let v = json!({ "type": "thread.started", "thread_id": "t-99" });
    let evts = translate_jsonl_event(&v, &TurnId("u".into()));
    assert_eq!(evts.len(), 1);
    match &evts[0] {
        ThreadEvent::ThreadStarted { thread_id } => assert_eq!(thread_id, "t-99"),
        _ => panic!(),
    }
}

#[test]
fn translate_jsonl_event_unknown_type_returns_empty() {
    let v = json!({ "type": "totally.bogus" });
    let evts = translate_jsonl_event(&v, &TurnId("u".into()));
    assert!(evts.is_empty());
}
