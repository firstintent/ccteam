//! V0.6.8 F193 — bot-to-bot @mention via daemon-internal mpsc.
//!
//! Background: chat-squad's `hop_limit=3` design supports a bot reply
//! mentioning `@<otherbot>` and getting routed to that other bot, but
//! V0.6.x never wired the path from a bot's outbound reply back into
//! another bot's inbox. Telegram's Bot API doesn't echo bot messages
//! to other bots' `getUpdates` stream, so the mention silently
//! disappeared.
//!
//! F193 closes the loop **purely inside the daemon**:
//! `spawn_outbound_dispatcher` (after the `channel.send` succeeds)
//! delegates to `dispatch_cross_bot_mention`, which:
//!
//! 1. parses the first `@<handle>` in the reply,
//! 2. resolves it through `build_handle_map_from_bots`,
//! 3. checks the self-mention guard (resolved tuple, not handle string),
//! 4. checks `within_hop_budget(hop + 1)`,
//! 5. `try_send`s a synthetic `InboxItem` into the target's `inbox_tx`.
//!
//! This test drives `dispatch_cross_bot_mention` directly with hand-built
//! `BotChannelMap` entries (drained by hand instead of via a live
//! supervisor) so we can assert the routing decisions without standing
//! up tmux + adapters.

use std::path::PathBuf;
use std::sync::Arc;

use ccteam_core::harness::AgentVendor;
use ccteam_im::bot_mpsc::{bot_key, BotChannelMap, BotChannels, InboxItem, OutboundItem};
use ccteam_im::daemon::{dispatch_cross_bot_mention, CrossBotDispatch};
use ccteam_im::outbound::OutboundCursor;
use ccteam_im::BotRegistration;
use tokio::sync::{mpsc, Mutex};

fn reg(slug: &str, role: &str) -> BotRegistration {
    BotRegistration {
        workflow_slug: slug.into(),
        role: role.into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "mock".into(),
        im_chat_id: format!("chat-{slug}"),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    }
}

/// Build a `BotChannelMap` containing one entry per `(slug, role)` in
/// `bots`, returning the inbox receivers so the test can `recv` what
/// the dispatcher pushed.
async fn build_channels(
    bots: &[BotRegistration],
) -> (
    BotChannelMap,
    Vec<(String, String, mpsc::Receiver<InboxItem>)>,
) {
    let map: BotChannelMap = Arc::new(Mutex::new(Default::default()));
    let mut receivers = Vec::with_capacity(bots.len());
    for b in bots {
        let (inbox_tx, inbox_rx) = mpsc::channel::<InboxItem>(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel::<OutboundItem>(64);
        // Cursor doesn't matter for the cross-mention scan — the helper
        // never reads it. Use a nonexistent path so load_from_disk
        // seeds to zero (the constructor's documented behaviour).
        let outbound_cursor =
            OutboundCursor::load_from_disk(PathBuf::from("/nonexistent-cross-bot-test-cursor"));
        map.lock().await.insert(
            bot_key(&b.workflow_slug, &b.role),
            BotChannels {
                inbox_tx,
                outbound_tx,
                outbound_cursor,
            },
        );
        receivers.push((b.workflow_slug.clone(), b.role.clone(), inbox_rx));
    }
    (map, receivers)
}

fn make_outbound(content: &str, hop: u8) -> OutboundItem {
    OutboundItem {
        turn_id: "t-1".into(),
        role: "assistant".into(),
        content: content.into(),
        cursor_after: 1,
        enqueue_unix_ms: 0,
        hop,
    }
}

// ---------------------------------------------------------------------
// 1. Happy path — bot reply `@explorer please help` routes to explorer
//    with hop=1, no IM round-trip.
// ---------------------------------------------------------------------

#[tokio::test]
async fn dispatches_cross_bot_mention_to_target_inbox() {
    let bots = vec![
        reg("reno-squad", "reporter"),
        reg("reno-squad", "explorer"),
        reg("reno-squad", "coordinator"),
    ];
    let (channels, mut receivers) = build_channels(&bots).await;

    // reporter's reply mentions @explorer. hop=0 → next_hop=1.
    let outcome = dispatch_cross_bot_mention(
        &make_outbound("@explorer please help with this", 0),
        "reno-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;

    match outcome {
        CrossBotDispatch::Dispatched {
            to_slug,
            to_role,
            hop,
        } => {
            assert_eq!(to_slug, "reno-squad");
            assert_eq!(to_role, "explorer");
            assert_eq!(hop, 1);
        }
        other => panic!("expected Dispatched, got {other:?}"),
    }

    // explorer's inbox MUST have one synthetic InboxItem.
    let (_, _, explorer_rx) = receivers
        .iter_mut()
        .find(|(_, role, _)| role == "explorer")
        .expect("explorer inbox receiver");
    let synth = explorer_rx
        .try_recv()
        .expect("explorer inbox should have one item");
    assert_eq!(synth.slug, "reno-squad");
    assert_eq!(synth.role, "explorer");
    assert_eq!(synth.hop, 1);
    assert_eq!(synth.payload, "please help with this");
    assert_eq!(
        synth.path,
        PathBuf::new(),
        "synthetic items have empty path"
    );
    assert!(
        synth.cid.starts_with("cross-"),
        "cid should be cross-prefixed for traceability"
    );

    // reporter + coordinator inboxes MUST be empty — no IM round-trip
    // (the dispatcher is daemon-internal; no second `channel.send`
    // happens for the synthesized item, only the original reply went
    // to IM, which lives outside this helper).
    let reporter_rx = receivers
        .iter_mut()
        .find(|(_, role, _)| role == "reporter")
        .map(|(_, _, rx)| rx)
        .unwrap();
    assert!(reporter_rx.try_recv().is_err());
    let coord_rx = receivers
        .iter_mut()
        .find(|(_, role, _)| role == "coordinator")
        .map(|(_, _, rx)| rx)
        .unwrap();
    assert!(coord_rx.try_recv().is_err());
}

// ---------------------------------------------------------------------
// 2. Hop budget — sender hop = MAX_HOPS - 1 → next_hop = MAX_HOPS → drop.
// ---------------------------------------------------------------------

#[tokio::test]
async fn drops_when_hop_budget_would_be_exceeded() {
    use ccteam_im::router::MAX_HOPS;

    let bots = vec![reg("reno-squad", "reporter"), reg("reno-squad", "explorer")];
    let (channels, mut receivers) = build_channels(&bots).await;

    // sender_hop = MAX_HOPS - 1 → next_hop = MAX_HOPS, which fails
    // `within_hop_budget(hop) = hop < MAX_HOPS`.
    let outcome = dispatch_cross_bot_mention(
        &make_outbound("@explorer loop forever", MAX_HOPS - 1),
        "reno-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;

    match outcome {
        CrossBotDispatch::HopExceeded { hop, .. } => assert_eq!(hop, MAX_HOPS),
        other => panic!("expected HopExceeded, got {other:?}"),
    }

    // explorer must have received nothing.
    let (_, _, explorer_rx) = receivers
        .iter_mut()
        .find(|(_, role, _)| role == "explorer")
        .unwrap();
    assert!(
        explorer_rx.try_recv().is_err(),
        "explorer inbox must be empty when hop budget exceeded"
    );
}

// ---------------------------------------------------------------------
// 3. Self-mention guard — reporter replying @reporter must NOT
//    re-trigger itself (would loop).
// ---------------------------------------------------------------------

#[tokio::test]
async fn does_not_route_self_mention() {
    let bots = vec![reg("reno-squad", "reporter"), reg("reno-squad", "explorer")];
    let (channels, mut receivers) = build_channels(&bots).await;

    let outcome = dispatch_cross_bot_mention(
        &make_outbound("@reporter ack received", 0),
        "reno-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;

    assert_eq!(outcome, CrossBotDispatch::SelfMention);

    // Neither inbox got anything.
    for (_, _, rx) in receivers.iter_mut() {
        assert!(rx.try_recv().is_err());
    }
}

// ---------------------------------------------------------------------
// 4. No mention — plain text reply has nothing to scan; fast-path noop.
// ---------------------------------------------------------------------

#[tokio::test]
async fn no_mention_returns_no_mention() {
    let bots = vec![reg("reno-squad", "reporter"), reg("reno-squad", "explorer")];
    let (channels, mut receivers) = build_channels(&bots).await;
    let outcome = dispatch_cross_bot_mention(
        &make_outbound("just a plain assistant reply with no @", 0),
        "reno-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;
    assert_eq!(outcome, CrossBotDispatch::NoMention);
    for (_, _, rx) in receivers.iter_mut() {
        assert!(rx.try_recv().is_err());
    }
}

// ---------------------------------------------------------------------
// 5. Unknown handle — `@nobody` doesn't resolve; no inbox push.
// ---------------------------------------------------------------------

#[tokio::test]
async fn unknown_handle_does_not_dispatch() {
    let bots = vec![reg("reno-squad", "reporter")];
    let (channels, _receivers) = build_channels(&bots).await;
    let outcome = dispatch_cross_bot_mention(
        &make_outbound("@nobody hi", 0),
        "reno-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;
    match outcome {
        CrossBotDispatch::UnknownHandle { handle } => assert_eq!(handle, "nobody"),
        other => panic!("expected UnknownHandle, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// 6. Target not wired — handle resolves but BotChannelMap doesn't have
//    an entry yet (race: target registered after sender's dispatcher
//    spawned, supervisor tick hasn't run ensure_bot_channels for it).
// ---------------------------------------------------------------------

#[tokio::test]
async fn target_not_wired_when_channel_missing() {
    let bots = vec![reg("reno-squad", "reporter"), reg("reno-squad", "explorer")];
    // Channel map only contains reporter, not explorer.
    let only_reporter = vec![reg("reno-squad", "reporter")];
    let (channels, _) = build_channels(&only_reporter).await;
    let outcome = dispatch_cross_bot_mention(
        &make_outbound("@explorer go", 0),
        "reno-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;
    match outcome {
        CrossBotDispatch::TargetNotWired { to_slug, to_role } => {
            assert_eq!(to_slug, "reno-squad");
            assert_eq!(to_role, "explorer");
        }
        other => panic!("expected TargetNotWired, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// 7. Cross-slug collision — two squads both have a `reporter`. Bot in
//    slug A replying `@reporter` MUST NOT loop back to itself; the
//    self-mention guard works on the resolved (slug, role) tuple.
//
//    handle map's collision policy: first-claimant keeps the bare
//    name. Sender = (alpha-squad, reporter) which sorts before
//    (beta-squad, reporter), so `@reporter` resolves to alpha-squad
//    too → self-mention → drop.
// ---------------------------------------------------------------------

#[tokio::test]
async fn self_mention_guard_uses_resolved_tuple_across_slugs() {
    let bots = vec![
        reg("alpha-squad", "reporter"),
        reg("beta-squad", "reporter"),
    ];
    let (channels, mut receivers) = build_channels(&bots).await;
    let outcome = dispatch_cross_bot_mention(
        &make_outbound("@reporter trying to ping the other squad", 0),
        "alpha-squad",
        "reporter",
        &bots,
        &channels,
    )
    .await;
    // `@reporter` resolves to alpha-squad (first claimant by sort);
    // sender IS alpha-squad → self-mention guard fires.
    assert_eq!(outcome, CrossBotDispatch::SelfMention);
    for (_, _, rx) in receivers.iter_mut() {
        assert!(rx.try_recv().is_err());
    }
}
