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
async fn create_session_returns_model_warning_in_body() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    seed_role_with_model(&project_dir, "reviewer", Some("deepseek-via-claude"));
    seed_role_with_model(&project_dir, "sonnet", Some("sonnet[1m]"));

    let factory = Arc::new(|vendor| {
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
