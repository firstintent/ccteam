//! V0.6.0 F108 — transcript-tail byte-offset incremental read tests.

use std::io::Write as _;
use std::path::Path;

use ccteam_harness::execution::transcript_tail::{
    cursor_path, encode_project_cwd, parse_transcript_line, read_new, PendingTools,
    TranscriptCursor,
};
use ccteam_harness::{ThreadEvent, ThreadItemDetails};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn cursor_path_under_chat_dir() {
    let p = cursor_path(Path::new("/p"), "alice");
    assert_eq!(p, Path::new("/p/.ccteam/chat/alice/transcript-cursor.json"));
}

#[test]
fn encode_project_cwd_replaces_slashes() {
    assert_eq!(
        encode_project_cwd(Path::new("/home/u/projects/dev-foo")),
        "-home-u-projects-dev-foo"
    );
}

#[tokio::test]
async fn read_new_emits_assistant_text_event() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("sess.jsonl");
    let row = serde_json::to_string(&json!({
        "type":"assistant","uuid":"u1",
        "message":{"content":[{"type":"text","text":"hi there"}]}
    }))
    .unwrap();
    std::fs::write(&path, format!("{row}\n")).unwrap();
    let cursor = TranscriptCursor::default();
    let delta = read_new(&path, &cursor, PendingTools::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.events.len(), 1);
    match &delta.events[0] {
        ThreadEvent::ItemCompleted { item } => match &item.details {
            ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hi there"),
            _ => panic!("wrong detail"),
        },
        _ => panic!("wrong event"),
    }
    assert_eq!(delta.new_offset, (row.len() + 1) as u64);
}

#[tokio::test]
async fn read_new_resumes_from_cursor_offset() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("sess.jsonl");
    let r1 = serde_json::to_string(&json!({
        "type":"assistant","uuid":"u1",
        "message":{"content":[{"type":"text","text":"first"}]}
    }))
    .unwrap();
    let r2 = serde_json::to_string(&json!({
        "type":"assistant","uuid":"u2",
        "message":{"content":[{"type":"text","text":"second"}]}
    }))
    .unwrap();
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{r1}").unwrap();
    writeln!(f, "{r2}").unwrap();
    drop(f);

    let cursor = TranscriptCursor {
        byte_offset: (r1.len() + 1) as u64,
        ..Default::default()
    };
    let delta = read_new(&path, &cursor, PendingTools::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.events.len(), 1, "only the post-cursor row");
    match &delta.events[0] {
        ThreadEvent::ItemCompleted { item } => match &item.details {
            ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "second"),
            _ => panic!("wrong detail"),
        },
        _ => panic!("wrong event"),
    }
}

#[tokio::test]
async fn read_new_pairs_tool_use_with_tool_result_across_calls() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("sess.jsonl");

    // Tick 1: tool_use only.
    let use_row = serde_json::to_string(&json!({
        "type":"assistant","uuid":"u1",
        "message":{"content":[{
            "type":"tool_use","id":"t42","name":"Read",
            "input":{"file_path":"/x"}
        }]}
    }))
    .unwrap();
    std::fs::write(&path, format!("{use_row}\n")).unwrap();
    let cursor = TranscriptCursor::default();
    let d1 = read_new(&path, &cursor, PendingTools::new())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(d1.events[0], ThreadEvent::ItemStarted { .. }));
    assert!(d1.pending_tools.contains_key("t42"));

    // Tick 2: append tool_result.
    let res_row = serde_json::to_string(&json!({
        "type":"user","uuid":"u2",
        "message":{"content":[{
            "type":"tool_result","tool_use_id":"t42","content":"ok"
        }]}
    }))
    .unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{res_row}").unwrap();
    drop(f);

    let cursor = TranscriptCursor {
        byte_offset: d1.new_offset,
        ..Default::default()
    };
    let d2 = read_new(&path, &cursor, d1.pending_tools)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d2.events.len(), 1);
    match &d2.events[0] {
        ThreadEvent::ItemCompleted { item } => match &item.details {
            ThreadItemDetails::ToolCall { name, .. } => {
                assert_eq!(name, "Read", "name pulled from pending map");
            }
            _ => panic!("wrong detail"),
        },
        _ => panic!("wrong event"),
    }
    assert!(!d2.pending_tools.contains_key("t42"));
}

#[test]
fn parse_unknown_block_type_yields_zero_events() {
    let mut pending = PendingTools::new();
    let row = json!({
        "type": "assistant",
        "uuid": "u1",
        "message": {"content": [{"type": "weird-future-block", "x": 1}]}
    });
    let events = parse_transcript_line(&row, &mut pending);
    assert!(events.is_empty());
}

#[tokio::test]
async fn read_new_returns_none_on_missing_file() {
    let tmp = TempDir::new().unwrap();
    let cursor = TranscriptCursor::default();
    let d = read_new(
        &tmp.path().join("nothing.jsonl"),
        &cursor,
        PendingTools::new(),
    )
    .await
    .unwrap();
    assert!(d.is_none());
}

#[test]
fn cursor_save_and_load_round_trip() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("c.json");
    let c = TranscriptCursor {
        project_encoded: "-foo".into(),
        session_id: "ab".into(),
        byte_offset: 123,
        last_event_id: None,
        prior_offsets: Default::default(),
    };
    c.save(&p).unwrap();
    let back = TranscriptCursor::load(&p).unwrap();
    assert_eq!(back.session_id, "ab");
    assert_eq!(back.byte_offset, 123);
}
