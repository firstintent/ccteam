use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, ApprovalIR, ApprovalRisk, HarnessAdapter, PermissionMode, PiApprovalDecision,
    PiDialogKind, PiDialogRequest, PiDialogResponse, PiInteractionResolver, PiRoleDocument,
    PiRpcAdapter, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItemDetails, TurnInput, TurnRouting,
};
use futures::StreamExt;
use serde_json::Value;
use serial_test::serial;
use tokio::sync::Notify;

#[path = "support/fake_mcp.rs"]
mod fake_mcp;
use fake_mcp::{start_fake_mcp, McpCapture};

fn role_reader() -> ccteam_harness::PiRoleReader {
    Arc::new(|_project_dir: &Path, _role: &str| Ok(None::<PiRoleDocument>))
}

struct TestEnv {
    _home: tempfile::TempDir,
    project: tempfile::TempDir,
    log: std::path::PathBuf,
    ccteam_home: std::path::PathBuf,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl TestEnv {
    fn new(mcp_url: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let ccteam_home = home.path().join("ccteam-home");
        let sessions = home.path().join("sessions");
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let fake = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi_rpc/fake_pi.py")
            .canonicalize()
            .unwrap();
        let log = home.path().join("fake-pi.jsonl");
        let owned = [
            ("HOME", home.path().as_os_str().to_os_string()),
            ("CCTEAM_HOME", ccteam_home.as_os_str().to_os_string()),
            ("CCTEAM_PI_BIN", fake.as_os_str().to_os_string()),
            ("CCTEAM_PI_FAKE_LOG", log.as_os_str().to_os_string()),
            (
                "CCTEAM_PI_FAKE_SESSION_DIR",
                sessions.as_os_str().to_os_string(),
            ),
            ("CCTEAM_MCP_HTTP_URL", mcp_url.into()),
        ];
        let mut previous = Vec::new();
        for (key, value) in &owned {
            previous.push((*key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        Self {
            _home: home,
            project,
            log,
            ccteam_home,
            previous,
        }
    }

    fn ctx(&self, permission_mode: PermissionMode) -> SpawnCtx {
        SpawnCtx {
            mode: None,
            slug: "pi-bridge".into(),
            sid: "s901".into(),
            owner: "user:web-api".into(),
            cwd: self.project.path().to_path_buf(),
            project_dir: self.project.path().to_path_buf(),
            extra_args: Vec::new(),
            model_id: None,
            effort: None,
            permission_mode,
            secret: "bridge-secret".into(),
            remote: None,
        }
    }

    fn log_rows(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

#[derive(Default)]
struct TestResolver {
    approvals: Mutex<Vec<ApprovalIR>>,
    dialogs: Mutex<Vec<PiDialogRequest>>,
    dialog_started: Notify,
    cancelled_sids: AtomicUsize,
}

#[async_trait]
impl PiInteractionResolver for TestResolver {
    fn classify_tool_risk(&self, tool_name: &str, _input: &Value) -> ApprovalRisk {
        match tool_name {
            "bash" => ApprovalRisk::Medium,
            "write" => ApprovalRisk::Medium,
            _ => ApprovalRisk::Unknown,
        }
    }

    async fn resolve_approval(&self, _sid: &str, request: &ApprovalIR) -> PiApprovalDecision {
        self.approvals.lock().unwrap().push(request.clone());
        match request
            .raw
            .pointer("/input/testDecision")
            .and_then(Value::as_str)
        {
            Some("approve") => PiApprovalDecision::Allow,
            Some("timeout") => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                PiApprovalDecision::Allow
            }
            _ => PiApprovalDecision::Deny("test denial".to_string()),
        }
    }

    async fn resolve_dialog(&self, _sid: &str, request: &PiDialogRequest) -> PiDialogResponse {
        self.dialogs.lock().unwrap().push(request.clone());
        if request.title == "Hang" {
            self.dialog_started.notify_one();
            std::future::pending::<()>().await;
        }
        match &request.kind {
            PiDialogKind::Select { .. } => PiDialogResponse::Value("B".to_string()),
            PiDialogKind::Confirm { .. } => PiDialogResponse::Confirmed(true),
            PiDialogKind::Input { .. } => PiDialogResponse::Value("input-text".to_string()),
            PiDialogKind::Editor { .. } => PiDialogResponse::Value("edited-text".to_string()),
        }
    }

    async fn cancel_sid(&self, _sid: &str) {
        self.cancelled_sids.fetch_add(1, Ordering::SeqCst);
    }
}

async fn submit_and_terminal(
    adapter: &PiRpcAdapter,
    handle: &ThreadHandle,
    text: &str,
) -> (Vec<ThreadEvent>, String) {
    let mut events = adapter.events(handle);
    let mut submission = adapter
        .submit_turn_routed(
            handle,
            TurnInput::UserText(text.to_string()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    submission.release_completion();
    let mut collected = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), events.next())
            .await
            .expect("Pi terminal timeout")
            .expect("Pi event stream closed");
        let terminal = matches!(
            event,
            ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. }
        );
        collected.push(event);
        if terminal {
            let answer = collected
                .iter()
                .filter_map(|event| match event {
                    ThreadEvent::ItemCompleted { item } => match &item.details {
                        ThreadItemDetails::AgentMessage(text) => Some(text.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .next_back()
                .unwrap_or_default();
            return (collected, answer);
        }
    }
}

#[tokio::test]
#[serial]
async fn managed_pi_spawn_performs_mcp_handshake_with_session_bearer() {
    let capture = McpCapture::default();
    let (server, url) = start_fake_mcp(capture.clone()).await;
    let env = TestEnv::new(&url);
    let adapter = PiRpcAdapter::new(role_reader());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &env.ctx(PermissionMode::Skip),
        )
        .await
        .unwrap();
    adapter.close_thread(&handle).await.unwrap();
    server.abort();

    let calls = capture.calls.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.method.as_str())
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/list"]
    );
    assert!(calls.iter().all(|call| {
        call.authorization.as_deref() == Some("Bearer ccteam-sid:s901:bridge-secret")
    }));

    let spawn = &env.log_rows()[0];
    assert_eq!(spawn["no_extensions"], false);
    let bridge_path = Path::new(spawn["bridge_path"].as_str().unwrap());
    assert!(bridge_path.starts_with(env.ccteam_home.join("runtime/pi")));
    assert!(bridge_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("ccteam-bridge-"));
    assert_eq!(spawn["bridge_body"], ccteam_harness::pi_bridge_source());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(bridge_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
#[serial]
async fn bridge_executes_status_and_session_list_while_skip_prompts_zero_times() {
    let capture = McpCapture::default();
    let (server, url) = start_fake_mcp(capture.clone()).await;
    let env = TestEnv::new(&url);
    let resolver = Arc::new(TestResolver::default());
    let adapter = PiRpcAdapter::new(role_reader());
    adapter.set_interaction_resolver(resolver.clone());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &env.ctx(PermissionMode::Skip),
        )
        .await
        .unwrap();

    let (_, answer) = submit_and_terminal(&adapter, &handle, "bridge-tools").await;
    assert_eq!(answer, "bridge-tools-ok");
    let (_, answer) = submit_and_terminal(&adapter, &handle, "skip-tool").await;
    assert_eq!(answer, "skip-no-prompt");
    assert!(resolver.approvals.lock().unwrap().is_empty());

    adapter.close_thread(&handle).await.unwrap();
    server.abort();
    let tool_names = capture
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.method == "tools/call")
        .filter_map(|call| {
            call.params
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        ["status".to_string(), "session_list".to_string()]
    );
}

#[tokio::test]
#[serial]
async fn strict_hitl_covers_auto_allow_approve_deny_timeout_and_oversize() {
    let capture = McpCapture::default();
    let (server, url) = start_fake_mcp(capture).await;
    let env = TestEnv::new(&url);
    let resolver = Arc::new(TestResolver::default());
    let adapter = PiRpcAdapter::new(role_reader());
    adapter.set_interaction_resolver(resolver.clone());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &env.ctx(PermissionMode::Hitl),
        )
        .await
        .unwrap();
    assert_eq!(env.log_rows()[0]["no_extensions"], true);

    let (_, answer) = submit_and_terminal(&adapter, &handle, "hitl:read:auto").await;
    assert_eq!(answer, "approved:read");
    assert!(resolver.approvals.lock().unwrap().is_empty());

    for tool in ["bash", "write", "unknown_custom"] {
        for (decision, expected) in [
            ("approve", "approved"),
            ("deny", "continued-after-deny"),
            ("timeout", "continued-after-deny"),
        ] {
            let message = format!("hitl:{tool}:{decision}");
            let (_, answer) = submit_and_terminal(&adapter, &handle, &message).await;
            assert_eq!(answer, format!("{expected}:{tool}"), "{message}");
        }
    }
    let approval_count = resolver.approvals.lock().unwrap().len();
    assert_eq!(approval_count, 9);
    for approval in resolver.approvals.lock().unwrap().iter() {
        assert!(approval.req_id.starts_with("s901/call-"));
        assert_eq!(approval.vendor, ccteam_harness::AgentVendor::Pi);
        assert_eq!(approval.scope, ccteam_harness::ApprovalScope::Once);
    }

    let (_, answer) = submit_and_terminal(&adapter, &handle, "hitl:bash:oversize").await;
    assert_eq!(answer, "continued-after-deny:bash");
    assert_eq!(resolver.approvals.lock().unwrap().len(), approval_count);

    adapter.close_thread(&handle).await.unwrap();
    server.abort();
}

#[tokio::test]
#[serial]
async fn generic_extension_dialogs_resolve_and_teardown_cancels_outstanding() {
    let capture = McpCapture::default();
    let (server, url) = start_fake_mcp(capture).await;
    let env = TestEnv::new(&url);
    let resolver = Arc::new(TestResolver::default());
    let adapter = PiRpcAdapter::new(role_reader());
    adapter.set_interaction_resolver(resolver.clone());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &env.ctx(PermissionMode::Skip),
        )
        .await
        .unwrap();
    let (_, answer) = submit_and_terminal(&adapter, &handle, "dialogs").await;
    assert!(answer.contains("\"value\":\"B\""));
    assert!(answer.contains("\"confirmed\":true"));
    assert!(answer.contains("\"value\":\"input-text\""));
    assert!(answer.contains("\"value\":\"edited-text\""));
    assert_eq!(resolver.dialogs.lock().unwrap().len(), 4);

    let mut submission = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("dialog-hang".to_string()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    submission.release_completion();
    tokio::time::timeout(Duration::from_secs(2), resolver.dialog_started.notified())
        .await
        .expect("hanging dialog reached resolver");
    adapter.close_thread(&handle).await.unwrap();
    assert!(resolver.cancelled_sids.load(Ordering::SeqCst) >= 1);
    assert!(env.log_rows().iter().any(|row| {
        row.pointer("/ui_response/id").and_then(Value::as_str) == Some("dialog-hang")
            && row
                .pointer("/ui_response/cancelled")
                .and_then(Value::as_bool)
                == Some(true)
    }));
    server.abort();
}

#[tokio::test]
#[serial]
async fn daemon_unreachable_is_a_readable_spawn_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let env = TestEnv::new(&format!("http://{addr}/mcp"));
    let adapter = PiRpcAdapter::new(role_reader());
    let error = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &env.ctx(PermissionMode::Skip),
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("Pi bridge extension"), "{message}");
    assert!(message.contains("ccteam bridge unavailable"), "{message}");
}
