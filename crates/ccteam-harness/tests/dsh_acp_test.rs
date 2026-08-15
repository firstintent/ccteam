//! DSH ACP adapter tests against the hermetic fake
//! (`fixtures/dsh_acp/fake_dsh_acp.py`).
//!
//! Gate: no real dsh / network. Set `CCTEAM_DSH_BIN` to the fake wrapper.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ccteam_harness::execution::claude_common::CHAT_SID_ENV;
use ccteam_harness::execution::dsh_acp::handshake::{DEFAULT_DSH_MODEL, DEFAULT_DSH_PROVIDER};
use ccteam_harness::execution::dsh_acp::spawn_spec::{
    build_spawn_spec, build_web_spawn_spec, dsh_bin, DshWebSpawnOptions, DEEPSEEK_API_KEY_ENV,
    DEEPSEEK_BASE_URL_ENV, DSH_APPROVAL_ENV, DSH_HOME_ENV, DSH_PROFILE, DSH_SYSTEM_PROMPT_ENV,
    DSH_TELEMETRY_DISABLED_ENV, DSH_TELEMETRY_MODE_ENV, DSH_TRANSPORT_ENV, DSH_WEB_PROFILE,
};
use ccteam_harness::execution::mcp_config::{
    SessionMcpEndpoint, BRIDGE_MCP_BEARER_ENV, BRIDGE_MCP_URL_ENV,
};
use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, DshAcpAdapter, ExecutionMode, HarnessAdapter,
    HarnessError, PermissionMode, SessionMeta, SessionOrigin, SessionProtocol, SpawnCtx,
    ThreadEvent, ThreadItemDetails, TurnInput, DSH_BIN_ENV,
};
use futures::StreamExt;
use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;

const ENV_KEYS: &[&str] = &[
    DSH_BIN_ENV,
    "HOME",
    "CCTEAM_HOME",
    "CCTEAM_WEB_URL",
    BRIDGE_MCP_URL_ENV,
    "CCTEAM_DSH_ACP_DUMP",
    "CCTEAM_DSH_ENV_DUMP",
    "CCTEAM_DSH_LOAD_FAIL",
    "CCTEAM_DSH_AGENT_NAME",
    DEEPSEEK_API_KEY_ENV,
    DEEPSEEK_BASE_URL_ENV,
    DSH_SYSTEM_PROMPT_ENV,
];

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn capture() -> Self {
        Self {
            saved: ENV_KEYS
                .iter()
                .copied()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.iter().rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dsh_acp/fake_dsh_acp.py")
}

fn install_fake(tmp: &TempDir) -> EnvGuard {
    let guard = EnvGuard::capture();
    let bin = fake_bin();
    assert!(bin.is_file(), "missing fake at {}", bin.display());
    let wrapper = tmp.path().join("fake-dsh");
    let body = format!("#!/bin/sh\nexec python3 {} \"$@\"\n", bin.display());
    std::fs::write(&wrapper, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&wrapper, perms).unwrap();
    }
    let home = tmp.path().join("home");
    let ccteam_home = tmp.path().join(".ccteam-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ccteam_home).unwrap();
    unsafe {
        std::env::set_var(DSH_BIN_ENV, &wrapper);
        std::env::set_var("HOME", &home);
        std::env::set_var("CCTEAM_HOME", &ccteam_home);
        std::env::set_var(BRIDGE_MCP_URL_ENV, "http://127.0.0.1:65535/mcp");
        std::env::remove_var("CCTEAM_WEB_URL");
        std::env::set_var(DEEPSEEK_API_KEY_ENV, "test-deepseek-key");
        std::env::remove_var(DEEPSEEK_BASE_URL_ENV);
        std::env::remove_var(DSH_SYSTEM_PROMPT_ENV);
        std::env::remove_var("CCTEAM_DSH_ACP_DUMP");
        std::env::remove_var("CCTEAM_DSH_ENV_DUMP");
        std::env::remove_var("CCTEAM_DSH_LOAD_FAIL");
        std::env::remove_var("CCTEAM_DSH_AGENT_NAME");
    }
    guard
}

fn spawn_ctx(tmp: &TempDir, sid: &str) -> SpawnCtx {
    SpawnCtx {
        slug: "demo".into(),
        sid: sid.into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: PermissionMode::Skip,
        secret: "seKret1234".into(),
        remote: None,
    }
}

fn write_meta(project: &Path, sid: &str, vendor_uuid: &str) {
    let meta = SessionMeta {
        managed_by: Default::default(),
        sid: sid.into(),
        slug: "demo".into(),
        vendor: AgentVendor::Dsh,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: vendor_uuid.into(),
        model: None,
        observed_model: None,
        effort: None,
        host: "local".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        last_active: chrono::Utc::now().to_rfc3339(),
        origin: SessionOrigin::Ccteam,
        title: None,
        title_source: None,
        turn_count: 1,
        cost_usd: None,
        tokens_total: None,
        role_sha: None,
        skills_sha: None,
        trigger: None,
        parent_sid: None,
        spawned_by_role: None,
        delegation_depth: 0,
    };
    write_session_meta(project, &meta).unwrap();
}

fn set_dump(path: &Path) {
    unsafe {
        std::env::set_var("CCTEAM_DSH_ACP_DUMP", path);
    }
}

fn read_dump(path: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
#[serial(dsh_env)]
fn default_bin_is_dsh_absent_override() {
    let _guard = EnvGuard::capture();
    unsafe {
        std::env::remove_var(DSH_BIN_ENV);
    }
    assert_eq!(dsh_bin(), "dsh");
}

#[test]
#[serial(dsh_env)]
fn env_override_wins() {
    let _guard = EnvGuard::capture();
    unsafe {
        std::env::set_var(DSH_BIN_ENV, "/opt/dsh/bin/dsh");
    }
    assert_eq!(dsh_bin(), "/opt/dsh/bin/dsh");
}

#[test]
#[serial(dsh_env)]
fn spawn_spec_env_table_is_bridge_only_and_scrubbed() {
    let tmp = TempDir::new().unwrap();
    let _guard = install_fake(&tmp);
    let ctx = spawn_ctx(&tmp, "s-env");
    let mcp = SessionMcpEndpoint::at("http://127.0.0.1:7331/mcp", &ctx.sid, &ctx.secret).unwrap();

    let spec = build_spawn_spec(&ctx, &mcp).expect("spawn spec");
    assert_eq!(
        spec.args,
        vec!["--profile".to_string(), DSH_PROFILE.to_string()]
    );
    assert_eq!(spec.cwd, tmp.path());
    assert!(
        !spec
            .args
            .iter()
            .any(|arg| arg.contains("system-prompt") || arg.contains("append-system")),
        "DSH argv must never carry a system prompt flag: {:?}",
        spec.args
    );
    let basename = Path::new(&spec.bin)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    assert_ne!(basename, "deepseek-harness-acp");
    assert_ne!(basename, "dsh-acp-demo");

    let env: BTreeMap<_, _> = spec.env.iter().cloned().collect();
    let keys: Vec<_> = env.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec![
            CHAT_SID_ENV,
            DSH_APPROVAL_ENV,
            DSH_TRANSPORT_ENV,
            BRIDGE_MCP_BEARER_ENV,
            BRIDGE_MCP_URL_ENV,
            DEEPSEEK_API_KEY_ENV,
            DSH_HOME_ENV,
            DSH_TELEMETRY_DISABLED_ENV,
            DSH_TELEMETRY_MODE_ENV,
        ]
    );
    assert_eq!(env[CHAT_SID_ENV], "s-env");
    assert_eq!(env[DSH_TRANSPORT_ENV], "1");
    assert_eq!(env[DSH_APPROVAL_ENV], "skip");
    assert_eq!(env[DSH_TELEMETRY_DISABLED_ENV], "1");
    assert_eq!(env[DSH_TELEMETRY_MODE_ENV], "DISABLED");
    assert_eq!(env[DEEPSEEK_API_KEY_ENV], "test-deepseek-key");
    assert_eq!(env[BRIDGE_MCP_URL_ENV], "http://127.0.0.1:7331/mcp");
    assert_eq!(env[BRIDGE_MCP_BEARER_ENV], "ccteam-sid:s-env:seKret1234");
    assert_eq!(
        env[DSH_HOME_ENV],
        tmp.path()
            .join(".ccteam-home/runtime/dsh/s-env")
            .to_string_lossy()
    );
    assert!(!env.contains_key(DSH_SYSTEM_PROMPT_ENV));
    assert!(!env.contains_key(DEEPSEEK_BASE_URL_ENV));
}

#[test]
#[serial(dsh_env)]
fn web_spawn_spec_uses_ccteam_web_profile_and_scrubs_tenant_provider_env() {
    let tmp = TempDir::new().unwrap();
    let _guard = install_fake(&tmp);
    let dsh_home = tmp.path().join(".ccteam-home/runtime/dsh/web/alice");

    let spec = build_web_spawn_spec(DshWebSpawnOptions {
        dsh_home: dsh_home.clone(),
        profile: DSH_WEB_PROFILE,
        materialize_profile: true,
        enrollment: Some("ccteam-enroll:abc:secret"),
        daemon_url: Some("http://127.0.0.1:7331"),
        scrub_provider_env: true,
    })
    .expect("web spawn spec");

    assert_eq!(
        spec.args,
        vec![
            "--profile".to_string(),
            DSH_WEB_PROFILE.to_string(),
            "--port".to_string(),
            "0".to_string()
        ]
    );
    assert_eq!(spec.cwd, dsh_home);
    assert_eq!(spec.dsh_home, dsh_home);
    assert!(spec.env_remove.contains(&DEEPSEEK_API_KEY_ENV.to_string()));
    assert!(spec.env_remove.contains(&DEEPSEEK_BASE_URL_ENV.to_string()));
    let env: BTreeMap<_, _> = spec.env.iter().cloned().collect();
    assert_eq!(env[DSH_HOME_ENV], dsh_home.to_string_lossy());
    assert!(!env.contains_key(DEEPSEEK_API_KEY_ENV));
    assert!(dsh_home
        .join("profiles")
        .join(DSH_WEB_PROFILE)
        .join("package.json")
        .is_file());
}

#[tokio::test]
#[serial(dsh_env)]
async fn session_new_never_sends_acp_mcp_servers_even_with_secret() {
    let tmp = TempDir::new().unwrap();
    let _guard = install_fake(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let env_dump = tmp.path().join("dsh_env_dump.json");
    set_dump(&dump);
    unsafe {
        std::env::set_var("CCTEAM_DSH_ENV_DUMP", &env_dump);
        std::env::set_var(DSH_SYSTEM_PROMPT_ENV, "must-not-reach-child");
    }

    let adapter = DshAcpAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-new"),
        ),
    )
    .await
    .expect("start timeout")
    .expect("start ok");

    assert_eq!(handle.vendor, AgentVendor::Dsh);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    assert!(!handle.identity.is_empty());

    let records = read_dump(&dump);
    let new = records
        .iter()
        .find(|v| v["method"] == "session/new")
        .unwrap_or_else(|| panic!("expected session/new dump, got {records:?}"));
    let params = &new["params"];
    assert!(
        params
            .get("mcpServers")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "DSH must not receive ACP mcpServers: {params}"
    );
    assert_eq!(
        params
            .pointer("/agentOptions/provider")
            .and_then(Value::as_str),
        Some(DEFAULT_DSH_PROVIDER)
    );
    assert_eq!(
        params
            .pointer("/agentOptions/model")
            .and_then(Value::as_str),
        Some(DEFAULT_DSH_MODEL)
    );
    assert!(
        !serde_json::to_string(params)
            .unwrap()
            .contains("ccteam-sid:"),
        "bearer must stay in child env, not ACP params: {params}"
    );
    let child_env: Value =
        serde_json::from_str(&std::fs::read_to_string(&env_dump).expect("env dump")).unwrap();
    assert_eq!(
        child_env["argv"],
        serde_json::json!(["--profile", DSH_PROFILE])
    );
    assert!(
        child_env
            .pointer("/env/DSH_SYSTEM_PROMPT")
            .and_then(Value::as_str)
            .is_none(),
        "DSH_SYSTEM_PROMPT must be scrubbed from inherited child env: {child_env}"
    );
    assert_eq!(
        child_env
            .pointer("/env/CCTEAM_MCP_BEARER")
            .and_then(Value::as_str),
        Some("ccteam-sid:s-new:seKret1234")
    );

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn meta_vendor_uuid_loads_before_new() {
    let tmp = TempDir::new().unwrap();
    let _guard = install_fake(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    set_dump(&dump);
    let sid = "s-load";
    let prior_uuid = "dsh-existing-session";
    write_meta(tmp.path(), sid, prior_uuid);

    let adapter = DshAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, sid),
        )
        .await
        .expect("load start");

    assert_eq!(handle.identity, prior_uuid);
    let records = read_dump(&dump);
    let methods: Vec<_> = records
        .iter()
        .filter_map(|v| v["method"].as_str())
        .collect();
    assert_eq!(methods.first().copied(), Some("session/load"));
    assert!(
        !methods.contains(&"session/new"),
        "successful load must not fall through to session/new: {methods:?}"
    );
    assert_eq!(records[0]["params"]["sessionId"].as_str(), Some(prior_uuid));

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn load_failure_falls_back_to_session_new_with_fresh_uuid() {
    let tmp = TempDir::new().unwrap();
    let _guard = install_fake(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    set_dump(&dump);
    unsafe {
        std::env::set_var("CCTEAM_DSH_LOAD_FAIL", "1");
    }
    let sid = "s-load-fail";
    let prior_uuid = "missing-old-session";
    write_meta(tmp.path(), sid, prior_uuid);

    let adapter = DshAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, sid),
        )
        .await
        .expect("fallback start");

    assert_ne!(handle.identity, prior_uuid);
    assert!(handle.identity.starts_with("dsh-fake-"));
    let records = read_dump(&dump);
    let methods: Vec<_> = records
        .iter()
        .filter_map(|v| v["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        vec!["session/load", "session/new"],
        "load failure must fall back through a fresh new handshake"
    );

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn prompt_roundtrip_uses_shared_acp_turn_runner() {
    let tmp = TempDir::new().unwrap();
    let _guard = install_fake(&tmp);
    let adapter = DshAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-prompt"),
        )
        .await
        .expect("start ok");

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut finals = Vec::new();
        let mut usage_in = None;
        let mut model = None;
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(text) = item.details {
                        finals.push(text);
                    }
                }
                ThreadEvent::TurnCompleted {
                    usage, model: m, ..
                } => {
                    usage_in = Some(usage.input_tokens);
                    model = m;
                    break;
                }
                ThreadEvent::TurnFailed { err, .. } => panic!("turn failed: {err:?}"),
                _ => {}
            }
        }
        (finals, usage_in, model)
    });

    adapter
        .submit_turn(&handle, TurnInput::UserText("hello".into()))
        .await
        .expect("submit");
    let (finals, usage_in, model) = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");
    assert_eq!(finals, vec!["echo:hello".to_string()]);
    assert_eq!(usage_in, Some(12));
    assert_eq!(model.as_deref(), Some(DEFAULT_DSH_MODEL));

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn resume_thread_returns_not_implemented() {
    let adapter = DshAcpAdapter::new();
    let err = adapter.resume_thread("dsh-cold-id").await.unwrap_err();
    assert!(matches!(err, HarnessError::NotImplemented { .. }));
}

#[test]
fn dsh_source_never_references_acp_mcp_servers_http() {
    fn walk(path: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/execution/dsh_acp");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no DSH source files found under {root:?}"
    );
    for file in files {
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(
            !body.contains("acp_mcp_servers_http"),
            "{} must not use ACP mcpServers projection",
            file.display()
        );
    }
}
