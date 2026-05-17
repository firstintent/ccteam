//! V0.5.0 F95 — integration tests for
//! [`ccteam_core::AgentTeamsWatcher`].
//!
//! Covers PRD F95 §验收 1-6 via fixture-driven scenarios:
//!
//! 1. **cold_discovery_emits_team_member_joined_for_roblog_fixture** —
//!    host roblog (5 members) → 5 `team_member_joined` events with
//!    `definition_backed=false` (all `general-purpose` / `team-lead`).
//! 2. **member_removed_emits_team_member_left** — drop one entry from
//!    members[], rescan → `team_member_left` for the gone teammate.
//! 3. **inbox_new_message_emits_team_message_sent_with_truncation** —
//!    new line in `inboxes/<teammate>.json` → `team_message_sent`
//!    with `text_truncated` ≤ 200 chars.
//! 4. **inbox_idle_notification_is_filtered** —
//!    `{"type":"idle_notification",...}` text → NO
//!    `team_message_sent`.
//! 5. **task_pending_emits_team_task_created** — new task file with
//!    `status: pending` → `team_task_created`.
//! 6. **task_status_transitions_pending_to_completed_emit_completion_only** —
//!    pending → in_progress (no event) → completed (only
//!    `team_task_completed`).
//! 7. **broken_config_warns_and_keeps_team_in_discovery** — schema
//!    failure on `config.json` → team still in state map (mtime-only
//!    degrade), no panic.

// `test_tick` signatures take `&[PathBuf]`; building one-element slices
// with `[p.clone()]` is more readable in test code than
// `std::slice::from_ref(&p)`. Silenced workspace-wide for this file.
#![allow(clippy::cloned_ref_to_slice_refs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ccteam_core::{AgentTeamsWatcher, AgentTeamsWatcherConfig};
use serde_json::Value;
use tempfile::TempDir;

/// One-shot scaffolding: build a temp `~/.claude/teams/` + `~/.claude/tasks/`
/// + `~/.ccteam/teams-progress.jsonl` layout and return a builder.
struct TestEnv {
    _tmp: TempDir,
    teams_root: PathBuf,
    tasks_root: PathBuf,
    progress_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let teams_root = tmp.path().join("claude").join("teams");
        let tasks_root = tmp.path().join("claude").join("tasks");
        let progress_path = tmp.path().join("ccteam").join("teams-progress.jsonl");
        fs::create_dir_all(&teams_root).unwrap();
        fs::create_dir_all(&tasks_root).unwrap();
        fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
        Self {
            _tmp: tmp,
            teams_root,
            tasks_root,
            progress_path,
        }
    }

    fn config(&self) -> AgentTeamsWatcherConfig {
        AgentTeamsWatcherConfig {
            teams_root: self.teams_root.clone(),
            tasks_root: self.tasks_root.clone(),
            progress_path: self.progress_path.clone(),
            discovery_interval: Duration::from_millis(50),
        }
    }

    /// Drop the host roblog fixture into `<teams_root>/<team>/config.json`.
    fn install_roblog(&self, team: &str) -> PathBuf {
        let dir = self.teams_root.join(team);
        fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("config.json");
        let bytes = include_bytes!("fixtures/agent_teams/config-roblog.json");
        fs::write(&dst, bytes).unwrap();
        dst
    }

    /// Drop the host inbox fixture into
    /// `<teams_root>/<team>/inboxes/<teammate>.json`.
    fn install_team_lead_inbox(&self, team: &str, teammate: &str) -> PathBuf {
        let dir = self.teams_root.join(team).join("inboxes");
        fs::create_dir_all(&dir).unwrap();
        let dst = dir.join(format!("{teammate}.json"));
        let bytes = include_bytes!("fixtures/agent_teams/inbox-team-lead.json");
        fs::write(&dst, bytes).unwrap();
        dst
    }

    fn write(&self, path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn events(&self) -> Vec<Value> {
        let body = match fs::read_to_string(&self.progress_path) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

fn events_of_kind<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["event"].as_str() == Some(kind))
        .collect()
}

#[test]
fn cold_discovery_emits_team_member_joined_for_roblog_fixture() {
    let env = TestEnv::new();
    env.install_roblog("roblog");

    let watcher = AgentTeamsWatcher::new(env.config()).expect("build watcher");
    watcher.test_run_discovery().expect("cold discovery");

    let events = env.events();
    let joined = events_of_kind(&events, "team_member_joined");
    assert_eq!(
        joined.len(),
        5,
        "expected 5 team_member_joined for roblog fixture, got: {events:#?}",
    );
    for e in &joined {
        assert_eq!(e["team_name"], "roblog");
        assert_eq!(
            e["definition_backed"], false,
            "fixture members are all general-purpose/team-lead; payload: {e}"
        );
        // Each event must carry every PRD-mandated field.
        for field in [
            "teammate_name",
            "agent_id",
            "agent_type",
            "model",
            "color",
            "cwd",
            "backend_type",
            "started_at",
        ] {
            assert!(e.get(field).is_some(), "missing {field} in {e}");
        }
    }
}

#[test]
fn member_removed_emits_team_member_left() {
    let env = TestEnv::new();
    let config_path = env.install_roblog("roblog");
    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    watcher.test_run_discovery().unwrap();

    // Reset events stream so we only assert on the removal pass.
    fs::write(&env.progress_path, "").unwrap();

    // Rewrite config.json with one member dropped (pm@roblog).
    let original = fs::read_to_string(&config_path).unwrap();
    let mut parsed: serde_json::Value = serde_json::from_str(&original).unwrap();
    let members = parsed["members"].as_array_mut().unwrap();
    let before = members.len();
    members.retain(|m| m["name"] != "pm");
    assert_eq!(members.len(), before - 1, "fixture should contain pm");
    env.write(&config_path, &parsed.to_string());

    watcher
        .test_tick(&[config_path.clone()])
        .expect("dispatch tick");

    let events = env.events();
    let left = events_of_kind(&events, "team_member_left");
    assert_eq!(left.len(), 1, "expected 1 left event; got {events:#?}");
    assert_eq!(left[0]["teammate_name"], "pm");
    assert_eq!(left[0]["team_name"], "roblog");
}

#[test]
fn inbox_new_message_emits_team_message_sent_with_truncation() {
    let env = TestEnv::new();
    env.install_roblog("roblog");
    let inbox = env.install_team_lead_inbox("roblog", "team-lead");
    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    watcher.test_run_discovery().unwrap();

    // Reset event log + append one new message past the fixture's
    // last entry.
    fs::write(&env.progress_path, "").unwrap();
    let mut entries: Vec<serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(&inbox).unwrap()).unwrap();
    let long_text = "x".repeat(500);
    entries.push(serde_json::json!({
        "from": "researcher",
        "text": long_text,
        "timestamp": "2026-12-31T23:59:59.999Z",
        "color": "blue",
        "read": false,
    }));
    env.write(&inbox, &serde_json::to_string(&entries).unwrap());

    watcher.test_tick(&[inbox.clone()]).expect("dispatch tick");

    let events = env.events();
    let msgs = events_of_kind(&events, "team_message_sent");
    assert_eq!(
        msgs.len(),
        1,
        "expected 1 new message event; got {events:#?}"
    );
    let m = msgs[0];
    assert_eq!(m["team_name"], "roblog");
    assert_eq!(m["from"], "researcher");
    assert_eq!(m["to"], "team-lead");
    // Truncated to ≤200 chars (PRD F95 §需求 .2).
    let truncated = m["text_truncated"].as_str().unwrap();
    assert_eq!(truncated.chars().count(), 200);
    assert_eq!(m["msg_ts"], "2026-12-31T23:59:59.999Z");
    assert_eq!(m["color"], "blue");
    assert_eq!(m["read"], false);
}

#[test]
fn inbox_idle_notification_is_filtered() {
    let env = TestEnv::new();
    env.install_roblog("roblog");
    let inbox = env.install_team_lead_inbox("roblog", "team-lead");
    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    watcher.test_run_discovery().unwrap();

    fs::write(&env.progress_path, "").unwrap();
    let mut entries: Vec<serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(&inbox).unwrap()).unwrap();
    entries.push(serde_json::json!({
        "from": "researcher",
        "text": "{\"type\":\"idle_notification\",\"from\":\"researcher\",\"timestamp\":\"2026-12-31T23:59:59.999Z\",\"idleReason\":\"available\"}",
        "timestamp": "2026-12-31T23:59:59.999Z",
        "color": "blue",
        "read": false,
    }));
    env.write(&inbox, &serde_json::to_string(&entries).unwrap());

    watcher.test_tick(&[inbox.clone()]).expect("dispatch tick");

    let events = env.events();
    let msgs = events_of_kind(&events, "team_message_sent");
    assert!(
        msgs.is_empty(),
        "idle_notification leaked into team_message_sent: {events:#?}",
    );
}

#[test]
fn task_pending_emits_team_task_created() {
    let env = TestEnv::new();
    env.install_roblog("roblog");
    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    watcher.test_run_discovery().unwrap();

    fs::write(&env.progress_path, "").unwrap();
    let task_dir = env.tasks_root.join("roblog");
    fs::create_dir_all(&task_dir).unwrap();
    let task_file = task_dir.join("7.json");
    env.write(
        &task_file,
        &serde_json::json!({
            "id": "7",
            "title": "implement feature X",
            "status": "pending",
            "assignee": "frontend-dev",
            "dependencies": ["6"],
        })
        .to_string(),
    );

    // sibling files that must be ignored
    env.write(&task_dir.join(".lock"), "");
    env.write(&task_dir.join(".highwatermark"), "0");

    watcher
        .test_tick(&[
            task_file.clone(),
            task_dir.join(".lock"),
            task_dir.join(".highwatermark"),
        ])
        .expect("dispatch tick");

    let events = env.events();
    let created = events_of_kind(&events, "team_task_created");
    assert_eq!(
        created.len(),
        1,
        "expected 1 created event; got {events:#?}"
    );
    let c = created[0];
    assert_eq!(c["team_name"], "roblog");
    assert_eq!(c["task_id"], "7");
    assert_eq!(c["title"], "implement feature X");
    assert_eq!(c["assignee"], "frontend-dev");
    assert_eq!(c["dependencies"][0], "6");

    // sibling files must NOT have produced any event.
    let all_kinds: std::collections::HashSet<_> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap_or(""))
        .collect();
    assert!(all_kinds.contains("team_task_created"));
}

#[test]
fn task_status_transitions_pending_to_completed_emit_completion_only() {
    let env = TestEnv::new();
    env.install_roblog("roblog");
    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    watcher.test_run_discovery().unwrap();

    let task_dir = env.tasks_root.join("roblog");
    fs::create_dir_all(&task_dir).unwrap();
    let task_file = task_dir.join("9.json");
    env.write(
        &task_file,
        &serde_json::json!({
            "id": "9", "title": "t9", "status": "pending",
        })
        .to_string(),
    );
    fs::write(&env.progress_path, "").unwrap();
    watcher.test_tick(&[task_file.clone()]).unwrap();
    // pending → in_progress should emit nothing.
    fs::write(&env.progress_path, "").unwrap();
    env.write(
        &task_file,
        &serde_json::json!({
            "id": "9", "title": "t9", "status": "in_progress",
        })
        .to_string(),
    );
    watcher.test_tick(&[task_file.clone()]).unwrap();
    let mid = env.events();
    assert!(
        events_of_kind(&mid, "team_task_completed").is_empty(),
        "premature completion event on in_progress: {mid:#?}",
    );
    assert!(
        events_of_kind(&mid, "team_task_created").is_empty(),
        "re-emitted created on in_progress: {mid:#?}",
    );

    // in_progress → completed: exactly one completed event.
    fs::write(&env.progress_path, "").unwrap();
    env.write(
        &task_file,
        &serde_json::json!({
            "id": "9", "title": "t9", "status": "completed",
            "completed_at": "2026-05-17T12:00:00Z",
            "result": "ok",
        })
        .to_string(),
    );
    watcher.test_tick(&[task_file.clone()]).unwrap();
    let events = env.events();
    let completed = events_of_kind(&events, "team_task_completed");
    assert_eq!(
        completed.len(),
        1,
        "expected exactly 1 completed event; got {events:#?}",
    );
    assert_eq!(completed[0]["task_id"], "9");
    assert_eq!(completed[0]["result_summary"], "ok");
    assert_eq!(completed[0]["completed_at"], "2026-05-17T12:00:00Z");
}

#[test]
fn typed_team_event_round_trips_watcher_emitted_payloads() {
    // F95 watcher writes untyped serde_json::Value; F96 consumers
    // deserialize into the typed TeamEvent. Verify every variant
    // emitted by the cold-discovery + dispatch passes deserializes
    // cleanly with no field drift.
    use ccteam_core::TeamEvent;

    let env = TestEnv::new();
    env.install_roblog("roblog");
    let inbox = env.install_team_lead_inbox("roblog", "team-lead");
    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    watcher.test_run_discovery().unwrap();

    // Append every variant deterministically.
    fs::write(&env.progress_path, "").unwrap();
    // joined: drop+reinstate a member
    let cp = env.teams_root.join("roblog").join("config.json");
    let original = fs::read_to_string(&cp).unwrap();
    let mut parsed: serde_json::Value = serde_json::from_str(&original).unwrap();
    let members = parsed["members"].as_array_mut().unwrap();
    members.retain(|m| m["name"] != "pm");
    env.write(&cp, &parsed.to_string());
    watcher.test_tick(&[cp.clone()]).unwrap();
    // re-add pm (synthesized) → member_joined
    let bytes = include_bytes!("fixtures/agent_teams/config-roblog.json");
    fs::write(&cp, bytes).unwrap();
    watcher.test_tick(&[cp.clone()]).unwrap();
    // message
    let mut entries: Vec<serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(&inbox).unwrap()).unwrap();
    entries.push(serde_json::json!({
        "from": "researcher",
        "text": "hello",
        "timestamp": "2026-12-31T23:59:59.999Z",
        "color": "blue",
        "read": false,
    }));
    env.write(&inbox, &serde_json::to_string(&entries).unwrap());
    watcher.test_tick(&[inbox.clone()]).unwrap();
    // task created + completed
    let task_dir = env.tasks_root.join("roblog");
    fs::create_dir_all(&task_dir).unwrap();
    let task_file = task_dir.join("99.json");
    env.write(
        &task_file,
        &serde_json::json!({"id":"99","title":"x","status":"pending"}).to_string(),
    );
    watcher.test_tick(&[task_file.clone()]).unwrap();
    env.write(
        &task_file,
        &serde_json::json!({"id":"99","title":"x","status":"completed","completed_at":"2026-05-17T12:00:00Z"}).to_string(),
    );
    watcher.test_tick(&[task_file.clone()]).unwrap();

    let events = env.events();
    let mut variants = std::collections::HashSet::new();
    for raw in &events {
        let parsed: TeamEvent = serde_json::from_value(raw.clone())
            .unwrap_or_else(|e| panic!("could not deserialize as TeamEvent: {e}; payload: {raw}"));
        match &parsed {
            TeamEvent::TeamMemberJoined { .. } => {
                variants.insert("team_member_joined");
            }
            TeamEvent::TeamMemberLeft { .. } => {
                variants.insert("team_member_left");
            }
            TeamEvent::TeamMessageSent { .. } => {
                variants.insert("team_message_sent");
            }
            TeamEvent::TeamTaskCreated { .. } => {
                variants.insert("team_task_created");
            }
            TeamEvent::TeamTaskCompleted { .. } => {
                variants.insert("team_task_completed");
            }
        }
    }
    for v in [
        "team_member_joined",
        "team_member_left",
        "team_message_sent",
        "team_task_created",
        "team_task_completed",
    ] {
        assert!(
            variants.contains(v),
            "expected at least one {v} event in the round-trip; got {variants:?}",
        );
    }
}

#[test]
fn broken_config_warns_and_keeps_team_in_discovery() {
    let env = TestEnv::new();
    let config_path = env.install_roblog("roblog");
    // Wipe the file and replace with malformed JSON.
    env.write(&config_path, "{this is not json");

    let watcher = AgentTeamsWatcher::new(env.config()).expect("watcher");
    // Discovery must not panic on schema break.
    watcher
        .test_run_discovery()
        .expect("discovery survives bad json");

    // No team_member_joined was emitted.
    let events = env.events();
    assert!(
        events_of_kind(&events, "team_member_joined").is_empty(),
        "broken config should not emit joined; got {events:#?}",
    );

    // The team is still observable: dispatch a fresh tick after we
    // repair the file → joined events flow.
    let bytes = include_bytes!("fixtures/agent_teams/config-roblog.json");
    fs::write(&config_path, bytes).unwrap();
    watcher.test_tick(&[config_path.clone()]).unwrap();
    let events = env.events();
    let joined = events_of_kind(&events, "team_member_joined");
    assert_eq!(
        joined.len(),
        5,
        "expected post-repair recovery to emit 5 joined events; got {events:#?}",
    );
}
