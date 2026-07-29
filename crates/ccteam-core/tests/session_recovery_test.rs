//! V0.6.0 F118 — recovery prompt builder tests.

use ccteam_harness::execution::session_recovery::{build_recovery_prompt, format_recovery_prompt};
use ccteam_harness::execution::turns_mirror::{append_turn, TurnRecord};
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
        outcome: None,
        error_kind: None,
        error: None,
    }
}

#[test]
fn build_recovery_prompt_returns_empty_when_no_history() {
    let tmp = TempDir::new().unwrap();
    let plan = build_recovery_prompt(tmp.path(), "alice", 20).unwrap();
    assert_eq!(plan.recovered_turns, 0);
    assert!(plan.prompt.is_empty());
}

#[test]
fn build_recovery_prompt_renders_history_block() {
    let tmp = TempDir::new().unwrap();
    for i in 0..3 {
        append_turn(
            tmp.path(),
            "alice",
            &turn(&format!("t{i}"), &format!("u{i}"), &format!("a{i}")),
        )
        .unwrap();
    }
    let plan = build_recovery_prompt(tmp.path(), "alice", 20).unwrap();
    assert_eq!(plan.recovered_turns, 3);
    assert!(plan.prompt.contains("<conversation_history>"));
    assert!(plan.prompt.contains("</conversation_history>"));
    // Chronological order.
    let p_u0 = plan.prompt.find("u0").unwrap();
    let p_u2 = plan.prompt.find("u2").unwrap();
    assert!(p_u0 < p_u2);
}

#[test]
fn build_recovery_prompt_respects_last_n_bound() {
    let tmp = TempDir::new().unwrap();
    for i in 0..8 {
        append_turn(
            tmp.path(),
            "carol",
            &turn(&format!("t{i}"), &format!("u{i}"), &format!("a{i}")),
        )
        .unwrap();
    }
    let plan = build_recovery_prompt(tmp.path(), "carol", 3).unwrap();
    assert_eq!(plan.recovered_turns, 3);
    assert!(plan.prompt.contains("u5"));
    assert!(plan.prompt.contains("u7"));
    assert!(!plan.prompt.contains("[user] u0"));
}

#[test]
fn format_recovery_prompt_collapses_multiline_turns() {
    let mut t = turn("multi", "line1\nline2", "ok\nfine");
    t.user = "line1\nline2".into();
    let s = format_recovery_prompt(&[t]);
    // Newlines inside a turn should be replaced with the ¶ separator.
    assert!(s.contains("¶"));
}
