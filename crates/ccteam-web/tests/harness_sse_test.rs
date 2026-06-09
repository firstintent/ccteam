//! V0.3.1 F46 — harness SSE endpoint integration tests.
//!
//! Mirrors the M5.2 `tests/sse_test.rs` shape with timing constants
//! transplanted (50ms subscribe delay, 2s read deadline) so the
//! flake characteristics stay equivalent across both pumps.
//!
//! Coverage (per dev-plan §2.4):
//!
//! 1. Synthetic `publish_harness_synthetic` → `/sse/harness/<slug>`
//!    sees the `harness_snapshot` event with the JSON envelope.
//! 2. End-to-end: write a real `<slug>-<sid>.json` file and assert
//!    the watcher → broadcast → SSE stream pump delivers within 2s.

use std::net::SocketAddr;
use std::time::Duration;

use ccteam_core::CcteamPaths;
use ccteam_harness::HarnessSnapshot;
use ccteam_web::{router_with_state, AppState, HarnessSnapshotEvent};
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_snapshot() -> HarnessSnapshot {
    HarnessSnapshot {
        harness: "claude-code".into(),
        model_display_name: "Claude Sonnet 4.5".into(),
        context_used_pct: 12,
        cost_usd_total: 0.42,
        rate_limit_pct: Some(7),
        cwd: None,
        raw: serde_json::json!({"tag":"fixture"}),
        captured_at: chrono::Utc::now(),
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

async fn open_sse(addr: SocketAddr, path: &str) -> tokio::io::Lines<impl AsyncBufReadExt + Unpin> {
    let url = format!("http://{addr}{path}");
    let resp = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(&url)
        .send()
        .await
        .expect("sse get");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "text/event-stream",
    );

    let stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mapped = stream.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(mapped);
    let buf = tokio::io::BufReader::new(reader);
    buf.lines()
}

async fn read_one_event(
    lines: &mut tokio::io::Lines<impl AsyncBufReadExt + Unpin>,
    deadline: Duration,
) -> Option<(Option<String>, String)> {
    let mut data = String::new();
    let mut event_name: Option<String> = None;
    tokio::time::timeout(deadline, async {
        loop {
            let next = lines.next_line().await.ok().flatten()?;
            if next.is_empty() {
                if !data.is_empty() || event_name.is_some() {
                    return Some((event_name.clone(), data.clone()));
                }
                continue;
            }
            if let Some(rest) = next.strip_prefix("data:") {
                let v = rest.trim_start();
                if data.is_empty() {
                    data.push_str(v);
                } else {
                    data.push('\n');
                    data.push_str(v);
                }
            } else if let Some(rest) = next.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
async fn sse_harness_emits_synthetic_snapshot_event() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.harness_dir()).unwrap();
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let bus = state.bus.clone();
    let addr = spawn_server(state).await;

    let mut lines = open_sse(addr, "/sse/harness/dev-foo").await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    bus.publish_harness_synthetic(HarnessSnapshotEvent {
        slug: "dev-foo".into(),
        sid: "claude-1".into(),
        snapshot: fixture_snapshot(),
    });

    let (ev_name, payload) = read_one_event(&mut lines, Duration::from_secs(2))
        .await
        .expect("SSE stream produced an event");
    assert_eq!(ev_name.as_deref(), Some("harness_snapshot"));
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("data is valid JSON");
    assert_eq!(parsed["slug"], "dev-foo");
    assert_eq!(parsed["sid"], "claude-1");
    assert_eq!(
        parsed["snapshot"]["model_display_name"],
        "Claude Sonnet 4.5"
    );
    assert_eq!(parsed["snapshot"]["context_used_pct"], 12);
}

#[tokio::test]
async fn sse_harness_session_filters_to_matching_sid() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.harness_dir()).unwrap();
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let state = AppState::new(paths);
    let bus = state.bus.clone();
    let addr = spawn_server(state).await;

    let mut lines = open_sse(addr, "/sse/harness/dev-foo/claude-1").await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Wrong sid — must be filtered out.
    bus.publish_harness_synthetic(HarnessSnapshotEvent {
        slug: "dev-foo".into(),
        sid: "claude-2".into(),
        snapshot: fixture_snapshot(),
    });
    // Correct sid.
    bus.publish_harness_synthetic(HarnessSnapshotEvent {
        slug: "dev-foo".into(),
        sid: "claude-1".into(),
        snapshot: fixture_snapshot(),
    });

    let (_, payload) = read_one_event(&mut lines, Duration::from_secs(2))
        .await
        .expect("expected a snapshot event");
    let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed["sid"], "claude-1");
    let bonus = read_one_event(&mut lines, Duration::from_millis(150)).await;
    assert!(
        bonus.is_none(),
        "unexpected extra event leaked: {:?}",
        bonus,
    );
}

#[tokio::test]
async fn sse_harness_end_to_end_file_write_reaches_stream() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.harness_dir()).unwrap();
    std::fs::create_dir_all(paths.progress_dir()).unwrap();

    let state = AppState::new(paths.clone());
    let addr = spawn_server(state).await;
    let mut lines = open_sse(addr, "/sse/harness/dev-watch").await;

    // Give watchers time to register inotify + the SSE handler time
    // to subscribe (M5.2 sse_test.rs uses the same 150ms).
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Write a snapshot file directly (F61 retired the V0.3.1
    // `write_harness_snapshot` helper; F68 will reintroduce the writer
    // path against the new `claude --bg` state.json source). The
    // watcher's contract here is unchanged: "see file <slug>-<sid>.json
    // + parse + broadcast".
    let target = paths.harness_dir().join("dev-watch-claude-1.json");
    let snap = fixture_snapshot();
    let body = serde_json::to_string(&snap).unwrap();
    // Atomic-style write: tmp + rename so the watcher only sees the
    // final file (matches the production writer's behavior).
    let tmpf = paths.harness_dir().join("dev-watch-claude-1.json.tmp");
    std::fs::write(&tmpf, &body).unwrap();
    std::fs::rename(&tmpf, &target).unwrap();

    let (ev_name, payload) = read_one_event(&mut lines, Duration::from_secs(3))
        .await
        .expect("SSE stream picked up file write");
    assert_eq!(ev_name.as_deref(), Some("harness_snapshot"));
    let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed["slug"], "dev-watch");
    assert_eq!(parsed["sid"], "claude-1");
    assert_eq!(
        parsed["snapshot"]["model_display_name"],
        "Claude Sonnet 4.5"
    );
}
