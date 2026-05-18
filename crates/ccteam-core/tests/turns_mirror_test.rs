//! V0.6.0 F108 — `<bot>/turns.jsonl` mirror tests.

use ccteam_core::execution::turns_mirror::{
    append_turn, last_n_turns, read_all_turns, turns_jsonl_path, ToolCallSummary, TurnRecord,
};
use chrono::Utc;
use serde_json::Value;
use tempfile::TempDir;

fn turn(id: &str, user: &str, assistant: &str) -> TurnRecord {
    TurnRecord {
        turn_id: id.into(),
        ts: Utc::now(),
        vendor: "claude".into(),
        role: "alice".into(),
        user: user.into(),
        assistant: assistant.into(),
        usage: Value::Null,
        tool_calls: vec![],
    }
}

#[test]
fn append_then_read_preserves_order_and_content() {
    let tmp = TempDir::new().unwrap();
    for i in 0..4 {
        append_turn(
            tmp.path(),
            "alice",
            &turn(&format!("t{i}"), &format!("u{i}"), &format!("a{i}")),
        )
        .unwrap();
    }
    let all = read_all_turns(tmp.path(), "alice").unwrap();
    assert_eq!(all.len(), 4);
    for (i, r) in all.iter().enumerate() {
        assert_eq!(r.turn_id, format!("t{i}"));
        assert_eq!(r.user, format!("u{i}"));
    }
}

#[test]
fn last_n_returns_tail_in_chronological_order() {
    let tmp = TempDir::new().unwrap();
    for i in 0..10 {
        append_turn(tmp.path(), "bob", &turn(&format!("t{i}"), "u", "a")).unwrap();
    }
    let tail = last_n_turns(tmp.path(), "bob", 3).unwrap();
    assert_eq!(tail.len(), 3);
    assert_eq!(tail[0].turn_id, "t7");
    assert_eq!(tail[2].turn_id, "t9");
}

#[test]
fn tool_call_summary_round_trips() {
    let tmp = TempDir::new().unwrap();
    let mut r = turn("t0", "u", "a");
    r.tool_calls.push(ToolCallSummary {
        name: "Read".into(),
        summary: Some("/x".into()),
    });
    append_turn(tmp.path(), "carol", &r).unwrap();
    let read = read_all_turns(tmp.path(), "carol").unwrap();
    assert_eq!(read[0].tool_calls.len(), 1);
    assert_eq!(read[0].tool_calls[0].name, "Read");
}

#[test]
fn read_all_missing_file_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let all = read_all_turns(tmp.path(), "ghost").unwrap();
    assert!(all.is_empty());
}

#[test]
fn turns_jsonl_path_uses_chat_subdir() {
    let p = turns_jsonl_path(std::path::Path::new("/proj"), "alice");
    assert!(p.ends_with(".ccteam/chat/alice/turns.jsonl"));
}
