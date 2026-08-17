use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, ExecutionMode,
    HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus,
    ToolSurfaceRebuild, TurnId, TurnInput, TurnRouting, TurnSubmission,
};
use ccteam_im::gateway::Gateway;
use ccteam_web::{router_with_state, AppState};
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::net::TcpListener;

#[derive(Debug, Default)]
struct DeadlineWebAdapter {
    submissions: AtomicUsize,
    stall_submit: AtomicBool,
}

#[async_trait]
impl HarnessAdapter for DeadlineWebAdapter {
    fn name(&self) -> &'static str {
        "web-deadline-test"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("web-deadline-{}", ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        })
    }

    async fn submit_turn_routed(
        &self,
        _handle: &ThreadHandle,
        _input: TurnInput,
        _routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        let sequence = self.submissions.fetch_add(1, Ordering::SeqCst) + 1;
        if self.stall_submit.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(TurnSubmission::started(TurnId::new(format!(
            "turn-{sequence}"
        ))))
    }

    fn events(&self, _handle: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(futures::stream::empty())
    }

    fn event_attachment(&self) -> EventAttachment {
        EventAttachment::OneShot
    }

    async fn rebuild_tool_surface(
        &self,
        _handle: &ThreadHandle,
    ) -> Result<ToolSurfaceRebuild, HarnessError> {
        Ok(ToolSurfaceRebuild::RespawnRequired {
            reason: "test double".to_string(),
        })
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "test double".to_string(),
        })
    }

    async fn close_thread(&self, _handle: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn handle_directive(
        &self,
        _handle: &ThreadHandle,
        directive: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Done {
            receipt: directive.name,
        })
    }

    async fn thread_status(&self, _handle: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

async fn spawn(state: AppState) -> SocketAddr {
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

fn assert_gateway_error(body: &Value, expected_code: &str) {
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "the existing human error remains present: {body}"
    );
    assert_eq!(body["error_code"], expected_code);
}

#[tokio::test]
async fn http_queue_and_vendor_deadlines_are_classified_and_recover() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let ccteam_home = tmp.path().join("ccteam-home");
    let projects_root = tmp.path().join("projects");
    let project_dir = projects_root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ccteam_home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("CCTEAM_HOME", &ccteam_home);
    std::env::set_var("CCTEAM_GATEWAY_QUEUE_DEADLINE_MS", "40");
    std::env::set_var("CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS", "40");

    let paths = CcteamPaths {
        root: ccteam_home,
        projects_root,
    };
    let adapter = Arc::new(DeadlineWebAdapter::default());
    let gateway = Arc::new(tokio::sync::Mutex::new(Gateway::new(
        Arc::clone(&adapter) as Arc<dyn HarnessAdapter + Send + Sync>,
        "demo",
        project_dir,
    )));
    let principals = gateway.lock().await.principals();
    let addr = spawn(AppState::new(paths).with_gateway(Arc::clone(&gateway), principals)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{addr}/api/v1");

    let created = client
        .post(format!("{base}/projects/demo/sessions"))
        .json(&serde_json::json!({"role": "", "vendor": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let sid = created.json::<Value>().await.unwrap()["sid"]
        .as_str()
        .unwrap()
        .to_string();

    let held = gateway.lock().await;
    let queued = tokio::time::timeout(
        Duration::from_secs(1),
        client
            .post(format!("{base}/sessions/{sid}/turn"))
            .json(&serde_json::json!({"text": "queued"}))
            .send(),
    )
    .await
    .expect("HTTP queue deadline fires while the lock remains held")
    .unwrap();
    assert_eq!(queued.status(), 502);
    assert_gateway_error(
        &queued.json::<Value>().await.unwrap(),
        "gateway_queue_deadline",
    );
    assert_eq!(adapter.submissions.load(Ordering::SeqCst), 0);
    drop(held);

    std::env::set_var("CCTEAM_GATEWAY_QUEUE_DEADLINE_MS", "1000");
    assert_eq!(
        client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{base}/sessions/{sid}/turn"))
            .json(&serde_json::json!({"text": "healthy after queue"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );

    adapter.stall_submit.store(true, Ordering::SeqCst);
    let vendor = client
        .post(format!("{base}/sessions/{sid}/turn"))
        .json(&serde_json::json!({"text": "slow vendor"}))
        .send()
        .await
        .unwrap();
    assert_eq!(vendor.status(), 502);
    assert_gateway_error(
        &vendor.json::<Value>().await.unwrap(),
        "vendor_submit_timeout",
    );

    adapter.stall_submit.store(false, Ordering::SeqCst);
    assert_eq!(
        client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{base}/sessions/{sid}/turn"))
            .json(&serde_json::json!({"text": "healthy after vendor"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert_eq!(adapter.submissions.load(Ordering::SeqCst), 3);
}
