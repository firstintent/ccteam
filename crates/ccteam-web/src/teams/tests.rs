//! V0.5.0 F96 — unit tests for the teams module's pure parsers.
//!
//! These cover the tolerant-superset behaviour we promise the API
//! handlers: malformed JSON, missing fields, ad-hoc-vs-definition
//! detection, idle-notification flagging, etc. The integration tests
//! in `tests/api_v1_teams_test.rs` cover the wire surface end-to-end.

use std::fs;

use super::*;
use crate::teams::discovery::{compute_definition_backed, definition_md_target};
use crate::teams::inbox::{filter_since, recent_preview};
use crate::teams::subagent_resolver::{candidate_paths, parse_definition, ResolvedScope};

const FIXTURE_CONFIG: &str =
    include_str!("../../../ccteam-core/tests/fixtures/agent_teams/config-roblog.json");
const FIXTURE_INBOX: &str =
    include_str!("../../../ccteam-core/tests/fixtures/agent_teams/inbox-team-lead.json");

#[test]
fn compute_definition_backed_distinguishes_adhoc_from_definition() {
    assert!(!compute_definition_backed(Some("general-purpose")));
    assert!(!compute_definition_backed(Some("team-lead")));
    assert!(!compute_definition_backed(None));
    assert!(compute_definition_backed(Some("code-reviewer")));
    assert!(compute_definition_backed(Some("security-reviewer")));
}

#[test]
fn discover_teams_returns_empty_when_root_missing() {
    let tmp = tempfile::tempdir().unwrap();
    // No `teams/` subdir at all.
    let out = discovery::discover_teams(tmp.path()).unwrap();
    assert!(out.is_empty());
}

#[test]
fn discover_teams_skips_non_team_dirs_and_keeps_malformed_with_zero_members() {
    let tmp = tempfile::tempdir().unwrap();
    let teams = tmp.path().join("teams");
    fs::create_dir_all(teams.join("roblog")).unwrap();
    fs::write(teams.join("roblog").join("config.json"), FIXTURE_CONFIG).unwrap();
    // A dir without `config.json` is skipped.
    fs::create_dir_all(teams.join("orphan")).unwrap();
    // A dir with malformed `config.json` should still appear with
    // member_count = 0 (PRD §F95 acceptance #5).
    fs::create_dir_all(teams.join("broken")).unwrap();
    fs::write(teams.join("broken").join("config.json"), "{ not json").unwrap();

    let teams_out = discovery::discover_teams(tmp.path()).unwrap();
    let names: Vec<&str> = teams_out.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["broken", "roblog"]); // sorted, no "orphan"
    let roblog = teams_out.iter().find(|t| t.name == "roblog").unwrap();
    assert_eq!(roblog.member_count, 5);
    let broken = teams_out.iter().find(|t| t.name == "broken").unwrap();
    assert_eq!(broken.member_count, 0);
}

#[test]
fn load_team_config_marks_all_roblog_members_as_adhoc() {
    let tmp = tempfile::tempdir().unwrap();
    let teams = tmp.path().join("teams").join("roblog");
    fs::create_dir_all(&teams).unwrap();
    fs::write(teams.join("config.json"), FIXTURE_CONFIG).unwrap();
    let cfg = discovery::load_team_config(tmp.path(), "roblog").unwrap();
    assert_eq!(cfg.name, "roblog");
    assert_eq!(cfg.members.len(), 5);
    for m in &cfg.members {
        assert!(
            !m.definition_backed,
            "roblog member {} should be ad-hoc (agentType={:?})",
            m.name, m.agent_type
        );
    }
    // Lead has no color in the fixture; teammates do.
    let lead = cfg.members.iter().find(|m| m.name == "team-lead").unwrap();
    assert_eq!(lead.color, None);
    let researcher = cfg.members.iter().find(|m| m.name == "researcher").unwrap();
    assert_eq!(researcher.color.as_deref(), Some("blue"));
    // Subscriptions are present (empty array each) — not Null.
    for m in &cfg.members {
        assert!(m.subscriptions.is_empty());
    }
}

#[test]
fn definition_md_target_only_returns_path_for_definition_backed_members() {
    let mut m = discovery::MemberView {
        agent_id: "code-reviewer@roblog".into(),
        name: "code-reviewer".into(),
        agent_type: Some("code-reviewer".into()),
        model: None,
        color: None,
        joined_at: None,
        cwd: None,
        prompt: None,
        subscriptions: Vec::new(),
        tmux_pane_id: None,
        backend_type: None,
        plan_mode_required: None,
        definition_backed: true,
    };
    let path = definition_md_target(&m).unwrap();
    assert_eq!(
        path.display().to_string(),
        ".claude/agents/code-reviewer.md"
    );
    m.definition_backed = false;
    assert!(definition_md_target(&m).is_none());
}

#[test]
fn inbox_loader_flags_idle_notifications_and_assigns_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("teams").join("roblog").join("inboxes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("team-lead.json"), FIXTURE_INBOX).unwrap();
    let msgs = inbox::load_inbox(tmp.path(), "roblog", "team-lead").unwrap();
    assert!(msgs.len() > 5, "fixture has 39 messages");
    // First message in fixture is the frontend-dev idle notification.
    assert_eq!(msgs[0].from, "frontend-dev");
    assert!(msgs[0].is_idle_notification);
    assert_eq!(msgs[0].to, "team-lead");
    // Plain prose message must not be flagged.
    let prose = msgs
        .iter()
        .find(|m| !m.is_idle_notification)
        .expect("at least one non-idle message");
    assert!(prose.text.len() > 20);
}

#[test]
fn filter_since_strips_messages_at_or_before_cursor() {
    let m = |ts: &str| inbox::InboxMessage {
        from: "x".into(),
        to: "y".into(),
        text: "t".into(),
        timestamp: ts.into(),
        color: None,
        read: true,
        summary: None,
        is_idle_notification: false,
    };
    let msgs = vec![
        m("2026-05-16T13:00:00.000Z"),
        m("2026-05-16T13:30:00.000Z"),
        m("2026-05-16T14:00:00.000Z"),
    ];
    let kept = filter_since(msgs.clone(), Some("2026-05-16T13:30:00.000Z"));
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].timestamp, "2026-05-16T14:00:00.000Z");
    // None cursor → unchanged.
    let kept_all = filter_since(msgs, None);
    assert_eq!(kept_all.len(), 3);
}

#[test]
fn recent_preview_excludes_idle_notifications_and_returns_newest_first() {
    let plain = |ts: &str, from: &str| inbox::InboxMessage {
        from: from.into(),
        to: "lead".into(),
        text: "plain".into(),
        timestamp: ts.into(),
        color: None,
        read: true,
        summary: None,
        is_idle_notification: false,
    };
    let idle = |ts: &str, from: &str| inbox::InboxMessage {
        from: from.into(),
        to: "lead".into(),
        text: r#"{"type":"idle_notification","from":"x"}"#.into(),
        timestamp: ts.into(),
        color: None,
        read: true,
        summary: None,
        is_idle_notification: true,
    };
    let msgs = vec![
        plain("2026-05-16T10:00:00Z", "a"),
        idle("2026-05-16T11:00:00Z", "b"),
        plain("2026-05-16T12:00:00Z", "c"),
        plain("2026-05-16T13:00:00Z", "d"),
    ];
    let preview = recent_preview(&msgs, 2);
    assert_eq!(preview.len(), 2);
    // Newest first, idle excluded.
    assert_eq!(preview[0].timestamp, "2026-05-16T13:00:00Z");
    assert_eq!(preview[1].timestamp, "2026-05-16T12:00:00Z");
}

#[test]
fn tasks_loader_skips_lock_files_and_groups_status() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tasks").join("roblog");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("1.json"),
        r#"{
            "id": "1",
            "title": "Build scaffolding",
            "status": "completed",
            "owner": "frontend-dev",
            "dependencies": [],
            "createdAt": "2026-05-16T10:00:00Z",
            "completedAt": "2026-05-16T11:00:00Z"
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("2.json"),
        r#"{
            "subject": "Wire content layer",
            "status": "in_progress",
            "assignee": "frontend-dev",
            "blockedBy": ["1"]
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("3.json"),
        r#"{
            "title": "Audit a11y",
            "status": "pending"
        }"#,
    )
    .unwrap();
    // Bookkeeping files must be ignored.
    fs::write(dir.join(".highwatermark"), b"42").unwrap();
    fs::write(dir.join(".lock"), b"").unwrap();
    // Malformed JSON must be skipped without 500-ing.
    fs::write(dir.join("broken.json"), b"{ bad").unwrap();

    let tasks = tasks::load_tasks(tmp.path(), "roblog").unwrap();
    assert_eq!(tasks.len(), 3);
    let counts = tasks::TaskCounts::from(&tasks);
    assert_eq!(counts.completed, 1);
    assert_eq!(counts.in_progress, 1);
    assert_eq!(counts.pending, 1);
    // Title fallback to `subject`.
    let t2 = tasks.iter().find(|t| t.id == "2").unwrap();
    assert_eq!(t2.title, "Wire content layer");
    // `blockedBy` populates dependencies.
    assert_eq!(t2.dependencies, vec!["1".to_string()]);
    // Filename-stem-as-id when JSON omits `id`.
    let t3 = tasks.iter().find(|t| t.title == "Audit a11y").unwrap();
    assert_eq!(t3.id, "3");
}

#[test]
fn parse_definition_splits_yaml_frontmatter_and_body() {
    let src = "---\nname: code-reviewer\nmodel: sonnet\ntools: Read, Grep\nskills:\n  - security-review\nmcpServers:\n  - github\n---\nYou are a code reviewer.\n";
    let (fm, body) = parse_definition(src);
    assert_eq!(
        fm.get("name").and_then(|x| x.as_str()),
        Some("code-reviewer")
    );
    assert_eq!(fm.get("model").and_then(|x| x.as_str()), Some("sonnet"));
    assert_eq!(body, "You are a code reviewer.\n");
}

#[test]
fn parse_definition_handles_missing_frontmatter() {
    let (fm, body) = parse_definition("Just a body, no frontmatter.\n");
    assert!(fm.is_object());
    assert!(fm.as_object().unwrap().is_empty());
    assert_eq!(body, "Just a body, no frontmatter.\n");
}

#[test]
fn parse_definition_handles_crlf_line_endings() {
    let src = "---\r\nname: x\r\n---\r\nbody\r\n";
    let (fm, body) = parse_definition(src);
    assert_eq!(fm.get("name").and_then(|x| x.as_str()), Some("x"));
    assert_eq!(body, "body\n");
}

#[test]
fn candidate_paths_orders_project_user_plugin_managed() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("repo");
    let paths = candidate_paths(tmp.path(), Some(&cwd), "code-reviewer");
    let scopes: Vec<ResolvedScope> = paths.iter().map(|(s, _)| *s).collect();
    // Project first, then user, then managed (no plugin marketplaces
    // exist in tempdir).
    assert_eq!(
        scopes,
        vec![
            ResolvedScope::Project,
            ResolvedScope::User,
            ResolvedScope::Managed,
        ]
    );
    // The project path is built off member_cwd.
    assert!(paths[0]
        .1
        .to_string_lossy()
        .contains("repo/.claude/agents/code-reviewer.md"));
}

#[test]
fn resolve_definition_picks_project_over_user() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("repo");
    let project_md = cwd.join(".claude/agents").join("code-reviewer.md");
    fs::create_dir_all(project_md.parent().unwrap()).unwrap();
    fs::write(
        &project_md,
        "---\nname: project-version\nskills: [sec]\n---\nProject body.\n",
    )
    .unwrap();
    let user_md = tmp.path().join("agents").join("code-reviewer.md");
    fs::create_dir_all(user_md.parent().unwrap()).unwrap();
    fs::write(&user_md, "---\nname: user-version\n---\nUser body.\n").unwrap();

    let def =
        subagent_resolver::resolve_definition(tmp.path(), Some(&cwd), "code-reviewer").unwrap();
    assert_eq!(def.scope, ResolvedScope::Project);
    assert!(def.path.contains("repo/.claude/agents"));
    assert_eq!(def.body, "Project body.\n");
    assert_eq!(def.skills_not_applied, vec!["sec".to_string()]);
}

#[test]
fn resolve_definition_returns_none_when_no_file_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let def = subagent_resolver::resolve_definition(tmp.path(), None, "code-reviewer");
    assert!(def.is_none());
}

#[test]
fn truncate_keeps_string_under_limit_intact() {
    assert_eq!(truncate("short", 200), "short");
    assert_eq!(truncate(&"a".repeat(250), 200).chars().count(), 201); // 200 + ellipsis
}
