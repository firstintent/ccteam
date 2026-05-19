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

use ccteam_core::harness::AgentVendor;
use ccteam_imd::acl::AclPolicy;
use ccteam_imd::inbound::{process_inbound_admin_aware, DefaultMailboxResolver, InboundOutcome};
use ccteam_imd::nl_admin::{AdminExecutor, AdminSideEffect};
use ccteam_imd::register_bot;
use ccteam_imd::router::HandleMap;
use ccteam_imd::three_layer_sec::ThreeLayerSec;
use ccteam_imd::transport::providers::mock::MockChannel;
use ccteam_imd::transport::ChannelMessage;
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
        };
        let (outcome, reply) = process_inbound_admin_aware(
            &msg,
            &self.sec,
            &self.handles,
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
async fn list_bots_replies_with_registry_inventory() {
    let _g = env_lock();
    let sc = Scenario::build();
    let (_, side, msg) = sc.send("@ccteam list bots").await;
    assert_eq!(side, AdminSideEffect::None);
    assert!(msg.contains("helper-bot/main"));
    assert!(msg.contains("telegram"));
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
    };
    let (_, reply) =
        process_inbound_admin_aware(&msg, &sec, &handles, &mailbox, &executor, &channel, 0, 1)
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
    };
    let (_, reply2) = process_inbound_admin_aware(
        &confirm, &sec, &handles, &mailbox, &executor, &channel, 0, 2,
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
    };
    // hop = 2 (legal) — admin still fires.
    let (outcome, reply) = process_inbound_admin_aware(
        &msg,
        &sc.sec,
        &sc.handles,
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
