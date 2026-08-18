use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, ExecutionMode,
    HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus,
    ToolSurfaceRebuild, TurnId, TurnInput, TurnRouting, TurnSubmission,
};
use ccteam_im::gateway::Gateway;
use ccteam_im::latency::{gateway_lock, gateway_lock_metrics};
use futures::stream::{self, BoxStream};

struct NoopAdapter;

#[async_trait::async_trait]
impl HarnessAdapter for NoopAdapter {
    fn name(&self) -> &'static str {
        "lock-metrics-test"
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
            identity: ctx.sid.clone(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        })
    }

    async fn submit_turn(
        &self,
        _handle: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("turn-lock-test"))
    }

    async fn submit_turn_routed(
        &self,
        handle: &ThreadHandle,
        input: TurnInput,
        _routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        self.submit_turn(handle, input)
            .await
            .map(TurnSubmission::started)
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

    fn events(&self, _handle: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::empty())
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

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn long_gateway_hold_warns_and_moves_p99() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let captured = Arc::clone(&captured);
        move || CaptureWriter(Arc::clone(&captured))
    };
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(writer)
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let gateway = Arc::new(tokio::sync::Mutex::new(Gateway::new(
        Arc::new(NoopAdapter),
        "demo",
        dir.path(),
    )));
    {
        let _guard = gateway_lock(&gateway, "test.long_hold").await;
        tokio::time::sleep(Duration::from_millis(275)).await;
    }

    let metrics = gateway_lock_metrics();
    assert_eq!(metrics.wait.count, 1);
    assert_eq!(metrics.hold.count, 1);
    assert!(metrics.hold.p99_us >= 250_000, "metrics: {metrics:?}");

    let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("gateway mutex held too long"), "logs: {logs}");
    assert!(logs.contains("test.long_hold"), "logs: {logs}");
    assert!(logs.contains("hold_ms"), "logs: {logs}");
}
