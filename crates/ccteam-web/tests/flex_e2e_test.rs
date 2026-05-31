//! V0.3.1 F51 - flex multi-session end-to-end canary.
//!
//! This complements the focused F48-F50 unit/integration suites by
//! sequencing the new flex surface in one server run:
//!
//!   GET /                         -> 301 /app/
//!   GET /api/v1/projects          -> dashboard JSON includes kind=flex
//!   GET /project/<slug>           -> 301 /app/p/<slug>
//!   GET /api/v1/projects/<slug>   -> flex session data renders in JSON
//!   GET /session/<slug>/<sid>     -> 301 /app/p/<slug>/s/<sid>
//!   GET /api/v1/projects/<slug>/sessions/<sid>
//!                                  -> per-session events + harness data
//!   /sse/project/<slug>/<sid>     -> progress event includes sid
//!   /sse/harness/<slug>/<sid>     -> harness snapshot event arrives
//!   POST /api/<slug>/<sid>/btw    -> session inbox file lands
//!   GET /screenshot/<slug>-<sid>.png -> PNG or graceful 504

use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use ccteam_core::{CcteamPaths, HarnessKind, ProjectState, SessionRecord, TeamKind};
use ccteam_harness::{HarnessSnapshot, SpawnCtx};
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
                job_id: None,
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

    let resp = nofollow
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 301);
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "/app/",
    );

    let rows: serde_json::Value = reqwest::get(format!("http://{addr}/api/v1/projects"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["slug"] == slug)
        .expect("dashboard JSON should list flex project");
    assert_eq!(row["kind"], "flex");

    let resp = nofollow
        .get(format!("http://{addr}/project/{slug}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 301);
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        format!("/app/p/{slug}"),
    );

    let project: serde_json::Value = reqwest::get(format!("http://{addr}/api/v1/projects/{slug}"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(project["slug"], slug);
    assert_eq!(project["kind"], "flex");
    let sessions = project["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|session| session["sid"] == "claude-1"));

    let resp = nofollow
        .get(format!("http://{addr}/session/{slug}/claude-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 301);
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        format!("/app/p/{slug}/s/claude-1"),
    );

    let session: serde_json::Value = reqwest::get(format!(
        "http://{addr}/api/v1/projects/{slug}/sessions/claude-1"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(session["harness_snapshot"]["model"], "Claude Sonnet 4.5",);
    assert!(
        session["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event"] == "PostToolUse"),
        "session JSON should include progress event: {session}",
    );

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
fn v0_6_0_codex_exec_adapter_pane_ingest_is_permissive() {
    // V0.6.0 F107 — `ingest_snapshot` was dropped from the trait
    // surface (Option C: 5-method trait alignment with Codex
    // ThreadManager). Codex pane-capture parsing now lives as a free
    // fn in `ccteam_core::execution::codex_exec::ingest_codex_pane`.
    // This regression guard preserves the V0.4.0 F62 contract: empty
    // pane bodies return a permissive fallback snapshot (model =
    // "codex", context_pct = 0), never `NotImplemented`.
    let snap = ccteam_core::execution::codex_exec::ingest_codex_pane("")
        .expect("ingest fallback must succeed post-F107");
    assert_eq!(snap.harness, "codex");
    assert_eq!(snap.model_display_name, "codex");
    assert_eq!(snap.context_used_pct, 0);

    // SpawnCtx (replaces V0.4.0 SpawnOpts on the trait surface) still
    // round-trips cleanly through the Wave 1 schema.
    let _ctx = SpawnCtx {
        slug: "flex-e2e".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: Vec::new(),
        model_id: None,
    };
}
