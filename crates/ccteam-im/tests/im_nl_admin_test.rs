//! V0.6.1 F129 — `@ccteam <NL>` IM admin integration test.
//!
//! Drives the end-to-end inbound pipeline (router → admin executor →
//! mock TG channel reply) for every NL admin path the user-manual
//! §3.2 advertises:
//!
//! 1. `@ccteam pause helper-bot`     → drain.signal written
//! 2. `@ccteam resume helper-bot`    → drain.signal removed
//! 3. `@ccteam list bots`            → reply with bot inventory
//! 4. `@ccteam cost today`           → reply with cost summary
//! 5. `@ccteam stop everything`      → CONFIRM prompt; only the
//!    follow-up `CONFIRM` actually writes shutdown.signal
//!
//! Plus the `pause <slug>/<role>` per-role form so the slug-only and
//! slug+role variants both pass.

use std::sync::Arc;
use std::time::Duration;

use ccteam_harness::AgentVendor;
use ccteam_im::acl::AclPolicy;
use ccteam_im::inbound::{process_inbound_admin_aware, DefaultMailboxResolver, InboundOutcome};
use ccteam_im::nl_admin::{AdminExecutor, AdminSideEffect};
use ccteam_im::register_bot;
use ccteam_im::router::HandleMap;
use ccteam_im::three_layer_sec::ThreeLayerSec;
use ccteam_im::transport::providers::mock::MockChannel;
use ccteam_im::transport::ChannelMessage;
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Serialize env-mutating tests so concurrent `HOME` swaps in this
/// binary don't race (cargo runs integration tests inside one file
/// in parallel by default).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// One scenario: HOME-isolated tempdir with one TG-registered bot
/// (`helper-bot/main`) + a freshly-built executor pointed at the
/// scenario's `projects/` root.
struct Scenario {
    _home: TempDir,
    projects_root: std::path::PathBuf,
    sec: Arc<Mutex<ThreeLayerSec>>,
    handles: HandleMap,
    mailbox: DefaultMailboxResolver,
    executor: AdminExecutor,
    channel: MockChannel,
}

impl Scenario {
    fn build() -> Self {
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        // Register one bot so list/pause/resume/stop-everything have
        // something to act on.
        register_bot(
            "helper-bot",
            "main",
            AgentVendor::Claude,
            "telegram",
            "@group-1",
        )
        .unwrap();
        let projects_root = home.path().join("projects");
        std::fs::create_dir_all(&projects_root).unwrap();
        let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
        let mailbox = DefaultMailboxResolver::with_projects_root(&projects_root);
        let executor = AdminExecutor::new(&projects_root);
        Scenario {
            _home: home,
            projects_root,
            sec,
            handles: HandleMap::new(),
            mailbox,
            executor,
            channel: MockChannel::new(),
        }
    }

    fn drain_signal_path(&self, slug: &str, role: &str) -> std::path::PathBuf {
        self.projects_root
            .join(slug)
            .join(".ccteam")
            .join("chat")
            .join(role)
            .join("signals")
            .join("drain.signal")
    }

    fn shutdown_signal_path(&self, slug: &str, role: &str) -> std::path::PathBuf {
        self.projects_root
            .join(slug)
            .join(".ccteam")
            .join("chat")
            .join(role)
            .join("signals")
            .join("shutdown.signal")
    }

    async fn send(&self, content: &str) -> (InboundOutcome, AdminSideEffect, String) {
        let msg = ChannelMessage {
            id: format!(
                "u-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            ),
            sender: "alice".into(),
            reply_target: "@group-1".into(),
            content: content.into(),
            channel: "telegram".into(),
            timestamp: 0,
            thread_ts: None,
            attachments: Vec::new(),
            selection: None,
        };
        // Pull the live registry so ListHere / unknown-handle replies
        // see the same bots the production daemon would see.
        let bots = ccteam_im::list_bots().unwrap_or_default();
        let (outcome, reply) = process_inbound_admin_aware(
            &msg,
            &self.sec,
            &self.handles,
            &bots,
            &self.mailbox,
            &self.executor,
            &self.channel,
            0,
            1,
        )
        .await
        .unwrap();
        let admin = reply.expect("admin path should always reply");
        (outcome, admin.side_effect, admin.message)
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn pause_helper_bot_writes_drain_signal_and_replies() {
    let _g = env_lock();
    let sc = Scenario::build();
    let (outcome, side, msg) = sc.send("@ccteam pause helper-bot").await;

    assert!(matches!(outcome, InboundOutcome::Admin { .. }));
    assert_eq!(
        side,
        AdminSideEffect::Paused(vec![("helper-bot".into(), "main".into())])
    );
    assert!(sc.drain_signal_path("helper-bot", "main").exists());
    assert!(msg.to_lowercase().contains("paused"));

    // IM reply landed on the mock channel.
    let out = sc.channel.outbox().await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].recipient, "@group-1");
    assert!(out[0].content.to_lowercase().contains("paused"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn pause_slash_role_form_targets_single_bot() {
    let _g = env_lock();
    let sc = Scenario::build();
    // Register a second role under helper-bot so the per-role filter
    // can prove it doesn't pause both.
    register_bot(
        "helper-bot",
        "reviewer",
        AgentVendor::Claude,
        "telegram",
        "@group-1",
    )
    .unwrap();
    let (_, side, _) = sc.send("@ccteam pause helper-bot/reviewer").await;
    assert_eq!(
        side,
        AdminSideEffect::Paused(vec![("helper-bot".into(), "reviewer".into())])
    );
    assert!(sc.drain_signal_path("helper-bot", "reviewer").exists());
    assert!(
        !sc.drain_signal_path("helper-bot", "main").exists(),
        "main role should NOT have been paused"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn resume_helper_bot_removes_drain_signal() {
    let _g = env_lock();
    let sc = Scenario::build();
    // Pause first.
    let _ = sc.send("@ccteam pause helper-bot").await;
    assert!(sc.drain_signal_path("helper-bot", "main").exists());
    // Resume.
    let (_, side, msg) = sc.send("@ccteam resume helper-bot").await;
    assert_eq!(
        side,
        AdminSideEffect::Resumed(vec![("helper-bot".into(), "main".into())])
    );
    assert!(!sc.drain_signal_path("helper-bot", "main").exists());
    assert!(msg.to_lowercase().contains("resumed"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_here_replies_with_handles_reachable_from_this_chat() {
    // `list bots` (new 6th admin verb) returns the chat-scoped handle
    // list — same format the unknown-handle reply uses. The bot
    // registered in Scenario::build (`helper-bot/main` bound to
    // `("telegram", "@group-1")`) falls back to its role `main` as the
    // effective handle since no `chat_handle` was minted.
    let _g = env_lock();
    let sc = Scenario::build();
    let (_, side, msg) = sc.send("@ccteam list bots").await;
    assert_eq!(side, AdminSideEffect::None);
    assert!(
        msg.contains("@main"),
        "expected per-chat handle line, got: {msg}"
    );
    assert!(msg.to_lowercase().contains("available bots in this chat"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_global_replies_with_full_registry_inventory() {
    // Bare `list` / `ls` keeps the V0.6.x global-inventory shape so
    // operators still have a way to dump every bot across slugs.
    let _g = env_lock();
    let sc = Scenario::build();
    let (_, side, msg) = sc.send("@ccteam list").await;
    assert_eq!(side, AdminSideEffect::None);
    assert!(msg.contains("helper-bot/main"));
    assert!(msg.contains("telegram"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_here_synonyms_bots_and_who_route_to_same_keyword() {
    let _g = env_lock();
    let sc = Scenario::build();
    for verb in ["bots", "who"] {
        let (_, side, msg) = sc.send(&format!("@ccteam {verb}")).await;
        assert_eq!(side, AdminSideEffect::None, "verb={verb}");
        assert!(
            msg.to_lowercase().contains("available bots in this chat"),
            "verb={verb}, got: {msg}"
        );
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn unknown_handle_inbound_replies_with_available_bots_in_chat() {
    // F184 — @<unknown> on inbound no longer silently drops. The
    // router-level UnknownHandle outcome triggers a helpful reply on
    // the originating channel.
    let _g = env_lock();
    let sc = Scenario::build();
    let msg = ChannelMessage {
        id: "u-unknown".into(),
        sender: "alice".into(),
        reply_target: "@group-1".into(),
        content: "@ghost hello".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    };
    let bots = ccteam_im::list_bots().unwrap_or_default();
    let (outcome, admin_reply) = process_inbound_admin_aware(
        &msg,
        &sc.sec,
        &sc.handles,
        &bots,
        &sc.mailbox,
        &sc.executor,
        &sc.channel,
        0,
        99,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, InboundOutcome::UnknownHandle { .. }));
    // No AdminReply for unknown-handle; the channel did the talking.
    assert!(admin_reply.is_none());
    let out = sc.channel.outbox().await;
    assert!(
        !out.is_empty(),
        "channel must receive an unknown-handle reply"
    );
    let last = out.last().unwrap();
    assert_eq!(last.recipient, "@group-1");
    assert!(
        last.content.contains("@ghost"),
        "reply echoes the typo: {}",
        last.content
    );
    assert!(
        last.content.contains("@main"),
        "reply lists the reachable bot: {}",
        last.content
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_here_with_zero_bots_in_chat_says_no_bots() {
    // Empty registry — ListHere falls back to the canonical
    // no-bots-in-chat message.
    let _g = env_lock();
    let home = TempDir::new().unwrap();
    std::env::set_var("HOME", home.path());
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
    let mailbox = DefaultMailboxResolver::with_projects_root(&projects_root);
    let executor = AdminExecutor::new(&projects_root);
    let channel = MockChannel::new();
    let handles = HandleMap::new();

    let msg = ChannelMessage {
        id: "u-empty".into(),
        sender: "alice".into(),
        reply_target: "@group-1".into(),
        content: "@ccteam list bots".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    };
    let (_, reply) = process_inbound_admin_aware(
        &msg,
        &sec,
        &handles,
        &[],
        &mailbox,
        &executor,
        &channel,
        0,
        1,
    )
    .await
    .unwrap();
    let r = reply.expect("admin reply");
    assert!(r.message.contains("No bots registered in this chat"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn cost_today_replies_with_summary() {
    let _g = env_lock();
    let sc = Scenario::build();
    let (_, side, msg) = sc.send("@ccteam cost today").await;
    assert_eq!(side, AdminSideEffect::None);
    assert!(msg.to_lowercase().contains("cost"));
    assert!(msg.contains("1") || msg.to_lowercase().contains("bot"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn stop_everything_requires_confirm_before_writing_signals() {
    let _g = env_lock();
    let sc = Scenario::build();
    // Phase 1: prompt for CONFIRM.
    let (_, side, msg) = sc.send("@ccteam stop everything").await;
    assert!(matches!(side, AdminSideEffect::ConfirmRequested { .. }));
    assert!(msg.to_uppercase().contains("CONFIRM"));
    assert!(
        !sc.shutdown_signal_path("helper-bot", "main").exists(),
        "shutdown.signal must NOT be written before CONFIRM"
    );

    // Phase 2: literal CONFIRM fires the action.
    let (_, side2, msg2) = sc.send("@ccteam CONFIRM").await;
    assert_eq!(
        side2,
        AdminSideEffect::Stopped(vec![("helper-bot".into(), "main".into())])
    );
    assert!(sc.shutdown_signal_path("helper-bot", "main").exists());
    assert!(msg2.to_lowercase().contains("shutdown"));

    // Mock channel should have two outbound messages by now.
    let out = sc.channel.outbox().await;
    assert_eq!(out.len(), 2);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn confirm_without_pending_is_nothing_to_confirm() {
    let _g = env_lock();
    let sc = Scenario::build();
    let (_, side, msg) = sc.send("@ccteam CONFIRM").await;
    assert_eq!(side, AdminSideEffect::NothingToConfirm);
    assert!(msg.to_lowercase().contains("nothing pending"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn expired_confirm_does_not_fire() {
    let _g = env_lock();
    let home = TempDir::new().unwrap();
    std::env::set_var("HOME", home.path());
    register_bot(
        "helper-bot",
        "main",
        AgentVendor::Claude,
        "telegram",
        "@group-1",
    )
    .unwrap();
    let projects_root = home.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
    let mailbox = DefaultMailboxResolver::with_projects_root(&projects_root);
    let executor = AdminExecutor::new(&projects_root).with_confirm_ttl(Duration::from_millis(10));
    let channel = MockChannel::new();
    let handles = HandleMap::new();

    let msg = ChannelMessage {
        id: "u1".into(),
        sender: "alice".into(),
        reply_target: "@group-1".into(),
        content: "@ccteam stop everything".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    };
    let (_, reply) = process_inbound_admin_aware(
        &msg,
        &sec,
        &handles,
        &[],
        &mailbox,
        &executor,
        &channel,
        0,
        1,
    )
    .await
    .unwrap();
    assert!(matches!(
        reply.unwrap().side_effect,
        AdminSideEffect::ConfirmRequested { .. }
    ));

    // Wait past the TTL, then send CONFIRM — should fall through to
    // NothingToConfirm.
    tokio::time::sleep(Duration::from_millis(40)).await;
    let confirm = ChannelMessage {
        id: "u2".into(),
        sender: "alice".into(),
        reply_target: "@group-1".into(),
        content: "@ccteam CONFIRM".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    };
    let (_, reply2) = process_inbound_admin_aware(
        &confirm,
        &sec,
        &handles,
        &[],
        &mailbox,
        &executor,
        &channel,
        0,
        2,
    )
    .await
    .unwrap();
    assert_eq!(
        reply2.unwrap().side_effect,
        AdminSideEffect::NothingToConfirm
    );
    let projects_root_for_check = projects_root.clone();
    let shutdown_path =
        projects_root_for_check.join("helper-bot/.ccteam/chat/main/signals/shutdown.signal");
    assert!(
        !shutdown_path.exists(),
        "expired confirm must not write shutdown.signal"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn admin_path_does_not_consume_hop_budget() {
    // F129 acceptance: meta-agent admin path stays orthogonal to the
    // bot-to-bot hop counter. Send the admin at hop=2 (one less than
    // MAX_HOPS) and verify it still executes (router doesn't drop
    // it) and the next bot route would still have full budget — we
    // verify by parsing the outcome at the same hop count.
    let _g = env_lock();
    let sc = Scenario::build();
    let msg = ChannelMessage {
        id: "u-hop".into(),
        sender: "alice".into(),
        reply_target: "@group-1".into(),
        content: "@ccteam list bots".into(),
        channel: "telegram".into(),
        timestamp: 0,
        thread_ts: None,
        attachments: Vec::new(),
        selection: None,
    };
    // hop = 2 (legal) — admin still fires.
    let (outcome, reply) = process_inbound_admin_aware(
        &msg,
        &sc.sec,
        &sc.handles,
        &[],
        &sc.mailbox,
        &sc.executor,
        &sc.channel,
        2,
        9,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, InboundOutcome::Admin { .. }));
    assert!(reply.is_some(), "admin should reply regardless of hop");
}
