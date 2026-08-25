//! DSH ACP adapter tests against the hermetic socket fake
//! (`fixtures/dsh_acp/fake_dsh_acp.py`).
//!
//! Gate: no real dsh, no network, no `dsh web`. The fake LISTENS on a unix
//! socket exactly as the ccteam Cordis plugin does inside a real runtime, and
//! `CCTEAM_DSH_SOCKET` points the adapter straight at it — the same
//! test-only-override precedent as `CCTEAM_{CLAUDE,CODEX}_BIN`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ccteam_harness::execution::dsh_acp::handshake::{
    DEFAULT_DSH_MODEL, DEFAULT_DSH_PROVIDER, MIN_DSH_CLIENT_VERSION,
};
use ccteam_harness::execution::dsh_acp::spawn_spec::{
    build_web_spawn_spec, dsh_bin, dsh_config_source, dsh_home_for_identity, identity_dsh_home,
    identity_socket_path, tenant_home_segment, DshConfigSource, DshWebSpawnOptions,
    DEEPSEEK_API_KEY_ENV, DEEPSEEK_BASE_URL_ENV, DSH_HOME_ENV, DSH_NATIVE_WEB_PROFILE,
    DSH_SOCKET_ENV, DSH_SYSTEM_PROMPT_ENV, DSH_TELEMETRY_DISABLED_ENV, DSH_TELEMETRY_MODE_ENV,
    DSH_WEB_PROFILE,
};
use ccteam_harness::execution::mcp_config::BRIDGE_MCP_URL_ENV;
use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, DshAcpAdapter, DshRuntimeManager,
    ExecutionMode, HarnessAdapter, HarnessError, PermissionMode, SessionMeta, SessionOrigin,
    SessionProtocol, SpawnCtx, ThreadEvent, ThreadItemDetails, TurnInput, DSH_BIN_ENV,
};
use futures::StreamExt;
use serde_json::Value;
use serial_test::serial;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const ENV_KEYS: &[&str] = &[
    DSH_BIN_ENV,
    DSH_SOCKET_ENV,
    "HOME",
    "CCTEAM_HOME",
    "CCTEAM_WEB_URL",
    BRIDGE_MCP_URL_ENV,
    "CCTEAM_DSH_ACP_DUMP",
    "CCTEAM_DSH_LOAD_FAIL",
    "CCTEAM_DSH_AGENT_NAME",
    "CCTEAM_DSH_AGENT_VERSION",
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

/// The fake DSH runtime: one listening socket serving many hires, killed on
/// drop. Sockets live directly in the system temp dir — Linux caps `sun_path`
/// at ~108 bytes, so a nested per-test temp path is exactly the wrong place for
/// one (production `~/.ccteam/runtime/dsh/acp/<id>.sock` is comfortably short).
///
/// Its knobs are passed as CHILD env, not process env: the runtime is a
/// long-lived process the test starts itself, so anything exported after the
/// spawn would never reach it (and would leak across serialized tests).
struct FakeRuntime {
    child: Child,
    socket: PathBuf,
    _stdout: BufReader<std::process::ChildStdout>,
}

static SOCKET_SEQ: AtomicU32 = AtomicU32::new(0);

/// A tenant's own ccteam REST bearer, in the wire form the API accepts. Only
/// ever reaches that tenant's own profile.
const TENANT_REST_TOKEN: &str = "ccteam:0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct FakeRuntimeBuilder {
    env: Vec<(&'static str, String)>,
}

impl FakeRuntimeBuilder {
    fn dump(mut self, path: &Path) -> Self {
        self.env
            .push(("CCTEAM_DSH_ACP_DUMP", path.to_string_lossy().into_owned()));
        self
    }

    fn agent_name(mut self, name: &str) -> Self {
        self.env.push(("CCTEAM_DSH_AGENT_NAME", name.to_string()));
        self
    }

    fn agent_version(mut self, version: &str) -> Self {
        self.env
            .push(("CCTEAM_DSH_AGENT_VERSION", version.to_string()));
        self
    }

    fn load_always_fails(mut self) -> Self {
        self.env.push(("CCTEAM_DSH_LOAD_FAIL", "1".to_string()));
        self
    }

    /// Start the fake and point the adapter at it (`CCTEAM_DSH_SOCKET`), which
    /// also guarantees no test can reach a real `dsh web`.
    fn start(self) -> FakeRuntime {
        let bin = fake_bin();
        assert!(bin.is_file(), "missing fake at {}", bin.display());
        let socket = std::env::temp_dir().join(format!(
            "ccdsh-{}-{}.sock",
            std::process::id(),
            SOCKET_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&socket);
        let mut command = Command::new("python3");
        command
            .arg(&bin)
            .arg(&socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for key in [
            "CCTEAM_DSH_ACP_DUMP",
            "CCTEAM_DSH_AGENT_NAME",
            "CCTEAM_DSH_AGENT_VERSION",
            "CCTEAM_DSH_LOAD_FAIL",
        ] {
            command.env_remove(key);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn fake dsh runtime");
        let mut stdout = BufReader::new(child.stdout.take().expect("fake stdout"));
        let mut ready = String::new();
        stdout.read_line(&mut ready).expect("fake readiness line");
        assert!(
            ready.contains("listening"),
            "fake did not announce readiness: {ready:?}"
        );
        unsafe {
            std::env::set_var(DSH_SOCKET_ENV, &socket);
        }
        FakeRuntime {
            child,
            socket,
            _stdout: stdout,
        }
    }
}

impl FakeRuntime {
    fn builder() -> FakeRuntimeBuilder {
        FakeRuntimeBuilder::default()
    }

    fn start() -> Self {
        FakeRuntimeBuilder::default().start()
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for FakeRuntime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Isolate HOME/CCTEAM_HOME (AGENTS.md: pinning only HOME is not enough) and
/// give the session an MCP endpoint to mint a bearer from.
fn isolate(tmp: &TempDir) -> EnvGuard {
    let guard = EnvGuard::capture();
    let home = tmp.path().join("home");
    let ccteam_home = tmp.path().join(".ccteam-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ccteam_home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("CCTEAM_HOME", &ccteam_home);
        std::env::set_var(BRIDGE_MCP_URL_ENV, "http://127.0.0.1:65535/mcp");
        std::env::remove_var("CCTEAM_WEB_URL");
        std::env::set_var(DEEPSEEK_API_KEY_ENV, "test-deepseek-key");
        std::env::remove_var(DSH_BIN_ENV);
        std::env::remove_var(DSH_SOCKET_ENV);
        std::env::remove_var(DEEPSEEK_BASE_URL_ENV);
        std::env::remove_var(DSH_SYSTEM_PROMPT_ENV);
        std::env::remove_var("CCTEAM_DSH_ACP_DUMP");
        std::env::remove_var("CCTEAM_DSH_LOAD_FAIL");
        std::env::remove_var("CCTEAM_DSH_AGENT_NAME");
        std::env::remove_var("CCTEAM_DSH_AGENT_VERSION");
    }
    guard
}

/// An adapter whose runtime manager is unconfigured: it answers `disabled` and
/// spawns nothing, so a test can only ever reach the fake through
/// `CCTEAM_DSH_SOCKET` — never a real `dsh web`.
fn adapter() -> DshAcpAdapter {
    DshAcpAdapter::new(Arc::new(DshRuntimeManager::new(
        PathBuf::from("/nonexistent/ccteam-home"),
        Arc::new(|_root, _owner| anyhow::bail!("no enrollment resolver in tests")),
        Arc::new(|_root, _owner| anyhow::bail!("no REST token resolver in tests")),
    )))
}

fn spawn_ctx(tmp: &TempDir, sid: &str) -> SpawnCtx {
    SpawnCtx {
        mode: None,
        slug: "demo".into(),
        sid: sid.into(),
        owner: "user:web-api".into(),
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

/// A ctx that names its model explicitly. The gateway normally fills this from
/// the model catalog; tests that want to get PAST `session/new` cannot depend on
/// a catalog the isolation shell does not have.
fn spawn_ctx_with_model(tmp: &TempDir, sid: &str) -> SpawnCtx {
    let mut ctx = spawn_ctx(tmp, sid);
    ctx.model_id = Some(format!("{DEFAULT_DSH_PROVIDER}/{DEFAULT_DSH_MODEL}"));
    ctx
}

fn write_meta(project: &Path, sid: &str, vendor_uuid: &str) {
    let meta = SessionMeta {
        mode: None,
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

fn write_config_pair(home: &Path, credentials: &[u8], settings: Option<&[u8]>) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(home.join(".credentials.yaml"), credentials).unwrap();
    if let Some(settings) = settings {
        std::fs::write(home.join("settings.yaml"), settings).unwrap();
    }
}

fn operator_dsh_home(tmp: &TempDir) -> PathBuf {
    tmp.path().join("home/.dsh")
}

fn ccteam_home(tmp: &TempDir) -> PathBuf {
    tmp.path().join(".ccteam-home")
}

fn tenant_web_home(tmp: &TempDir, id: &str) -> PathBuf {
    ccteam_home(tmp)
        .join("runtime")
        .join("dsh")
        .join("web")
        .join(tenant_home_segment(id))
}

fn seed_marker_path(home: &Path) -> PathBuf {
    home.join(".ccteam-dsh-seed.json")
}

fn file_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_seed_marker(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(seed_marker_path(home)).unwrap()).unwrap()
}

fn read_dump(path: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn methods(records: &[Value]) -> Vec<&str> {
    records
        .iter()
        .filter_map(|v| v["method"].as_str())
        .collect()
}

fn find_method<'a>(records: &'a [Value], method: &str) -> &'a Value {
    records
        .iter()
        .find(|v| v["method"] == method)
        .unwrap_or_else(|| panic!("expected a {method} record, got {records:?}"))
}

fn patch_config(profile_dir: &Path) -> serde_yaml::Mapping {
    patch_config_for(profile_dir, "ccteam-client")
}

fn patch_config_for(profile_dir: &Path, row_id: &str) -> serde_yaml::Mapping {
    let raw = std::fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap();
    let patch: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
    let row = patch
        .as_sequence()
        .expect("patch is a sequence")
        .iter()
        .find(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some(row_id))
        .unwrap_or_else(|| panic!("no {row_id} row in {raw}"));
    assert!(
        row.get("insert").is_none(),
        "ccteam rows OVERRIDE the bundle-inserted entry; a duplicate id aborts \
         the Cordis boot: {raw}"
    );
    row.get("config")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("flat plugin config")
        .clone()
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
fn dsh_config_source_resolves_all_authorized_arms() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    let root = ccteam_home(&tmp);
    let operator_home = operator_dsh_home(&tmp);

    assert_eq!(
        dsh_config_source("user:web-api", &root),
        DshConfigSource::None
    );

    write_config_pair(&operator_home, b"operator-creds", None);
    assert_eq!(
        dsh_config_source("user:web-api", &root),
        DshConfigSource::OperatorHome(operator_home.clone())
    );
    assert_eq!(
        dsh_config_source("telegram:123", &root),
        DshConfigSource::OperatorHome(operator_home.clone())
    );
    assert_eq!(
        dsh_config_source("user:alice", &root),
        DshConfigSource::OperatorHome(operator_home)
    );

    let alice_home = tenant_web_home(&tmp, "alice");
    std::fs::create_dir_all(&alice_home).unwrap();
    assert_eq!(
        dsh_config_source("user:alice", &root),
        DshConfigSource::OperatorHome(operator_dsh_home(&tmp))
    );
    write_config_pair(&alice_home, b"alice-creds", None);
    assert_eq!(
        dsh_config_source("user:alice", &root),
        DshConfigSource::TenantHome(alice_home)
    );

    unsafe {
        std::env::set_var(DEEPSEEK_API_KEY_ENV, "env-key");
    }
    assert_eq!(dsh_config_source("user:alice", &root), DshConfigSource::Env);
}

/// A tenant who has their own credentials is served from their own home; a
/// tenant who does not falls back to the operator's.
#[test]
#[serial(dsh_env)]
fn config_source_prefers_tenant_home_with_credentials() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    let root = ccteam_home(&tmp);

    let operator_home = operator_dsh_home(&tmp);
    write_config_pair(&operator_home, b"operator", Some(b"operator-settings"));
    let tenant_home = tenant_web_home(&tmp, "alice");
    write_config_pair(&tenant_home, b"tenant", None);

    assert_eq!(
        dsh_config_source("user:alice", &root),
        DshConfigSource::TenantHome(tenant_home)
    );
    assert_eq!(
        dsh_config_source("user:bob", &root),
        DshConfigSource::OperatorHome(operator_home)
    );
}

/// The identity home resolver and the socket path are what make the adapter and
/// the runtime manager meet in the same place.
#[test]
#[serial(dsh_env)]
fn identity_home_and_socket_agree_with_the_managed_tenant_layout() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let root = ccteam_home(&tmp);

    assert_eq!(
        identity_dsh_home("user:alice", &root).unwrap(),
        tenant_web_home(&tmp, "alice")
    );
    assert_eq!(
        identity_dsh_home("user:web-api", &root).unwrap(),
        operator_dsh_home(&tmp)
    );
    assert_eq!(
        identity_dsh_home("telegram:123", &root).unwrap(),
        operator_dsh_home(&tmp),
        "an IM chat is the same human at the same ~/.dsh"
    );
    assert_eq!(
        identity_socket_path("user:alice", &root),
        root.join("runtime/dsh/acp/alice.sock")
    );
    assert_eq!(
        identity_socket_path("telegram:123", &root),
        identity_socket_path("user:web-api", &root),
        "operator-shaped owners share one runtime, so one socket"
    );
}

/// One person, one DSH home — and the same one no matter which key shape
/// the caller holds (`owner_tag` on the adapter path, `operator` + `id` on
/// the runtime-manager path). A drift here would put the adapter's socket
/// in one home and the runtime's process in another.
#[test]
#[serial(dsh_env)]
fn identity_home_maps_tenants_managed_and_everyone_else_to_the_operator_home() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let root = ccteam_home(&tmp);

    let tenant = identity_dsh_home("user:alice", &root).unwrap();
    assert_eq!(tenant, root.join("runtime/dsh/web/alice"));
    assert_eq!(
        identity_dsh_home("user:alice", &root).unwrap(),
        tenant,
        "same owner must always resolve to the same home"
    );
    assert_eq!(
        dsh_home_for_identity(false, "alice", &root).unwrap(),
        tenant,
        "the runtime manager's key shape must agree with the owner tag"
    );

    let operator = operator_dsh_home(&tmp);
    for tag in ["user:web-api", "user:", "telegram:123", ""] {
        assert_eq!(
            identity_dsh_home(tag, &root).unwrap(),
            operator,
            "`{tag}` is the operator and owns ~/.dsh"
        );
    }
    assert_eq!(
        dsh_home_for_identity(true, "admin", &root).unwrap(),
        operator
    );
}

#[test]
#[serial(dsh_env)]
fn web_spawn_spec_uses_ccteam_web_profile_and_publishes_the_transport_socket() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dsh_home = tenant_web_home(&tmp, "alice");
    let socket = identity_socket_path("user:alice", &ccteam_home(&tmp));
    unsafe {
        std::env::set_var(DEEPSEEK_BASE_URL_ENV, "https://example.invalid");
    }

    let spec = build_web_spawn_spec(DshWebSpawnOptions {
        owner_tag: "user:alice",
        ccteam_home: ccteam_home(&tmp),
        dsh_home: dsh_home.clone(),
        profile: DSH_WEB_PROFILE,
        materialize_profile: true,
        enrollment: Some("ccteam-enroll:abc:secret"),
        daemon_url: Some("http://127.0.0.1:7331"),
        transport_socket: Some(&socket),
        rest_token: Some(TENANT_REST_TOKEN),
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
    assert!(spec.env_remove.is_empty());

    // The runtime's env is per IDENTITY, never per session: no sid, no bearer,
    // no approval mode, no transport switch — those all travel in
    // `_meta.ccteam` now.
    let env: BTreeMap<_, _> = spec.env.iter().cloned().collect();
    let keys: Vec<_> = env.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec![
            DEEPSEEK_API_KEY_ENV,
            DEEPSEEK_BASE_URL_ENV,
            DSH_HOME_ENV,
            DSH_TELEMETRY_DISABLED_ENV,
            DSH_TELEMETRY_MODE_ENV,
        ]
    );
    assert_eq!(env[DSH_HOME_ENV], dsh_home.to_string_lossy());
    assert_eq!(env[DEEPSEEK_API_KEY_ENV], "test-deepseek-key");
    assert_eq!(env[DEEPSEEK_BASE_URL_ENV], "https://example.invalid");

    // The plugin activates its ACP listener on this config key alone.
    let profile_dir = dsh_home.join("profiles").join(DSH_WEB_PROFILE);
    assert!(profile_dir.join("package.json").is_file());
    let config = patch_config(&profile_dir);
    assert_eq!(
        config["transportSocket"],
        serde_yaml::Value::String(socket.to_string_lossy().into_owned())
    );
    assert_eq!(
        config["enrollment"],
        serde_yaml::Value::String("ccteam-enroll:abc:secret".into())
    );

    // Zero user steps: the team panel is installed AND credentialed in the
    // tenant's own home by the same start.
    let team = patch_config_for(&profile_dir, "ccteam-team");
    assert_eq!(
        team["daemonUrl"],
        serde_yaml::Value::String("http://127.0.0.1:7331".into())
    );
    assert_eq!(
        team["restToken"],
        serde_yaml::Value::String(TENANT_REST_TOKEN.into())
    );
    assert!(
        profile_dir
            .join("node_modules")
            .join("@ccteam")
            .join("dsh-team")
            .join("package.json")
            .is_file(),
        "the panel package is materialized into the tenant profile"
    );

    let socket_dir = socket.parent().unwrap();
    assert!(
        socket_dir.is_dir(),
        "the socket dir is created before spawn"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(socket_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "sockets live in a private directory");
    }
}

/// Gate ①: on the branch where ccteam itself starts `dsh web` in the operator's
/// own `~/.dsh`, it may register its plugin there — additively, and nothing else.
#[test]
#[serial(dsh_env)]
fn operator_web_spawn_registers_only_ccteam_rows_in_the_native_profile() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dsh_home = operator_dsh_home(&tmp);
    let profile_dir = dsh_home.join("profiles").join(DSH_NATIVE_WEB_PROFILE);
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(
        profile_dir.join("package.json"),
        serde_json::json!({
            "name": "dsh-web-profile",
            "dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-web-app"]}}
        })
        .to_string(),
    )
    .unwrap();
    let user_patch = "- id: operator-own-plugin\n  config:\n    keepMe: true\n";
    std::fs::write(profile_dir.join("cordis.patch.yml"), user_patch).unwrap();
    let socket = identity_socket_path("user:web-api", &ccteam_home(&tmp));

    build_web_spawn_spec(DshWebSpawnOptions {
        owner_tag: "user:web-api",
        ccteam_home: ccteam_home(&tmp),
        dsh_home: dsh_home.clone(),
        profile: DSH_NATIVE_WEB_PROFILE,
        materialize_profile: false,
        enrollment: None,
        daemon_url: Some("http://127.0.0.1:7331"),
        transport_socket: Some(&socket),
        rest_token: None,
    })
    .expect("operator web spawn spec");

    let package: Value =
        serde_json::from_slice(&std::fs::read(profile_dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(package["name"], "dsh-web-profile");
    assert_eq!(
        package["dsh"]["profile"]["bundles"],
        serde_json::json!([
            "@deepseek-ai/dsh-web-app",
            "@ccteam/dsh-client",
            "@ccteam/dsh-team"
        ]),
        "only ccteam's own bundles are appended"
    );
    let raw_patch = std::fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap();
    assert!(
        raw_patch.contains("operator-own-plugin") && raw_patch.contains("keepMe"),
        "the operator's own patch rows must survive: {raw_patch}"
    );
    let config = patch_config(&profile_dir);
    assert_eq!(
        config["transportSocket"],
        serde_yaml::Value::String(socket.to_string_lossy().into_owned())
    );
    assert!(
        config.get("enrollment").is_none(),
        "ccteam never injects enrollment into the operator's own home: {config:?}"
    );

    // The panel is registered and pointed at this daemon, but its credential
    // is the operator's to paste — same line as `enrollment` above.
    let team = patch_config_for(&profile_dir, "ccteam-team");
    assert_eq!(
        team["daemonUrl"],
        serde_yaml::Value::String("http://127.0.0.1:7331".into())
    );
    assert!(
        team.get("restToken").is_none(),
        "no credential in a home ccteam does not own: {team:?}"
    );
}

#[test]
#[serial(dsh_env)]
fn web_profile_factory_seeds_operator_credentials_and_marker_for_tenant_home() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    write_config_pair(
        &operator_dsh_home(&tmp),
        b"seed-creds",
        Some(b"seed-settings"),
    );
    let dsh_home = tenant_web_home(&tmp, "alice");

    build_web_spawn_spec(DshWebSpawnOptions {
        owner_tag: "user:alice",
        ccteam_home: ccteam_home(&tmp),
        dsh_home: dsh_home.clone(),
        profile: DSH_WEB_PROFILE,
        materialize_profile: true,
        enrollment: Some("ccteam-enroll:abc:secret"),
        daemon_url: Some("http://127.0.0.1:7331"),
        transport_socket: None,
        rest_token: None,
    })
    .expect("web spawn spec");

    assert_eq!(
        std::fs::read(dsh_home.join(".credentials.yaml")).unwrap(),
        b"seed-creds"
    );
    assert_eq!(
        std::fs::read(dsh_home.join("settings.yaml")).unwrap(),
        b"seed-settings"
    );
    let marker = read_seed_marker(&dsh_home);
    assert_eq!(
        marker["credentials_sha256"].as_str(),
        Some(file_sha256(b"seed-creds").as_str())
    );
    assert_eq!(
        marker["settings_sha256"].as_str(),
        Some(file_sha256(b"seed-settings").as_str())
    );
    assert!(marker["seeded_at"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
#[serial(dsh_env)]
fn web_profile_factory_empty_without_source_writes_no_marker() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    let dsh_home = tenant_web_home(&tmp, "alice");

    build_web_spawn_spec(DshWebSpawnOptions {
        owner_tag: "user:alice",
        ccteam_home: ccteam_home(&tmp),
        dsh_home: dsh_home.clone(),
        profile: DSH_WEB_PROFILE,
        materialize_profile: true,
        enrollment: Some("ccteam-enroll:abc:secret"),
        daemon_url: Some("http://127.0.0.1:7331"),
        transport_socket: None,
        rest_token: None,
    })
    .expect("web spawn spec");

    assert!(!dsh_home.join(".credentials.yaml").exists());
    assert!(!dsh_home.join("settings.yaml").exists());
    assert!(!seed_marker_path(&dsh_home).exists());
}

#[test]
#[serial(dsh_env)]
fn web_profile_refreshes_seeded_files_that_tenant_did_not_touch() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    write_config_pair(
        &operator_dsh_home(&tmp),
        b"seed-creds-v1",
        Some(b"seed-settings-v1"),
    );
    let dsh_home = tenant_web_home(&tmp, "alice");
    let build = || {
        build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: "user:alice",
            ccteam_home: ccteam_home(&tmp),
            dsh_home: dsh_home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some("ccteam-enroll:abc:secret"),
            daemon_url: Some("http://127.0.0.1:7331"),
            transport_socket: None,
            rest_token: None,
        })
        .expect("web spawn spec");
    };
    build();

    write_config_pair(
        &operator_dsh_home(&tmp),
        b"seed-creds-v2",
        Some(b"seed-settings-v2"),
    );
    build();

    assert_eq!(
        std::fs::read(dsh_home.join(".credentials.yaml")).unwrap(),
        b"seed-creds-v2"
    );
    assert_eq!(
        std::fs::read(dsh_home.join("settings.yaml")).unwrap(),
        b"seed-settings-v2"
    );
    let marker = read_seed_marker(&dsh_home);
    assert_eq!(
        marker["credentials_sha256"].as_str(),
        Some(file_sha256(b"seed-creds-v2").as_str())
    );
    assert_eq!(
        marker["settings_sha256"].as_str(),
        Some(file_sha256(b"seed-settings-v2").as_str())
    );
}

/// The seed-then-refresh ladder observed at BOTH stops: the marker must be
/// right after the first seed (not just after a later refresh), because that
/// first marker is what the refresh arm compares against to decide the tenant
/// has not touched the file.
///
/// Reaches `seed_or_refresh_tenant_web_config_home` (private) through the same
/// public door as its neighbours: `build_web_spawn_spec` with
/// `materialize_profile: true` calls it with exactly these three arguments.
#[test]
#[serial(dsh_env)]
fn tenant_web_seed_refreshes_unmodified_files_from_operator_home() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    let operator_home = operator_dsh_home(&tmp);
    let tenant_home = tenant_web_home(&tmp, "alice");
    let seed_or_refresh = || {
        build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: "user:alice",
            ccteam_home: ccteam_home(&tmp),
            dsh_home: tenant_home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some("ccteam-enroll:abc:secret"),
            daemon_url: Some("http://127.0.0.1:7331"),
            transport_socket: None,
            rest_token: None,
        })
        .expect("web spawn spec");
    };
    write_config_pair(&operator_home, b"creds-v1", Some(b"settings-v1"));

    seed_or_refresh();
    assert_eq!(
        std::fs::read(tenant_home.join(".credentials.yaml")).unwrap(),
        b"creds-v1"
    );
    assert_eq!(
        std::fs::read(tenant_home.join("settings.yaml")).unwrap(),
        b"settings-v1"
    );
    let marker = read_seed_marker(&tenant_home);
    assert_eq!(
        marker["credentials_sha256"].as_str(),
        Some(file_sha256(b"creds-v1").as_str())
    );
    assert_eq!(
        marker["settings_sha256"].as_str(),
        Some(file_sha256(b"settings-v1").as_str())
    );

    write_config_pair(&operator_home, b"creds-v2", Some(b"settings-v2"));
    seed_or_refresh();
    assert_eq!(
        std::fs::read(tenant_home.join(".credentials.yaml")).unwrap(),
        b"creds-v2"
    );
    assert_eq!(
        std::fs::read(tenant_home.join("settings.yaml")).unwrap(),
        b"settings-v2"
    );
}

#[test]
#[serial(dsh_env)]
fn web_profile_keeps_tenant_modified_seeded_files() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    write_config_pair(
        &operator_dsh_home(&tmp),
        b"seed-creds-v1",
        Some(b"seed-settings-v1"),
    );
    let dsh_home = tenant_web_home(&tmp, "alice");
    let build = || {
        build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: "user:alice",
            ccteam_home: ccteam_home(&tmp),
            dsh_home: dsh_home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some("ccteam-enroll:abc:secret"),
            daemon_url: Some("http://127.0.0.1:7331"),
            transport_socket: None,
            rest_token: None,
        })
        .expect("web spawn spec");
    };
    build();

    std::fs::write(dsh_home.join(".credentials.yaml"), b"tenant-creds").unwrap();
    std::fs::write(dsh_home.join("settings.yaml"), b"tenant-settings").unwrap();
    write_config_pair(
        &operator_dsh_home(&tmp),
        b"seed-creds-v2",
        Some(b"seed-settings-v2"),
    );
    build();

    assert_eq!(
        std::fs::read(dsh_home.join(".credentials.yaml")).unwrap(),
        b"tenant-creds"
    );
    assert_eq!(
        std::fs::read(dsh_home.join("settings.yaml")).unwrap(),
        b"tenant-settings"
    );
}

#[test]
#[serial(dsh_env)]
fn web_profile_keeps_preexisting_files_without_marker() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DEEPSEEK_API_KEY_ENV);
    }
    write_config_pair(
        &operator_dsh_home(&tmp),
        b"operator-creds",
        Some(b"operator-settings"),
    );
    let dsh_home = tenant_web_home(&tmp, "alice");
    write_config_pair(
        &dsh_home,
        b"preexisting-creds",
        Some(b"preexisting-settings"),
    );

    build_web_spawn_spec(DshWebSpawnOptions {
        owner_tag: "user:alice",
        ccteam_home: ccteam_home(&tmp),
        dsh_home: dsh_home.clone(),
        profile: DSH_WEB_PROFILE,
        materialize_profile: true,
        enrollment: Some("ccteam-enroll:abc:secret"),
        daemon_url: Some("http://127.0.0.1:7331"),
        transport_socket: None,
        rest_token: None,
    })
    .expect("web spawn spec");

    assert_eq!(
        std::fs::read(dsh_home.join(".credentials.yaml")).unwrap(),
        b"preexisting-creds"
    );
    assert_eq!(
        std::fs::read(dsh_home.join("settings.yaml")).unwrap(),
        b"preexisting-settings"
    );
    assert!(!seed_marker_path(&dsh_home).exists());
}

#[tokio::test]
#[serial(dsh_env)]
async fn session_new_never_sends_acp_mcp_servers_even_with_secret() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder().dump(&dump).start();
    unsafe {
        std::env::set_var(DSH_SYSTEM_PROMPT_ENV, "must-not-reach-child");
    }

    let adapter = adapter();
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
    let params = &find_method(&records, "session/new")["params"];
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

    adapter.close_thread(&handle).await.unwrap();
}

/// The runtime is shared, so ccteam's identity for THIS hire has to travel with
/// the session request. `_meta.ccteam` is the whole story: sid, the per-session
/// MCP bearer, the daemon URL, and the approval posture the deleted
/// `CCTEAM_DSH_APPROVAL` child env used to carry.
#[tokio::test]
#[serial(dsh_env)]
async fn session_new_carries_the_per_session_ccteam_identity() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder().dump(&dump).start();

    let adapter = adapter();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-meta"),
        )
        .await
        .expect("start ok");

    let records = read_dump(&dump);
    let params = &find_method(&records, "session/new")["params"];
    assert_eq!(
        params.pointer("/_meta/ccteam"),
        Some(&serde_json::json!({
            "sid": "s-meta",
            "bearer": "ccteam-sid:s-meta:seKret1234",
            "mcpUrl": "http://127.0.0.1:65535/mcp",
            "approvalMode": "skip",
            // Unset mode = the ccteam default: `standard` (the vendor's own).
            "agentPreset": "standard",
        })),
        "session/new must install this hire's ccteam identity: {params}"
    );
    assert!(
        params["cwd"].as_str().is_some_and(|cwd| !cwd.is_empty()),
        "the plugin mounts the workspace from cwd: {params}"
    );

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn session_load_carries_the_per_session_ccteam_identity() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder().dump(&dump).start();
    let sid = "s-meta-load";
    let prior_uuid = "dsh-existing-session";
    write_meta(tmp.path(), sid, prior_uuid);

    let adapter = adapter();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, sid),
        )
        .await
        .expect("load start");

    assert_eq!(handle.identity, prior_uuid);
    let records = read_dump(&dump);
    let params = &find_method(&records, "session/load")["params"];
    assert_eq!(params["sessionId"].as_str(), Some(prior_uuid));
    assert_eq!(
        params
            .pointer("/_meta/ccteam/bearer")
            .and_then(Value::as_str),
        Some("ccteam-sid:s-meta-load:seKret1234"),
        "a resumed session re-installs its ccteam identity: {params}"
    );
    assert!(
        params.pointer("/_meta/ccteam/agentPreset").is_none(),
        "session/load never names a preset — the stored one is authoritative: {params}"
    );

    adapter.close_thread(&handle).await.unwrap();
}

/// The ccteam `mode` axis picks the DSH agent preset on session/new — vendor
/// spelling and ccteam alias both accepted, unknown tokens refused readably.
#[tokio::test]
#[serial(dsh_env)]
async fn mode_axis_picks_the_agent_preset_on_session_new() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder().dump(&dump).start();

    let adapter = adapter();
    let mut ctx = spawn_ctx_with_model(&tmp, "s-mode");
    ctx.mode = Some("minimal".to_string());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("start ok");
    let records = read_dump(&dump);
    let params = &find_method(&records, "session/new")["params"];
    assert_eq!(
        params
            .pointer("/_meta/ccteam/agentPreset")
            .and_then(Value::as_str),
        Some("minimal"),
        "{params}"
    );
    adapter.close_thread(&handle).await.unwrap();

    let mut bad = spawn_ctx_with_model(&tmp, "s-mode-bad");
    bad.mode = Some("turbo".to_string());
    let err = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &bad,
        )
        .await
        .expect_err("unknown mode must refuse the spawn");
    let text = err.to_string();
    assert!(
        text.contains("turbo") && text.contains("ptc"),
        "the refusal names the token and the valid set: {text}"
    );
}

/// hitl vs skip is a per-session posture on a runtime that serves both.
#[tokio::test]
#[serial(dsh_env)]
async fn approval_mode_follows_the_sessions_permission_mode() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder().dump(&dump).start();

    let mut ctx = spawn_ctx_with_model(&tmp, "s-hitl");
    ctx.permission_mode = PermissionMode::Hitl;
    let adapter = adapter();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("start ok");

    let records = read_dump(&dump);
    assert_eq!(
        find_method(&records, "session/new")
            .pointer("/params/_meta/ccteam/approvalMode")
            .and_then(Value::as_str),
        Some("hitl")
    );

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn meta_vendor_uuid_loads_before_new() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder().dump(&dump).start();
    let sid = "s-load";
    let prior_uuid = "dsh-existing-session";
    write_meta(tmp.path(), sid, prior_uuid);

    let adapter = adapter();
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
    let session_methods: Vec<_> = methods(&records)
        .into_iter()
        .filter(|method| method.starts_with("session/"))
        .collect();
    assert_eq!(session_methods.first().copied(), Some("session/load"));
    assert!(
        !session_methods.contains(&"session/new"),
        "successful load must not fall through to session/new: {session_methods:?}"
    );

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn load_failure_falls_back_to_session_new_with_fresh_uuid() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let _fake = FakeRuntime::builder()
        .dump(&dump)
        .load_always_fails()
        .start();
    let sid = "s-load-fail";
    let prior_uuid = "missing-old-session";
    write_meta(tmp.path(), sid, prior_uuid);

    let adapter = adapter();
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
    let session_methods: Vec<_> = methods(&records)
        .into_iter()
        .filter(|method| method.starts_with("session/"))
        .collect();
    assert_eq!(
        session_methods,
        vec!["session/load", "session/new"],
        "load failure must fall back through a fresh new handshake"
    );
    // The fallback stays on the SAME connection: a rejected load never
    // invalidates the transport, and reconnecting would be a second peer on a
    // runtime that is serving other hires.
    let connections: Vec<_> = records
        .iter()
        .filter(|record| record["method"] == "connection/opened")
        .collect();
    assert_eq!(connections.len(), 1, "one hire opens one connection");

    adapter.close_thread(&handle).await.unwrap();
}

#[tokio::test]
#[serial(dsh_env)]
async fn prompt_roundtrip_uses_shared_acp_turn_runner() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let _fake = FakeRuntime::start();
    let adapter = adapter();
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

/// A plugin that predates the socket transport answers `initialize` perfectly
/// and then ignores every `_meta.ccteam` credential, so the floor is a hard gate
/// with a remedy in the message.
#[tokio::test]
#[serial(dsh_env)]
async fn version_gate_refuses_a_plugin_older_than_the_socket_transport() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let _fake = FakeRuntime::builder().agent_version("0.10.2").start();

    let err = adapter()
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-old-plugin"),
        )
        .await
        .expect_err("an old plugin must be refused");
    let message = err.to_string();
    assert!(message.contains("0.10.2"), "got {message}");
    assert!(message.contains(MIN_DSH_CLIENT_VERSION), "got {message}");
    assert!(
        message.contains("dsh plugin add @ccteam/dsh-client"),
        "the error must name the remedy: {message}"
    );
}

#[tokio::test]
#[serial(dsh_env)]
async fn a_foreign_acp_peer_is_refused() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let _fake = FakeRuntime::builder()
        .agent_name("deepseek-harness-acp")
        .start();

    let err = adapter()
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-demo-peer"),
        )
        .await
        .expect_err("the official demo must be refused");
    assert!(
        err.to_string().contains("deepseek-harness-acp"),
        "got {err}"
    );
}

/// Two hires for the same identity are two CONNECTIONS to one runtime — the
/// property the whole card exists for. (The manager-side half, "every
/// operator-shaped owner tag collapses to one instance key", is unit-tested in
/// `dsh_runtime`.)
#[tokio::test]
#[serial(dsh_env)]
async fn two_hires_for_one_identity_share_a_single_runtime() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let mut fake = FakeRuntime::builder().dump(&dump).start();

    let adapter = adapter();
    let first = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-hire-1"),
        )
        .await
        .expect("first hire");
    let second = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-hire-2"),
        )
        .await
        .expect("second hire");

    assert_ne!(
        first.identity, second.identity,
        "each hire is its own DSH session inside the shared runtime"
    );
    let records = read_dump(&dump);
    let opened: Vec<_> = records
        .iter()
        .filter(|record| record["method"] == "connection/opened")
        .filter_map(|record| record["conn"].as_u64())
        .collect();
    assert_eq!(opened, vec![1, 2], "two peers on one listening socket");
    let sids: Vec<_> = records
        .iter()
        .filter(|record| record["method"] == "session/new")
        .filter_map(|record| {
            record
                .pointer("/params/_meta/ccteam/sid")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(
        sids,
        vec!["s-hire-1", "s-hire-2"],
        "each connection installs its own hire's identity"
    );
    assert!(fake.is_alive(), "one runtime served both hires");

    adapter.close_thread(&first).await.unwrap();
    adapter.close_thread(&second).await.unwrap();
}

/// Closing a hire cancels ITS turn and drops ITS connection. The runtime is the
/// identity's, not the session's: killing it would take the human's DSH web UI
/// and every other hire down with it.
#[tokio::test]
#[serial(dsh_env)]
async fn close_thread_cancels_the_turn_and_drops_only_the_connection() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let dump = tmp.path().join("dsh_acp_dump.jsonl");
    let mut fake = FakeRuntime::builder().dump(&dump).start();

    let adapter = adapter();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-close"),
        )
        .await
        .expect("start ok");
    adapter.close_thread(&handle).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let records = read_dump(&dump);
        let tail: Vec<_> = methods(&records)
            .into_iter()
            .filter(|method| *method == "session/cancel" || *method == "connection/closed")
            .collect();
        if tail == vec!["session/cancel", "connection/closed"] {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected cancel-then-disconnect, got {:?}",
            methods(&records)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        fake.is_alive(),
        "closing a hire must never stop the identity's runtime"
    );
    assert!(!adapter.thread_is_live(&handle));

    // The runtime is still serving: a fresh hire connects to the same socket.
    let next = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-close-next"),
        )
        .await
        .expect("the runtime still accepts hires");
    adapter.close_thread(&next).await.unwrap();
}

/// Without a runtime and without the test override there is nothing to connect
/// to, and the error has to say so rather than blaming the socket.
#[tokio::test]
#[serial(dsh_env)]
async fn a_daemon_without_a_dsh_runtime_says_so() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::remove_var(DSH_SOCKET_ENV);
    }

    let err = adapter()
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx_with_model(&tmp, "s-no-runtime"),
        )
        .await
        .expect_err("an unconfigured runtime manager cannot serve a hire");
    assert!(err.to_string().contains("no DSH runtime"), "got {err}");
}

#[tokio::test]
#[serial(dsh_env)]
async fn resume_thread_returns_not_implemented() {
    let err = adapter().resume_thread("dsh-cold-id").await.unwrap_err();
    assert!(matches!(err, HarnessError::NotImplemented { .. }));
}

/// Grep-clean guard for the retired per-hire plumbing: ccteam's DSH identity is
/// ACP `_meta`, never child env and never an ACP `mcpServers` projection.
#[test]
fn dsh_source_carries_no_retired_per_session_plumbing() {
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
        for retired in [
            "acp_mcp_servers_http",
            "CCTEAM_DSH_TRANSPORT",
            "CCTEAM_DSH_APPROVAL",
            "CCTEAM_MCP_BEARER",
        ] {
            assert!(
                !body.contains(retired),
                "{} must not use `{retired}`: DSH credentials travel per session in _meta.ccteam",
                file.display()
            );
        }
    }
}
