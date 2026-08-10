//! v0.9.0 W4 (F4) — team visualization integration tests:
//! `GET /api/v1/agents/graph` (snapshot + ACL) and `GET /api/v1/agents/events`
//! (global SSE: delegation frames carry `slug`, `Last-Event-ID` replay, tenant
//! frame filter). Drives a REAL [`ccteam_im::gateway::Gateway::create_delegated_session`]
//! (the same call `session_spawn` routes through) against a `FakeAdapter`, so
//! the emitted `delegation_spawned` progress event + its
//! `GatewayEventKind::Delegation` broadcast twin are the genuine article, not
//! a hand-built fixture.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ccteam_core::tenants::TenantRegistry;
use ccteam_core::{CcteamPaths, ProjectState};
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, PermissionMode, SessionProtocol, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadStatus, TurnId, TurnInput,
};
use ccteam_im::gateway::{DelegationParent, Gateway, SpawnTuning};
use ccteam_web::{router_with_state, AppState, AuthState};
use futures::stream::{self, BoxStream};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";

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
        "agents-test"
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
        Ok(TurnId::new("turn-agents-test"))
    }

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: ccteam_harness::TurnRouting,
    ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(ccteam_harness::TurnSubmission::started)
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ccteam_harness::ToolSurfaceRebuild, HarnessError> {
        // Test double: no tool face to rebuild.
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }

    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        // Scripted test stream: one-shot. Re-attaching would replay
        // the script, which is exactly what `Rebuildable` forbids.
        ccteam_harness::EventAttachment::OneShot
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::empty())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "agents-test".into(),
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

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // A session in the `stall` project models an adapter that cannot answer
        // (a wedged vendor transport / a huge blocking transcript read). The
        // graph must NOT wait on it — see
        // `agents_graph_survives_a_stalled_adapter`. Keyed off the handle
        // identity (`{slug}-{sid}`) so it needs no shared mutable state and
        // stays correct under parallel tests.
        if h.identity.starts_with("stall-") {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        // Every other LIVE session's statusline reports this model + effort —
        // the graph's statusline join (TEAM-4) reads it through the same source
        // as `GET /sessions/{sid}/status`.
        Ok(ThreadStatus {
            model: Some("agents-test-model".to_string()),
            effort: Some("high".to_string()),
            ..ThreadStatus::default()
        })
    }
}

fn factory(
) -> Arc<dyn Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync>
{
    Arc::new(|vendor, _protocol| {
        Arc::new(FakeAdapter { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    })
}

async fn spawn(state: AppState) -> SocketAddr {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn bearer(hex: &str) -> String {
    format!("Bearer ccteam:{hex}")
}

/// Register "demo" on disk (legacy-fallback discovery: `collect_projects`
/// walks `projects_root` for any dir with a parseable `.ccteam/state.json`,
/// no `config.yaml` entry required — mirrors `tenant_acl_test.rs`).
fn register_project(paths: &CcteamPaths, slug: &str, owner: Option<&str>) -> std::path::PathBuf {
    let state_path = paths.project_state(slug);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut st = ProjectState::initial_for_team(slug.to_string(), "dev".to_string());
    st.owner = owner.map(str::to_string);
    st.save(&state_path).unwrap();
    paths.projects_root.join(slug)
}

/// Build a gateway over "demo" + spawn ONE delegated child (mirrors what
/// `session_spawn` does for an Ambient caller) — the real code path that
/// emits `delegation_spawned` (progress.jsonl, when project_paths is wired)
/// AND its [`ccteam_im::gateway::GatewayEventKind::Delegation`] broadcast
/// twin (unconditional — see `Gateway::emit_delegation_progress`).
/// Returns `(gateway, child_sid)`.
async fn spawn_delegated_child(project_dir: std::path::PathBuf) -> (Gateway, String) {
    spawn_delegated_child_in("demo", project_dir).await
}

/// [`spawn_delegated_child`] for an arbitrary slug (the slug reaches the
/// `FakeAdapter` through the handle identity, which is how the stalled-adapter
/// test selects its behaviour).
async fn spawn_delegated_child_in(
    slug: &str,
    project_dir: std::path::PathBuf,
) -> (Gateway, String) {
    let mut gateway = Gateway::new_with_factory(factory(), slug, project_dir);
    let outcome = gateway
        .create_delegated_session(
            slug.to_string(),
            "worker".to_string(),
            AgentVendor::Claude,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".to_string(),
            SpawnTuning::default(),
            Some(DelegationParent {
                sid: "s0".to_string(),
                depth: 0,
                role: "brain".to_string(),
            }),
            Some("research task".to_string()),
        )
        .await
        .expect("create_delegated_session succeeds against a FakeAdapter");
    (gateway, outcome.sid)
}

/// Persist a `meta.json` for a sid the gateway does NOT track — an idle
/// (stopped/evicted) session the graph must still list — optionally carrying
/// a spawn-time `model` override, which the graph must NOT echo as a live
/// model.
fn idle_meta(project_dir: &std::path::Path, sid: &str, model: Option<&str>) {
    let m = ccteam_harness::SessionMeta {
        managed_by: Default::default(),
        sid: sid.to_string(),
        slug: "demo".to_string(),
        vendor: AgentVendor::Claude,
        protocol: SessionProtocol::StreamJson,
        role: "worker".to_string(),
        permission_mode: PermissionMode::Skip,
        owner: "user:web".to_string(),
        vendor_uuid: String::new(),
        model: model.map(str::to_string),
        effort: None,
        host: String::new(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        last_active: "2026-01-01T00:00:00Z".to_string(),
        origin: ccteam_harness::SessionOrigin::Ccteam,
        title: None,
        title_source: None,
        turn_count: 0,
        cost_usd: None,
        tokens_total: None,
        role_sha: None,
        skills_sha: None,
        trigger: None,
        parent_sid: None,
        spawned_by_role: None,
        delegation_depth: 0,
    };
    ccteam_harness::write_session_meta(project_dir, &m).unwrap();
}

/// Open `path` as an SSE stream and return a line reader. Self-contained copy
/// (mirrors the pattern every other SSE-reading test file keeps its own copy
/// of, e.g. the retired `sse_test.rs`/`e2e_test.rs` helpers).
async fn open_sse(
    addr: SocketAddr,
    path: &str,
    auth: &str,
) -> tokio::io::Lines<impl AsyncBufReadExt + Unpin> {
    let url = format!("http://{addr}{path}");
    let resp = client()
        .get(&url)
        .header("Authorization", auth)
        .send()
        .await
        .expect("sse get");
    assert_eq!(resp.status(), 200);
    let stream = resp.bytes_stream();
    use futures::stream::StreamExt;
    let mapped = stream.map(|r| r.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(mapped);
    let buf = tokio::io::BufReader::new(reader);
    buf.lines()
}

/// Read SSE frames for up to `deadline`, returning every `(event, data)` pair
/// seen (not just the first) so a test can assert absence as well as presence.
async fn read_events(
    lines: &mut tokio::io::Lines<impl AsyncBufReadExt + Unpin>,
    deadline: Duration,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut data = String::new();
    let mut event_name = String::from("message");
    let _ = tokio::time::timeout(deadline, async {
        loop {
            let Some(next) = lines.next_line().await.ok().flatten() else {
                return;
            };
            if next.is_empty() {
                if !data.is_empty() {
                    out.push((event_name.clone(), data.clone()));
                }
                data.clear();
                event_name = "message".to_string();
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
                event_name = rest.trim().to_string();
            }
        }
    })
    .await;
    out
}

#[tokio::test]
async fn agents_graph_no_gateway_is_503() {
    let tmp = tempfile::TempDir::new().unwrap();
    let addr = spawn(AppState::new(fake_paths(tmp.path()))).await;
    let resp = client()
        .get(format!("http://{addr}/api/v1/agents/graph"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

#[tokio::test]
async fn agents_graph_shape_and_tenant_acl_filter() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    // Unowned ("demo" belongs to nobody in particular) — admin-visible (legacy
    // CLI-created projects are admin-visible, per `Identity::can_see_owner`);
    // a tenant never sees an unowned project.
    let project_dir = register_project(&paths, "demo", None);

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    let (gateway, child_sid) = spawn_delegated_child(project_dir).await;
    let gw = Arc::new(tokio::sync::Mutex::new(gateway));
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()))
        .with_gateway(Arc::clone(&gw), gw.lock().await.principals());
    let addr = spawn(state).await;

    // Admin sees the graph: one node (the delegated child) + one edge s0→child.
    let admin_auth = bearer(ADMIN_HEX);
    let resp = client()
        .get(format!("http://{addr}/api/v1/agents/graph"))
        .header("Authorization", &admin_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let nodes = body["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1, "one delegated child node; got {body}");
    assert_eq!(nodes[0]["sid"], Value::String(child_sid.clone()));
    assert_eq!(nodes[0]["slug"], Value::String("demo".to_string()));
    assert_eq!(nodes[0]["parent_sid"], Value::String("s0".to_string()));
    assert_eq!(nodes[0]["status"], Value::String("live".to_string()));
    let edges = body["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["parent"], Value::String("s0".to_string()));
    assert_eq!(edges[0]["child"], Value::String(child_sid.clone()));
    assert_eq!(
        body["hosts"],
        serde_json::json!(["local"]),
        "local host surfaced"
    );

    // A tenant that does not own "demo" sees an empty graph (no slug filter).
    let tenant_auth = bearer(&tenant_tok);
    let resp = client()
        .get(format!("http://{addr}/api/v1/agents/graph"))
        .header("Authorization", &tenant_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["nodes"].as_array().unwrap().len(),
        0,
        "tenant must not see the admin-visible project's sessions"
    );

    // Explicit `?slug=demo` 404s for the tenant (don't reveal existence).
    let resp = client()
        .get(format!("http://{addr}/api/v1/agents/graph?slug=demo"))
        .header("Authorization", &tenant_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// TEAM-4 — the graph joins each LIVE node with the model + reasoning effort
/// its session runs right now, read through the SAME statusline source as
/// `GET /sessions/{sid}/status` (this file's `FakeAdapter` reports
/// `agents-test-model` / `high` from `thread_status`). An idle
/// (persisted-only) node stays `null` even though its `meta.json` carries a
/// spawn-time `model` override — nothing live to report.
#[tokio::test]
async fn agents_graph_joins_live_session_model_and_leaves_idle_null() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = register_project(&paths, "demo", None);

    let (gateway, child_sid) = spawn_delegated_child(project_dir.clone()).await;
    idle_meta(&project_dir, "s9", Some("spawn-time-override"));

    let gw = Arc::new(tokio::sync::Mutex::new(gateway));
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()))
        .with_gateway(Arc::clone(&gw), gw.lock().await.principals());
    let addr = spawn(state).await;

    let resp = client()
        .get(format!("http://{addr}/api/v1/agents/graph"))
        .header("Authorization", bearer(ADMIN_HEX))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let nodes = body["nodes"].as_array().unwrap();
    let node = |sid: &str| {
        nodes
            .iter()
            .find(|n| n["sid"] == sid)
            .unwrap_or_else(|| panic!("expected node {sid} in {body}"))
    };
    let live = node(&child_sid);
    assert_eq!(live["status"], Value::String("live".to_string()));
    assert_eq!(
        live["model"],
        Value::String("agents-test-model".to_string()),
        "live node carries the statusline model: {body}"
    );
    assert_eq!(
        live["effort"],
        Value::String("high".to_string()),
        "live node carries the statusline effort: {body}"
    );
    let idle = node("s9");
    assert_eq!(idle["status"], Value::String("idle".to_string()));
    assert!(
        idle["model"].is_null(),
        "idle node must not echo the spawn-time meta model: {body}"
    );
    assert!(
        idle["effort"].is_null(),
        "idle node has no live effort to report: {body}"
    );
}

/// 2026-08-02 — ONE stuck adapter must not cost the whole team view.
///
/// The statusline join used to be a sequential loop with no deadline, so the
/// endpoint's latency was the SUM over live sessions of however long each
/// vendor took — unbounded. A single wedged transport (or a live session whose
/// transcript read blocked) held the entire graph, the SPA's poller stacked up
/// behind it, and the browser's per-origin connection budget filled with stuck
/// requests until nothing on the page could load.
///
/// The graph must now answer within its per-session deadline and report the
/// unresponsive node honestly (`model`/`effort` absent — exactly what an idle
/// node reports), not hang. `stall-*` sessions sleep 30s in `thread_status`, so
/// under the old sequential-and-unbounded shape this test would take 30s and
/// blow the assertion below.
#[tokio::test]
async fn agents_graph_survives_a_stalled_adapter() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = register_project(&paths, "stall", None);

    let (gateway, child_sid) = spawn_delegated_child_in("stall", project_dir.clone()).await;

    let gw = Arc::new(tokio::sync::Mutex::new(gateway));
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()))
        .with_gateway(Arc::clone(&gw), gw.lock().await.principals());
    let addr = spawn(state).await;

    let started = std::time::Instant::now();
    let resp = client()
        .get(format!("http://{addr}/api/v1/agents/graph"))
        .header("Authorization", bearer(ADMIN_HEX))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let elapsed = started.elapsed();
    let body: Value = resp.json().await.unwrap();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the graph must not wait on a stalled adapter (took {elapsed:?})"
    );
    let node = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["sid"] == child_sid.as_str())
        .unwrap_or_else(|| panic!("expected node {child_sid} in {body}"));
    // Still reported, still honestly LIVE — only its statusline is missing.
    assert_eq!(node["status"], Value::String("live".to_string()));
    assert!(
        node["model"].is_null(),
        "a session that missed the deadline reports no model: {body}"
    );
    assert!(
        node["effort"].is_null(),
        "a session that missed the deadline reports no effort: {body}"
    );
}

#[tokio::test]
async fn agents_events_delivers_delegation_frame_with_slug_and_replays_last_event_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = register_project(&paths, "demo", None);

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    let gateway = Gateway::new_with_factory(factory(), "demo", project_dir.clone());
    let gw = Arc::new(tokio::sync::Mutex::new(gateway));
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()))
        .with_gateway(Arc::clone(&gw), gw.lock().await.principals());
    let addr = spawn(state).await;

    // Let the global-ring feeder task actually subscribe to the gateway's
    // broadcast (spawned by `with_gateway`, above) before the delegation
    // event fires — otherwise a `tokio::sync::broadcast` send with no
    // subscriber yet is simply lost (not queued for a late subscriber).
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }

    let outcome = gw
        .lock()
        .await
        .create_delegated_session(
            "demo".to_string(),
            "worker".to_string(),
            AgentVendor::Claude,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".to_string(),
            SpawnTuning::default(),
            Some(DelegationParent {
                sid: "s0".to_string(),
                depth: 0,
                role: "brain".to_string(),
            }),
            Some("research task".to_string()),
        )
        .await
        .unwrap();
    let child_sid = outcome.sid.clone();

    // Admin, `?last_event_id=0` → replays the ring's full backlog (the event
    // above already landed in it, no live-tap race).
    let admin_auth = bearer(ADMIN_HEX);
    let mut lines = open_sse(addr, "/api/v1/agents/events?last_event_id=0", &admin_auth).await;
    let events = read_events(&mut lines, Duration::from_secs(2)).await;
    let delegation = events
        .iter()
        .find(|(name, _)| name == "delegation")
        .unwrap_or_else(|| panic!("no `event: delegation` frame among {events:?}"));
    let payload: Value = serde_json::from_str(&delegation.1).unwrap();
    assert_eq!(payload["relation"], Value::String("spawned".to_string()));
    assert_eq!(payload["parent_sid"], Value::String("s0".to_string()));
    assert_eq!(payload["child_sid"], Value::String(child_sid.clone()));
    assert_eq!(payload["slug"], Value::String("demo".to_string()));
    assert_eq!(payload["title"], Value::String("research task".to_string()));

    // A tenant that doesn't own "demo" replays the SAME backlog window but
    // must see ZERO frames naming this slug (fail-closed ACL filter).
    let tenant_auth = bearer(&tenant_tok);
    let mut tenant_lines =
        open_sse(addr, "/api/v1/agents/events?last_event_id=0", &tenant_auth).await;
    let tenant_events = read_events(&mut tenant_lines, Duration::from_millis(500)).await;
    assert!(
        tenant_events.iter().all(|(_, data)| {
            serde_json::from_str::<Value>(data)
                .map(|v| v["slug"] != Value::String("demo".to_string()))
                .unwrap_or(true)
        }),
        "tenant must not see any frame naming the admin-visible project; got {tenant_events:?}",
    );
}
