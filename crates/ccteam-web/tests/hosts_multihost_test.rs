//! v0.8.24 Track D (reverse-connection since v0.9.0) — multi-host join /
//! control channel / ACL / remote-spawn offline.
//!
//! Proves:
//! 1. mint join-token (admin) → join registers a host
//! 2. the reverse `ccteam-host.v1` control channel registers in the hub;
//!    `report` frames keep the registry fresh; stale last_heartbeat → offline
//! 3. remote spawn against offline host returns a readable error and does
//!    **not** create a session
//! 4. non-admin tenant is 403 on hosts list / join-token / join; a
//!    non-satellite bearer is rejected on the channel/dial-back endpoints

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ccteam_core::host_registry::{now_unix, HostRecord, HostRegistry, DEFAULT_HEARTBEAT_TTL_SECS};
use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, HarnessAdapter, HarnessError,
    PermissionMode, SessionProtocol, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId,
    TurnInput,
};
use ccteam_im::gateway::Gateway;
use ccteam_im::remote_host::{FakeRemoteHostProxy, RemoteHostProxy};
use ccteam_web::{router_with_state, AppState, AuthState};
use futures::stream::{self, BoxStream};
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn join_registers_host_and_heartbeat_online_offline() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(paths.secrets_dir()).unwrap();
    std::fs::create_dir_all(paths.hosts_dir()).unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let hub = state.host_hub.clone();
    let addr = spawn(state).await;
    let c = client();
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");

    // GET before any mint → token: null (read-only, never mints).
    let empty: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/hosts/join-token"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty["token"].is_null(), "no token minted yet: {empty}");

    // Mint join token (admin).
    let mint: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/hosts/join-token"))
        .header("Authorization", &auth)
        .json(&serde_json::json!({"label": "lab", "max_uses": 2}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let join_tok = mint["token"].as_str().unwrap().to_string();
    assert!(mint["command"]
        .as_str()
        .unwrap()
        .contains("ccteam host join"));

    // GET after mint → the newest valid token round-trips (the SPA join
    // card composes the full command from this).
    let read: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/hosts/join-token"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read["token"].as_str(), Some(join_tok.as_str()));
    assert!(read["command"].as_str().unwrap().contains(&join_tok));

    // Join with join-token bearer (not admin).
    let join: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/hosts/join"))
        .header("Authorization", format!("Bearer ccteam:{join_tok}"))
        .json(&serde_json::json!({
            "token": join_tok,
            "host_id": "lab-mac",
            "hostname": "lab-mac.local",
            "os": "linux",
            "arch": "x86_64",
            "ccteam_version": "0.8.24",
            "agents": [{"vendor":"claude","installed":true,"version":"1.0","status":"ready"}]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(join["host"], "lab-mac");
    let agent_token = join["agent_token"].as_str().unwrap().to_string();

    // List hosts (admin): local + lab-mac online.
    let list: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/hosts"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let hosts = list["hosts"].as_array().unwrap();
    assert!(hosts.iter().any(|h| h["host"] == "local"));
    let sat = hosts
        .iter()
        .find(|h| h["host"] == "lab-mac")
        .expect("satellite listed");
    assert_eq!(sat["status"], "online");
    assert_eq!(sat["is_local"], false);

    // Reverse control channel (replaces the retired HTTP heartbeat): the
    // agent-token bearer upgrades `GET /api/v1/hosts/channel`; a `report`
    // frame refreshes the registry.
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let mut ws_req = format!("ws://{addr}/api/v1/hosts/channel")
        .into_client_request()
        .unwrap();
    ws_req.headers_mut().insert(
        "Authorization",
        format!("Bearer ccteam:{agent_token}").parse().unwrap(),
    );
    ws_req
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ccteam-host.v1".parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_req).await.unwrap();
    // Hub registration is synchronous with the upgrade completing.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !hub.is_connected("lab-mac") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "control channel never registered in the hub"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    ws.send(Message::Text(
        serde_json::json!({"op":"report","ccteam_version":"0.9.0-report"}).to_string(),
    ))
    .await
    .unwrap();
    // The report lands in the persisted registry (version proves it was
    // THIS frame, not the on-connect presence bump).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let reg = HostRegistry::load(&paths.host_registry_path()).unwrap();
        if reg
            .get("lab-mac")
            .map(|h| h.ccteam_version == "0.9.0-report")
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "report frame never applied to the registry"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    // Unknown ops are ignored (forward compat) — the channel must survive.
    ws.send(Message::Text(
        serde_json::json!({"op":"from-the-future","x":1}).to_string(),
    ))
    .await
    .unwrap();
    // An admin bearer is NOT a satellite: channel upgrade is refused.
    let mut admin_req = format!("ws://{addr}/api/v1/hosts/channel")
        .into_client_request()
        .unwrap();
    admin_req
        .headers_mut()
        .insert("Authorization", auth.parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(admin_req).await.is_err(),
        "admin bearer must not open a host channel"
    );
    // An unknown exec nonce is refused before upgrade.
    let mut nonce_req = format!("ws://{addr}/api/v1/hosts/exec/deadbeef")
        .into_client_request()
        .unwrap();
    nonce_req.headers_mut().insert(
        "Authorization",
        format!("Bearer ccteam:{agent_token}").parse().unwrap(),
    );
    assert!(
        tokio_tungstenite::connect_async(nonce_req).await.is_err(),
        "unknown nonce must not upgrade"
    );
    // Channel teardown unregisters from the hub.
    drop(ws);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while hub.is_connected("lab-mac") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "dropped channel never unregistered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // Force offline by rewriting last_heartbeat.
    let mut reg = HostRegistry::load(&paths.host_registry_path()).unwrap();
    {
        let h = reg.get_mut("lab-mac").unwrap();
        h.last_heartbeat_unix = now_unix().saturating_sub(DEFAULT_HEARTBEAT_TTL_SECS + 30);
    }
    reg.save(&paths.host_registry_path()).unwrap();

    let list2: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/hosts"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sat2 = list2["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "lab-mac")
        .unwrap();
    assert_eq!(sat2["status"], "offline");
    // Host record still present (not deleted).
    assert!(HostRegistry::load(&paths.host_registry_path())
        .unwrap()
        .get("lab-mac")
        .is_some());
}

#[tokio::test]
async fn tenant_can_manage_hosts_tokens_but_cannot_join_as_a_host() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(paths.users_dir()).unwrap();

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let tauth = format!("Bearer ccteam:{tenant_tok}");

    // Host discovery + join-token management are shared operational surfaces.
    for path in ["/api/v1/hosts", "/api/v1/hosts/join-token"] {
        let r = if path.ends_with("join-token") {
            c.post(format!("http://{addr}{path}"))
                .header("Authorization", &tauth)
                .json(&serde_json::json!({"label": "x"}))
                .send()
                .await
                .unwrap()
        } else {
            c.get(format!("http://{addr}{path}"))
                .header("Authorization", &tauth)
                .send()
                .await
                .unwrap()
        };
        let expected = if path.ends_with("join-token") {
            201
        } else {
            200
        };
        assert_eq!(r.status(), expected, "tenant reaches {path}");
    }
    // The token just minted is readable by the same tenant.
    let r = c
        .get(format!("http://{addr}/api/v1/hosts/join-token"))
        .header("Authorization", &tauth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "tenant reaches GET join-token");
    // Join: tenant is neither admin nor join-token identity → 403 (after a
    // well-formed body so the extractor does not 422 first).
    let r = c
        .post(format!("http://{addr}/api/v1/hosts/join"))
        .header("Authorization", &tauth)
        .json(&serde_json::json!({
            "token": "not-a-real-join-token",
            "hostname": "evil",
            "os": "linux",
            "arch": "x86_64",
            "ccteam_version": "0"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant must be 403 on join");
}

#[tokio::test]
async fn remote_spawn_offline_error_does_not_create_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.hosts_dir()).unwrap();

    let mut reg = HostRegistry::default();
    reg.upsert(HostRecord {
        id: "dead-sat".into(),
        hostname: "dead-sat".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ccteam_version: "0.8.24".into(),
        agent_token: "agenttok".into(),
        last_heartbeat_unix: now_unix().saturating_sub(10_000),
        agents: vec![],
        projects: vec![ccteam_core::HostProjectReport {
            slug: "demo".into(),
            path: "/home/sat/projects/demo".into(),
        }],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    reg.save(&paths.host_registry_path()).unwrap();
    let fake = Arc::new(FakeRemoteHostProxy::default());
    let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
    let err = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
        "demo",
        SessionProtocol::StreamJson,
        Some(&proxy),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("offline"),
        "expected offline error, got: {msg}"
    );
    // Proxy never called.
    assert!(fake.last_host.lock().unwrap().is_none());
    // Registry host still present (not deleted).
    assert!(HostRegistry::load(&paths.host_registry_path())
        .unwrap()
        .get("dead-sat")
        .is_some());

    // Bring online → fake proxy succeeds.
    {
        let mut reg = HostRegistry::load(&paths.host_registry_path()).unwrap();
        reg.get_mut("dead-sat").unwrap().last_heartbeat_unix = now_unix();
        reg.save(&paths.host_registry_path()).unwrap();
    }
    let target = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
        "demo",
        SessionProtocol::StreamJson,
        Some(&proxy),
    )
    .await
    .unwrap();
    assert_eq!(target.host, "dead-sat");
    assert!(target.remote.is_some());
    assert_eq!(fake.last_host.lock().unwrap().as_deref(), Some("dead-sat"));

    // Terminal on remote is rejected even when online.
    let err = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
        "demo",
        SessionProtocol::Terminal,
        Some(&proxy),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("terminal"));

    // A slug the satellite never registered is rejected even when online.
    let err = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
        "not-registered-there",
        SessionProtocol::StreamJson,
        Some(&proxy),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("not registered"), "got: {err}");
}

/// End-to-end: gateway create on offline host fails; session map stays empty.
#[tokio::test]
async fn gateway_create_on_offline_host_fails_clean() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.hosts_dir()).unwrap();
    let project_dir = paths.projects_root.join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut reg = HostRegistry::default();
    reg.upsert(HostRecord {
        id: "sat1".into(),
        hostname: "sat1".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ccteam_version: "0.8.24".into(),
        agent_token: "t".into(),
        last_heartbeat_unix: now_unix().saturating_sub(9999),
        agents: vec![],
        projects: vec![],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    reg.save(&paths.host_registry_path()).unwrap();
    ccteam_core::config::upsert_project(
        &paths.root,
        ccteam_core::ProjectEntry {
            slug: "alpha".into(),
            path: project_dir.clone(),
            host: "sat1".into(),
            remote_slug: Some("alpha".into()),
            remote_path: Some(project_dir.clone()),
            team: "dev".into(),
            installed_at: chrono::Utc::now(),
        },
    )
    .unwrap();

    struct Stub;
    #[async_trait]
    impl HarnessAdapter for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn vendor(&self) -> AgentVendor {
            AgentVendor::Claude
        }
        async fn start_thread(
            &self,
            _spec: &AgentSpecBrief,
            _ctx: &SpawnCtx,
        ) -> Result<ThreadHandle, HarnessError> {
            Err(HarnessError::SpawnFailed("should not spawn".into()))
        }
        async fn submit_turn(
            &self,
            _h: &ThreadHandle,
            _input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            unreachable!()
        }
        async fn submit_turn_routed(
            &self,
            _h: &ThreadHandle,
            _input: TurnInput,
            _routing: ccteam_harness::TurnRouting,
        ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
            unreachable!()
        }
        fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
            Box::pin(stream::empty())
        }
        async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
            unreachable!()
        }
        async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
            Ok(())
        }
        async fn handle_directive(
            &self,
            _h: &ThreadHandle,
            _d: Directive,
        ) -> Result<DirectiveOutcome, HarnessError> {
            unreachable!()
        }
        async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
            unreachable!()
        }
    }

    let adapter: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(Stub);
    let mut gw = Gateway::new(adapter, "alpha", project_dir);
    gw.enable_project_creation(paths.clone());
    let fake = Arc::new(FakeRemoteHostProxy::default());
    let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
    gw.set_remote_host_proxy(proxy);

    let err = gw
        .create_session_api_tuned(
            "alpha".into(),
            "".into(),
            AgentVendor::Claude,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".into(),
            ccteam_im::gateway::SpawnTuning::default(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("offline"), "got: {err}");
    // No live sessions.
    assert!(gw.session_views().is_empty());
}

/// TEAM-5 — `DELETE /api/v1/hosts/{host}` deregisters an offline satellite
/// (drops its registry record) and refuses `local` / an unknown host.
#[tokio::test]
async fn remove_deregisters_offline_host_and_rejects_local_and_unknown() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.hosts_dir()).unwrap();

    let mut reg = HostRegistry::default();
    reg.upsert(HostRecord {
        id: "stale-sat".into(),
        hostname: "stale-sat".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ccteam_version: "0.9.11".into(),
        agent_token: "t".into(),
        last_heartbeat_unix: now_unix().saturating_sub(DEFAULT_HEARTBEAT_TTL_SECS + 30),
        agents: vec![],
        projects: vec![],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    reg.save(&paths.host_registry_path()).unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");

    // Unknown host → 404.
    let r = c
        .delete(format!("http://{addr}/api/v1/hosts/never-joined"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "unknown host must 404");

    // `local` can never be removed → 400.
    let r = c
        .delete(format!("http://{addr}/api/v1/hosts/local"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "local must 400");

    // Offline satellite → 200, drops the registry record.
    let r = c
        .delete(format!("http://{addr}/api/v1/hosts/stale-sat"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "offline host removal must succeed");
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["host"], "stale-sat");
    assert!(HostRegistry::load(&paths.host_registry_path())
        .unwrap()
        .get("stale-sat")
        .is_none());

    // The follow-up list no longer carries it.
    let list: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/hosts"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!list["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["host"] == "stale-sat"));

    // Removing it again is now unknown → 404 (idempotent-shaped, not a crash).
    let r = c
        .delete(format!("http://{addr}/api/v1/hosts/stale-sat"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

/// TEAM-5 — an online (heartbeating) host is refused without `?force=true`
/// (409), and removable with it.
#[tokio::test]
async fn remove_online_host_requires_force() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(paths.hosts_dir()).unwrap();

    let mut reg = HostRegistry::default();
    reg.upsert(HostRecord {
        id: "live-sat".into(),
        hostname: "live-sat".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ccteam_version: "0.9.11".into(),
        agent_token: "t".into(),
        last_heartbeat_unix: now_unix(),
        agents: vec![],
        projects: vec![],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    reg.save(&paths.host_registry_path()).unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");

    // Online, no force → 409, record survives.
    let r = c
        .delete(format!("http://{addr}/api/v1/hosts/live-sat"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409, "online host without force must 409");
    assert!(HostRegistry::load(&paths.host_registry_path())
        .unwrap()
        .get("live-sat")
        .is_some());

    // Online, ?force=true → 200, record dropped.
    let r = c
        .delete(format!("http://{addr}/api/v1/hosts/live-sat?force=true"))
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "force must remove a live satellite");
    assert!(HostRegistry::load(&paths.host_registry_path())
        .unwrap()
        .get("live-sat")
        .is_none());
}
