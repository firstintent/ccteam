//! v0.8.6 W5b ResSessions — session resource API integration tests.
//!
//! These exercise the **no-gateway** (standalone internal-web) path:
//! `AppState::new` leaves `gateway = None`, so every session endpoint must
//! return 503 (the locked W5b contract) — except the SSE endpoint, which
//! keeps the stream open and emits a one-shot `gateway_unavailable` frame
//! so a browser `EventSource` doesn't retry-loop on a 503.
//!
//! The gateway-attached happy path (create/list/turn/stop driving a real
//! `Gateway`) needs a live daemon + harness fakes and is covered by the
//! gateway spine's own unit tests in `ccteam-im`; here we lock the network
//! contract + that the router builds without a route-matcher conflict
//! (`/api/v1/sessions/active` from api_v1 vs `/api/v1/sessions/{sid}` here).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};
use ccteam_web::{router_with_state, AppState};
use futures::stream::{self, BoxStream};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

struct FakeAdapter {
    vendor: AgentVendor,
}

#[async_trait::async_trait]
impl HarnessAdapter for FakeAdapter {
    fn name(&self) -> &'static str {
        "web-test"
    }

    fn vendor(&self) -> AgentVendor {
        self.vendor
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: self.vendor,
            mode: ExecutionMode::Chat,
            identity: format!("{}-{}", ctx.slug, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        })
    }

    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("turn-web-test"))
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::empty())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "web-test".into(),
        })
    }

    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn handle_directive(
        &self,
        _h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Done { receipt: d.name })
    }

    async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

fn seed_role_with_model(project_dir: &std::path::Path, role: &str, model: Option<&str>) {
    let agents = project_dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    let model = model.map(|m| format!("model: {m}\n")).unwrap_or_default();
    std::fs::write(
        agents.join(format!("{role}.md")),
        format!("---\nname: {role}\n{model}---\n{role} body\n"),
    )
    .unwrap();
}

async fn spawn_server(state: AppState) -> SocketAddr {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // router_with_state builds the FULL stateful_router; if the new
    // session routes conflicted with api_v1's `/api/v1/sessions/active`
    // in the matchit router, this would panic here.
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

#[tokio::test]
async fn list_sessions_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_some());
}

#[tokio::test]
async fn create_session_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "reviewer"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn create_session_rejects_removed_host_parameter() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "", "host": "sat-a"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"],
        ccteam_im::remote_host::HOST_SPAWN_PARAM_REMOVED
    );
}

#[tokio::test]
async fn create_session_returns_model_warning_in_body() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    seed_role_with_model(&project_dir, "reviewer", Some("deepseek-via-claude"));
    seed_role_with_model(&project_dir, "sonnet", Some("sonnet[1m]"));

    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway = ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir);
    let addr =
        spawn_server(AppState::new(paths).with_gateway(Arc::new(tokio::sync::Mutex::new(gateway))))
            .await;
    let client = reqwest::Client::new();

    let warned = client
        .post(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "reviewer", "vendor": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(warned.status(), 201);
    let body: Value = warned.json().await.unwrap();
    assert_eq!(body["sid"], "s1");
    assert!(
        body["model_warning"]
            .as_str()
            .is_some_and(|msg| msg.contains("deepseek-via-claude")),
        "expected model_warning in 201 body, got {body}"
    );

    let ok = client
        .post(format!("http://{addr}/api/v1/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "sonnet", "vendor": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 201);
    let body: Value = ok.json().await.unwrap();
    assert_eq!(body["sid"], "s2");
    assert!(
        body.get("model_warning").is_none(),
        "Claude-family model must not warn: {body}"
    );
}

#[tokio::test]
async fn session_history_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/sessions/s1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn session_turn_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/sessions/s1/turn"))
        .json(&serde_json::json!({"text": "hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn session_stop_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/sessions/s1/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

/// The interrupt route (non-destructive twin of stop) is wired + gated the same
/// way: with no live gateway it 503s (standalone web), proving the endpoint
/// exists and reaches the spine's `interrupt_session` path.
#[tokio::test]
async fn session_interrupt_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/sessions/s1/interrupt"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

/// v0.8.22 P1 — `PATCH /sessions/{sid}` (rename) follows the same no-gateway
/// contract as every other by-sid session route.
#[tokio::test]
async fn session_patch_no_gateway_is_503() {
    let tmp = TempDir::new().unwrap();
    let addr = spawn_server(AppState::new(fake_paths(tmp.path()))).await;
    let client = reqwest::Client::new();
    let resp = client
        .patch(format!("http://{addr}/api/v1/sessions/s1"))
        .json(&serde_json::json!({"title": "new title"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

/// The SSE endpoint must NOT 503 — it keeps the stream open and emits a
/// one-shot `gateway_unavailable` frame so a browser EventSource shows the
/// state without hammering reconnects. It is still a 200 text/event-stream.
#[tokio::test]
async fn session_events_no_gateway_streams_unavailable_notice() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.progress_dir()).unwrap();
    let addr = spawn_server(AppState::new(paths)).await;

    let url = format!("http://{addr}/api/v1/sessions/s1/events");
    let resp = reqwest::get(&url).await.expect("sse get");
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
    let mut lines = tokio::io::BufReader::new(reader).lines();

    // Read until we see the `gateway_unavailable` event name (skip the
    // 15s keep-alive comment lines, which never arrive this fast anyway).
    let saw_notice = tokio::time::timeout(Duration::from_secs(5), async {
        let mut event_name: Option<String> = None;
        loop {
            let next = lines.next_line().await.ok().flatten()?;
            if let Some(rest) = next.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            }
            if next.is_empty() {
                if let Some(name) = event_name.take() {
                    return Some(name);
                }
            }
        }
    })
    .await
    .ok()
    .flatten();

    assert_eq!(saw_notice.as_deref(), Some("gateway_unavailable"));
}

// ── v0.8.22 P1 (review §3.1-3) — SSE Last-Event-ID replay + approval reseed ──

/// Seed a pending HITL approval directly against a gateway's shared pending
/// registry — the SAME `register` + `tag_sid` steps
/// `ccteam_im::hitl::ask_permission` takes, without needing a live stream-json
/// turn actually blocked on one.
async fn seed_pending_approval(
    gateway: &Arc<tokio::sync::Mutex<ccteam_im::gateway::Gateway>>,
    sid: &str,
    token: &str,
) {
    let pending = gateway.lock().await.pending_handle();
    let mut guard = pending.lock().await;
    let (tx, _rx) = tokio::sync::oneshot::channel();
    guard.register(
        token.to_string(),
        ccteam_harness::ChoicePrompt {
            token: token.to_string(),
            title: format!("🔴 session {sid} wants to run: Bash rm -rf /tmp/x"),
            options: vec![
                ccteam_harness::ChoiceOption {
                    id: "allow".into(),
                    label: "✅ Approve".into(),
                },
                ccteam_harness::ChoiceOption {
                    id: "deny".into(),
                    label: "⛔ Deny".into(),
                },
            ],
            multi: false,
        },
        ccteam_im::pending::InteractionOrigin::External { reply: tx },
        std::time::Instant::now() + Duration::from_secs(60),
    );
    guard.tag_sid(token, sid.to_string());
}

/// Read `data:` lines off an SSE response body until `pred` matches one, or
/// the timeout lapses (`None`). Mirrors the existing `event:`-line scanner
/// above, but for the JSON `data:` payload.
async fn read_sse_data_until(
    resp: reqwest::Response,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    use futures_util::StreamExt;
    let stream = resp.bytes_stream();
    let mapped = stream.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(mapped);
    let mut lines = tokio::io::BufReader::new(reader).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let next = lines.next_line().await.ok().flatten()?;
            if let Some(rest) = next.strip_prefix("data:") {
                let data = rest.trim().to_string();
                if pred(&data) {
                    return Some(data);
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Like [`read_sse_data_until`], but returns the matching frame's SSE `id:`
/// (parsed as the ring seq) instead of its `data:` payload — frames are
/// blank-line delimited, so this tracks both fields per-frame regardless of
/// which order axum renders them in.
async fn read_sse_seq_until(resp: reqwest::Response, pred: impl Fn(&str) -> bool) -> Option<u64> {
    use futures_util::StreamExt;
    let stream = resp.bytes_stream();
    let mapped = stream.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(mapped);
    let mut lines = tokio::io::BufReader::new(reader).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut frame_id: Option<u64> = None;
        let mut frame_data: Option<String> = None;
        loop {
            let next = lines.next_line().await.ok().flatten()?;
            if let Some(rest) = next.strip_prefix("id:") {
                frame_id = rest.trim().parse().ok();
            } else if let Some(rest) = next.strip_prefix("data:") {
                frame_data = Some(rest.trim().to_string());
            } else if next.is_empty() {
                if let Some(data) = frame_data.take() {
                    if pred(&data) {
                        return frame_id;
                    }
                }
                frame_id = None;
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// review §3.1-3's explicit ask: "a fresh page load must also see a pending
/// approval, not just reconnects". A BRAND-NEW SSE connection (no
/// `Last-Event-ID` at all) while an approval is outstanding for that sid must
/// still render the approve/deny prompt.
#[tokio::test]
async fn session_events_fresh_connect_reseeds_a_pending_approval() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway = ccteam_im::gateway::Gateway::new_with_factory(
        factory,
        "demo",
        paths.projects_root.join("demo"),
    );
    let gateway = Arc::new(tokio::sync::Mutex::new(gateway));
    seed_pending_approval(&gateway, "s1", "ptok").await;

    let addr = spawn_server(AppState::new(paths).with_gateway(gateway)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/sessions/s1/events"))
        .await
        .expect("sse get");
    assert_eq!(resp.status(), 200);

    let payload = read_sse_data_until(resp, |d| d.contains("\"token\""))
        .await
        .expect("expected a reseeded approval frame on a fresh connect");
    let json: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(json["token"], "ptok");
    assert!(json["content"].as_str().unwrap().contains("rm -rf"));
}

/// End-to-end proof that the `?last_event_id=` query wiring (axum's `Query`
/// extractor → [`parse_last_event_id`](ccteam_web) → the catchup batch) works
/// over a real HTTP round-trip, and that it composes correctly with the
/// pending-approval reseed's token dedup: connection #1 observes approval
/// "first" and records its SSE seq; "first" then resolves and approval
/// "second" fires for the same sid while connection #1 is gone; connection
/// #2 reconnects naming that seq as `last_event_id` and must see ONLY
/// "second" — a stale/already-delivered approval is not re-sent just
/// because a later one shares its sid. (The ring's plain-event backlog
/// replay itself — no approval involved — is covered directly by
/// `build_catchup_entries_replays_the_ring_gap` in the lib's own unit tests,
/// which can seed the ring without a live turn.)
#[tokio::test]
async fn session_events_reconnect_with_last_event_id_replays_the_gap() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway = ccteam_im::gateway::Gateway::new_with_factory(
        factory,
        "demo",
        paths.projects_root.join("demo"),
    );
    let gateway = Arc::new(tokio::sync::Mutex::new(gateway));
    let addr = spawn_server(AppState::new(paths).with_gateway(Arc::clone(&gateway))).await;

    // Connection #1 observes the first HITL prompt for s1 and records its
    // seq (the SSE frame's `id:` line) as the watermark it'll reconnect
    // with.
    seed_pending_approval(&gateway, "s1", "first").await;
    let resp1 = reqwest::get(format!("http://{addr}/api/v1/sessions/s1/events"))
        .await
        .unwrap();
    let seq1 = read_sse_seq_until(resp1, |d| d.contains("first"))
        .await
        .expect("connection #1 sees the first approval");

    // "first" gets resolved (simulating the user's click) — no longer
    // outstanding — THEN a second approval fires for the same sid while
    // connection #1 is gone. This is the "missed while disconnected" event
    // `pending_for_sid` must now report (single-flight: only one prompt
    // outstanding per sid at a time, matching the real HITL flow).
    let pending = gateway.lock().await.pending_handle();
    pending.lock().await.take_by_token("first");
    seed_pending_approval(&gateway, "s1", "second").await;

    // Connection #2 reconnects naming seq1 as its watermark: it must see the
    // second approval (the gap), not a re-delivery of the first.
    let resp2 = reqwest::get(format!(
        "http://{addr}/api/v1/sessions/s1/events?last_event_id={seq1}"
    ))
    .await
    .unwrap();
    let payload = read_sse_data_until(resp2, |d| d.contains("second"))
        .await
        .expect("expected the missed second approval to be replayed");
    assert!(
        !payload.contains("\"token\":\"first\""),
        "must not re-deliver what connection #1 already had: {payload}"
    );
}

// ── v0.8.21 history + resume + external-import (gateway-attached, real HTTP) ──

/// End-to-end user flow over a real router + real gateway: create a session
/// (meta.json lands on disk), stop it (meta.json SURVIVES the stop), the live
/// list drops it while the history list now shows it, then resume puts it back
/// into the live list (and out of history). This is the "resume any past
/// session" acceptance path exercised exactly as the SPA drives it.
#[tokio::test]
async fn history_and_resume_roundtrip_over_http() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    seed_role_with_model(&project_dir, "cto", None);

    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway =
        ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir.clone());
    let addr =
        spawn_server(AppState::new(paths).with_gateway(Arc::new(tokio::sync::Mutex::new(gateway))))
            .await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/api/v1");

    // 1. Create → 201 {sid:"s1"}.
    let created = client
        .post(format!("{base}/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "cto", "vendor": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let sid = created.json::<Value>().await.unwrap()["sid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(sid, "s1");

    // 2. meta.json was written at spawn.
    let meta_path = project_dir
        .join(".ccteam")
        .join("chat")
        .join(&sid)
        .join("meta.json");
    assert!(
        meta_path.exists(),
        "create must write meta.json at {meta_path:?}"
    );

    // 3. Stop → 200; meta.json must NOT be deleted (resume depends on it).
    let stopped = client
        .post(format!("{base}/sessions/{sid}/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(stopped.status(), 200);
    assert!(meta_path.exists(), "stop must NOT delete meta.json");

    // 4. Live list no longer shows it.
    let live: Value = client
        .get(format!("{base}/projects/demo/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        live.as_array().unwrap().len(),
        0,
        "stopped session is not live"
    );

    // 5. History list shows exactly the one stopped session.
    let hist: Value = client
        .get(format!("{base}/projects/demo/sessions/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = hist.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "history shows the one stopped session: {hist}"
    );
    assert_eq!(rows[0]["sid"], "s1");
    assert_eq!(rows[0]["role"], "cto");
    assert_eq!(rows[0]["origin"], "ccteam");

    // 6. Resume → 200 {sid:"s1"}.
    let resumed = client
        .post(format!("{base}/projects/demo/sessions/{sid}/resume"))
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), 200, "resume a stopped session");
    assert_eq!(resumed.json::<Value>().await.unwrap()["sid"], "s1");

    // 7. Back in the live list…
    let live2: Value = client
        .get(format!("{base}/projects/demo/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        live2.as_array().unwrap().len(),
        1,
        "resumed session is live again"
    );
    assert_eq!(live2.as_array().unwrap()[0]["sid"], "s1");

    // 8. …and dropped from history (history excludes live sessions).
    let hist2: Value = client
        .get(format!("{base}/projects/demo/sessions/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        hist2.as_array().unwrap().len(),
        0,
        "a live session is not in history"
    );
}

/// End-to-end adopt flow: a native Claude session whose recorded `cwd` matches
/// the project is discovered as adoptable; importing a uuid that does NOT match
/// is rejected (the cross-project ACL — Fix 2); importing the real uuid mints a
/// ccteam session with an `adopted` meta.json, after which it drops out of
/// external discovery. Mutates `$HOME` (discovery reads `~/.claude/projects/`);
/// the other tests in this binary never read `$HOME`, so there is no clash.
#[tokio::test]
async fn import_external_claude_session_over_http() {
    let home = TempDir::new().unwrap();
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    let cwd = project_dir.to_string_lossy().to_string();

    // A fake native Claude transcript whose cwd == this project.
    let claude_dir = home.path().join(".claude").join("projects").join("enc");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let uuid = "abcdef01-2345-6789-abcd-ef0123456789";
    std::fs::write(
        claude_dir.join(format!("{uuid}.jsonl")),
        format!(
            "{{\"type\":\"user\",\"cwd\":\"{cwd}\"}}\n{{\"type\":\"custom-title\",\"customTitle\":\"adopt me\"}}"
        ),
    )
    .unwrap();
    std::env::set_var("HOME", home.path());
    // CCTEAM_HOME wins over HOME in the root resolvers; a shell that
    // exports it would redirect every "isolated" write back into the
    // REAL ~/.ccteam. Pin both.
    std::env::set_var("CCTEAM_HOME", home.path().join(".ccteam"));

    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway =
        ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir.clone());
    let addr =
        spawn_server(AppState::new(paths).with_gateway(Arc::new(tokio::sync::Mutex::new(gateway))))
            .await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/api/v1");

    // Discovery lists the matching-cwd session as adoptable.
    let ext: Value = client
        .get(format!("{base}/projects/demo/external-sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ext.as_array()
            .unwrap()
            .iter()
            .any(|r| r["vendor_uuid"] == uuid && r["adoptable"] == true),
        "external discovery lists the matching-cwd session: {ext}"
    );

    // A uuid with no matching transcript is NOT adoptable for this project → 400.
    let bad = client
        .post(format!("{base}/projects/demo/sessions/import"))
        .json(&serde_json::json!({
            "vendor": "claude",
            "vendor_uuid": "00000000-0000-0000-0000-000000000000"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400, "a uuid with no matching cwd is rejected");

    // The real uuid adopts → 201 {sid}.
    let ok = client
        .post(format!("{base}/projects/demo/sessions/import"))
        .json(&serde_json::json!({"vendor": "claude", "vendor_uuid": uuid}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 201, "adopt the matching session");
    let new_sid = ok.json::<Value>().await.unwrap()["sid"]
        .as_str()
        .unwrap()
        .to_string();

    // meta.json written with the foreign uuid + adopted origin.
    let meta_path = project_dir
        .join(".ccteam")
        .join("chat")
        .join(&new_sid)
        .join("meta.json");
    assert!(meta_path.exists(), "import writes meta.json");
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(
        meta["vendor_uuid"], uuid,
        "adopted meta keeps the foreign uuid"
    );
    assert_eq!(meta["origin"], "adopted");
    // v0.8.22 P1 — the vendor's own `custom-title` (extracted by discovery for
    // the import dialog) is now PERSISTED into meta.json instead of being
    // discarded once the dialog closes.
    assert_eq!(
        meta["title"], "adopt me",
        "the vendor custom-title survives into meta.json: {meta}"
    );
    assert_eq!(meta["title_source"], "vendor");

    // Now adopted → it drops out of external discovery (known uuid excluded).
    let ext2: Value = client
        .get(format!("{base}/projects/demo/external-sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !ext2
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["vendor_uuid"] == uuid),
        "an adopted session is no longer offered for import"
    );
}

// ── v0.8.22 P1 — session-title system: PATCH /api/v1/sessions/{sid} ─────────

/// Happy path + input validation for the rename route: 200 `{sid, title}`
/// with the rule-based-cleaned title persisted to meta.json (and reflected in
/// the live session list), a blank title 400s, and an unknown sid 404s.
#[tokio::test]
async fn rename_session_over_http_happy_path_and_validation() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    seed_role_with_model(&project_dir, "cto", None);

    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway =
        ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir.clone());
    let addr =
        spawn_server(AppState::new(paths).with_gateway(Arc::new(tokio::sync::Mutex::new(gateway))))
            .await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/api/v1");

    let created = client
        .post(format!("{base}/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "cto", "vendor": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let sid = created.json::<Value>().await.unwrap()["sid"]
        .as_str()
        .unwrap()
        .to_string();

    // Blank title → 400, meta.json untouched.
    let blank = client
        .patch(format!("{base}/sessions/{sid}"))
        .json(&serde_json::json!({"title": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(blank.status(), 400);

    // Happy path: whitespace-padded, multi-space title is rule-truncated
    // (collapsed + trimmed) server-side, never stored verbatim with padding.
    let renamed = client
        .patch(format!("{base}/sessions/{sid}"))
        .json(&serde_json::json!({"title": "  Fix   the login   bug  "}))
        .send()
        .await
        .unwrap();
    assert_eq!(renamed.status(), 200);
    let body: Value = renamed.json().await.unwrap();
    assert_eq!(body["sid"], sid);
    assert_eq!(body["title"], "Fix the login bug");

    // meta.json on disk carries the User-sourced title.
    let meta_path = project_dir
        .join(".ccteam")
        .join("chat")
        .join(&sid)
        .join("meta.json");
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(meta["title"], "Fix the login bug");
    assert_eq!(meta["title_source"], "user");

    // The live session list reflects the new title too.
    let live: Value = client
        .get(format!("{base}/projects/demo/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(live.as_array().unwrap()[0]["title"], "Fix the login bug");

    // Unknown sid → 404.
    let unknown = client
        .patch(format!("{base}/sessions/s999"))
        .json(&serde_json::json!({"title": "whatever"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
}

/// ACL: a tenant may rename its OWN project's session, but a different
/// tenant (no ownership of that project) gets 404 — the same project-owned
/// gate every other `/sessions/{sid}/*` route uses (`gate_sid` →
/// `can_see_project`), proven end to end with real per-tenant tokens.
#[tokio::test]
async fn rename_session_denies_cross_tenant_project() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    seed_role_with_model(&project_dir, "cto", None);

    // Two tenants; the project is owned by tenant A only.
    let mut reg = ccteam_core::tenants::TenantRegistry::default();
    let tenant_a = reg.add("alice");
    let tenant_b = reg.add("bob");
    reg.save(&paths.users_dir()).unwrap();
    let token_a = tenant_a.web_token.clone();
    let token_b = tenant_b.web_token.clone();

    let state_path = paths.project_state("demo");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut st = ccteam_core::ProjectState::initial_for_team("demo".into(), "dev".into());
    st.owner = Some(format!("user:{}", tenant_a.id));
    st.save(&state_path).unwrap();

    let factory = Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway =
        ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir.clone());
    const ADMIN_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
    let state = AppState::with_auth(paths, ccteam_web::AuthState::enabled(ADMIN_HEX.into()))
        .with_gateway(Arc::new(tokio::sync::Mutex::new(gateway)));
    let addr = spawn_server(state).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{addr}/api/v1");

    // Tenant A creates a session in its own project.
    let created = client
        .post(format!("{base}/projects/demo/sessions"))
        .header("Authorization", format!("Bearer ccteam:{token_a}"))
        .json(&serde_json::json!({"role": "cto", "vendor": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "tenant A creates in its own project");
    let sid = created.json::<Value>().await.unwrap()["sid"]
        .as_str()
        .unwrap()
        .to_string();

    // Tenant B (no ownership) is denied — 404, not a leak of the sid's
    // existence.
    let denied = client
        .patch(format!("{base}/sessions/{sid}"))
        .header("Authorization", format!("Bearer ccteam:{token_b}"))
        .json(&serde_json::json!({"title": "hijacked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 404, "cross-tenant rename must be denied");

    // Tenant A (the owner) can rename it.
    let ok = client
        .patch(format!("{base}/sessions/{sid}"))
        .header("Authorization", format!("Bearer ccteam:{token_a}"))
        .json(&serde_json::json!({"title": "my own session"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200, "the owning tenant can rename its session");
}
