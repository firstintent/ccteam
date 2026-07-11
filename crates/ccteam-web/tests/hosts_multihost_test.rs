//! v0.8.24 Track D — multi-host join / heartbeat / ACL / remote-spawn offline.
//!
//! Proves:
//! 1. mint join-token (admin) → join registers a host
//! 2. heartbeat keeps the host online; stale last_heartbeat → offline
//! 3. remote spawn against offline host returns a readable error and does
//!    **not** create a session
//! 4. non-admin tenant is 403 on hosts list / join-token / join

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
    let addr = spawn(state).await;
    let c = client();
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");

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
            "agent_url": "http://127.0.0.1:9",
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

    // Heartbeat with agent token.
    let hb: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/hosts/lab-mac/heartbeat"))
        .header("Authorization", format!("Bearer ccteam:{agent_token}"))
        .json(&serde_json::json!({"agent_token": agent_token, "ccteam_version": "0.8.24"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hb["status"], "online");

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
async fn non_admin_403_on_hosts_and_join() {
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

    // List + mint are hard admin-only (deny_non_admin).
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
        assert_eq!(r.status(), 403, "tenant must be 403 on {path}");
    }
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
        agent_url: Some("http://127.0.0.1:9".into()),
        agent_token: "agenttok".into(),
        last_heartbeat_unix: now_unix().saturating_sub(10_000),
        agents: vec![],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    reg.save(&paths.host_registry_path()).unwrap();

    let fake = Arc::new(FakeRemoteHostProxy::default());
    let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
    let err = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
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
    let host = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
        SessionProtocol::StreamJson,
        Some(&proxy),
    )
    .await
    .unwrap();
    assert_eq!(host, "dead-sat");
    assert_eq!(fake.last_host.lock().unwrap().as_deref(), Some("dead-sat"));

    // Terminal on remote is rejected even when online.
    let err = ccteam_im::remote_host::prepare_host_for_spawn(
        Some(&paths.root),
        "dead-sat",
        SessionProtocol::Terminal,
        Some(&proxy),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("terminal"));
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
        agent_url: Some("http://127.0.0.1:9".into()),
        agent_token: "t".into(),
        last_heartbeat_unix: now_unix().saturating_sub(9999),
        agents: vec![],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    reg.save(&paths.host_registry_path()).unwrap();

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
        .create_session_api_on_host(
            "alpha".into(),
            "".into(),
            AgentVendor::Claude,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".into(),
            "sat1".into(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("offline"), "got: {err}");
    // No live sessions.
    assert!(gw.session_views().is_empty());
}
