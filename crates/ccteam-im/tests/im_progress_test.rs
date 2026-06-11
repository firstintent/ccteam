//! V0.8.4 P1 (B1) — IM progress status messages.
//!
//! Drives the full daemon (`MockChannel` → gateway → event pump →
//! `spawn_gateway_event_consumer`) with a scripted adapter that emits a
//! realistic Claude/Codex event sequence, and asserts the progress UX:
//!
//! - tool / reasoning steps fold into **one** status message that is
//!   *edited* in place (not one ping per step);
//! - the final answer is a separate **new** message;
//! - `CCTEAM_IM_PROGRESS=off` falls back to answers-only;
//! - Codex streaming `ItemUpdated{AgentMessage}` deltas are NOT sent as
//!   their own answers.
//!
//! These tests mutate process env (`CCTEAM_IM_PROGRESS*`), so they live
//! in their own integration binary and serialize via `env_lock`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, ThreadStatus,
    TurnId, TurnInput,
};
use ccteam_im::daemon::{run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs};
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::Channel;
use futures::stream::BoxStream;
use tempfile::TempDir;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn isolate_home() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("HOME", tmp.path());
    tmp
}

/// Adapter whose `submit_turn` enqueues a fixed script of `ThreadEvent`s
/// onto the `events()` stream, modelling one turn's transcript.
#[derive(Debug)]
struct ScriptedAdapter {
    script: Vec<ThreadEvent>,
    events: Arc<tokio::sync::Mutex<VecDeque<ThreadEvent>>>,
    starts: AtomicUsize,
    submits: AtomicUsize,
}

impl ScriptedAdapter {
    fn new(script: Vec<ThreadEvent>) -> Self {
        Self {
            script,
            events: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
            starts: AtomicUsize::new(0),
            submits: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl HarnessAdapter for ScriptedAdapter {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }
    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("scripted-{}-{}-{}", ctx.slug, spec.role, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let mut q = self.events.lock().await;
        for ev in &self.script {
            q.push_back(ev.clone());
        }
        Ok(TurnId::new("scripted-turn"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let events = Arc::clone(&self.events);
        Box::pin(futures::stream::unfold((), move |_| {
            let events = Arc::clone(&events);
            async move {
                loop {
                    if let Some(evt) = events.lock().await.pop_front() {
                        return Some((evt, ()));
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }))
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "stub".into(),
        })
    }
    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn handle_directive(
        &self,
        _h: &ThreadHandle,
        _d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Rejected {
            reason: "test double".to_string(),
        })
    }

    async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

fn tool_started(id: &str, name: &str, args: serde_json::Value) -> ThreadEvent {
    ThreadEvent::ItemStarted {
        item: ThreadItem {
            id: id.into(),
            details: ThreadItemDetails::ToolCall {
                name: name.into(),
                args,
            },
        },
    }
}

fn tool_completed(id: &str, name: &str) -> ThreadEvent {
    ThreadEvent::ItemCompleted {
        item: ThreadItem {
            id: id.into(),
            details: ThreadItemDetails::ToolCall {
                name: name.into(),
                args: serde_json::json!("result"),
            },
        },
    }
}

fn reasoning(id: &str, text: &str) -> ThreadEvent {
    ThreadEvent::ItemUpdated {
        item: ThreadItem {
            id: id.into(),
            details: ThreadItemDetails::Reasoning(text.into()),
        },
    }
}

fn answer(id: &str, text: &str) -> ThreadEvent {
    ThreadEvent::ItemCompleted {
        item: ThreadItem {
            id: id.into(),
            details: ThreadItemDetails::AgentMessage(text.into()),
        },
    }
}

fn agent_delta(id: &str, text: &str) -> ThreadEvent {
    ThreadEvent::ItemUpdated {
        item: ThreadItem {
            id: id.into(),
            details: ThreadItemDetails::AgentMessage(text.into()),
        },
    }
}

fn is_status(text: &str) -> bool {
    ["⏳", "💭", "✍️", "✅"].iter().any(|p| text.starts_with(p))
}

/// Run the daemon over one `/new` + one trigger message with `script` as
/// the turn's events. Returns the mock channel for outbox/edits asserts.
async fn run_scripted(script: Vec<ThreadEvent>) -> Arc<MockChannel> {
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    let mock = Arc::new(MockChannel::new());
    for (id, content) in [("m-1", "/new claude helper"), ("m-2", "trigger turn")] {
        mock.push(ccteam_im::transport::ChannelMessage {
            id: id.into(),
            sender: "alice".into(),
            reply_target: "chat-1".into(),
            content: content.into(),
            channel: "telegram".into(),
            timestamp: 0,
            thread_ts: None,
            attachments: Vec::new(),
            selection: None,
        })
        .await;
    }

    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(ScriptedAdapter::new(script));
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_, _| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };
    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root),
        max_runtime: Some(Duration::from_millis(900)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
        extra_channels: None,
        ..Default::default()
    };
    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();
    // keep `home` alive until the daemon finished
    drop(home);
    mock
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_edits_one_status_message_not_spam() {
    let _g = env_lock();
    std::env::set_var("CCTEAM_IM_PROGRESS", "on");
    std::env::set_var("CCTEAM_IM_PROGRESS_THROTTLE_MS", "0");

    let mock = run_scripted(vec![
        tool_started("t1", "Bash", serde_json::json!({"command": "cargo test"})),
        tool_completed("t1", "Bash"),
        tool_started("t2", "Read", serde_json::json!({"file_path": "/a"})),
        reasoning("r1", "let me think"),
        answer("a1", "done"),
    ])
    .await;

    let outbox: Vec<String> = mock.outbox().await.into_iter().map(|m| m.content).collect();
    let edits = mock.edits().await;

    // Exactly one *status* message was newly sent (the seed) — progress
    // is folded, not one ping per step.
    let status_sends = outbox.iter().filter(|c| is_status(c)).count();
    assert_eq!(
        status_sends, 1,
        "expected exactly one status seed in outbox, got: {outbox:?}"
    );
    // The answer is its own (new) message.
    assert!(
        outbox.iter().any(|c| c == "done"),
        "answer not delivered as a new message: {outbox:?}"
    );
    // V0.8.4 P1 (F1): the machine-ish "submitted … turn …" ack must be
    // folded away — the turn yields exactly 2 NEW messages (status seed +
    // answer) on top of the one-off `/new` reply; everything else is an
    // edit. (Counting only `is_status` before missed the ack regression.)
    assert!(
        !outbox.iter().any(|c| c.starts_with("submitted")),
        "submit ack must be folded away, got: {outbox:?}"
    );
    assert_eq!(
        outbox.len(),
        3,
        "expected `created` + status seed + answer (no ack), got: {outbox:?}"
    );
    // The status was edited (≥1 edit), and finalized to a ✅ summary.
    assert!(!edits.is_empty(), "status message was never edited");
    assert!(
        edits.iter().any(|(_, c)| c.starts_with("✅ done")),
        "status was not finalized, edits: {edits:?}"
    );
    // A tool count surfaced in some status text.
    assert!(
        outbox
            .iter()
            .chain(edits.iter().map(|(_, c)| c))
            .any(|c| c.contains("bash ×1")),
        "no folded tool count appeared"
    );
    std::env::remove_var("CCTEAM_IM_PROGRESS");
    std::env::remove_var("CCTEAM_IM_PROGRESS_THROTTLE_MS");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_off_sends_only_answers() {
    let _g = env_lock();
    std::env::set_var("CCTEAM_IM_PROGRESS", "off");
    std::env::set_var("CCTEAM_IM_PROGRESS_THROTTLE_MS", "0");

    let mock = run_scripted(vec![
        tool_started("t1", "Bash", serde_json::json!({"command": "cargo test"})),
        tool_completed("t1", "Bash"),
        answer("a1", "done"),
    ])
    .await;

    let outbox: Vec<String> = mock.outbox().await.into_iter().map(|m| m.content).collect();
    let edits = mock.edits().await;

    assert!(edits.is_empty(), "no edits expected when progress is off");
    assert!(
        !outbox.iter().any(|c| is_status(c)),
        "no status messages expected when off: {outbox:?}"
    );
    assert!(
        outbox.iter().any(|c| c == "done"),
        "answer must still be delivered: {outbox:?}"
    );
    std::env::remove_var("CCTEAM_IM_PROGRESS");
    std::env::remove_var("CCTEAM_IM_PROGRESS_THROTTLE_MS");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_streaming_delta_not_sent_as_answer() {
    let _g = env_lock();
    std::env::set_var("CCTEAM_IM_PROGRESS", "on");
    std::env::set_var("CCTEAM_IM_PROGRESS_THROTTLE_MS", "0");

    let mock = run_scripted(vec![
        agent_delta("d1", "par"),
        agent_delta("d1", "partial ans"),
        answer("a1", "full answer"),
    ])
    .await;

    let outbox: Vec<String> = mock.outbox().await.into_iter().map(|m| m.content).collect();

    // No streaming delta leaked as its own answer message.
    assert!(
        !outbox.iter().any(|c| c == "par" || c == "partial ans"),
        "codex delta was sent as a standalone message: {outbox:?}"
    );
    // The final answer is delivered exactly once.
    assert_eq!(
        outbox.iter().filter(|c| *c == "full answer").count(),
        1,
        "final answer not delivered exactly once: {outbox:?}"
    );
    std::env::remove_var("CCTEAM_IM_PROGRESS");
    std::env::remove_var("CCTEAM_IM_PROGRESS_THROTTLE_MS");
}
