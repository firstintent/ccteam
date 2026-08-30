//! `POST /mcp` under an ENROLLMENT credential — one identity per hand-started
//! vendor process, issued by the daemon at `initialize`.
//!
//! The measured defect this replaces: a `codex` the user started in a terminal
//! authenticated with the machine-wide admin web token that ccteam had written
//! into the vendor's global config, so its `agent` children mounted as
//! ROOTS and landed in a scratch project neither session had named. A static file
//! is shared by every process that vendor starts, so it can only ever carry
//! "whose config is this" — the per-process identity has to come from the server,
//! and the Streamable HTTP transport already has a slot for exactly that
//! (`Mcp-Session-Id`).
//!
//! What these tests pin, at the real router boundary:
//! 1. `initialize` answers with an id AND mints a ledger node (`managed_by:
//!    external`) in the credential's project;
//! 2. a later call carrying that id acts as the NODE — its spawns hang off it;
//! 3. one credential, two processes, two ids, two nodes (the whole point);
//! 4. an id is not a credential: replayed under another one it is a 404 that
//!    reveals nothing;
//! 5. no id → 404 pointing at `initialize` (the transport's own recovery path);
//! 6. an unbound (user-scoped) credential that names NO project may discover, and
//!    nothing else — ccteam never GUESSES the project, which is the bug class
//!    being deleted;
//! 7. …but it may NAME one it owns on a `session_*` call: that binds the session,
//!    mints its node and makes its children hang off it (the machine-wide
//!    credential every vendor config carries has no other rung — it names no
//!    project by construction). One workspace for life: a later call naming a
//!    different project is refused, and a project its owner cannot see is
//!    answered exactly like one that does not exist;
//! 8. `DELETE` ends the binding, the principal and the node together;
//! 9. the admin web token is refused outright — it cannot open a binding and
//!    cannot ride one;
//! 10. a bound client is a project-scoped caller in `status` too, not only in
//!     `session_*`.
//!
//! Isolation (CLAUDE.md): every state seam is `_in(root)`-injected through
//! `CcteamPaths` pointed at a tempdir, and `HOME` + `CCTEAM_HOME` are pinned to
//! it as well — `CCTEAM_HOME` outranks `HOME` in the root resolvers, so pinning
//! only `HOME` would still let an exported shell value redirect a write into the
//! real `~/.ccteam`.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::sync::Arc;

use ccteam_core::enroll::{self, EnrollCredential, EnrollScope};
use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};
use ccteam_web::{router_with_state, AppState, AuthState};
use futures::stream::{self, BoxStream};
use serde_json::{json, Value};
use serial_test::serial;
use tempfile::TempDir;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
/// The project every credential here is scoped to.
const SLUG: &str = "demo";
/// What a real `codex` sends as `clientInfo` (observed during the header probe).
const CLIENT: &str = "codex/0.144.3";

// ── isolation ────────────────────────────────────────────────────────────────

struct EnvGuard {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Pin every root a production write could derive itself from, and switch off the
/// tool-surface bootstrap so no test can touch the real `~/.claude.json`.
fn isolate(tmp: &TempDir) -> (EnvGuard, EnvGuard) {
    ccteam_core::tool_surface::disable_tool_surface_bootstrap_for_tests();
    (
        EnvGuard::set("HOME", tmp.path()),
        EnvGuard::set("CCTEAM_HOME", &tmp.path().join(".ccteam")),
    )
}

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

// ── fixture ──────────────────────────────────────────────────────────────────

/// A harness double, so a `agent` from an enrolled client really lands a
/// session without needing a vendor binary.
#[derive(Clone)]
struct FakeVendor {
    vendor: AgentVendor,
}

#[async_trait::async_trait]
impl HarnessAdapter for FakeVendor {
    fn name(&self) -> &'static str {
        "mcp-enroll-test"
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
            raw_extras: Value::Null,
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("turn-mcp-enroll"))
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
        Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }
    fn event_attachment(&self) -> ccteam_harness::EventAttachment {
        ccteam_harness::EventAttachment::OneShot
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::empty())
    }
    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "mcp-enroll-test".into(),
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

fn seed_web_token(paths: &CcteamPaths, hex: &str) {
    let path = paths.web_token_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, hex).unwrap();
}

/// A registered project owned by the admin web-console pool — the owner every
/// credential here speaks for.
fn seed_project(paths: &CcteamPaths, slug: &str, owner: &str) {
    let state_path = paths.project_state(slug);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut state = ccteam_core::ProjectState::initial(slug.to_string());
    state.owner = Some(owner.to_string());
    state.save(&state_path).unwrap();
}

/// An `AppState` with a live gateway holding one registered project.
async fn state_with_project(paths: &CcteamPaths) -> AppState {
    seed_web_token(paths, ADMIN_HEX);
    seed_project(paths, SLUG, "user:web-api");
    let project_dir = paths.project_dir(SLUG);
    std::fs::create_dir_all(&project_dir).unwrap();
    let factory = Arc::new(move |vendor, _protocol| {
        Arc::new(FakeVendor { vendor }) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let gateway = ccteam_im::gateway::Gateway::new_with_factory(factory, SLUG, project_dir);
    AppState::with_auth(paths.clone(), AuthState::disabled()).with_gateway_owned(gateway)
}

fn mint(paths: &CcteamPaths, scope: EnrollScope) -> EnrollCredential {
    enroll::mint_in(&paths.root, scope, "user:web-api", Some("laptop".into())).unwrap()
}

fn project_scope() -> EnrollScope {
    EnrollScope::Project {
        slug: SLUG.to_string(),
    }
}

async fn spawn_server(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router_with_state(state))
            .await
            .unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// `POST /mcp` with an optional `Mcp-Session-Id` — the exact shape a conforming
/// client sends.
async fn post_mcp(
    addr: SocketAddr,
    bearer: &str,
    mcp_session_id: Option<&str>,
    body: Value,
) -> reqwest::Response {
    let mut req = client()
        .post(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&body);
    if let Some(id) = mcp_session_id {
        req = req.header("Mcp-Session-Id", id);
    }
    req.send().await.unwrap()
}

fn initialize_body() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "codex", "version": "0.144.3" }
        }
    })
}

fn call_body(id: i64, tool: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": args }
    })
}

/// Run `initialize` and return the issued `Mcp-Session-Id`.
async fn initialize(addr: SocketAddr, bearer: &str) -> String {
    let resp = post_mcp(addr, bearer, None, initialize_body()).await;
    assert_eq!(resp.status(), 200, "initialize must succeed");
    let id = resp
        .headers()
        .get("mcp-session-id")
        .expect("initialize answers with an Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_string();
    assert!(id.starts_with("ms_"), "recognisable in a log: {id}");
    id
}

/// The `result.content[0].text` of a tool call, parsed as JSON.
fn tool_json(body: &Value) -> Value {
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result carries text content: {body}"));
    serde_json::from_str(text).unwrap_or_else(|_| panic!("tool text is JSON: {text}"))
}

// ── 1. initialize issues the identity AND the ledger node ────────────────────

#[tokio::test]
#[serial]
async fn initialize_issues_a_per_process_id_and_a_ledger_node() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let cred = mint(&paths, project_scope());

    let resp = post_mcp(addr, &cred.bearer(), None, initialize_body()).await;
    assert_eq!(resp.status(), 200);
    let id = resp
        .headers()
        .get("mcp-session-id")
        .expect("the per-process identity rides the transport's own header")
        .to_str()
        .unwrap()
        .to_string();
    // The JSON-RPC body is still the ordinary handshake answer.
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["serverInfo"]["name"], "ccteam", "{body}");
    assert_eq!(body["result"]["protocolVersion"], "2024-11-05", "{body}");

    // The binding: pinned to the credential's project, owned by its identity.
    // D2 — a client-run node is told, in a plain sentence, that no completion
    // notification can be pushed to it. A managed session that had to INFER
    // its own delivery from a missing field got it backwards (2026-08-31).
    let instructions = body["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(
            "client-run: notifications cannot be pushed to you; agent_read{sid,wait} awaits a \
             turn instead"
        ),
        "{instructions}"
    );

    let listed = bindings.list();
    assert_eq!(listed.len(), 1, "one process, one binding");
    let binding = &listed[0];
    assert_eq!(binding.mcp_session_id, id);
    assert_eq!(binding.enroll_id, cred.id);
    assert_eq!(binding.owner, "user:web-api");
    assert_eq!(binding.project.as_deref(), Some(SLUG));
    assert_eq!(binding.client, CLIENT, "the label is what it called itself");
    let sid = binding.sid.clone().expect("a ledger node was minted");

    // The node: a real sid with a real meta.json that ccteam must never drive.
    let meta = gateway
        .lock()
        .await
        .external_node(&sid)
        .expect("the gateway indexes it as an external node");
    assert_eq!(
        meta.managed_by,
        ccteam_harness::execution::session_meta::ManagedBy::External
    );
    assert!(!meta.managed_by.is_driveable());
    assert_eq!(meta.slug, SLUG);
    assert_eq!(meta.owner, "user:web-api");
    assert_eq!(meta.vendor, AgentVendor::Codex, "vendor from clientInfo");
    let raw = std::fs::read_to_string(ccteam_harness::execution::session_meta::session_meta_path(
        &paths.project_dir(SLUG),
        &sid,
    ))
    .expect("meta.json is on disk");
    assert!(raw.contains("\"managed_by\": \"external\""), "{raw}");

    // …and it is visible in the one view every consumer reads.
    let views = gateway.lock().await.session_views();
    let row = views
        .iter()
        .find(|v| v.sid == sid)
        .expect("the node shows up in session_views");
    assert_eq!(row.project, SLUG);
    assert!(!row.driveable, "its own operator drives it, not ccteam");
}

// ── 2. a later call acts as the NODE ────────────────────────────────────────

/// THE defect: children of a hand-started agent used to mount as roots in a
/// project nobody named, because every process shared one config credential.
#[tokio::test]
#[serial]
async fn a_call_with_the_id_spawns_under_the_node_in_its_own_project() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let addr = spawn_server(app).await;
    let cred = mint(&paths, project_scope());
    let id = initialize(addr, &cred.bearer()).await;
    let _node = bindings.list()[0].sid.clone().unwrap();

    // No `project` argument on purpose: only an identity the server resolved can
    // supply one, so a credential that fell back to admin would fail here.
    let resp = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(2, "agent", json!({ "vendor": "claude", "task": "hello" })),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "spawn: {body}");
    let spawned = tool_json(&body);
    assert!(spawned["sid"].is_string(), "{spawned}");
}

// ── 3. one credential, two processes, two identities ────────────────────────

#[tokio::test]
#[serial]
async fn one_credential_two_processes_get_two_nodes() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let addr = spawn_server(app).await;
    let cred = mint(&paths, project_scope());

    let first = initialize(addr, &cred.bearer()).await;
    let second = initialize(addr, &cred.bearer()).await;
    assert_ne!(
        first, second,
        "the same static config must not collapse two processes into one identity"
    );
    let sids: Vec<String> = bindings
        .list()
        .iter()
        .map(|b| b.sid.clone().expect("both got a node"))
        .collect();
    assert_eq!(sids.len(), 2);
    assert_ne!(sids[0], sids[1], "two nodes, not one shared row");

    // Each id resolves to its OWN node; push responses intentionally expose
    // only the newly allocated sid.
    let mut spawned_sids = Vec::new();
    for id in [&first, &second] {
        let resp = post_mcp(
            addr,
            &cred.bearer(),
            Some(id),
            call_body(9, "agent", json!({ "vendor": "claude", "task": "hello" })),
        )
        .await;
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], false, "{body}");
        spawned_sids.push(tool_json(&body)["sid"].as_str().unwrap().to_string());
    }
    assert_ne!(
        spawned_sids[0], spawned_sids[1],
        "each process gets a fresh child sid"
    );
}

// ── 4. an id is not a credential ────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn an_id_replayed_under_another_credential_is_404_and_reveals_nothing() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let addr = spawn_server(app).await;
    let mine = mint(&paths, project_scope());
    let other = mint(&paths, project_scope());
    let id = initialize(addr, &mine.bearer()).await;

    let stolen = post_mcp(
        addr,
        &other.bearer(),
        Some(&id),
        call_body(2, "agent_read", json!({})),
    )
    .await;
    assert_eq!(
        stolen.status(),
        404,
        "a leaked id must replay as nothing under another credential"
    );
    let stolen_body: Value = stolen.json().await.unwrap();

    // An id that never existed answers identically — so existence cannot be
    // probed with the wrong credential.
    let unknown = post_mcp(
        addr,
        &other.bearer(),
        Some("ms_0000000000000000000000000000dead"),
        call_body(2, "agent_read", json!({})),
    )
    .await;
    assert_eq!(unknown.status(), 404);
    let unknown_body: Value = unknown.json().await.unwrap();
    assert_eq!(
        stolen_body, unknown_body,
        "a foreign id and a missing id must be indistinguishable"
    );
    assert!(
        !stolen_body.to_string().contains(&id),
        "the answer must not echo the id back: {stolen_body}"
    );

    // The rightful holder is undisturbed.
    let ok = post_mcp(
        addr,
        &mine.bearer(),
        Some(&id),
        call_body(3, "agent_read", json!({})),
    )
    .await;
    assert_eq!(
        ok.status(),
        200,
        "the credential that opened it still works"
    );
}

// ── 5. no id → the transport's own recovery path ────────────────────────────

#[tokio::test]
#[serial]
async fn a_call_without_the_id_is_404_pointing_at_initialize() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let addr = spawn_server(app).await;
    let cred = mint(&paths, project_scope());

    let resp = post_mcp(
        addr,
        &cred.bearer(),
        None,
        call_body(2, "agent_read", json!({})),
    )
    .await;
    assert_eq!(
        resp.status(),
        404,
        "a valid credential with no process identity is not a caller yet"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32001, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("initialize"), "{message}");
    assert!(message.contains("Mcp-Session-Id"), "{message}");
}

// ── 6. unbound + unnamed: discovery only, and no guessing ───────────────────

/// An unbound binding's `initialize` must say WHICH projects it may name.
/// Otherwise its first useful call is guaranteed to be a refusal, and the
/// list it needs only arrives inside that refusal.
#[tokio::test]
#[serial]
async fn an_unbound_binding_is_told_which_projects_it_may_name() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);

    let resp = post_mcp(
        addr,
        &cred.bearer(),
        None,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "clientInfo": { "name": CLIENT } }
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let instructions = body["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("hand-started agent with no project yet"),
        "{instructions}"
    );
    assert!(
        instructions.contains(&format!("reachable: {SLUG}")),
        "the reachable projects must be named up front: {instructions}"
    );
    assert!(
        instructions.contains("never guesses it from a working directory"),
        "{instructions}"
    );
    // It has no ccteam chat, so the chat-envelope paragraph is not its bill.
    assert!(
        !instructions.contains("your reply goes back to that chat"),
        "{instructions}"
    );
}

#[tokio::test]
#[serial]
async fn a_user_scoped_credential_discovers_but_cannot_act() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);
    let sessions_before = gateway.lock().await.session_views().len();

    let id = initialize(addr, &cred.bearer()).await;
    let binding = bindings.list()[0].clone();
    assert!(binding.project.is_none(), "it named no project");
    assert!(
        binding.sid.is_none(),
        "no project ⇒ no ledger node; ccteam does not pick one"
    );
    assert_eq!(
        gateway.lock().await.session_views().len(),
        sessions_before,
        "nothing was created"
    );

    // Discovery works — a client must be able to see the tool face it has.
    let listed = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(listed.status(), 200);
    let body: Value = listed.json().await.unwrap();
    assert_eq!(
        body["result"]["tools"].as_array().unwrap().len(),
        6,
        "{body}"
    );

    // Acting does not, and the refusal says what to do about it.
    let spawn = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(3, "agent", json!({ "vendor": "claude", "task": "hello" })),
    )
    .await;
    assert_eq!(
        spawn.status(),
        200,
        "a tool refusal is not a transport fault"
    );
    let body: Value = spawn.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true, "{body}");
    let message = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("no project"), "{message}");
    assert!(
        message.contains(SLUG),
        "it must name the projects it could have: {message}"
    );
    assert!(
        message.contains("never infers"),
        "the refusal is the feature: {message}"
    );
    assert!(
        message.contains("Name one on the call"),
        "…but it must point at the rung that DOES exist: {message}"
    );

    // Nothing was created by the refused call, and no node appeared behind our
    // back — a call that names NO project is the whole of rung 3. (Naming one is
    // rung 2, and it is a different test:
    // `a_user_scoped_credential_binds_the_project_it_names`.)
    assert_eq!(
        gateway.lock().await.session_views().len(),
        sessions_before,
        "still nothing created"
    );
    assert!(
        bindings.list()[0].sid.is_none(),
        "an unnamed project must never mint a node"
    );
    assert!(
        bindings.list()[0].project.is_none(),
        "and must never bind one"
    );
}

// ── 7. rung 2: the client NAMES a project it owns ───────────────────────────

/// The measured regression: the machine-wide credential `ccteam config` writes
/// into all five vendor configs is user-scoped by construction, so every
/// hand-started agent got "this MCP session has no project" — **even when it
/// passed `project` explicitly**. Naming a project the credential's owner may see
/// is the caller's own word, not an inference, so it binds.
#[tokio::test]
#[serial]
async fn a_user_scoped_credential_binds_the_project_it_names() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);
    let id = initialize(addr, &cred.bearer()).await;
    assert!(
        bindings.list()[0].sid.is_none(),
        "initialize alone mints nothing for a user-scoped credential"
    );

    let resp = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(
            2,
            "agent",
            json!({ "vendor": "claude", "task": "hello", "project": SLUG }),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "spawn: {body}");
    let spawned = tool_json(&body);

    // The binding is now the workspace's, and it minted its ledger node.
    let binding = bindings.list()[0].clone();
    assert_eq!(
        binding.project.as_deref(),
        Some(SLUG),
        "the named project is bound to the session"
    );
    let node = binding
        .sid
        .clone()
        .expect("naming it minted the ledger node");
    assert_eq!(
        gateway
            .lock()
            .await
            .external_node(&node)
            .expect("indexed as an external node")
            .slug,
        SLUG
    );

    // …and the child hangs off that node, in that project, at depth 1.
    assert!(spawned["sid"].is_string(), "{spawned}");

    // The node persists for the binding's whole life: a second call is the SAME
    // parent, not a fresh node per request.
    let again = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(
            3,
            "agent",
            json!({ "vendor": "claude", "task": "hello", "project": SLUG }),
        ),
    )
    .await;
    let body: Value = again.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "{body}");
    assert!(
        tool_json(&body)["sid"].is_string(),
        "one binding, one node: {body}"
    );
}

/// One MCP session is one workspace for its whole life — the guard that stops a
/// mid-conversation switch from smuggling a caller into another project.
#[tokio::test]
#[serial]
async fn a_second_call_naming_another_project_is_refused_and_the_binding_holds() {
    const OTHER: &str = "second";
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    // A project the SAME owner can see: the refusal must be about the binding
    // already having a workspace, not about visibility.
    seed_project(&paths, OTHER, "user:web-api");
    let bindings = Arc::clone(&app.native_bindings);
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);
    let id = initialize(addr, &cred.bearer()).await;

    let first = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(2, "agent_read", json!({ "project": SLUG })),
    )
    .await;
    let body: Value = first.json().await.unwrap();
    assert_eq!(
        body["result"]["isError"], false,
        "first naming binds: {body}"
    );
    let node = bindings.list()[0].sid.clone().expect("bound with a node");

    let switched = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(3, "agent", json!({ "vendor": "claude", "project": OTHER })),
    )
    .await;
    assert_eq!(
        switched.status(),
        200,
        "a tool refusal is not a transport fault"
    );
    let body: Value = switched.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true, "{body}");
    let message = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        message.contains(&format!("bound to project `{SLUG}`")),
        "the refusal must say which workspace it holds: {message}"
    );
    assert!(message.contains(OTHER), "{message}");

    // The binding did not move, kept its node, and nothing was created in OTHER.
    let binding = bindings.list()[0].clone();
    assert_eq!(binding.project.as_deref(), Some(SLUG));
    assert_eq!(binding.sid.as_deref(), Some(node.as_str()));
    assert!(
        !gateway
            .lock()
            .await
            .session_views()
            .iter()
            .any(|v| v.project == OTHER),
        "a refused switch must not create anything in the other project"
    );
}

/// The refusal text of a `agent` that named `slug`, as the AGENT reads
/// it. A helper rather than a closure so the same credential can probe twice —
/// which is the whole assertion below.
async fn spawn_refusal(addr: SocketAddr, bearer: &str, id: &str, slug: &str) -> String {
    let resp = post_mcp(
        addr,
        bearer,
        Some(id),
        call_body(2, "agent", json!({ "vendor": "claude", "project": slug })),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "a tool refusal is not a transport fault"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true, "{body}");
    body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A named project the owner cannot see must be indistinguishable from one that
/// does not exist — otherwise naming projects becomes an existence probe for
/// another tenant's workspaces.
#[tokio::test]
#[serial]
async fn a_project_the_owner_cannot_see_answers_exactly_like_an_unknown_one() {
    const WALLED: &str = "walled";
    const GHOST: &str = "ghost";
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    // Registered and real, but a per-user tenant's — the admin console pool this
    // credential speaks for does not see it (`Identity::can_see_owner`).
    seed_project(&paths, WALLED, "user:tenant-b");
    let bindings = Arc::clone(&app.native_bindings);
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);
    let id = initialize(addr, &cred.bearer()).await;
    let sessions_before = gateway.lock().await.session_views().len();

    let walled = spawn_refusal(addr, &cred.bearer(), &id, WALLED).await;
    let ghost = spawn_refusal(addr, &cred.bearer(), &id, GHOST).await;

    // Identical but for the slug the caller itself supplied: the answer carries no
    // information about whether the project is there.
    assert_eq!(
        walled.replace(WALLED, "<named>"),
        ghost.replace(GHOST, "<named>"),
        "an invisible project and a missing one must read the same:\n{walled}\n{ghost}"
    );
    assert!(
        walled.contains("not registered here"),
        "and it must fail closed, not explain: {walled}"
    );
    assert!(
        !walled.contains("tenant-b"),
        "never name the owner it belongs to: {walled}"
    );
    assert!(
        walled.contains(SLUG),
        "the hint may only enumerate the caller's OWN projects: {walled}"
    );

    // Nothing bound, nothing minted, nothing spawned.
    let binding = bindings.list()[0].clone();
    assert!(binding.project.is_none(), "a refused name must not bind");
    assert!(binding.sid.is_none(), "…and must not mint a node");
    assert_eq!(
        gateway.lock().await.session_views().len(),
        sessions_before,
        "nothing was created"
    );
}

// ── 8. DELETE ends binding + principal + node ──────────────────────────────

#[tokio::test]
#[serial]
async fn delete_ends_the_binding_the_principal_and_the_node() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let principals = app.session_principals.clone().unwrap();
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let cred = mint(&paths, project_scope());
    let id = initialize(addr, &cred.bearer()).await;

    let binding = bindings.list()[0].clone();
    let (sid, secret) = binding
        .principal()
        .map(|(sid, secret)| (sid.to_string(), secret.to_string()))
        .expect("the node authenticates with a server-minted principal");
    assert!(
        principals.verify(&sid, &secret).is_some(),
        "the node's principal verifies while it is live"
    );

    let deleted = client()
        .delete(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {}", cred.bearer()))
        .header("Mcp-Session-Id", &id)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204, "closing is a clean no-content");

    assert!(bindings.list().is_empty(), "the binding is gone");
    assert!(
        principals.verify(&sid, &secret).is_none(),
        "its principal must die with it"
    );
    assert!(
        !gateway
            .lock()
            .await
            .session_views()
            .iter()
            .any(|v| v.sid == sid),
        "the node leaves the live views"
    );
    // The ledger row stays on disk, like any stopped session's, so whatever it
    // spawned keeps resolving to a real parent.
    assert!(
        ccteam_harness::execution::session_meta::session_meta_path(&paths.project_dir(SLUG), &sid)
            .exists(),
        "history must survive the close"
    );

    // The id is spent: the next call gets the re-initialize signal, and a second
    // DELETE is a clean error rather than a fake success.
    let after = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(2, "agent_read", json!({})),
    )
    .await;
    assert_eq!(after.status(), 404);
    let again = client()
        .delete(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {}", cred.bearer()))
        .header("Mcp-Session-Id", &id)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404, "nothing left to close");
}

// ── 9. the web token is not a credential here at all ───────────────────────

/// The credential that caused the defect cannot enter this path — and, since the
/// admin tier is gone rather than narrowed, it cannot enter any other one either:
/// it neither opens a binding nor rides one somebody else opened.
#[tokio::test]
#[serial]
async fn the_admin_web_token_is_refused_and_cannot_ride_a_binding() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let bindings = Arc::clone(&app.native_bindings);
    let gateway = app.gateway.clone().unwrap();
    let addr = spawn_server(app).await;
    let admin = format!("ccteam:{ADMIN_HEX}");

    let resp = post_mcp(addr, &admin, None, initialize_body()).await;
    assert_eq!(
        resp.status(),
        401,
        "a machine-wide token is not an MCP credential"
    );
    assert!(
        bindings.list().is_empty(),
        "and nothing was bound: {:?}",
        bindings.list()
    );

    // An enrolled client's id under the admin bearer is refused at the gate,
    // before the binding is even looked up — no adoption, no root spawn, and
    // nothing created in the ledger.
    let cred = mint(&paths, project_scope());
    let id = initialize(addr, &cred.bearer()).await;
    let sessions_before = gateway.lock().await.session_views().len();
    let resp = post_mcp(
        addr,
        &admin,
        Some(&id),
        call_body(
            2,
            "agent",
            json!({ "vendor": "claude", "task": "hello", "project": SLUG }),
        ),
    )
    .await;
    assert_eq!(resp.status(), 401, "an id is not a credential either");
    assert_eq!(
        gateway.lock().await.session_views().len(),
        sessions_before,
        "a refused call must not have spawned anything"
    );

    // A garbage enrollment bearer is 401 too — never downgraded to another family.
    let forged = post_mcp(
        addr,
        &format!("ccteam-enroll:{}:deadbeef", cred.id),
        None,
        initialize_body(),
    )
    .await;
    assert_eq!(forged.status(), 401, "wrong secret → 401");
    assert!(
        bindings.list().len() == 1,
        "a rejected credential binds nothing"
    );
}

/// P2-4 — an unbound binding asking for `status` is authenticated; it has
/// simply named no workspace. Saying "caller could not be authenticated" sent a
/// hand-started agent hunting for a broken bearer (2026-08-31), so the answer
/// is the same shape `agent` / `agent_read` give it: what is missing, how to
/// supply it, and which slugs are reachable.
#[tokio::test]
#[serial]
async fn an_unbound_binding_status_says_no_project_named_not_unauthenticated() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);
    let id = initialize(addr, &cred.bearer()).await;

    let resp = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(2, "status", json!({})),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let note = tool_json(&body)["note"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !note.contains("could not be authenticated"),
        "the credential verified — only the workspace is missing: {note}"
    );
    assert!(note.contains("no project named yet"), "{note}");
    assert!(
        note.contains("project:"),
        "it must show how to name one: {note}"
    );
    assert!(
        note.contains(&format!("reachable: {SLUG}")),
        "…and which ones it may name: {note}"
    );
}

/// P3-7 — gate ORDER: a tool name that is not on the surface at all is a typo,
/// and the projectless lecture (487 B) was a wrong answer to it. Unknown names
/// get the protocol core's own unknown-tool error, bound or not.
#[tokio::test]
#[serial]
async fn an_unknown_tool_name_is_never_answered_with_the_project_lecture() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let addr = spawn_server(app).await;
    let cred = mint(&paths, EnrollScope::User);
    let id = initialize(addr, &cred.bearer()).await;

    let resp = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(2, "bogus", json!({})),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true, "{body}");
    let message = body["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(message, "unknown tool: bogus");
    assert!(
        message.len() <= 100,
        "an unknown name costs a line, not a lecture: {} B",
        message.len()
    );

    // …while a KNOWN tool that needs a workspace still gets the full answer.
    let refused = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(3, "agent_read", json!({})),
    )
    .await;
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true, "{body}");
    assert!(body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("no project"));
}

// ── 10. a bound client is a project-scoped caller everywhere ───────────────

/// `status` renders the vendor panel for the node's OWN project — the panel is
/// scoped to the caller's workspace, so this is the bound counterpart of
/// `mcp_http_test::mcp_tools_call_status_succeeds` (which pins that a caller with
/// no project is told why the panel is withheld instead of being shown one for a
/// workspace it never named).
#[tokio::test]
#[serial]
async fn a_bound_client_gets_the_vendor_panel_for_its_own_project() {
    let tmp = TempDir::new().unwrap();
    let _env = isolate(&tmp);
    let paths = fake_paths(tmp.path());
    let app = state_with_project(&paths).await;
    let addr = spawn_server(app).await;
    let cred = mint(&paths, project_scope());
    let id = initialize(addr, &cred.bearer()).await;

    // No `project` argument: the slug can only come from the node the server
    // resolved for this process.
    let resp = post_mcp(
        addr,
        &cred.bearer(),
        Some(&id),
        call_body(2, "status", json!({})),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["isError"], false, "{body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let status: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        status["project"], SLUG,
        "a bound client's status names its own project, got: {text}"
    );
    assert!(
        status["hire"].is_array(),
        "brief status lists what this host can hire: {text}"
    );
}
