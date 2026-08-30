//! v0.9.0 W1 (F1/G4) — `POST /mcp` with a session bearer
//! `ccteam-sid:<sid>:<secret>`: the four-field caller identity is injected so
//! the Ambient principal gate authenticates the live session and derives the
//! project slug SERVER-side. Proves session_* over HTTP works (pre-fix it
//! failed closed with "no project scope"), plus the auth-negative cases.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};

use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};
use ccteam_web::{router_with_state, AppState, AuthState};
use futures::stream::{self, BoxStream};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpListener;
use tokio::process::{ChildStdin, ChildStdout, Command};

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

/// A fake harness that records each spawned session's per-session secret, so a
/// test can construct a real `ccteam-sid:<sid>:<secret>` bearer.
#[derive(Clone)]
struct SecretRecordingAdapter {
    vendor: AgentVendor,
    secrets: Arc<StdMutex<HashMap<String, String>>>,
    /// Stands in for a vendor that uses its principal DURING the spawn
    /// (OpenCode's `session/new`, Pi's bridge `session_start`): filled by the
    /// test once the gateway exists, read inside `start_thread`.
    principals: Arc<StdMutex<Option<Arc<ccteam_im::principals::SessionPrincipals>>>>,
    /// What that mid-spawn verification returned.
    verified_during_spawn: Arc<StdMutex<Option<bool>>>,
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
        // The vendor's tool-face handshake, at the only moment that matters.
        let registry = self.principals.lock().unwrap().clone();
        if let Some(registry) = registry {
            let ok = registry.verify(&ctx.sid, &ctx.secret).is_some();
            *self.verified_during_spawn.lock().unwrap() = Some(ok);
        }
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

async fn write_app_server_rpc(stdin: &mut ChildStdin, message: Value) {
    let mut bytes = serde_json::to_vec(&message).unwrap();
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_app_server_response(
    lines: &mut Lines<BufReader<ChildStdout>>,
    expected_id: i64,
) -> Value {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), lines.next_line())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for app-server response {expected_id}"))
            .unwrap()
            .unwrap_or_else(|| panic!("app-server stdout closed before response {expected_id}"));
        let message: Value = serde_json::from_str(&line).unwrap();
        if message["id"].as_i64() == Some(expected_id) {
            return message;
        }
    }
}

/// Build a gateway with one live session and return (AppState, sid, secret).
/// `auth` sets the web gate: `AuthState::enabled(...)` reproduces the
/// production non-loopback bind (the configuration where `/mcp` behind
/// `auth_layer` used to 401 session bearers).
async fn state_with_one_session(paths: CcteamPaths, auth: AuthState) -> (AppState, String, String) {
    let project_dir = paths.projects_root.join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    let secrets: Arc<StdMutex<HashMap<String, String>>> = Arc::new(StdMutex::new(HashMap::new()));
    let secrets_f = Arc::clone(&secrets);
    let principals_cell: Arc<StdMutex<Option<Arc<ccteam_im::principals::SessionPrincipals>>>> =
        Arc::new(StdMutex::new(None));
    let verified: Arc<StdMutex<Option<bool>>> = Arc::new(StdMutex::new(None));
    let (principals_f, verified_f) = (Arc::clone(&principals_cell), Arc::clone(&verified));
    let factory = Arc::new(move |vendor, _protocol| {
        Arc::new(SecretRecordingAdapter {
            vendor,
            secrets: Arc::clone(&secrets_f),
            principals: Arc::clone(&principals_f),
            verified_during_spawn: Arc::clone(&verified_f),
        }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let mut gateway = ccteam_im::gateway::Gateway::new_with_factory(factory, "demo", project_dir);
    *principals_cell.lock().unwrap() = Some(gateway.principals());
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
    // THE regression this registry exists for: OpenCode dials `/mcp` inside
    // `session/new` and Pi's bridge blocks `session_start` on `initialize` +
    // `tools/list`. When the principal was only registered at apply, that
    // handshake 401'd and OpenCode burned a 30s startup timeout on every
    // managed spawn.
    assert_eq!(
        *verified.lock().unwrap(),
        Some(true),
        "the session principal must verify WHILE the vendor is spawning"
    );
    let app = AppState::with_auth(paths, auth).with_gateway_owned(gateway);
    (app, sid, secret)
}

/// A vendor uses its principal DURING spawn — OpenCode dials `/mcp` inside
/// `session/new`, Pi's bridge blocks `session_start` on `initialize` +
/// `tools/list`. Before the principal registry, that authenticated against a
/// session that was not in the live map yet, so it 401'd and OpenCode burned
/// its 30s MCP startup timeout on EVERY managed spawn.
///
/// A reserved principal must therefore be able to DISCOVER its tool face — and
/// nothing more, because the session it belongs to does not exist yet.
#[tokio::test]
async fn a_spawning_principal_can_list_its_tools_but_not_call_them() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, _sid, _secret) = state_with_one_session(paths, AuthState::disabled()).await;
    // A sid that is mid-spawn: reserved, never applied.
    let principals = app
        .session_principals
        .as_ref()
        .expect("the registry travels with the gateway")
        .clone();
    principals.reserve(
        "s99",
        "spawning-secret",
        ccteam_im::principals::PrincipalFacts::new("demo", "reviewer", 0),
    );
    let addr = spawn_server(app).await;
    let bearer = "ccteam-sid:s99:spawning-secret".to_string();

    // Discovery: what the vendor's handshake actually needs.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(resp.status(), 200, "a spawning session must not be 401'd");
    let body: Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools/list answers a spawning principal");
    assert!(!tools.is_empty(), "the tool face must be discoverable");

    // …and it is the face the spawn ASKED for, not the full one. The child's
    // MCP client fetches this list exactly once, during startup, so a face
    // resolved later is a face it never sees (measured 2026-08-31: a
    // `tools:"none"` child was served six tools here and went on to call one).
    principals.reserve(
        "s98",
        "muted-secret",
        ccteam_im::principals::PrincipalFacts {
            tool_face: Some("none".into()),
            parent_sid: Some("s1".into()),
            ..ccteam_im::principals::PrincipalFacts::new("demo", "", 1)
        },
    );
    let resp = post_mcp(
        addr,
        "ccteam-sid:s98:muted-secret",
        json!({"jsonrpc":"2.0","id":9,"method":"tools/list","params":{}}),
    )
    .await;
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["result"]["tools"].as_array().map(Vec::len),
        Some(0),
        "a mid-spawn child hired with no tools must list none: {body}"
    );

    // Authority: withheld until the session is real.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
    )
    .await;
    let body: Value = resp.json().await.unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("still starting"),
        "a session that does not exist yet must not be able to act: {body}"
    );

    // …and a wrong secret is still just 401, spawning or not.
    let resp = post_mcp(
        addr,
        "ccteam-sid:s99:wrong",
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(resp.status(), 401);
}

/// The full sid-bearer round-trip: agent_read + agent (with the slug
/// derived server-side) both succeed under `ccteam-sid:<sid>:<secret>`.
#[tokio::test]
async fn session_bearer_round_trip_list_and_spawn() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    // agent_read authenticates by principal and reaches the live gateway.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "list: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"sessions\""), "got: {text}");

    // agent with NO caller-supplied project — the slug is derived
    // server-side from the caller's session (demo). Roleless keeps it simple.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"agent","arguments":{"vendor":"claude","task":"hello"}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "spawn: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let spawned: Value = serde_json::from_str(text).unwrap();
    assert!(spawned["sid"].is_string(), "spawn: {text}");
    assert!(
        spawned["sid"].as_str().is_some_and(|s| s != sid),
        "spawn must mint a NEW sid, got: {text}"
    );
    // v0.9.2 — an Ambient spawn carries the delegation-parent edge: the
    // server-verified caller sid, depth = parent + 1, and the caller label.
}

/// The per-caller face reaches the wire: a session bearer's `initialize` is
/// answered with instructions that NAME the caller. Without the transport
/// injecting the caller identity on non-`tools/call` methods, the daemon can
/// only serve the anonymous full face, and a leaf pays an orchestrator's bill.
#[tokio::test]
async fn initialize_under_a_session_bearer_states_the_caller_identity() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    // No `params` at all — the shape a minimal client actually sends.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let instructions = body["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(&format!(
            "You are {sid} in project demo. Completion notifications from your hires arrive here."
        )),
        "identity + delivery fact missing: {instructions}"
    );
    assert!(
        instructions.contains("Read those files before answering"),
        "the attachment policy is unconditional: {instructions}"
    );
}

/// THE v0.9.2 regression: with the web gate ENABLED (production default — the
/// daemon binds non-loopback), a session bearer must still reach `/mcp`.
/// Pre-fix, `/mcp` sat behind `auth_layer`, which only understands
/// `ccteam:<hex>` and 401'd `ccteam-sid:...` before the handler ran — managed
/// sessions lost their Ambient identity, their calls fell back to
/// admin-authenticated servers, and every A2A spawn came out rootless
/// (`parent_sid: null`). Also pins that the fallback it fell back TO is gone:
/// the live web token — the one this very `AuthState` accepts for `/api/v1/**` —
/// is refused here, so there is no longer a tier a lost identity can land on.
#[tokio::test]
async fn auth_enabled_session_bearer_reaches_mcp_and_spawn_links_parent() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, secret) =
        state_with_one_session(paths, AuthState::enabled(TOKEN_HEX.into())).await;
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    // The exact call that used to die at the outer layer with a plain-text 401
    // before require_mcp_auth could accept the session bearer.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "session bearer must pass /mcp with the web gate enabled"
    );

    // Ambient spawn → the delegation-parent edge is the caller's sid.
    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"agent","arguments":{"vendor":"claude","task":"hello"}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "ambient spawn: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let spawned: Value = serde_json::from_str(text).unwrap();
    assert!(spawned["sid"].is_string(), "ambient spawn: {text}");

    // The VALID web token — the same one `AuthState::enabled` accepts on every
    // `/api/v1` route — buys nothing here. It used to be an owner front door that
    // spawned rootlessly into any project it named; that tier is deleted, because
    // a credential shared by every process of a vendor cannot say who is calling.
    let resp = post_mcp(
        addr,
        &format!("ccteam:{TOKEN_HEX}"),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
               "params":{"name":"agent","arguments":{"vendor":"claude","task":"hello","project":"demo"}}}),
    )
    .await;
    assert_eq!(
        resp.status(),
        401,
        "the web-token family is not an MCP credential"
    );

    // Garbage bearers of BOTH families are still rejected.
    let resp = post_mcp(
        addr,
        "ccteam:deadbeef",
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(resp.status(), 401, "bad admin bearer → 401");
    let resp = post_mcp(
        addr,
        &format!("ccteam-sid:{sid}:ffffffffffffffffffffffffffffffff"),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(resp.status(), 401, "bad session bearer → 401");
}

/// The `parent_sid` declaration survives the family cull — and stays a
/// declaration.
///
/// It exists for a caller that holds no per-session principal (the local
/// `mcp.sock` fallback, where same-uid file access is already the trust
/// boundary): it may NAME its own sid so the ledger keeps an edge that really
/// exists, and the daemon validates that name against the live set. Kept per
/// owner decision. What it must never become is a second identity source — a
/// caller that DOES hold a principal is answered by the principal, so a declared
/// parent from a session bearer is ignored rather than honoured. Otherwise any
/// managed session could mount its children anywhere in somebody else's tree by
/// asking.
#[tokio::test]
async fn a_declared_parent_sid_never_overrides_a_verified_session_principal() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    let resp = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"agent",
                         "arguments":{"vendor":"claude","task":"hello","parent_sid":"s404"}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    // Not an error: the declaration is inert on this path, not rejected — the
    // caller's verified identity simply answers the question first.
    assert_eq!(body["result"]["isError"], false, "spawn: {body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let spawned: Value = serde_json::from_str(text).unwrap();
    assert!(spawned["sid"].is_string(), "spawn: {text}");
}

/// Deterministic fake-Codex acceptance: build the exact
/// `thread/start.config.mcp_servers.ccteam` HTTP entry, then use its URL and
/// Authorization header against the real `/mcp` route. This composes the
/// harness-side config builder with the daemon principal gate; a field-name,
/// bearer, or auth-prefix drift fails here instead of only in a live CLI.
#[tokio::test]
async fn codex_http_thread_config_passes_session_principal_gate() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let addr = spawn_server(app).await;
    let url = format!("http://{addr}/mcp");

    let config = ccteam_harness::execution::mcp_config::SessionMcpEndpoint::at(&url, &sid, &secret)
        .map(|ep| ccteam_harness::execution::mcp_config::project_codex_thread_config(&ep))
        .expect("live session principal produces a Codex thread override");
    let server = &config["mcp_servers"]["ccteam"];
    assert_eq!(server["url"], url);
    assert!(server.get("command").is_none(), "Codex MCP must be HTTP");
    let authorization = server["http_headers"]["Authorization"]
        .as_str()
        .expect("Codex static Authorization header");
    let bearer = authorization
        .strip_prefix("Bearer ")
        .expect("Authorization uses Bearer scheme");

    let resp = post_mcp(
        addr,
        bearer,
        json!({"jsonrpc":"2.0","id":41,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["result"]["isError"], false,
        "Codex HTTP config: {body}"
    );
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(&sid),
        "authenticated caller session missing: {text}"
    );
}

/// Real-machine acceptance for the deferred Codex HTTP migration. It starts
/// the installed `codex app-server`, gives it an HTTP global ccteam entry plus
/// a per-thread session override, then asks Codex itself to call
/// `agent` through `/mcp`. The project is intentionally omitted: only
/// a successfully authenticated session principal can derive `demo`
/// server-side, so a call that rode the global entry's enrollment credential
/// instead of the per-thread principal fails.
///
/// Run explicitly on a machine with Codex 0.144.x:
/// `cargo test -p ccteam-web --test mcp_session_bearer_test \
///   real_codex_http_mcp_passes_session_principal_gate -- --ignored --nocapture`
#[tokio::test]
#[ignore = "requires an installed codex 0.144.x binary"]
async fn real_codex_http_mcp_passes_session_principal_gate() {
    let version = std::process::Command::new("codex")
        .arg("--version")
        .output()
        .expect("codex binary must be installed");
    assert!(version.status.success(), "codex --version failed");
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    eprintln!("real Codex HTTP MCP smoke: {version}");

    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let addr = spawn_server(app).await;
    let url = format!("http://{addr}/mcp");

    // Pin an isolated global config to HTTP too. Codex deep-merges this with
    // the per-thread table; a legacy stdio global entry would make
    // thread/start fail with `url is not supported for stdio`.
    let codex_home = tmp.path().join("codex-home");
    let config_toml = codex_home.join("config.toml");
    // The global entry's IDENTITY is irrelevant here — what this test needs is
    // that it is HTTP, so Codex's deep merge of global + per-thread tables does
    // not reject a mixed transport. It still has to be a credential the writer
    // accepts, so mint a throwaway enrollment one under the temp root.
    let global_bearer = ccteam_core::enroll::mint_in(
        tmp.path(),
        ccteam_core::enroll::EnrollScope::User,
        "user:web-api",
        None,
    )
    .unwrap()
    .bearer();
    ccteam_core::mcp_register::install_codex_mcp_into(&config_toml, &url, &global_bearer).unwrap();

    let thread_config =
        ccteam_harness::execution::mcp_config::SessionMcpEndpoint::at(&url, &sid, &secret)
            .map(|ep| ccteam_harness::execution::mcp_config::project_codex_thread_config(&ep))
            .unwrap();
    let mut child = Command::new("codex")
        .args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", &codex_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn real codex app-server");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();

    write_app_server_rpc(
        &mut stdin,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{
                "clientInfo":{"name":"ccteam-real-http-smoke","version":"0"},
                "capabilities":{"experimentalApi":true}
            }
        }),
    )
    .await;
    let initialized = read_app_server_response(&mut stdout, 1).await;
    assert!(
        initialized.get("error").is_none(),
        "initialize: {initialized}"
    );
    write_app_server_rpc(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"initialized","params":null}),
    )
    .await;

    write_app_server_rpc(
        &mut stdin,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"thread/start",
            "params":{
                "cwd":tmp.path(),
                "threadSource":"user",
                "sessionStartSource":"startup",
                "config":thread_config
            }
        }),
    )
    .await;
    let started = read_app_server_response(&mut stdout, 2).await;
    assert!(started.get("error").is_none(), "thread/start: {started}");
    let thread_id = started
        .pointer("/result/thread/id")
        .or_else(|| started.pointer("/result/thread/threadId"))
        .or_else(|| started.pointer("/result/thread/thread_id"))
        .and_then(Value::as_str)
        .expect("thread/start result carries thread id");

    write_app_server_rpc(
        &mut stdin,
        json!({
            "jsonrpc":"2.0", "id":3, "method":"mcpServer/tool/call",
            "params":{
                "threadId":thread_id,
                "server":"ccteam",
                "tool":"agent",
                "arguments":{"vendor":"claude"}
            }
        }),
    )
    .await;
    let called = read_app_server_response(&mut stdout, 3).await;
    assert!(
        called.get("error").is_none(),
        "mcpServer/tool/call: {called}"
    );
    assert_ne!(called.pointer("/result/isError"), Some(&Value::Bool(true)));
    let text = called
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("agent returns MCP text content");
    let spawned: Value = serde_json::from_str(text).expect("agent result is JSON");
    assert!(spawned["sid"].is_string(), "real Codex MCP call: {spawned}");

    child.kill().await.ok();
    child.wait().await.ok();
}

/// A wrong secret or an unknown sid → 401 (the session bearer fails to resolve
/// a principal, so the handler rejects before dispatch).
#[tokio::test]
async fn session_bearer_wrong_secret_or_unknown_sid_is_401() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (app, sid, _secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let addr = spawn_server(app).await;

    let wrong = post_mcp(
        addr,
        &format!("ccteam-sid:{sid}:ffffffffffffffffffffffffffffffff"),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
    )
    .await;
    assert_eq!(wrong.status(), 401, "wrong secret → 401");

    let unknown = post_mcp(
        addr,
        "ccteam-sid:s999:deadbeefdeadbeefdeadbeefdeadbeef",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
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
    let (app, sid, secret) = state_with_one_session(paths, AuthState::disabled()).await;
    let gw = app.gateway.clone().expect("gateway attached");
    let addr = spawn_server(app).await;
    let bearer = format!("ccteam-sid:{sid}:{secret}");

    // Works before stop.
    let ok = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
    )
    .await;
    assert_eq!(ok.status(), 200);

    // Stop the session → the principal no longer resolves.
    gw.lock().await.stop_session(&sid).await.unwrap();

    let denied = post_mcp(
        addr,
        &bearer,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"agent_read","arguments":{}}}),
    )
    .await;
    assert_eq!(denied.status(), 401, "stale bearer after stop → 401");
}
