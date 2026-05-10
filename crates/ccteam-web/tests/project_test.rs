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
use ccteam_core::{CcteamPaths, ProjectState};
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

    // Screenshot panel placeholder is present (M5.2 will replace it).
    assert!(
        body.contains("Screenshot panel") || body.contains("screenshot"),
        "screenshot placeholder must be present",
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
