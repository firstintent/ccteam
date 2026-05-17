//! V0.5.0 F96 — integration tests for the Agent Teams API surface.
//!
//! Each test stages a tempdir representing `~/.claude/` + `~/.ccteam/`
//! and runs an in-process axum server, hitting the JSON endpoints with
//! `reqwest`. Covered:
//!
//! - `GET /api/v1/teams` happy path + empty path.
//! - `GET /api/v1/teams/{name}` shape (config, task_count, recent).
//! - `GET /api/v1/teams/{name}/tasks` 3-status grouping.
//! - `GET /api/v1/teams/{name}/inbox?teammate=&since=` filtering.
//! - `GET /api/v1/teams/{name}/inbox` (merged across inboxes).
//! - `GET /api/v1/teams/{name}/member/{teammate}/definition` ad-hoc
//!   gets 404; definition-backed with file present returns parsed
//!   markdown; definition-backed with file absent returns
//!   `definition_missing: true`.
//! - SSE channel forwards matching `team_name` lines and ignores
//!   others.
//!
//! Red-line — backend never writes under `<tmp>/.claude/teams/`.
//! Tests assert mtime invariance post-call to enforce.

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState};
use reqwest::Client;
use serde_json::Value;
use tempfile::TempDir;
use tokio::net::TcpListener;

const FIXTURE_CONFIG: &str =
    include_str!("../../ccteam-core/tests/fixtures/agent_teams/config-roblog.json");
const FIXTURE_INBOX: &str =
    include_str!("../../ccteam-core/tests/fixtures/agent_teams/inbox-team-lead.json");

/// Build a sandbox tempdir + an AppState pointed at it. Returns the
/// owned TempDir so it stays alive while the test runs.
fn sandbox() -> (TempDir, AppState) {
    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path().join(".claude");
    let ccteam_home = tmp.path().join(".ccteam");
    let projects_root = tmp.path().join("projects");
    fs::create_dir_all(claude_home.join("teams")).unwrap();
    fs::create_dir_all(&ccteam_home).unwrap();
    fs::create_dir_all(&projects_root).unwrap();
    let paths = CcteamPaths {
        root: ccteam_home.clone(),
        projects_root,
    };
    let teams_progress = ccteam_home.join("teams-progress.jsonl");
    let state = AppState::new(paths)
        .with_claude_home(claude_home)
        .with_teams_progress_path(teams_progress);
    (tmp, state)
}

fn stage_roblog(tmp: &TempDir) {
    let team_dir = tmp.path().join(".claude").join("teams").join("roblog");
    fs::create_dir_all(team_dir.join("inboxes")).unwrap();
    fs::write(team_dir.join("config.json"), FIXTURE_CONFIG).unwrap();
    fs::write(
        team_dir.join("inboxes").join("team-lead.json"),
        FIXTURE_INBOX,
    )
    .unwrap();
}

fn stage_definition_team(tmp: &TempDir) {
    // A second team with a `code-reviewer` member (agentType
    // ≠ general-purpose / team-lead) so we can exercise the
    // definition-backed branch.
    let team_dir = tmp.path().join(".claude").join("teams").join("audit-loop");
    fs::create_dir_all(team_dir.join("inboxes")).unwrap();
    fs::write(
        team_dir.join("config.json"),
        r#"{
            "name": "audit-loop",
            "description": "Code-review pipeline",
            "createdAt": 1779000000000,
            "leadAgentId": "team-lead@audit-loop",
            "members": [
                {
                    "agentId": "team-lead@audit-loop",
                    "name": "team-lead",
                    "agentType": "team-lead",
                    "model": "sonnet",
                    "joinedAt": 1779000000000,
                    "tmuxPaneId": "",
                    "cwd": "/home/rob/projects/audit",
                    "subscriptions": []
                },
                {
                    "agentId": "code-reviewer@audit-loop",
                    "name": "code-reviewer",
                    "color": "red",
                    "joinedAt": 1779000010000,
                    "tmuxPaneId": "in-process",
                    "subscriptions": [],
                    "agentType": "code-reviewer",
                    "model": "sonnet",
                    "cwd": "/home/rob/projects/audit",
                    "backendType": "in-process"
                }
            ]
        }"#,
    )
    .unwrap();
}

fn stage_tasks(tmp: &TempDir) {
    let dir = tmp.path().join(".claude").join("tasks").join("roblog");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("1.json"),
        r#"{
            "id": "1",
            "title": "Scaffolding",
            "status": "completed",
            "owner": "frontend-dev",
            "dependencies": [],
            "createdAt": "2026-05-16T10:00:00Z"
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("2.json"),
        r#"{
            "id": "2",
            "subject": "Content layer",
            "status": "in_progress",
            "assignee": "frontend-dev",
            "blockedBy": ["1"]
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("3.json"),
        r#"{
            "id": "3",
            "title": "Audit a11y",
            "status": "pending"
        }"#,
    )
    .unwrap();
    fs::write(dir.join(".highwatermark"), b"42").unwrap();
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .build()
        .expect("reqwest client")
}

fn mtimes_of_teams_root(claude_home: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut out = Vec::new();
    walk_collect(&claude_home.join("teams"), &mut out);
    out
}

fn walk_collect(dir: &Path, out: &mut Vec<(PathBuf, SystemTime)>) {
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        let m = fs::metadata(&p).unwrap().modified().unwrap();
        out.push((p.clone(), m));
        if p.is_dir() {
            walk_collect(&p, out);
        }
    }
}

#[tokio::test]
async fn list_returns_empty_when_no_teams_exist() {
    let (_tmp, state) = sandbox();
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn list_returns_roblog_card_with_member_count_and_description() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    let addr = spawn(state).await;
    let pre = mtimes_of_teams_root(&tmp.path().join(".claude"));
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "roblog");
    assert_eq!(arr[0]["member_count"], 5);
    assert!(arr[0]["description"]
        .as_str()
        .unwrap()
        .contains("个人博客项目"));
    // Read-only: mtimes unchanged.
    let post = mtimes_of_teams_root(&tmp.path().join(".claude"));
    assert_eq!(pre, post, "list endpoint must be read-only");
}

#[tokio::test]
async fn list_includes_malformed_team_with_zero_members() {
    let (tmp, state) = sandbox();
    let claude_home = tmp.path().join(".claude");
    let broken = claude_home.join("teams").join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("config.json"), "{ not valid").unwrap();
    stage_roblog(&tmp);
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams"))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    let names: Vec<&str> = arr.iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"broken"));
    let broken_entry = arr.iter().find(|v| v["name"] == "broken").unwrap();
    assert_eq!(broken_entry["member_count"], 0);
}

#[tokio::test]
async fn detail_returns_config_task_counts_and_recent_messages() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    stage_tasks(&tmp);
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams/roblog"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["config"]["name"], "roblog");
    let members = body["config"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 5);
    for m in members {
        // All roblog teammates are ad-hoc.
        assert_eq!(m["definition_backed"], false);
    }
    assert_eq!(body["task_count"]["completed"], 1);
    assert_eq!(body["task_count"]["in_progress"], 1);
    assert_eq!(body["task_count"]["pending"], 1);
    let recent = body["recent_messages"].as_array().unwrap();
    assert!(!recent.is_empty());
    // No idle notifications in the preview slice.
    for m in recent {
        assert_eq!(m["is_idle_notification"], false);
    }
}

#[tokio::test]
async fn detail_404s_for_missing_team() {
    let (_tmp, state) = sandbox();
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams/ghost"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn tasks_endpoint_returns_three_status_buckets() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    stage_tasks(&tmp);
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams/roblog/tasks"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    let task2 = arr.iter().find(|t| t["id"] == "2").unwrap();
    // `subject` fell through to `title`.
    assert_eq!(task2["title"], "Content layer");
    // `blockedBy` became `dependencies`.
    assert_eq!(task2["dependencies"][0], "1");
}

#[tokio::test]
async fn inbox_filters_by_teammate_and_since_cursor() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    let addr = spawn(state).await;

    // Whole box.
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/teams/roblog/inbox?teammate=team-lead"
        ))
        .send()
        .await
        .unwrap();
    let all: Vec<Value> = resp.json().await.unwrap();
    assert!(all.len() > 30, "fixture has ~39 entries");
    // First fixture entry timestamp; everything after the cursor is
    // strictly later.
    let cursor = all[0]["timestamp"].as_str().unwrap().to_string();
    let resp2 = client()
        .get(format!(
            "http://{addr}/api/v1/teams/roblog/inbox?teammate=team-lead&since={cursor}"
        ))
        .send()
        .await
        .unwrap();
    let after: Vec<Value> = resp2.json().await.unwrap();
    assert!(after.len() < all.len());
    for m in &after {
        assert!(m["timestamp"].as_str().unwrap() > cursor.as_str());
    }
    // To-field is filled by the loader.
    assert_eq!(all[0]["to"], "team-lead");
}

#[tokio::test]
async fn inbox_without_teammate_merges_every_inbox() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    // Add a second teammate's inbox.
    let inbox = tmp
        .path()
        .join(".claude")
        .join("teams")
        .join("roblog")
        .join("inboxes")
        .join("researcher.json");
    fs::write(
        &inbox,
        r#"[
            {"from":"team-lead","text":"Welcome","timestamp":"2026-05-16T13:50:00.000Z","color":"red","read":false}
        ]"#,
    )
    .unwrap();
    let addr = spawn(state).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/teams/roblog/inbox"))
        .send()
        .await
        .unwrap();
    let all: Vec<Value> = resp.json().await.unwrap();
    let owners: std::collections::HashSet<&str> =
        all.iter().map(|m| m["to"].as_str().unwrap()).collect();
    assert!(owners.contains("team-lead"));
    assert!(owners.contains("researcher"));
}

#[tokio::test]
async fn definition_returns_404_for_adhoc_teammate() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    let addr = spawn(state).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/teams/roblog/member/researcher/definition"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ad_hoc"], true);
}

#[tokio::test]
async fn definition_returns_parsed_markdown_for_definition_backed() {
    let (tmp, state) = sandbox();
    stage_definition_team(&tmp);
    // Stage the .md under user scope.
    let agents = tmp.path().join(".claude").join("agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("code-reviewer.md"),
        "---\nname: code-reviewer\nmodel: opus\ntools: Read, Grep\nskills:\n  - security-review\n---\nYou are a code reviewer.\n",
    )
    .unwrap();
    let addr = spawn(state).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/teams/audit-loop/member/code-reviewer/definition"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["agent_type"], "code-reviewer");
    assert_eq!(body["definition_missing"], false);
    let def = &body["definition"];
    assert!(def["body"].as_str().unwrap().contains("code reviewer"));
    assert_eq!(def["scope"], "user");
    assert_eq!(def["skills_not_applied"][0], "security-review");
}

#[tokio::test]
async fn definition_flags_missing_when_md_file_is_absent() {
    let (tmp, state) = sandbox();
    stage_definition_team(&tmp);
    let addr = spawn(state).await;
    let resp = client()
        .get(format!(
            "http://{addr}/api/v1/teams/audit-loop/member/code-reviewer/definition"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["definition_missing"], true);
    assert!(body["definition"].is_null());
}

#[tokio::test]
async fn sse_channel_forwards_matching_team_lines() {
    let (tmp, state) = sandbox();
    stage_roblog(&tmp);
    let progress_path = (*state.teams_progress_path).clone();
    fs::write(
        &progress_path,
        r#"{"event":"team_member_joined","team_name":"roblog","teammate_name":"researcher"}
{"event":"team_message_sent","team_name":"other","from":"a","to":"b","text_truncated":"x"}
"#,
    )
    .unwrap();
    let addr = spawn(state).await;
    let url = format!("http://{addr}/api/v1/teams/roblog/events");
    let resp = client().get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    // Pull a few bytes to confirm at least one progress frame surfaces.
    use tokio::io::AsyncReadExt;
    let mut stream = tokio_util::io::StreamReader::new(
        futures_util::stream::TryStreamExt::map_err(resp.bytes_stream(), std::io::Error::other),
    );
    let mut buf = [0u8; 1024];
    let read = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf))
        .await
        .expect("SSE handshake within timeout")
        .unwrap();
    assert!(read > 0);
    let chunk = std::str::from_utf8(&buf[..read]).unwrap();
    assert!(chunk.contains("team_member_joined"), "chunk = {chunk}");
    assert!(!chunk.contains("\"other\""));
}
