//! Main daemon event loop.
//!
//! Composes credentials → Channel listeners → inbound pipeline →
//! supervisor → outbound tailers. The loop is `tokio::select`-driven
//! across:
//!
//! - one inbound mpsc receiver (multiplexed across active Channels),
//! - a supervisor tick timer,
//! - a SIGTERM future for graceful shutdown,
//! - an optional max-runtime watchdog (test-only — production is `0`).
//!
//! Wave 3 follow-up to Wave 2 skeleton: the supervisor tick now
//! actually drives [`BotSupervisor::apply_action`] against the live
//! per-bot state, so `decide(...) = Spawn` reaches `start_thread`,
//! `Restart` reaches `close_thread` + `start_thread`, and `Shutdown`
//! reaches `close_thread`. One [`BotSupervisor`] is built per
//! `BotRegistration` on first sight via the daemon's
//! [`AdapterFactory`] (defaults to `ClaudeTuiAdapter` for
//! `AgentVendor::Claude`; tests inject a stub).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use ccteam_core::execution::{ClaudeTuiAdapter, CodexExecAdapter};
use ccteam_core::harness::{AgentVendor, HarnessAdapter};
use tokio::sync::{mpsc, Mutex};

use crate::acl::AclPolicy;
use crate::bot_mpsc::{bot_key, BotChannelMap, BotChannels, InboxItem, OutboundItem, CHANNEL_BUF};
use crate::credentials::{self, Credentials};
use crate::inbound::{
    auto_route_dm_mention, parse_envelope, process_inbound_admin_aware, DefaultMailboxResolver,
    InboundOutcome, MailboxResolver,
};
use crate::latency::now_unix_ms;
use crate::nl_admin::AdminExecutor;
use crate::outbound;
use crate::router::HandleMap;
use crate::supervisor::{self, BotSupervisor};
use crate::three_layer_sec::ThreeLayerSec;
use crate::transport::providers::telegram::TelegramChannel;
use crate::transport::{Channel, ChannelMessage};
use crate::{list_bots, BotRegistration};

/// V0.6.1 F132 — keyed map of live IM Channels, keyed by
/// `ChannelMessage::channel` (`"telegram"`, `"slack"`, `"discord"`,
/// `"mock"`).
///
/// Built once at daemon startup from [`Credentials`], or test-injected
/// via [`DaemonArgs::channels_override`]. The daemon spawns one
/// `Channel::listen` task per entry and a single inbound consumer
/// that demultiplexes messages back to the right Channel for
/// admin-reply send-back.
pub type ChannelMap = HashMap<String, Arc<dyn Channel + Send + Sync>>;

/// Builds the production [`HarnessAdapter`] for one bot's `vendor`.
///
/// Hidden behind a function pointer so integration tests can swap
/// the real `ClaudeTuiAdapter` for a stub. The default returned by
/// [`default_adapter_factory`] is what `main.rs` wires.
pub type AdapterFactory =
    Arc<dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync>;

/// Pick the canonical production adapter for `vendor`.
///
/// - `Claude` → [`ClaudeTuiAdapter`] (the mode 3 chat adapter).
/// - `Codex` → [`CodexExecAdapter`] — per-turn `codex exec --json`
///   subprocess wrapped by the V0.6.5 advise-ledger hook so every
///   Codex call (chat bot, daemon-routed critic, advise_vote / parallel)
///   funnels through the same `<ccteam_root>/cost-budget.json` rollup.
///   Previously fell back to `ClaudeTuiAdapter` (a silent no-op);
///   that left Codex calls outside the cost ledger.
pub fn default_adapter_factory() -> AdapterFactory {
    Arc::new(|vendor: AgentVendor| match vendor {
        AgentVendor::Claude => {
            Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
        }
        AgentVendor::Codex => {
            Arc::new(CodexExecAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
        }
    })
}

/// CLI arguments forwarded from `main.rs`.
#[derive(Clone)]
pub struct DaemonArgs {
    /// Override credentials path (`None` → default).
    pub credentials: Option<PathBuf>,
    /// Override registry root.
    pub registry: Option<PathBuf>,
    /// Supervisor tick interval.
    pub tick: Duration,
    /// Optional max-runtime watchdog (`None` → unbounded; tests
    /// pass `Some(_)` to keep the harness from hanging).
    pub max_runtime: Option<Duration>,
    /// Wave 3 — adapter factory the supervisor registry uses to
    /// instantiate one [`HarnessAdapter`] per registered bot.
    /// `None` → [`default_adapter_factory`].
    pub adapter_factory: Option<AdapterFactory>,
    /// V0.6.1 F132 — test-only override for the Channel set. When
    /// `Some`, the daemon skips credential-driven channel construction
    /// and uses these channels verbatim (keyed by `ChannelMessage::channel`
    /// — `"telegram"`, `"mock"`, …). Production callers leave this
    /// `None`; the daemon then builds a [`TelegramChannel`] from
    /// `credentials.json` when `creds.telegram.is_some()`.
    pub channels_override: Option<ChannelMap>,
}

impl std::fmt::Debug for DaemonArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonArgs")
            .field("credentials", &self.credentials)
            .field("registry", &self.registry)
            .field("tick", &self.tick)
            .field("max_runtime", &self.max_runtime)
            .field("adapter_factory", &self.adapter_factory.is_some())
            .field(
                "channels_override",
                &self.channels_override.as_ref().map(|m| m.len()),
            )
            .finish()
    }
}

impl Default for DaemonArgs {
    fn default() -> Self {
        Self {
            credentials: None,
            registry: None,
            tick: Duration::from_secs(5),
            max_runtime: None,
            adapter_factory: None,
            channels_override: None,
        }
    }
}

/// V0.6.0 Wave 3 — keyed map of live [`BotSupervisor`]s.
///
/// Keyed by `"<slug>/<role>"` (same convention as `bot_dir`). New
/// registrations grow the map; the daemon never removes entries
/// during the loop (`unregister_bot` deletes the JSON on disk; the
/// supervisor stays Shutdown via the signal file).
#[derive(Default)]
pub struct SupervisorRegistry {
    inner: HashMap<String, Arc<BotSupervisor>>,
}

impl SupervisorRegistry {
    fn key(reg: &BotRegistration) -> String {
        format!("{}/{}", reg.workflow_slug, reg.role)
    }

    /// Add a supervisor for `reg` if one isn't already registered.
    /// Returns the live (existing-or-new) supervisor handle.
    pub fn ensure(
        &mut self,
        reg: &BotRegistration,
        projects_root: &Path,
        factory: &AdapterFactory,
    ) -> Arc<BotSupervisor> {
        let k = Self::key(reg);
        self.inner
            .entry(k)
            .or_insert_with(|| {
                let adapter = factory(reg.vendor);
                Arc::new(BotSupervisor::new(
                    reg.clone(),
                    projects_root.to_path_buf(),
                    adapter,
                ))
            })
            .clone()
    }

    /// Snapshot of every live supervisor for test introspection.
    pub fn all(&self) -> Vec<Arc<BotSupervisor>> {
        self.inner.values().cloned().collect()
    }

    /// V0.6.1 F132 — lookup the live supervisor for `reg` (used by the
    /// inbox drain pass which iterates the registry and dispatches each
    /// mailbox envelope to the matching `BotSupervisor::handle_inbound`).
    pub fn lookup(&self, reg: &BotRegistration) -> Option<Arc<BotSupervisor>> {
        self.inner.get(&Self::key(reg)).cloned()
    }
}

/// Run the daemon with a caller-supplied shutdown future. Returns
/// `Ok(())` on graceful shutdown (either the shutdown future resolves
/// or `args.max_runtime` elapses).
///
/// V0.6.1 F130 — this is the supervisor-loop core, callable from both
/// the standalone `ccteam-imd` historical entry point and from the
/// merged `ccteam start` daemon (which folds IMD as one tokio task
/// alongside orchestrator + web, all sharing a single
/// `tokio::sync::watch` shutdown channel).
pub async fn run_daemon_with_shutdown<F>(args: DaemonArgs, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let creds = credentials::load(args.credentials.as_deref())?;
    let initial = list_bots()?;
    tracing::info!(
        bots = initial.len(),
        has_telegram = creds.telegram.is_some(),
        has_slack = creds.slack.is_some(),
        has_discord = creds.discord.is_some(),
        "ccteam-imd daemon starting"
    );

    let factory = args
        .adapter_factory
        .clone()
        .unwrap_or_else(default_adapter_factory);
    let registry: Arc<Mutex<SupervisorRegistry>> =
        Arc::new(Mutex::new(SupervisorRegistry::default()));

    // V0.6.1 F132 — projects_root used for both supervisor bot_dir
    // resolution and the mailbox writer. Mirrors `tick_supervisors`'s
    // fallback so test and production paths share one root.
    let projects_root: PathBuf = args.registry.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join("projects")
    });

    // V0.6.1 F132 — build the Channel set: test override (MockChannel
    // injection) wins; otherwise auto-construct from credentials. Only
    // telegram lights up in V0.6.1 — slack / discord stay dark until
    // their producers register credentials (matches the host-probe
    // shape that landed in F121).
    let channels: ChannelMap = build_channels(&args, &creds, &initial);

    // V0.6.1 F132 — spawn one `Channel::listen` task per active
    // channel. Each listener pushes ChannelMessages into a shared mpsc
    // that the inbound consumer drains.
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel::<ChannelMessage>(INBOUND_BUF);
    let mut listener_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for (name, ch) in channels.iter() {
        let tx = inbound_tx.clone();
        let ch = ch.clone();
        let name_log = name.clone();
        listener_handles.push(tokio::spawn(async move {
            if let Err(err) = ch.listen(tx).await {
                tracing::warn!(
                    channel = %name_log,
                    error = %err,
                    "imd: channel listener exited with error"
                );
            } else {
                tracing::debug!(channel = %name_log, "imd: channel listener exited cleanly");
            }
        }));
        tracing::info!(
            channel = %name,
            bots = initial.len(),
            "imd: {} channel listener spawned bots={}",
            name,
            initial.len()
        );
    }
    // Drop our extra clone so the consumer's `recv()` returns `None`
    // once every listener exits.
    drop(inbound_tx);

    // V0.6.1 F132 — shared inbound state (sec layer, mailbox writer,
    // admin executor). `Arc` so the consumer task owns its own clones
    // without re-locking the daemon loop.
    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
    let mailbox: Arc<dyn MailboxResolver> = Arc::new(DefaultMailboxResolver::with_projects_root(
        projects_root.clone(),
    ));
    let executor = Arc::new(AdminExecutor::new(projects_root.clone()));

    // V0.6.1 fast-path — per-bot in-process mpsc channels. The daemon's
    // inbound consumer + each BotSupervisor's events consumer push items
    // into the right entry; per-bot inbox/outbound dispatcher tasks
    // (spawned by `ensure_bot_channels`) drain them and call the
    // adapter / IM channel directly. drain_inboxes / drain_outboxes
    // stay alive as a slow safety net (see `SAFETY_NET_TICK`).
    let bot_channels: BotChannelMap = Arc::new(Mutex::new(HashMap::new()));

    let inbound_consumer = spawn_inbound_consumer(
        inbound_rx,
        channels.clone(),
        sec.clone(),
        mailbox.clone(),
        executor.clone(),
        bot_channels.clone(),
    );

    // V0.6.1 F134 — outbound forwarder runs per supervisor tick (inside
    // the `loop` below). Log once at startup so operators see the
    // wiring is live; per-dispatch logs live in `drain_outboxes`.
    tracing::info!(
        channels = channels.len(),
        bots = initial.len(),
        "imd: F134 outbound forwarder spawned (per-tick drain_outboxes)"
    );

    let mut ticker = tokio::time::interval(args.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // V0.6.1 fast-path — drain_inboxes / drain_outboxes downgraded to a
    // safety net: the hot path is per-bot mpsc dispatchers spawned in
    // `ensure_bot_channels`. This slower ticker only fires every 60s to
    // catch orphan files from a daemon crash mid-handle or any mpsc
    // race miss. Aligned with `STALE_THRESHOLD` (60s) so a stuck bot
    // re-triggering a Restart still gets one safety-net drain pass.
    let mut safety_net_ticker = tokio::time::interval(SAFETY_NET_TICK);
    safety_net_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let started = std::time::Instant::now();
    let mut shutdown: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(shutdown);

    let result = loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(err) = supervisor::refresh_global_heartbeat() {
                    tracing::warn!(error = %err, "heartbeat refresh failed");
                }
                let bots = match list_bots() {
                    Ok(b) => b,
                    Err(err) => {
                        tracing::warn!(error = %err, "list_bots failed");
                        Vec::new()
                    }
                };
                tick_supervisors(
                    &bots,
                    &registry,
                    Some(&projects_root),
                    &factory,
                    Some(&bot_channels),
                )
                .await;
                ensure_bot_channels(
                    &bots,
                    &registry,
                    &channels,
                    &bot_channels,
                    &projects_root,
                )
                .await;

                if let Some(max) = args.max_runtime {
                    if started.elapsed() >= max {
                        tracing::info!("max_runtime reached; exiting");
                        break Ok(());
                    }
                }
            }
            _ = safety_net_ticker.tick() => {
                let bots = match list_bots() {
                    Ok(b) => b,
                    Err(err) => {
                        tracing::warn!(error = %err, "list_bots failed (safety net)");
                        Vec::new()
                    }
                };
                drain_inboxes(&bots, &registry, &projects_root).await;
                drain_outboxes(&bots, &channels, &bot_channels, &projects_root).await;
            }
            _ = &mut shutdown => {
                tracing::info!("ccteam-imd: shutdown signalled; exiting cleanly");
                break Ok(());
            }
        }
    };

    // V0.6.1 F132 — abort listener + consumer tasks on shutdown so the
    // daemon doesn't leak background tokio tasks. `JoinHandle::abort`
    // is best-effort but matches the rest of the F130 supervisor's
    // shutdown semantics.
    for h in listener_handles {
        h.abort();
    }
    inbound_consumer.abort();
    result
}

/// V0.6.1 F132 — channel-listener mpsc buffer. 64 is enough headroom
/// for a slow consumer to lag behind a burst without dropping; if it
/// fills the listener `await`s on `send`, which is what we want
/// (backpressure, not silent drop).
const INBOUND_BUF: usize = 64;

/// V0.6.1 fast-path — safety-net `drain_*` cadence. The hot path is
/// per-bot mpsc dispatchers (see [`crate::bot_mpsc`]); this slow tick
/// only catches orphan envelope files from a daemon crash mid-handle
/// or any mpsc race miss. 60s matches the supervisor's
/// `STALE_THRESHOLD` so a Restart cycle still sees one drain pass.
pub(crate) const SAFETY_NET_TICK: Duration = Duration::from_secs(60);

/// V0.6.1 F132 — assemble the Channel set the daemon listens on.
///
/// Resolution order:
/// 1. `args.channels_override` (tests inject `MockChannel`),
/// 2. `creds.telegram` → build a [`TelegramChannel`] with the union of
///    the user-configured allowlist + every registered telegram bot's
///    `im_chat_id`,
/// 3. (slack / discord wiring).
///
// TODO(V0.7-im-providers): construct `SlackChannel` / `DiscordChannel`
//   here when `creds.slack` / `creds.discord` are set. Provider modules
//   already exist (`transport/providers/{slack,discord}.rs`) but only
//   telegram is exercised by the V0.6.x host probe.
// Reason deferred: bundled with V0.7 Epic C (国内 IM enablement +
//   Slack Socket Mode / inbound HTTP) so the daemon wiring, HMAC
//   verification, and onboarding skill ship as one wave instead of
//   trickling per-provider half-changes through V0.6.x patch releases.
// Tracking: docs/versions/v0-6-6/prd.md §F168 (decision row #2) +
//   docs/dev-coupling-audit.md V0.6.6 V0.7-deferred segment.
fn build_channels(args: &DaemonArgs, creds: &Credentials, bots: &[BotRegistration]) -> ChannelMap {
    if let Some(ch) = args.channels_override.clone() {
        return ch;
    }
    let mut out: ChannelMap = HashMap::new();
    if let Some(tg) = creds.telegram.as_ref() {
        let mut allowed = tg.allowed_chat_ids.clone();
        for b in bots.iter().filter(|b| b.im_platform == "telegram") {
            allowed.push(b.im_chat_id.clone());
        }
        allowed.sort();
        allowed.dedup();
        let ch = Arc::new(TelegramChannel::new(tg.bot_token.clone(), allowed));
        out.insert("telegram".to_string(), ch as Arc<dyn Channel + Send + Sync>);
    }
    out
}

/// V0.6.1 F132 — drain the mpsc receiving from every listener, routing
/// each `ChannelMessage` through the security + admin + mailbox path.
/// Side-effects ultimately land as one mailbox `.md` file per inbound
/// bot turn (consumed in the next [`drain_inboxes`] tick).
fn spawn_inbound_consumer(
    mut rx: tokio::sync::mpsc::Receiver<ChannelMessage>,
    channels: ChannelMap,
    sec: Arc<Mutex<ThreeLayerSec>>,
    mailbox: Arc<dyn MailboxResolver>,
    executor: Arc<AdminExecutor>,
    bot_channels: BotChannelMap,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut seq: u64 = 0;
        while let Some(mut msg) = rx.recv().await {
            seq = seq.wrapping_add(1);
            let cid = msg.id.clone();
            let route_t0 = std::time::Instant::now();
            tracing::info!(
                event = "latency",
                stage = "imd.route.begin",
                cid = %cid,
                channel = %msg.channel,
                "latency imd.route.begin"
            );
            // V0.6.1 F135 — DM auto-route: when exactly one registered
            // bot owns this (channel, chat_id), prepend `@<role> ` so
            // the router resolves the message to that bot. List_bots()
            // is a small disk read; V0.6.1 IM traffic volume keeps it
            // cheap (single-bot host probe).
            // TODO(V0.7-listbots-cache): hoist `list_bots()` behind an
            //   in-memory cache (invalidated on `BotRegistry` write +
            //   workflow.yaml reload) so per-inbound-message disk I/O
            //   collapses to a hash lookup.
            // Reason deferred: at V0.6.x single-bot host-probe traffic
            //   volume the disk read is unmeasurable noise; the cache
            //   only pays for itself at multi-bot per-platform scale,
            //   which lands with V0.7 Epic C. Hoisting it now would
            //   bake an invalidation contract that has to be revisited
            //   when per-bot `chat_handle` lands in the same wave.
            // Tracking: docs/versions/v0-6-6/prd.md §F168 (decision
            //   row #3) + docs/dev-coupling-audit.md V0.6.6 segment.
            let bots_for_route = list_bots().unwrap_or_default();
            auto_route_dm_mention(&mut msg, &bots_for_route);
            let handles = build_handle_map();
            let Some(channel) = channels.get(&msg.channel).cloned() else {
                tracing::debug!(
                    channel = %msg.channel,
                    sender = %msg.sender,
                    "imd: no Channel for inbound msg.channel; dropping"
                );
                continue;
            };
            match process_inbound_admin_aware(
                &msg,
                &sec,
                &handles,
                mailbox.as_ref(),
                executor.as_ref(),
                channel.as_ref(),
                0,
                seq,
            )
            .await
            {
                Ok((outcome, admin)) => {
                    tracing::info!(
                        event = "latency",
                        stage = "imd.route.done",
                        cid = %cid,
                        elapsed_ms = route_t0.elapsed().as_millis() as u64,
                        outcome = ?outcome,
                        admin_side_effect = ?admin.as_ref().map(|r| &r.side_effect),
                        "latency imd.route.done"
                    );
                    // V0.6.1 fast-path — if the outcome routed to a
                    // bot AND that bot's mpsc is wired, push directly
                    // into the per-bot inbox dispatcher. Falls through
                    // silently when the dispatcher isn't ready yet —
                    // the safety-net `drain_inboxes` tick picks the
                    // envelope up from disk next pass.
                    if let InboundOutcome::DroppedToBot {
                        slug,
                        role,
                        path,
                        payload,
                        cid: item_cid,
                    } = outcome
                    {
                        let guard = bot_channels.lock().await;
                        if let Some(ch) = guard.get(&bot_key(&slug, &role)) {
                            let item = InboxItem {
                                cid: item_cid,
                                slug: slug.clone(),
                                role: role.clone(),
                                payload,
                                path,
                                enqueue_unix_ms: now_unix_ms(),
                            };
                            if let Err(err) = ch.inbox_tx.try_send(item) {
                                tracing::warn!(
                                    event = "latency",
                                    stage = "imd.mpsc_full",
                                    cid = %cid,
                                    slug = %slug,
                                    role = %role,
                                    error = %err,
                                    "latency imd inbox mpsc full; safety-net drain will retry"
                                );
                            }
                        } else {
                            tracing::debug!(
                                cid = %cid,
                                slug = %slug,
                                role = %role,
                                "imd: bot mpsc not yet wired; safety-net drain will pick this up"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        event = "latency",
                        stage = "imd.route.err",
                        cid = %cid,
                        elapsed_ms = route_t0.elapsed().as_millis() as u64,
                        error = %err,
                        "latency imd.route.err"
                    );
                }
            }
        }
        tracing::debug!("imd: inbound consumer exited (all senders closed)");
    })
}

/// V0.6.1 F132 — build a [`HandleMap`] from the current registry so
/// the router can resolve `@<role>` mentions to `(slug, role)`.
///
/// V0.6.1 keeps it simple: each registered bot's `role` becomes a
/// handle. Two bots sharing the same role across different slugs
/// **collide** here (last-wins). For F132 the typical production
/// registry has one bot ("web3op_bot") on one slug, so the collision
/// is theoretical for the V0.6.x host probe.
///
// TODO(V0.7-chat-handle): extend `AgentSpec` with a `chat_handle:
//   Option<String>` field (workflow.yaml schema additive) and build
//   handles from `(slug, role, chat_handle.unwrap_or(role))` so two
//   bots can share `role: chatops` across slugs without collision.
// Reason deferred: the schema extension is paired with V0.7 Epic C
//   multi-platform per-bot routing — landing it in isolation forces a
//   second workflow.yaml migration when Epic C ships and would
//   pre-commit to a handle-collision UX (warn vs. error vs. namespace
//   per-slug) that the Epic C IM coverage will inform.
// Tracking: docs/versions/v0-6-6/prd.md §F168 (decision row #4) +
//   docs/dev-coupling-audit.md V0.6.6 segment.
fn build_handle_map() -> HandleMap {
    let mut map = HandleMap::new();
    if let Ok(bots) = list_bots() {
        for b in bots {
            map.insert(&b.role, &b.workflow_slug, &b.role);
        }
    }
    map
}

/// V0.6.1 fast-path — for every bot that is `is_started()`:
///
/// 1. If `<slug>/<role>` has no entry in `bot_channels` yet, build a
///    fresh `(inbox_tx, outbound_tx)` pair, spawn the per-bot inbox
///    consumer + outbound dispatcher tasks, and register the senders
///    so the daemon's inbound consumer + the supervisor's events
///    consumer can route into them.
/// 2. Push the outbound sender clone onto the supervisor so its
///    next event-stream item fans out into the dispatcher.
///
/// Idempotent on subsequent ticks: existing entries short-circuit.
/// Bots that haven't yet started skip silently — the safety-net
/// `drain_inboxes` pass will still pick up any queued envelopes for
/// them once they boot, and the inbox consumer's `try_send` falls
/// through to `tracing::debug!` when an inbox_tx isn't wired.
async fn ensure_bot_channels(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    channels: &ChannelMap,
    bot_channels: &BotChannelMap,
    projects_root: &Path,
) {
    for bot in bots {
        let sup = {
            let reg = registry.lock().await;
            match reg.lookup(bot) {
                Some(s) => s,
                None => continue,
            }
        };
        if !sup.is_started().await {
            continue;
        }
        let key = bot_key(&bot.workflow_slug, &bot.role);
        {
            let guard = bot_channels.lock().await;
            if guard.contains_key(&key) {
                continue;
            }
        }
        let Some(channel) = channels.get(&bot.im_platform).cloned() else {
            tracing::debug!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                platform = %bot.im_platform,
                "imd: ensure_bot_channels: no IM channel for bot's platform; skipping fast-path"
            );
            continue;
        };

        let (inbox_tx, inbox_rx) = mpsc::channel::<InboxItem>(CHANNEL_BUF);
        let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundItem>(CHANNEL_BUF);

        // Inbox dispatcher: drain the channel, call
        // `supervisor.handle_inbound`, `unlink` the envelope on success.
        let sup_for_inbox = sup.clone();
        let slug_log = bot.workflow_slug.clone();
        let role_log = bot.role.clone();
        tokio::spawn(spawn_inbox_dispatcher(
            inbox_rx,
            sup_for_inbox,
            slug_log,
            role_log,
        ));

        // Outbound dispatcher: drain the channel, call
        // `channel.send`, advance the shared OutboundCursor on
        // successful TG ack. The cursor is also handed to
        // drain_outboxes (via bot_channels) so both writers share the
        // same monotonic primitive.
        let chat_id = bot.im_chat_id.clone();
        let platform = bot.im_platform.clone();
        let cursor_path =
            outbound::outbound_cursor_path(projects_root, &bot.workflow_slug, &bot.role);
        let outbound_cursor = outbound::OutboundCursor::load_from_disk(cursor_path);
        let slug_log = bot.workflow_slug.clone();
        let role_log = bot.role.clone();
        tokio::spawn(spawn_outbound_dispatcher(
            outbound_rx,
            channel,
            chat_id,
            platform,
            outbound_cursor.clone(),
            slug_log,
            role_log,
        ));

        // Tell the supervisor about the outbound side so its events
        // consumer can fan out from the next ItemCompleted onward.
        sup.set_outbound_tx(outbound_tx.clone()).await;

        // Register both senders + the shared cursor so the inbound
        // consumer + future ticks (and drain_outboxes) see them.
        bot_channels.lock().await.insert(
            key,
            BotChannels {
                inbox_tx,
                outbound_tx,
                outbound_cursor,
            },
        );
        tracing::info!(
            slug = %bot.workflow_slug,
            role = %bot.role,
            platform = %bot.im_platform,
            "imd: V0.6.1 fast-path per-bot mpsc wired"
        );
    }
}

/// V0.6.1 fast-path — per-bot inbox dispatcher task body. Drains
/// [`InboxItem`]s from `rx`, calls
/// [`BotSupervisor::handle_inbound`], `unlink`s the envelope file on
/// success (and on error too — the alternative is re-delivering on
/// every safety-net pass, which would double-submit a slow tmux reply).
async fn spawn_inbox_dispatcher(
    mut rx: mpsc::Receiver<InboxItem>,
    sup: Arc<BotSupervisor>,
    slug_log: String,
    role_log: String,
) {
    while let Some(item) = rx.recv().await {
        let queue_age_ms = now_unix_ms().saturating_sub(item.enqueue_unix_ms) as u64;
        let submit_t0 = std::time::Instant::now();
        match sup.handle_inbound(item.payload).await {
            Ok(turn_id) => {
                tracing::info!(
                    event = "latency",
                    stage = "imd.inbox.dispatch",
                    cid = %item.cid,
                    turn_id = %turn_id.0,
                    slug = %slug_log,
                    role = %role_log,
                    queue_age_ms,
                    submit_ms = submit_t0.elapsed().as_millis() as u64,
                    file = %item.path.display(),
                    "latency imd.inbox.dispatch (mpsc)"
                );
            }
            Err(err) => {
                tracing::warn!(
                    event = "latency",
                    stage = "imd.inbox.dispatch.err",
                    cid = %item.cid,
                    slug = %slug_log,
                    role = %role_log,
                    queue_age_ms,
                    submit_ms = submit_t0.elapsed().as_millis() as u64,
                    error = %err,
                    "latency imd.inbox.dispatch (mpsc, failed)"
                );
            }
        }
        // `unlink` regardless of submit outcome — re-delivery on safety
        // net would double-submit. Failed sends are tracked via
        // `chat_session_reset` / hook progress, not by retrying the
        // mailbox file. Best-effort: ignore IO errors here.
        let _ = std::fs::remove_file(&item.path);
    }
    tracing::debug!(slug = %slug_log, role = %role_log, "imd: inbox dispatcher exited");
}

/// V0.6.1 fast-path — per-bot outbound dispatcher task body. Drains
/// [`OutboundItem`]s from `rx`, calls `channel.send`, advances the
/// shared [`OutboundCursor`] on success.
///
/// **Dedup invariant**: before sending, the dispatcher checks whether
/// the safety-net `drain_outboxes` has already covered this row (via
/// `cursor.current() >= item.cursor_after`). If so, skip the send
/// entirely — TG already received this content from the other path.
/// Combined with [`OutboundCursor::try_advance`]'s monotonic guard,
/// this gives both writers a single source of truth and closes both
/// the cursor-rewind loop and the per-row double-send window that
/// the NAS-environment bug exposed.
async fn spawn_outbound_dispatcher(
    mut rx: mpsc::Receiver<OutboundItem>,
    channel: Arc<dyn Channel + Send + Sync>,
    chat_id: String,
    platform: String,
    cursor: Arc<outbound::OutboundCursor>,
    slug_log: String,
    role_log: String,
) {
    while let Some(item) = rx.recv().await {
        // Already covered by drain_outboxes — skip the redundant TG
        // send. Without this check, both paths would deliver the same
        // row on overlap.
        if item.cursor_after <= cursor.current().await {
            tracing::debug!(
                slug = %slug_log,
                role = %role_log,
                turn_id = %item.turn_id,
                cursor_after = item.cursor_after,
                "imd: outbound dispatcher skipped (cursor already advanced past this row)"
            );
            continue;
        }
        if item.role != "assistant" {
            // Non-assistant rows still advance the cursor so the
            // safety-net drain doesn't re-read them.
            cursor.try_advance(item.cursor_after).await;
            continue;
        }
        let tail_age_ms = now_unix_ms().saturating_sub(item.enqueue_unix_ms) as u64;
        let send_t0 = std::time::Instant::now();
        let msg = crate::transport::SendMessage::new(item.content.clone(), &chat_id);
        match channel.send(&msg).await {
            Ok(tg_msg_id) => {
                tracing::info!(
                    event = "latency",
                    stage = "imd.outbound.dispatch",
                    turn_id = %item.turn_id,
                    slug = %slug_log,
                    role = %role_log,
                    platform = %platform,
                    tail_age_ms,
                    send_ms = send_t0.elapsed().as_millis() as u64,
                    tg_msg_id = tg_msg_id.as_deref().unwrap_or(""),
                    content_len = item.content.len(),
                    "latency imd.outbound.dispatch (mpsc)"
                );
                cursor.try_advance(item.cursor_after).await;
            }
            Err(err) => {
                // Don't advance cursor on failure — the safety-net
                // drain will retry from the previous cursor position.
                tracing::warn!(
                    event = "latency",
                    stage = "imd.outbound.dispatch.err",
                    turn_id = %item.turn_id,
                    slug = %slug_log,
                    role = %role_log,
                    tail_age_ms,
                    send_ms = send_t0.elapsed().as_millis() as u64,
                    error = %err,
                    "latency imd.outbound.dispatch (mpsc, failed)"
                );
            }
        }
    }
    tracing::debug!(slug = %slug_log, role = %role_log, "imd: outbound dispatcher exited");
}

/// V0.6.1 F132 — once per tick, for each registered bot:
///
/// - List `<projects_root>/<slug>/.ccteam/chat/<role>/inbox/*.md`,
/// - Sort by file name (timestamp prefix gives FIFO order),
/// - For each: parse envelope, hand the payload to
///   [`BotSupervisor::handle_inbound`] (which calls `submit_turn`
///   under the hood), then `unlink` the file regardless of whether
///   the submit succeeded (one-shot semantics — re-tries would
///   double-submit a slow tmux reply).
async fn drain_inboxes(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    projects_root: &Path,
) {
    let supervisors: Vec<(BotRegistration, Arc<BotSupervisor>)> = {
        let reg = registry.lock().await;
        bots.iter()
            .filter_map(|b| reg.lookup(b).map(|s| (b.clone(), s)))
            .collect()
    };
    for (bot, sup) in supervisors {
        let inbox = projects_root
            .join(&bot.workflow_slug)
            .join(".ccteam")
            .join("chat")
            .join(&bot.role)
            .join("inbox");
        if !inbox.exists() {
            continue;
        }
        let mut entries: Vec<_> = match std::fs::read_dir(&inbox) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(err) => {
                tracing::warn!(
                    path = %inbox.display(),
                    error = %err,
                    "imd: read_dir inbox failed"
                );
                continue;
            }
        };
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let body = match std::fs::read_to_string(&path) {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "imd: read inbox file failed"
                    );
                    continue;
                }
            };
            match parse_envelope(&body) {
                Ok(env) => {
                    if !sup.is_started().await {
                        // Supervisor not up yet — defer to next tick so
                        // `tick_supervisors`' Spawn action lands first.
                        tracing::debug!(
                            slug = %bot.workflow_slug,
                            role = %bot.role,
                            file = %path.display(),
                            "imd: bot not started; deferring mailbox dispatch"
                        );
                        continue;
                    }
                    let cid = env.message_id.clone();
                    let received_ms = env.received_at.timestamp_millis().max(0) as u128;
                    let queue_age_ms = now_unix_ms().saturating_sub(received_ms) as u64;
                    let drain_t0 = std::time::Instant::now();
                    match sup.handle_inbound(env.payload).await {
                        Ok(id) => {
                            tracing::info!(
                                event = "latency",
                                stage = "imd.inbox.drain",
                                cid = %cid,
                                turn_id = %id.0,
                                slug = %bot.workflow_slug,
                                role = %bot.role,
                                queue_age_ms,
                                submit_ms = drain_t0.elapsed().as_millis() as u64,
                                file = %path.display(),
                                "latency imd.inbox.drain"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                event = "latency",
                                stage = "imd.inbox.drain.err",
                                cid = %cid,
                                slug = %bot.workflow_slug,
                                role = %bot.role,
                                queue_age_ms,
                                submit_ms = drain_t0.elapsed().as_millis() as u64,
                                file = %path.display(),
                                error = %err,
                                "latency imd.inbox.drain (failed; deleting envelope anyway)"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "imd: parse_envelope failed; deleting"
                    );
                }
            }
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "imd: remove inbox file failed"
                );
            }
        }
    }
}

/// V0.6.1 F134 — once per tick, for each registered bot:
///
/// - Read `<projects_root>/<slug>/.ccteam/chat/<role>/turns.jsonl`
///   from the persisted byte-offset cursor,
/// - For every new `assistant` row, dispatch through the Channel
///   matching `bot.im_platform` to `bot.im_chat_id`,
/// - Persist the new cursor so a daemon restart doesn't re-forward.
///
/// Errors at each stage are warn-logged and the loop continues — one
/// flaky bot must not stall the rest. Missing channels (e.g. bot
/// registered to slack but creds.json only configures telegram) are
/// debug-logged and skipped silently.
async fn drain_outboxes(
    bots: &[BotRegistration],
    channels: &ChannelMap,
    bot_channels: &BotChannelMap,
    projects_root: &Path,
) {
    for bot in bots {
        let Some(channel) = channels.get(&bot.im_platform) else {
            tracing::debug!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                platform = %bot.im_platform,
                "imd: F134 outbound: no channel registered for bot's platform; skipping"
            );
            continue;
        };
        let path = outbound::turns_jsonl_path(projects_root, &bot.workflow_slug, &bot.role);

        // Prefer the shared OutboundCursor owned by the fast-path
        // dispatcher (so we update the same in-memory state and the
        // dispatcher's pre-send dedup check sees our progress
        // immediately). If the fast-path hasn't been wired yet
        // (supervisor still starting up), fall back to a freshly
        // loaded cursor — it's a transient state during startup and
        // the next tick will pick up the shared one.
        let key = bot_key(&bot.workflow_slug, &bot.role);
        let cursor: Arc<outbound::OutboundCursor> = {
            let guard = bot_channels.lock().await;
            match guard.get(&key) {
                Some(ch) => ch.outbound_cursor.clone(),
                None => {
                    let cursor_path = outbound::outbound_cursor_path(
                        projects_root,
                        &bot.workflow_slug,
                        &bot.role,
                    );
                    outbound::OutboundCursor::load_from_disk(cursor_path)
                }
            }
        };

        let start = cursor.current().await;
        let (rows, _eof) =
            match outbound::read_new_rows_indexed(&path, &outbound::TailCursor { position: start })
            {
                Ok(x) => x,
                Err(err) => {
                    tracing::warn!(
                        slug = %bot.workflow_slug,
                        role = %bot.role,
                        error = %err,
                        "imd: F134 outbound: read_new_rows_indexed failed; continuing"
                    );
                    continue;
                }
            };

        // Truncation handling: if turns.jsonl shrunk below the current
        // cursor, read_new_rows_indexed rewinds `start` to 0 internally
        // and returns rows from the new (shorter) file. We mirror that
        // rewind in the OutboundCursor via `force_set(0)` so the
        // per-row dedup check below doesn't reject the post-truncation
        // content (which has byte offsets smaller than the pre-rotation
        // cursor). After force_set(0), each forwarded row advances the
        // cursor row-by-row via try_advance, the same as steady-state.
        //
        // Trade-off: post-rotation content gets forwarded once; the
        // user may see duplicates of pre-rotation content. That is
        // acceptable for the rare rotation case and matches the V0.6.1
        // policy. The NAS-bug we are closing here is the *unbounded*
        // re-forward loop, not the one-shot rotation re-send.
        let file_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if start > 0 && file_len < start {
            cursor.force_set(0).await;
            tracing::info!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                old_position = start,
                new_eof = file_len,
                "imd: F134 outbound: turns.jsonl truncation detected; cursor force-reset to 0"
            );
        }

        if rows.is_empty() {
            continue;
        }

        let mut sent = 0usize;
        for indexed in &rows {
            let row = &indexed.row;
            let row_end = indexed.end_pos;

            // Per-row dedup: the dispatcher may have advanced the
            // cursor past this row between read and now. Skip both
            // the TG send AND the cursor write in that case.
            if row_end <= cursor.current().await {
                continue;
            }

            let tail_age_ms = row.ts.map(|t| {
                crate::latency::now_unix_ms().saturating_sub(t.timestamp_millis().max(0) as u128)
                    as u64
            });
            tracing::info!(
                event = "latency",
                stage = "outbound.tail",
                turn_id = %row.turn_id.clone().unwrap_or_default(),
                slug = %bot.workflow_slug,
                role = %bot.role,
                role_field = %row.role,
                tail_age_ms = tail_age_ms.unwrap_or(0),
                content_len = row.content.len(),
                "latency outbound.tail"
            );

            if !outbound::should_forward(row, &[]) {
                cursor.try_advance(row_end).await;
                continue;
            }

            let mut msg = crate::transport::SendMessage::new(row.content.clone(), &bot.im_chat_id);
            msg.thread_ts = row.thread_ts.clone();
            match channel.send(&msg).await {
                Ok(_tg_msg_id) => {
                    cursor.try_advance(row_end).await;
                    sent += 1;
                }
                Err(err) => {
                    // Stop advancing on send failure so a later tick
                    // retries from the same position. Continuing to
                    // later rows would orphan this one behind a higher
                    // cursor.
                    tracing::warn!(
                        slug = %bot.workflow_slug,
                        role = %bot.role,
                        error = %err,
                        "imd: F134 outbound: channel.send failed; halting drain for this bot"
                    );
                    break;
                }
            }
        }

        if sent > 0 {
            tracing::info!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                platform = %bot.im_platform,
                chat_id = %bot.im_chat_id,
                "imd: F134 outbound forwarded {} rows",
                sent
            );
        }
    }
}

/// Run the daemon with the default SIGINT (ctrl-C) shutdown trigger.
///
/// Preserved as the lib-level entry point used by integration tests
/// that don't supply their own shutdown future. V0.6.1 F130 folded the
/// `ccteam-imd` binary into `ccteam start`, so production now goes via
/// [`run_daemon_with_shutdown`] with the shared `watch::channel`
/// shutdown signal.
pub async fn run_daemon(args: DaemonArgs) -> Result<()> {
    run_daemon_with_shutdown(args, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// V0.6.0 Wave 3 — per-tick driver. Registers any new bots with the
/// supervisor registry, then for each known bot calls
/// `decide(...)` against its live state and applies the resulting
/// action through the supervisor (`apply_action`).
///
/// V0.6.5 F147 — when `bot_channels` is provided **and** the
/// supervisor's decision is `ResetSession`, we force-reset the
/// in-memory `OutboundCursor` (Bug B防线) before calling
/// `apply_action`. The supervisor handles the on-disk side
/// (archive + transcript cursor wipe + close + start) but cannot reach
/// the cursor `Arc` itself — `bot_channels` owns those handles. When
/// `bot_channels` is `None` (legacy callers / tests without an outbound
/// pipeline wired), the cursor reset is skipped and the supervisor's
/// disk-side wipe still applies; the next `load_from_disk` will pick
/// up the cleared cursor on the next daemon restart.
pub(crate) async fn tick_supervisors(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    projects_root_override: Option<&Path>,
    factory: &AdapterFactory,
    bot_channels: Option<&BotChannelMap>,
) {
    let owned;
    let projects_root: &Path = match projects_root_override {
        Some(p) => p,
        None => {
            owned = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join("projects");
            &owned
        }
    };

    // First pass: ensure a supervisor exists per bot.
    let supervisors: Vec<(BotRegistration, Arc<BotSupervisor>)> = {
        let mut reg = registry.lock().await;
        bots.iter()
            .map(|b| {
                let sup = reg.ensure(b, projects_root, factory);
                (b.clone(), sup)
            })
            .collect()
    };

    // Second pass: decide + apply per bot (drop the registry lock for
    // each adapter call so a slow start_thread doesn't stall other
    // bots' decisions).
    for (bot, sup) in supervisors {
        let state = sup.state_snapshot().await;
        let action = supervisor::decide(projects_root, &bot, &state, SystemTime::now());
        tracing::debug!(
            slug = %bot.workflow_slug,
            role = %bot.role,
            ?action,
            "supervisor decision"
        );

        // V0.6.5 F147 — Bug B防线: reset the in-memory OutboundCursor
        // BEFORE the supervisor archives + restarts so any concurrent
        // events-task push that lands during the gap is dedup-checked
        // against position 0 (the post-reset baseline), not the
        // pre-reset position that would silently drop new content.
        if action == supervisor::SupervisorAction::ResetSession {
            if let Some(ch_map) = bot_channels {
                let key = bot_key(&bot.workflow_slug, &bot.role);
                let cursor_opt = {
                    let guard = ch_map.lock().await;
                    guard.get(&key).map(|c| c.outbound_cursor.clone())
                };
                if let Some(cur) = cursor_opt {
                    cur.force_set(0).await;
                    tracing::info!(
                        slug = %bot.workflow_slug,
                        role = %bot.role,
                        "F147 reset: in-memory OutboundCursor force-reset to 0 (Bug B防线)"
                    );
                }
            }
        }

        if let Err(err) = sup.apply_action(action.clone()).await {
            tracing::warn!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                ?action,
                error = %err,
                "apply_action failed"
            );
        }
    }
}

/// Compatibility shim — `lib.rs` re-exports this so the existing
/// `pub use daemon::run_daemon;` keeps working without forcing
/// callers to depend on this module path directly.
pub fn _link_check(_c: &Credentials) {}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ccteam_core::harness::{
        AgentSpecBrief, ExecutionMode, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, TurnId,
        TurnInput,
    };
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    #[tokio::test(flavor = "current_thread", start_paused = false)]
    async fn daemon_boots_and_exits_on_max_runtime() {
        // Point HOME at a tempdir so no real credentials are read.
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let args = DaemonArgs {
            credentials: None,
            registry: None,
            tick: Duration::from_millis(50),
            max_runtime: Some(Duration::from_millis(120)),
            adapter_factory: None,
            channels_override: None,
        };
        run_daemon(args).await.unwrap();
        // Heartbeat was written at least once.
        let hb = crate::imd_heartbeat_path();
        assert!(hb.exists(), "heartbeat at {} should exist", hb.display());
        std::env::remove_var("HOME");
    }

    #[derive(Debug, Default)]
    struct RecordingAdapter {
        starts: AtomicUsize,
        closes: AtomicUsize,
    }
    #[async_trait]
    impl HarnessAdapter for RecordingAdapter {
        fn name(&self) -> &'static str {
            "recording"
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
                identity: format!("rec-{}-{}", ctx.slug, spec.role),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({}),
            })
        }
        async fn submit_turn(
            &self,
            _h: &ThreadHandle,
            _input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            Ok(TurnId::new("t"))
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
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn tick_supervisors_registers_and_spawns_bot() {
        let projects = TempDir::new().unwrap();
        let adapter = Arc::new(RecordingAdapter::default());
        let adapter_factory: AdapterFactory = {
            let cloned = adapter.clone();
            Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
        };
        let registry = Arc::new(Mutex::new(SupervisorRegistry::default()));
        let bot = BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "telegram".into(),
            im_chat_id: "1".into(),
            created_at: chrono::Utc::now(),
        };

        // Tick 1: decide() returns Spawn (no handle yet) → start_thread.
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
            None,
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

        // Tick 2: heartbeat missing → decide() returns Restart → close + start.
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
            None,
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 2);
        assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);

        // Drop in a fresh heartbeat: next tick is a NoOp.
        let bot_dir = projects.path().join("dev-foo/.ccteam/chat/lead");
        std::fs::create_dir_all(&bot_dir).unwrap();
        std::fs::write(bot_dir.join("heartbeat"), "x").unwrap();
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
            None,
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 2, "no new start");

        // Verify the supervisor is registered + holds a live handle.
        let supervisors = registry.lock().await.all();
        assert_eq!(supervisors.len(), 1);
        assert!(supervisors[0].is_started().await);
    }

    #[tokio::test]
    async fn tick_supervisors_honors_shutdown_signal() {
        let projects = TempDir::new().unwrap();
        let adapter = Arc::new(RecordingAdapter::default());
        let adapter_factory: AdapterFactory = {
            let cloned = adapter.clone();
            Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
        };
        let registry = Arc::new(Mutex::new(SupervisorRegistry::default()));
        let bot = BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "telegram".into(),
            im_chat_id: "1".into(),
            created_at: chrono::Utc::now(),
        };
        // Tick 1: Spawn.
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
            None,
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

        // Drop shutdown.signal; next tick → Shutdown action.
        let sig_dir = projects.path().join("dev-foo/.ccteam/chat/lead/signals");
        std::fs::create_dir_all(&sig_dir).unwrap();
        std::fs::write(sig_dir.join("shutdown.signal"), "").unwrap();
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
            None,
        )
        .await;
        assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
        let supervisors = registry.lock().await.all();
        assert!(!supervisors[0].is_started().await);
    }

    /// V0.6.6 F173 — `default_adapter_factory` must route the Codex arm
    /// to a Codex-vendor adapter (CodexExecAdapter), not the Claude
    /// fallback that lived here from V0.6.0 Wave 3. The previous fallback
    /// silently broke the unified cost rollup (Codex chat-mode spawns
    /// never appended a ledger row). This test pins the fix.
    #[test]
    fn default_adapter_factory_codex_arm_returns_codex_vendor() {
        let factory = default_adapter_factory();
        let claude = factory(AgentVendor::Claude);
        assert_eq!(
            claude.vendor(),
            AgentVendor::Claude,
            "claude arm must return a Claude adapter"
        );
        let codex = factory(AgentVendor::Codex);
        assert_eq!(
            codex.vendor(),
            AgentVendor::Codex,
            "F173: codex arm must return a Codex adapter, not the Claude fallback"
        );
    }
}
