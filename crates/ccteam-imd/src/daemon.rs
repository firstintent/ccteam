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
use ccteam_core::execution::ClaudeTuiAdapter;
use ccteam_core::harness::{AgentVendor, HarnessAdapter};
use tokio::sync::Mutex;

use crate::acl::AclPolicy;
use crate::credentials::{self, Credentials};
use crate::inbound::{
    auto_route_dm_mention, parse_envelope, process_inbound_admin_aware, DefaultMailboxResolver,
    MailboxResolver,
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

/// V0.6.0 Wave 3 — pick the canonical production adapter for `vendor`.
///
/// - `Claude` → [`ClaudeTuiAdapter`] (the mode 3 chat adapter).
/// - `Codex` → also [`ClaudeTuiAdapter`] today; the codex-exec-impl
///   teammate's `CodexAppServerAdapter` will replace this arm in a
///   follow-up commit. Falling back to the Claude adapter (rather
///   than panicking) keeps any mis-registered Codex bot inert
///   (`start_thread` will fail noisily on the wrong vendor) instead
///   of taking down the daemon.
pub fn default_adapter_factory() -> AdapterFactory {
    Arc::new(|vendor: AgentVendor| match vendor {
        AgentVendor::Claude => {
            Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
        }
        AgentVendor::Codex => {
            // TODO(wave-3 codex-exec-impl): swap to CodexAppServerAdapter
            // once it lands.
            Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
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

    let inbound_consumer = spawn_inbound_consumer(
        inbound_rx,
        channels.clone(),
        sec.clone(),
        mailbox.clone(),
        executor.clone(),
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
                tick_supervisors(&bots, &registry, Some(&projects_root), &factory).await;
                drain_inboxes(&bots, &registry, &projects_root).await;
                drain_outboxes(&bots, &channels, &projects_root).await;

                if let Some(max) = args.max_runtime {
                    if started.elapsed() >= max {
                        tracing::info!("max_runtime reached; exiting");
                        break Ok(());
                    }
                }
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

/// V0.6.1 F132 — assemble the Channel set the daemon listens on.
///
/// Resolution order:
/// 1. `args.channels_override` (tests inject `MockChannel`),
/// 2. `creds.telegram` → build a [`TelegramChannel`] with the union of
///    the user-configured allowlist + every registered telegram bot's
///    `im_chat_id`,
/// 3. (slack / discord — TODO in V0.7: providers exist but the host
///    probe's first round only exercises telegram).
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
            // cheap (single-bot host probe), V0.7 will cache.
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
/// **collide** here (last-wins) — workflow.yaml's `chat_handle` field
/// (V0.7) will land per-bot custom handles. For F132 the typical
/// production registry has one bot ("web3op_bot") on one slug, so the
/// collision is theoretical until V0.7.
fn build_handle_map() -> HandleMap {
    let mut map = HandleMap::new();
    if let Ok(bots) = list_bots() {
        for b in bots {
            map.insert(&b.role, &b.workflow_slug, &b.role);
        }
    }
    map
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
                    let received_ms =
                        env.received_at.timestamp_millis().max(0) as u128;
                    let queue_age_ms =
                        now_unix_ms().saturating_sub(received_ms) as u64;
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
        let cursor_path =
            outbound::outbound_cursor_path(projects_root, &bot.workflow_slug, &bot.role);
        let cursor = outbound::load_cursor(&cursor_path);
        let (rows, new_cursor) = match outbound::read_new_rows(&path, &cursor) {
            Ok(x) => x,
            Err(err) => {
                tracing::warn!(
                    slug = %bot.workflow_slug,
                    role = %bot.role,
                    error = %err,
                    "imd: F134 outbound: read_new_rows failed; continuing"
                );
                continue;
            }
        };
        if rows.is_empty() {
            // Still persist a cursor advance on truncation (read_new_rows
            // rewinds to 0 on shrink — record the new position so we
            // don't keep rewinding on every tick).
            if new_cursor.position != cursor.position {
                if let Err(err) = outbound::save_cursor(&cursor_path, &new_cursor) {
                    tracing::warn!(
                        slug = %bot.workflow_slug,
                        role = %bot.role,
                        error = %err,
                        "imd: F134 outbound: save_cursor (truncation rewind) failed"
                    );
                }
            }
            continue;
        }
        // Latency: log per-row tail age so we can see how long each
        // assistant row sat in turns.jsonl before this tick picked it
        // up. Bounded by `args.tick` (5s default) but a busy daemon
        // with many bots can drift higher; this log makes it visible.
        for row in &rows {
            let tail_age_ms = row.ts.map(|t| {
                crate::latency::now_unix_ms()
                    .saturating_sub(t.timestamp_millis().max(0) as u128) as u64
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
        }
        let sent =
            outbound::forward_new_rows(&rows, channel.as_ref(), &bot.im_chat_id, &[]).await;
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
        if let Err(err) = outbound::save_cursor(&cursor_path, &new_cursor) {
            tracing::warn!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                error = %err,
                "imd: F134 outbound: save_cursor failed"
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
pub(crate) async fn tick_supervisors(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    projects_root_override: Option<&Path>,
    factory: &AdapterFactory,
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
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

        // Tick 2: heartbeat missing → decide() returns Restart → close + start.
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
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
        )
        .await;
        assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
        let supervisors = registry.lock().await.all();
        assert!(!supervisors[0].is_started().await);
    }
}
