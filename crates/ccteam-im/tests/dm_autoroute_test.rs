//! V0.6.1 F135 — DM auto-route integration test.
//!
//! Before F135 the router dropped any message lacking an `@<handle>`
//! mention. For Pocket Assistant DM UX a user typing "hi" must reach
//! the bound bot without ceremony. F135 adds
//! [`ccteam_im::inbound::auto_route_dm_mention`] which prepends
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
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_im::daemon::{run_daemon_with_shutdown, AdapterFactory, ChannelMap, DaemonArgs};
use ccteam_im::inbound::{
    auto_route_dm_mention, format_ambiguous_dm_reply, has_at_mention, DmRoutingHint,
};
use ccteam_im::register_bot;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::{Channel, ChannelMessage};
use ccteam_im::{list_bots, BotRegistration};
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
        attachments: Vec::new(),
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
    let hint = auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "@lead hi");
    assert_eq!(
        hint,
        DmRoutingHint::Routed {
            to_role: "lead".into()
        }
    );
}

#[test]
fn auto_route_dm_skips_when_zero_bots() {
    let bots: Vec<BotRegistration> = vec![];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    let hint = auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi", "no bots → no mutation");
    assert_eq!(hint, DmRoutingHint::NoMatch);
}

#[test]
fn auto_route_dm_ambiguous_when_multiple_bots_share_chat_id() {
    // V0.6.8 F194 — the V0.6.x silent-drop path now returns
    // DmRoutingHint::Ambiguous so the daemon can reply with an
    // available-bots hint instead of dropping the message.
    let bots = vec![
        mk_bot("dev-foo", "lead", "telegram", "chat-1"),
        mk_bot("dev-bar", "reviewer", "telegram", "chat-1"),
    ];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    let hint = auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(
        msg.content, "hi",
        "ambiguous → message untouched (caller short-circuits with a reply)"
    );
    match hint {
        DmRoutingHint::Ambiguous { available } => {
            assert_eq!(
                available,
                vec!["lead".to_string(), "reviewer".to_string()],
                "available handles sorted alphabetically"
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn auto_route_dm_ambiguous_three_bots_lists_all_handles() {
    // F194 — same trigger as above but with three bots to confirm the
    // hint covers all reachable handles (not just two).
    let bots = vec![
        mk_bot("dev-a", "alice", "telegram", "chat-1"),
        mk_bot("dev-b", "bob", "telegram", "chat-1"),
        mk_bot("dev-c", "carol", "telegram", "chat-1"),
    ];
    let mut msg = mk_msg("hello team", "chat-1", "telegram");
    let hint = auto_route_dm_mention(&mut msg, &bots);
    match hint {
        DmRoutingHint::Ambiguous { available } => {
            assert_eq!(available.len(), 3);
            assert!(available.contains(&"alice".into()));
            assert!(available.contains(&"bob".into()));
            assert!(available.contains(&"carol".into()));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn format_ambiguous_dm_reply_renders_handles() {
    let text = format_ambiguous_dm_reply(&["alice".into(), "bob".into()]);
    assert_eq!(text, "Multiple bots in this chat. Specify one: @alice @bob");
}

#[test]
fn format_ambiguous_dm_reply_handles_empty_race() {
    // Race: bots unregistered between probe and reply → graceful
    // fallback so the user still gets *something*.
    let text = format_ambiguous_dm_reply(&[]);
    assert_eq!(text, "No bots available in this chat.");
}

#[test]
fn auto_route_dm_skips_when_message_already_has_mention() {
    let bots = vec![mk_bot("dev-foo", "lead", "telegram", "chat-1")];
    let mut msg = mk_msg("@lead already", "chat-1", "telegram");
    let hint = auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(
        msg.content, "@lead already",
        "existing @ → idempotent (no double-prepend)"
    );
    // We treat "user typed their own @" identically to NoMatch — the
    // router resolves the explicit mention itself.
    assert_eq!(hint, DmRoutingHint::NoMatch);
}

#[test]
fn auto_route_dm_filters_by_platform_and_chat_id() {
    // Right chat_id, wrong platform → no match.
    let bots = vec![mk_bot("dev-foo", "lead", "slack", "chat-1")];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    let hint = auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi");
    assert_eq!(hint, DmRoutingHint::NoMatch);

    // Right platform, wrong chat_id → no match.
    let bots = vec![mk_bot("dev-foo", "lead", "telegram", "chat-OTHER")];
    let mut msg = mk_msg("hi", "chat-1", "telegram");
    let hint = auto_route_dm_mention(&mut msg, &bots);
    assert_eq!(msg.content, "hi");
    assert_eq!(hint, DmRoutingHint::NoMatch);
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
        attachments: Vec::new(),
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
        max_runtime: Some(Duration::from_millis(1200)),
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
async fn daemon_dm_multiple_bots_same_chat_id_replies_with_ambiguity_hint() {
    // V0.6.8 F194 — what used to be a silent drop is now an active
    // hint: the daemon answers with the available-bots list so the
    // user knows their message was received and how to address it.
    // R1 (no progress.jsonl write for this synthetic reply) + R12 (no
    // turn submitted) still hold: only the IM channel.send fires.
    let _g = env_lock();
    let home = isolate_home();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();

    // Two bots bound to the same telegram chat_id → group-style: V0.6.8
    // F194 surfaces a hint reply instead of dropping silently.
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
        attachments: Vec::new(),
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
        max_runtime: Some(Duration::from_millis(800)),
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

    assert_eq!(
        adapter.submits.load(Ordering::SeqCst),
        0,
        "group with 2 bots + no @ must NOT submit a turn (got submits={})",
        adapter.submits.load(Ordering::SeqCst)
    );

    let outbox = mock.outbox().await;
    let hints: Vec<&ccteam_im::transport::SendMessage> = outbox
        .iter()
        .filter(|m| m.content.starts_with("Multiple bots in this chat."))
        .collect();
    assert_eq!(
        hints.len(),
        1,
        "F194: expected exactly 1 ambiguity-hint reply, got {} (outbox: {outbox:?})",
        hints.len()
    );
    let text = &hints[0].content;
    assert!(
        text.contains("@lead") && text.contains("@reviewer"),
        "F194: hint must list both bots' handles, got {text:?}"
    );
    assert_eq!(hints[0].recipient, "chat-group");
}
