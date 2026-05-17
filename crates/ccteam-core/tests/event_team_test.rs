//! V0.5.0 F94 — `TeamEvent` enum serde round-trip coverage.
//!
//! The on-wire schema for the 6 team_* events is fixed by
//! `docs/interfaces.md §4.1`. This file exercises each variant
//! through `serde_json::{from_value, to_value}` so a schema drift in
//! `orchestrator.rs::TeamEvent` shows up as a test failure rather
//! than as a runtime parse error in the web SPA SSE consumer.
//!
//! F95 watcher emit-path coverage (untyped Value writes round-tripping
//! through the typed enum) lives in
//! `agent_teams_watcher_test.rs::typed_team_event_round_trips_watcher_emitted_payloads`.
//! This file complements that with explicit assertions on the
//! `team_teammate_idle` variant (F94 hook-only — never emitted by the
//! watcher).

use ccteam_core::TeamEvent;
use serde_json::json;

#[test]
fn team_member_joined_round_trips() {
    let raw = json!({
        "event": "team_member_joined",
        "team_name": "flaky-debate",
        "teammate_name": "pm",
        "agent_id": "abc123",
        "agent_type": "general-purpose",
        "model": "sonnet",
        "color": "orange",
        "cwd": "/home/user/projects/flaky-debate",
        "backend_type": "in-process",
        "definition_backed": false,
        "started_at": "2026-05-17T12:00:00Z",
        "ts": "2026-05-17T12:00:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).expect("parse");
    matches!(parsed, TeamEvent::TeamMemberJoined { .. });
    let back = serde_json::to_value(&parsed).expect("re-serialize");
    assert_eq!(raw["event"], back["event"]);
    assert_eq!(raw["team_name"], back["team_name"]);
    assert_eq!(raw["color"], back["color"]);
}

#[test]
fn team_member_left_round_trips() {
    let raw = json!({
        "event": "team_member_left",
        "team_name": "flaky-debate",
        "teammate_name": "pm",
        "ts": "2026-05-17T12:00:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).unwrap();
    matches!(parsed, TeamEvent::TeamMemberLeft { .. });
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(raw, back);
}

#[test]
fn team_message_sent_round_trips_with_optional_color() {
    let raw = json!({
        "event": "team_message_sent",
        "team_name": "flaky-debate",
        "from": "pm",
        "to": "team-lead",
        "text_truncated": "I found something",
        "msg_ts": "2026-05-17T12:00:00Z",
        "color": "orange",
        "read": false,
        "ts": "2026-05-17T12:00:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).unwrap();
    matches!(parsed, TeamEvent::TeamMessageSent { .. });
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(raw["from"], back["from"]);
    assert_eq!(raw["text_truncated"], back["text_truncated"]);
}

#[test]
fn team_task_created_round_trips_with_optional_assignee() {
    let raw = json!({
        "event": "team_task_created",
        "team_name": "flaky-debate",
        "task_id": "42",
        "title": "Find the race",
        "assignee": "pm",
        "dependencies": ["3"],
        "ts": "2026-05-17T12:00:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).unwrap();
    matches!(parsed, TeamEvent::TeamTaskCreated { .. });
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(raw["task_id"], back["task_id"]);
    assert_eq!(raw["assignee"], back["assignee"]);
    assert_eq!(raw["dependencies"], back["dependencies"]);
}

#[test]
fn team_task_completed_round_trips() {
    let raw = json!({
        "event": "team_task_completed",
        "team_name": "flaky-debate",
        "task_id": "42",
        "result_summary": "race in src/auth/sessions.rs:340",
        "completed_at": "2026-05-17T12:30:00Z",
        "ts": "2026-05-17T12:30:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).unwrap();
    matches!(parsed, TeamEvent::TeamTaskCompleted { .. });
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(raw["task_id"], back["task_id"]);
    assert_eq!(raw["completed_at"], back["completed_at"]);
}

#[test]
fn team_teammate_idle_round_trips() {
    // V0.5.0 F94 — the 6th variant, hook-only. Verify both required
    // (`team_name`, `teammate_name`, `idle_since`, `ts`) and optional
    // (`idle_reason`) fields survive the round-trip.
    let raw = json!({
        "event": "team_teammate_idle",
        "team_name": "flaky-debate",
        "teammate_name": "pm",
        "idle_reason": "available",
        "idle_since": "2026-05-17T12:30:00Z",
        "ts": "2026-05-17T12:30:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).expect("parse");
    matches!(parsed, TeamEvent::TeamTeammateIdle { .. });
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(raw["event"], back["event"]);
    assert_eq!(raw["teammate_name"], back["teammate_name"]);
    assert_eq!(raw["idle_reason"], back["idle_reason"]);
    assert_eq!(raw["idle_since"], back["idle_since"]);
}

#[test]
fn team_teammate_idle_optional_reason_omitted() {
    let raw = json!({
        "event": "team_teammate_idle",
        "team_name": "flaky-debate",
        "teammate_name": "pm",
        "idle_since": "2026-05-17T12:30:00Z",
        "ts": "2026-05-17T12:30:01Z",
    });
    let parsed: TeamEvent = serde_json::from_value(raw.clone()).expect("parse without idle_reason");
    matches!(parsed, TeamEvent::TeamTeammateIdle { .. });
    let back = serde_json::to_value(&parsed).unwrap();
    // The optional `idle_reason` shouldn't appear at all when omitted.
    assert!(
        back.get("idle_reason").is_none(),
        "skip_serializing_if must drop missing idle_reason; got {back}",
    );
}
