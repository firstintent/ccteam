//! V0.3 M5.1 — `GET /project/<slug>` integration tests.
//!
//! Fixtures `state.json` + `progress.jsonl` + an outbox file under a
//! tempdir-backed `CcteamPaths`, asserts the project page renders
//! state / events / outbox sections, and a missing slug returns 404.

use std::fs;
use std::net::SocketAddr;

use ccteam_core::inbox::{
    outbox_filename, OutboxEventKind, OutboxFrontMatter, OutboxMessage, OutboxPriority,
};
use ccteam_core::{CcteamPaths, HarnessKind, ProjectState, SessionRecord, TeamKind};
use ccteam_web::{router_with_state, AppState};
use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_flex_project(paths: &CcteamPaths, slug: &str) {
    let mut state = ProjectState::initial(slug.to_string());
    state.team_kind = TeamKind::Flex;
    state.current_phase = "must-not-render".into();
    state.sessions.insert(
        "claude-1".into(),
        SessionRecord {
            harness: HarnessKind::Claude,
            tmux_session: format!("ccteam-{slug}-claude-1"),
            started_at: Utc::now(),
            pid: None,
        },
    );
    state.sessions.insert(
        "codex-1".into(),
        SessionRecord {
            harness: HarnessKind::Codex,
            tmux_session: format!("ccteam-{slug}-codex-1"),
            started_at: Utc::now(),
            pid: None,
        },
    );
    state.save(&paths.project_state(slug)).unwrap();

    let claude_progress = paths.progress_jsonl_for_session(slug, "claude-1");
    fs::create_dir_all(claude_progress.parent().unwrap()).unwrap();
    fs::write(
        &claude_progress,
        format!(
            "{}\n",
            json!({"ts": "2026-05-10T10:01:00Z", "event": "PostToolUse", "tool": "Read"})
        ),
    )
    .unwrap();
}

async fn spawn_server(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn fixture_one_project(paths: &CcteamPaths, slug: &str) {
    // state.json under <projects_root>/<slug>/.ccteam/state.json
    let mut state = ProjectState::initial(slug.to_string());
    state.current_phase = "implement".into();
    state.cost_used_usd = 1.23;
    state.save(&paths.project_state(slug)).unwrap();

    // progress.jsonl under <root>/progress/<slug>.jsonl
    let progress = paths.progress_jsonl(slug);
    fs::create_dir_all(progress.parent().unwrap()).unwrap();
    let body = format!(
        "{}\n{}\n",
        json!({"ts": "2026-05-10T10:00:00Z", "event": "phase_inject", "phase": "implement"}),
        json!({"ts": "2026-05-10T10:01:00Z", "event": "PostToolUse", "tool": "Read"}),
    );
    fs::write(&progress, body).unwrap();

    // Outbox file under <projects_root>/<slug>/.ccteam/outbox/.
    let outbox_dir = paths.project_ccteam_dir(slug).join("outbox");
    fs::create_dir_all(&outbox_dir).unwrap();
    let now = Utc::now();
    let msg = OutboxMessage {
        front: OutboxFrontMatter {
            schema_version: 1,
            in_reply_to: None,
            in_reply_to_source_msg_id: None,
            target_channels: vec!["telegram".into()],
            created_at: now,
            priority: OutboxPriority::Normal,
            event_kind: OutboxEventKind::Progress,
        },
        body: "implementation phase wrapping up; ready for review.\n".into(),
    };
    let path = outbox_dir.join(outbox_filename(now, 1));
    msg.save(&path).unwrap();
}

#[tokio::test]
async fn project_detail_renders_state_events_and_outbox() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-foo";
    fixture_one_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/project/{slug}");
    let resp = reqwest::get(&url).await.expect("GET /project/<slug>");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // Header + state echoes.
    assert!(body.contains(slug), "body must mention slug");
    assert!(
        body.contains("current_phase"),
        "body must include state.json key"
    );
    assert!(
        body.contains("implement"),
        "body must show current phase value"
    );

    // Recent events table.
    assert!(
        body.contains("phase_inject"),
        "events table must include phase_inject"
    );
    assert!(
        body.contains("PostToolUse"),
        "events table must include PostToolUse"
    );

    // Outbox section.
    assert!(
        body.contains("implementation phase wrapping up"),
        "outbox preview must render. body=\n{body}",
    );
    assert!(body.contains("progress"), "outbox kind must render");

    // xterm-backed pane snapshot panel is present, with PNG fallback
    // still linked for degraded browsers.
    assert!(
        body.contains("/assets/xterm.js")
            && body.contains("pane-snapshot.ansi")
            && body.contains("PNG fallback"),
        "xterm pane snapshot wiring must be present",
    );
}

#[tokio::test]
async fn project_detail_returns_404_for_unknown_slug() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/project/does-not-exist");
    let resp = reqwest::get(&url).await.expect("GET /project/<missing>");
    assert_eq!(resp.status(), 404);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("not found"),
        "404 body should mention 'not found'. got: {body}",
    );
}

#[tokio::test]
async fn project_detail_handles_no_progress_or_outbox() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-bar";
    let state = ProjectState::initial(slug.to_string());
    state.save(&paths.project_state(slug)).unwrap();

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/project/{slug}");
    let resp = reqwest::get(&url).await.expect("GET /project/<slug>");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains(slug));
    assert!(body.contains("No events") || body.contains("Recent events (0)"));
    assert!(body.contains("No outbox") || body.contains("Outbox (0)"));
}

#[tokio::test]
async fn project_detail_limits_recent_events_to_latest_ten() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-many-events";
    let state = ProjectState::initial(slug.to_string());
    state.save(&paths.project_state(slug)).unwrap();

    let progress = paths.progress_jsonl(slug);
    fs::create_dir_all(progress.parent().unwrap()).unwrap();
    let mut body = String::new();
    for i in 0..12 {
        body.push_str(
            &json!({
                "ts": format!("2026-05-10T10:{i:02}:00Z"),
                "event": format!("event_{i:02}"),
                "phase": "implement",
            })
            .to_string(),
        );
        body.push('\n');
    }
    fs::write(&progress, body).unwrap();

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/project/{slug}");
    let resp = reqwest::get(&url).await.expect("GET /project/<slug>");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    assert!(body.contains("Recent events (10)"), "body=\n{body}");
    assert!(
        !body.contains("event_00") && !body.contains("event_01"),
        "oldest events should not be rendered. body=\n{body}",
    );
    assert!(
        body.contains("event_02") && body.contains("event_11"),
        "latest ten events should be rendered. body=\n{body}",
    );
    assert!(
        body.contains("var MAX_ROWS = 10"),
        "SSE row cap should match the server-rendered limit"
    );
}

#[tokio::test]
async fn flex_project_detail_renders_session_cards() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/project/{slug}");
    let resp = reqwest::get(&url).await.expect("GET /project/<slug>");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    assert!(body.contains("Sessions (2)"), "body=\n{body}");
    assert!(body.contains("/session/dev-flex/claude-1"), "body=\n{body}");
    assert!(body.contains("/session/dev-flex/codex-1"), "body=\n{body}");
    assert!(body.contains("harness-claude"), "body=\n{body}");
    assert!(body.contains("harness-codex"), "body=\n{body}");
    assert!(
        body.contains("/screenshot/dev-flex-claude-1.png")
            && body.contains("tmux attach -t ccteam-dev-flex-claude-1"),
        "session card screenshot and attach command missing. body=\n{body}",
    );
    assert!(
        !body.contains("must-not-render"),
        "flex project detail must not render workflow current_phase. body=\n{body}",
    );
}

#[tokio::test]
async fn session_detail_renders_registered_flex_session() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/session/{slug}/claude-1");
    let resp = reqwest::get(&url).await.expect("GET /session/<slug>/<sid>");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    assert!(body.contains("dev-flex / claude-1"), "body=\n{body}");
    assert!(body.contains("harness-claude"), "body=\n{body}");
    assert!(
        body.contains("/sse/project/dev-flex/claude-1")
            && body.contains("/sse/harness/dev-flex/claude-1"),
        "session detail should subscribe to per-session streams. body=\n{body}",
    );
    assert!(
        body.contains("/api/dev-flex/claude-1/pane-snapshot.ansi")
            && body.contains("/screenshot/dev-flex-claude-1.png"),
        "session detail should expose sid-scoped snapshots. body=\n{body}",
    );
    assert!(
        body.contains("/api/dev-flex/claude-1/btw")
            && body.contains("/api/dev-flex/claude-1/pause")
            && body.contains("/api/dev-flex/claude-1/resume"),
        "session detail should post through sid-scoped actions. body=\n{body}",
    );
}

#[tokio::test]
async fn session_detail_returns_404_for_unknown_sid() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let url = format!("http://{addr}/session/{slug}/claude-99");
    let resp = reqwest::get(&url).await.expect("GET /session/<slug>/<sid>");
    assert_eq!(resp.status(), 404);
    let body = resp.text().await.unwrap();
    assert!(body.contains("session not found"), "body=\n{body}");
}

#[tokio::test]
async fn session_btw_posts_to_session_inbox() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let paths_for_assert = paths.clone();
    let slug = "dev-flex";
    fixture_flex_project(&paths, slug);

    let addr = spawn_server(AppState::new(paths)).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://{addr}/api/{slug}/claude-1/btw");
    let resp = client
        .post(url)
        .form(&[("text", "hello session")])
        .send()
        .await
        .expect("POST session btw");
    assert_eq!(resp.status(), 303);

    let inbox_dir = paths_for_assert
        .project_session_dir(slug, "claude-1")
        .join("inbox");
    let mut entries = fs::read_dir(&inbox_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries.len(), 1);
    let body = fs::read_to_string(&entries[0]).unwrap();
    assert!(body.contains("source: ccteam-web"), "body=\n{body}");
    assert!(body.contains("hello session"), "body=\n{body}");
}
