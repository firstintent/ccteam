//! V0.3.1 F51 - flex multi-session end-to-end canary.
//!
//! This complements the focused F48-F50 unit/integration suites by
//! sequencing the new flex surface in one server run:
//!
//!   GET /                         -> dashboard renders kind=flex
//!   GET /project/<slug>           -> flex session cards render
//!   GET /session/<slug>/<sid>     -> per-session events + harness panel
//!   /sse/project/<slug>/<sid>     -> progress event includes sid
//!   /sse/harness/<slug>/<sid>     -> harness snapshot event arrives
//!   POST /api/<slug>/<sid>/btw    -> session inbox file lands
//!   GET /screenshot/<slug>-<sid>.png -> PNG or graceful 504

use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use ccteam_core::{
    CcteamPaths, CodexAdapter, HarnessAdapter, HarnessKind, HarnessSnapshot, ProjectState,
    SessionRecord, SpawnOpts, TeamKind,
};
use ccteam_web::{router_with_state, AppState};
use reqwest::redirect::Policy;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
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

async fn open_sse(addr: SocketAddr, path: &str) -> tokio::io::Lines<impl AsyncBufReadExt + Unpin> {
    let url = format!("http://{addr}{path}");
    let resp = reqwest::get(&url).await.expect("sse get");
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("text/event-stream"));
    let stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mapped = stream.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(mapped);
    tokio::io::BufReader::new(reader).lines()
}

async fn read_one_event(
    lines: &mut tokio::io::Lines<impl AsyncBufReadExt + Unpin>,
    deadline: Duration,
) -> Option<String> {
    let mut data = String::new();
    let mut event_name: Option<String> = None;
    tokio::time::timeout(deadline, async {
        loop {
            let next = lines.next_line().await.ok().flatten()?;
            if next.is_empty() {
                if !data.is_empty() || event_name.is_some() {
                    return Some(data.clone());
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

fn fixture_snapshot(model: &str) -> HarnessSnapshot {
    HarnessSnapshot {
        harness: "claude-code".into(),
        model_display_name: model.into(),
        context_used_pct: 17,
        cost_usd_total: 1.23,
        rate_limit_pct: Some(4),
        cwd: None,
        raw: serde_json::json!({"source": "flex-e2e"}),
        captured_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn v0_3_1_flex_dashboard_session_sse_harness_and_actions() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = format!("flex-e2e-{}", std::process::id());

    let mut state = ProjectState::initial_for_team(slug.clone(), "flex".into());
    state.team_kind = TeamKind::Flex;
    for sid in ["claude-1", "claude-2"] {
        state.sessions.insert(
            sid.into(),
            SessionRecord {
                harness: HarnessKind::Claude,
                tmux_session: format!("ccteam-{slug}-{sid}"),
                started_at: chrono::Utc::now(),
                pid: None,
            },
        );
    }
    state.next_sid_seq.insert(HarnessKind::Claude, 3);
    state.save(&paths.project_state(&slug)).unwrap();

    let progress_file = paths.progress_jsonl_for_session(&slug, "claude-1");
    fs::create_dir_all(progress_file.parent().unwrap()).unwrap();
    fs::write(
        &progress_file,
        b"{\"ts\":\"2026-05-10T15:00:00Z\",\"event\":\"PostToolUse\",\"tool\":\"Read\"}\n",
    )
    .unwrap();

    fs::create_dir_all(paths.harness_dir()).unwrap();
    let harness_file = paths.harness_dir().join(format!("{slug}-claude-1.json"));
    fs::write(
        &harness_file,
        serde_json::to_string(&fixture_snapshot("Claude Sonnet 4.5")).unwrap(),
    )
    .unwrap();

    let app_state = AppState::new(paths.clone());
    let addr = spawn(app_state).await;
    let nofollow = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap();

    let body = reqwest::get(format!("http://{addr}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains(&slug), "dashboard should list flex project");
    assert!(
        body.contains("<code>flex</code>"),
        "dashboard should show kind"
    );

    let body = reqwest::get(format!("http://{addr}/project/{slug}"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("Sessions (2)"), "project page body:\n{body}");
    assert!(body.contains(&format!("/session/{slug}/claude-1")));
    assert!(body.contains(&format!("/screenshot/{slug}-claude-1.png")));

    let body = reqwest::get(format!("http://{addr}/session/{slug}/claude-1"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        body.contains("Claude Sonnet 4.5"),
        "session page body:\n{body}"
    );
    assert!(body.contains("PostToolUse"), "session page body:\n{body}");
    assert!(body.contains(&format!("/sse/project/{slug}/claude-1")));
    assert!(body.contains(&format!("/api/{slug}/claude-1/btw")));

    let mut progress_lines = open_sse(addr, &format!("/sse/project/{slug}/claude-1")).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut progress = fs::OpenOptions::new()
        .append(true)
        .open(&progress_file)
        .unwrap();
    progress
        .write_all(
            b"{\"ts\":\"2026-05-10T15:01:00Z\",\"event\":\"PostToolUse\",\"tool\":\"Edit\"}\n",
        )
        .unwrap();
    progress.flush().unwrap();
    drop(progress);
    let payload = read_one_event(&mut progress_lines, Duration::from_secs(3))
        .await
        .expect("progress SSE should emit appended flex session event");
    let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed["slug"].as_str(), Some(slug.as_str()));
    assert_eq!(parsed["sid"], "claude-1");
    assert_eq!(parsed["tool"], "Edit");

    let mut harness_lines = open_sse(addr, &format!("/sse/harness/{slug}/claude-1")).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    fs::write(
        &harness_file,
        serde_json::to_string(&fixture_snapshot("Claude Opus 4.7")).unwrap(),
    )
    .unwrap();
    let payload = read_one_event(&mut harness_lines, Duration::from_secs(3))
        .await
        .expect("harness SSE should emit modified snapshot");
    let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed["slug"].as_str(), Some(slug.as_str()));
    assert_eq!(parsed["sid"], "claude-1");
    assert_eq!(parsed["snapshot"]["model_display_name"], "Claude Opus 4.7");

    let resp = nofollow
        .post(format!("http://{addr}/api/{slug}/claude-1/btw"))
        .form(&[("text", "v0.3.1 flex e2e nudge")])
        .send()
        .await
        .expect("POST sid btw");
    assert_eq!(resp.status(), 303);
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        format!("/session/{slug}/claude-1"),
    );
    let inbox_dir = paths.project_session_dir(&slug, "claude-1").join("inbox");
    let mut entries = fs::read_dir(&inbox_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries.len(), 1);
    let inbox_body = fs::read_to_string(&entries[0]).unwrap();
    assert!(inbox_body.contains("v0.3.1 flex e2e nudge"));

    let resp = reqwest::get(format!("http://{addr}/screenshot/{slug}-claude-1.png"))
        .await
        .expect("GET sid screenshot");
    match resp.status().as_u16() {
        200 => {
            let bytes = resp.bytes().await.unwrap();
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        504 => {
            let body = resp.text().await.unwrap();
            assert!(body.contains("screenshot unavailable"), "body={body}");
        }
        status => panic!("unexpected screenshot status {status}"),
    }
}

#[test]
fn v0_3_1_codex_adapter_remains_trait_stub() {
    let err = CodexAdapter::new()
        .spawn_session(SpawnOpts {
            harness: "codex",
            slug: "flex-e2e".into(),
            sid: "codex-1".into(),
            cwd: std::env::temp_dir(),
            extra_args: Vec::new(),
        })
        .expect_err("CodexAdapter must stay stubbed in V0.3.1");
    let msg = err.to_string();
    assert!(msg.contains("trait-stub in V0.3.1"), "msg={msg}");
    assert!(msg.contains("V0.3.2"), "msg={msg}");
    assert!(
        msg.contains("docs/research/ccteam-codex-integration.md"),
        "msg={msg}",
    );
}
