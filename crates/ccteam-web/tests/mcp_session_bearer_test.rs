//! v0.9.0 W1 (F1/G4) — `POST /mcp` with a session bearer
//! `ccteam-sid:<sid>:<secret>`: the four-field caller identity is injected so
//! the Ambient principal gate authenticates the live session and derives the
//! project slug SERVER-side. Proves session_* over HTTP works (pre-fix it
//! failed closed with "no project scope"), plus the auth-negative cases.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};
use ccteam_web::{router_with_state, AppState};
use futures::stream::{self, BoxStream};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn seed_web_token(paths: &CcteamPaths, hex: &str) {
    let path = paths.web_token_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, hex).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();
    }
}

/// A fake harness that records each spawned session's per-session secret, so a
/// test can construct a real `ccteam-sid:<sid>:<secret>` bearer.
#[derive(Clone)]
struct SecretRecordingAdapter {
    vendor: AgentVendor,
    secrets: Arc<StdMutex<HashMap<String, String>>>,
}

#[async_trait::async_trait]
impl HarnessAdapter for SecretRecordingAdapter {
    fn name(&self) -> &'static str {
        "mcp-bearer-test"
    }
    fn vendor(&self) -> AgentVendor {
        self.vendor
    }
    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        self.secrets
            .lock()
            .unwrap()
            .insert(ctx.sid.clone(), ctx.secret.clone());
        Ok(ThreadHandle {
            vendor: self.vendor,
            mode: ExecutionMode::Chat,
            identity: format!("{}-{}", ctx.slug, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: Value::Null,
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("turn-mcp-bearer"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::empty())
    }
    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "mcp-bearer-test".into(),
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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn post_mcp(addr: SocketAddr, bearer: &str, body: Value) -> reqwest::Response {
    client()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// Build a gateway with one live session and return (AppState, sid, secret).
async fn state_with_one_session(paths: CcteamPaths) -> (AppState, String, String) {
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    let secrets: Arc<StdMutex<HashMap<String, String>>> = Arc::new(StdMutex::new(HashMap::new()));
    let secrets_f = Arc::clone(&secrets);
    let factory = Arc::new(move |vendor, _protocol| {
        Arc::new(SecretRecordingAdapter {
            vendor,
            secrets: Arc::clone(&secrets_f),
        }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let mut gateway = ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir);
    let sid = gateway
        .create_session_api(
            "demo".into(),
            "reviewer".into(),
            AgentVendor::Claude,
            ccteam_harness::PermissionMode::Skip,
        )
        .await
        .unwrap()
        .sid;
    let secret = secrets
        .lock()
        .unwrap()
        .get(&sid)
        .cloned()
        .expect("adapter recorded the session secret");
    assert_eq!(secret.len(), 32, "the minted secret is 128-bit hex");
    let app = AppState::new(paths).with_gateway(Arc::new(tokio::sync::Mutex::new(gateway)));
    (app, sid, secret)
}

/// The full sid-bearer round-trip: session_list + session_spawn (with the slug
/// derived server-side) both succeed under `ccteam-sid:<sid>:<secret>`.
#[tokio::test]
async fn session_bearer_round_trip_list_and_spawn() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let (app, sid, secret) = state_with_one_session(paths).await;
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    // session_list authenticates by principal and reaches the live gateway.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"ccteam__session_list","arguments":{}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "list: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"sessions\""), "got: {text}");

    // session_spawn with NO caller-supplied project — the slug is derived
    // server-side from the caller's session (demo). Roleless keeps it simple.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"ccteam__session_spawn","arguments":{"vendor":"claude"}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "spawn: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let spawned: Value = serde_json::from_str(text).unwrap();
    assert_eq!(spawned["ok"], true);
    assert_eq!(
        spawned["project"], "demo",
        "slug must be derived server-side from the caller's session, got: {text}"
    );
    assert!(
        spawned["sid"].as_str().is_some_and(|s| s != sid),
        "spawn must mint a NEW sid, got: {text}"
    );
    // vendor_session_id + host are always returned (may be empty honestly).
    assert!(spawned.get("vendor_session_id").is_some(), "got: {text}");
    assert_eq!(spawned["host"], "local");
}

/// A wrong secret or an unknown sid → 401 (the session bearer fails to resolve
/// a principal, so the handler rejects before dispatch).
#[tokio::test]
async fn session_bearer_wrong_secret_or_unknown_sid_is_401() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let (app, sid, _secret) = state_with_one_session(paths).await;
    let addr = spawn_server(app).await;

    let wrong = post_mcp(
        addr,
        &format!("ccteam-sid:{sid}:ffffffffffffffffffffffffffffffff"),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"ccteam__session_list","arguments":{}}}),
    )
    .await;
    assert_eq!(wrong.status(), 401, "wrong secret → 401");

    let unknown = post_mcp(
        addr,
        "ccteam-sid:s999:deadbeefdeadbeefdeadbeefdeadbeef",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"ccteam__session_list","arguments":{}}}),
    )
    .await;
    assert_eq!(unknown.status(), 401, "unknown sid → 401");
}

/// After the session is stopped, its bearer goes stale: the principal no longer
/// resolves, so a subsequent call is denied (401).
#[tokio::test]
async fn session_bearer_denied_after_stop() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    seed_web_token(&paths, TOKEN_HEX);
    let (app, sid, secret) = state_with_one_session(paths).await;
    let gw = app.gateway.clone().expect("gateway attached");
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    // Works before stop.
    let ok = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"ccteam__session_list","arguments":{}}}),
    )
    .await;
    assert_eq!(ok.status(), 200);

    // Stop the session → the principal no longer resolves.
    gw.lock().await.stop_session(&sid).await.unwrap();

    let denied = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"ccteam__session_list","arguments":{}}}),
    )
    .await;
    assert_eq!(denied.status(), 401, "stale bearer after stop → 401");
}
