//! V0.6.1 F135 — DM auto-route integration test.
//!
//! Before F135 the router dropped any message lacking an `@<handle>`
//! mention. For Pocket Assistant DM UX a user typing "hi" must reach
//! the bound bot without ceremony. F135 adds
//! [`ccteam_imd::inbound::auto_route_dm_mention`] which prepends
//! `@<role> ` when exactly one registered bot owns the
//! `(channel, reply_target)` pair, falling through unchanged otherwise.
//!
//! Three scenarios covered:
//! 1. **1 bot bound to chat_id** → auto-prepend, supervisor receives.
//! 2. **0 bots bound** → no prepend, router drops.
//! 3. **2 bots bound (group-style)** → no prepend, router drops; users
//!    must explicit-@ the target.
//! 4. **existing `@` in content** → no double-prepend (idempotent).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_imd::daemon::{run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs};
use ccteam_imd::inbound::{auto_route_dm_mention, has_at_mention};
use ccteam_imd::register_bot;
use ccteam_imd::transport::providers::mock::MockChannel;
use ccteam_imd::transport::{Channel, ChannelMessage};
use ccteam_imd::{list_bots, BotRegistration};
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

fn mk_msg(content: &str, reply_target: &str, channel: &str) -> ChannelMessage {
    ChannelMessage {
        id: "m1".into(),
        sender: "alice".into(),
        reply_target: reply_target.into(),
        content: content.into(),
        channel: channel.into(),
        timestamp: 0,
        thread_ts: None,
    }
}

fn mk_bot(slug: &str, role: &str, platform: &str, chat_id: &str) -> BotRegistration {
    BotRegistration {
        workflow_slug: slug.into(),
        role: role.into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: platform.into(),
        im_chat_id: chat_id.into(),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    }
}

// ----- unit-style helper tests (no daemon, no HOME mutation) ---------

#[test]
fn has_at_mention_detects_handle() {
    assert!(has_at_mention("@lead hi"));
    assert!(has_at_mention("hello @lead"));
    assert!(has_at_mention("@bot_1-foo go"));
}

#[test]
fn has_at_mention_ignores_bare_at() {
    // `@` followed by space → no handle char → not a mention.
    assert!(!has_at_mention("@ hi"));
    // `@` at end-of-string → no handle char.
    assert!(!has_at_mention("trailing @"));
    // No `@` at all.
    assert!(!has_at_mention("just a normal sentence"));
}

#[test]
fn auto_route_dm_prepends_for_single_match() {
    let bots = vec![mk_bot("dev-foo", "lead", "telegram", "chat-1")];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "@lead hi");
}

#[test]
fn auto_route_dm_skips_when_zero_bots() {
    let bots: Vec<BotRegistration> = vec![];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi", "no bots → no mutation");
}

#[test]
fn auto_route_dm_skips_when_multiple_bots_share_chat_id() {
    let bots = vec![
        mk_bot("dev-foo", "lead", "telegram", "chat-1"),
        mk_bot("dev-bar", "reviewer", "telegram", "chat-1"),
    ];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi", "group-style → require explicit @");
}

#[test]
fn auto_route_dm_skips_when_message_already_has_mention() {
    let bots = vec![mk_bot("dev-foo", "lead", "telegram", "chat-1")];
    let mut msg = mk_msg("@lead already", "chat-1", "telegram");
    auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(
        msg.content, "@lead already",
        "existing @ → idempotent (no double-prepend)"
    );
}

#[test]
fn auto_route_dm_filters_by_platform_and_chat_id() {
    // Right chat_id, wrong platform → no match.
    let bots = vec![mk_bot("dev-foo", "lead", "slack", "chat-1")];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi");

    // Right platform, wrong chat_id → no match.
    let bots = vec![mk_bot("dev-foo", "lead", "telegram", "chat-OTHER")];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi");
}

// ----- end-to-end: daemon + MockChannel + registered bot ------------

#[derive(Debug, Default)]
struct StubAdapter {
    submits: AtomicUsize,
    submitted: tokio::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl HarnessAdapter for StubAdapter {
    fn name(&self) -> &'static str {
        "f135-stub"
    }
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }
    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("stub-{}-{}", ctx.slug, spec.role),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }
    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        if let TurnInput::UserText(s) = input {
            self.submitted.lock().await.push(s);
        }
        Ok(TurnId::new("stub"))
    }
    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(futures::stream::empty())
    }
    async fn resume_thread(&self, _id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "stub".into(),
        })
    }
    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_dm_no_at_mention_auto_routes_to_single_bot() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    register_bot(
        "dev-foo",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "chat-77",
    )
    .unwrap();

    // Sanity: list_bots from isolated HOME returns exactly the one bot.
    let bots = list_bots().unwrap();
    assert_eq!(bots.len(), 1, "expected 1 registered bot in isolated HOME");

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "m1".into(),
        sender: "alice".into(),
        reply_target: "chat-77".into(),
        content: "hi".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(StubAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(1200)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };

    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    assert_eq!(
        adapter.submits.load(Ordering::SeqCst),
        1,
        "DM 'hi' (no @) must auto-route → 1 submit_turn"
    );
    let submitted = adapter.submitted.lock().await.clone();
    assert_eq!(
        submitted,
        vec!["hi".to_string()],
        "router strips the synthetic @lead prefix → just 'hi'"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_dm_multiple_bots_same_chat_id_drops() {
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    // Two bots bound to the same telegram chat_id → group-style: must
    // require explicit @ to disambiguate.
    register_bot(
        "dev-foo",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "chat-group",
    )
    .unwrap();
    register_bot(
        "dev-bar",
        "reviewer",
        AgentVendor::Claude,
        "telegram",
        "chat-group",
    )
    .unwrap();

    let mock = Arc::new(MockChannel::new());
    mock.push(ChannelMessage {
        id: "m1".into(),
        sender: "alice".into(),
        reply_target: "chat-group".into(),
        content: "hi everyone".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
    })
    .await;
    let mut channels: ChannelMap = std::collections::HashMap::new();
    channels.insert(
        "telegram".to_string(),
        mock.clone() as Arc<dyn Channel + Send + Sync>,
    );

    let adapter = Arc::new(StubAdapter::default());
    let adapter_factory: AdapterFactory = {
        let cloned = adapter.clone();
        Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
    };

    let args = DaemonArgs {
        credentials: None,
        registry: Some(projects_root.clone()),
        tick: Duration::from_millis(50),
        max_runtime: Some(Duration::from_millis(800)),
        adapter_factory: Some(adapter_factory),
        channels_override: Some(channels),
    };

    run_daemon_with_shutdown(args, async {
        futures::future::pending::<()>().await;
    })
    .await
    .unwrap();

    assert_eq!(
        adapter.submits.load(Ordering::SeqCst),
        0,
        "group with 2 bots + no @ must drop (got submits={})",
        adapter.submits.load(Ordering::SeqCst)
    );
}
