//! Main daemon event loop.
//!
//! Composes credentials → Channel listeners → gateway routing. The loop
//! is `tokio::select`-driven across:
//!
//! - one inbound mpsc receiver (multiplexed across active Channels),
//! - a SIGTERM future for graceful shutdown,
//! - an optional max-runtime watchdog (test-only — production is `0`).

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use ccteam_harness::execution::{ClaudeTuiAdapter, CodexAppServerAdapter};
use ccteam_harness::{AgentVendor, HarnessAdapter};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::acl::AclPolicy;
use crate::bot_mpsc::{bot_key, BotChannelMap, InboxItem, OutboundItem};
use crate::credentials::{self, Credentials};
use crate::gateway::Gateway;
use crate::latency::now_unix_ms;
use crate::router::{self, HandleMap};
use crate::supervisor::{self, BotSupervisor};
use crate::three_layer_sec::{SecOutcome, ThreeLayerSec};
use crate::transport::providers::telegram::TelegramChannel;
use crate::transport::{Channel, ChannelMessage, SendMessage};
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
/// - `Codex` → [`CodexAppServerAdapter`] — mode-3 chat sessions use the
///   app-server JSON-RPC control plane so `/compact` and `/review` map to
///   Codex-native RPCs instead of `codex exec` subprocess turns.
pub fn default_adapter_factory() -> AdapterFactory {
    Arc::new(|vendor: AgentVendor| match vendor {
        AgentVendor::Claude => {
            Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
        }
        AgentVendor::Codex => {
            Arc::new(CodexAppServerAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
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
    /// Legacy supervisor tick interval. v8.1 daemon no longer uses
    /// this; the field remains until P6 CLI/test surface cleanup.
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
    /// Returns the live (existing-or-new) supervisor handle. Wrapper
    /// around [`Self::ensure_with_config`] that passes an empty F190
    /// config-yaml tier (legacy / test callers without a config map).
    pub fn ensure(
        &mut self,
        reg: &BotRegistration,
        projects_root: &Path,
        factory: &AdapterFactory,
    ) -> Arc<BotSupervisor> {
        self.ensure_with_config(reg, projects_root, factory, &HashMap::new())
    }

    /// V0.6.8 F190 — config-yaml-aware companion to [`Self::ensure`].
    /// Wires the loaded `~/.ccteam/config.yaml::projects[]` slug → path
    /// map into the new [`BotSupervisor`] so its
    /// `project_dir()` / `bot_dir()` resolution honors the F190 tier
    /// for legacy registrations.
    pub fn ensure_with_config(
        &mut self,
        reg: &BotRegistration,
        projects_root: &Path,
        factory: &AdapterFactory,
        config_projects: &std::collections::HashMap<String, PathBuf>,
    ) -> Arc<BotSupervisor> {
        let k = Self::key(reg);
        self.inner
            .entry(k)
            .or_insert_with(|| {
                let adapter = factory(reg.vendor);
                Arc::new(BotSupervisor::new_with_config(
                    reg.clone(),
                    projects_root.to_path_buf(),
                    adapter,
                    config_projects.clone(),
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
/// the standalone `ccteam-im` historical entry point and from the
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
        "ccteam-im daemon starting"
    );

    let factory = args
        .adapter_factory
        .clone()
        .unwrap_or_else(default_adapter_factory);
    // V0.6.8 F190 — load `~/.ccteam/config.yaml::projects[]` once at
    // startup so legacy bots (no `reg.project_dir`) whose project
    // lives outside the projects_root tree resolve correctly. Daemon
    // restart is the standard "config changed" workflow, so a one-shot
    // disk read here is enough (no live reload). A missing config.yaml
    // / parse error yields an empty map; the third tier of
    // `resolve_project_dir` (projects_root/slug) still applies.
    let config_projects: std::collections::HashMap<String, PathBuf> = {
        let ccteam_root = crate::default_ccteam_root_public();
        match crate::load_config_projects_map(&ccteam_root) {
            Ok(map) => {
                tracing::info!(
                    entries = map.len(),
                    "F190: loaded ~/.ccteam/config.yaml::projects[] for legacy bot resolution"
                );
                map
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "F190: failed to load config.yaml; legacy bots fall through to projects_root/slug"
                );
                std::collections::HashMap::new()
            }
        }
    };

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
    replay_durable_outbox(&channels).await;
    let gateway = Arc::new(Mutex::new(build_gateway(
        factory.clone(),
        &projects_root,
        &config_projects,
        &initial,
    )));

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

    // Shared inbound security state. v8.1 routes accepted messages
    // directly through the gateway; mailbox/admin/supervisor tick paths
    // are legacy helpers and are not part of the daemon hot path.
    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));

    let inbound_consumer =
        spawn_inbound_consumer(inbound_rx, channels.clone(), sec.clone(), gateway.clone());

    tracing::info!(
        channels = channels.len(),
        bots = initial.len(),
        "ccteam-im: gateway router started (no supervisor tick)"
    );
    if let Err(err) = supervisor::refresh_global_heartbeat() {
        tracing::warn!(error = %err, "heartbeat refresh failed");
    }

    let mut shutdown: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(shutdown);
    let mut max_runtime: Pin<Box<dyn Future<Output = ()> + Send>> = match args.max_runtime {
        Some(max) => Box::pin(tokio::time::sleep(max)),
        None => Box::pin(std::future::pending()),
    };

    let result = tokio::select! {
        _ = &mut shutdown => {
            tracing::info!("ccteam-im: shutdown signalled; exiting cleanly");
            Ok(())
        }
        _ = &mut max_runtime => {
            tracing::info!("max_runtime reached; exiting");
            Ok(())
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

fn build_gateway(
    factory: AdapterFactory,
    projects_root: &Path,
    config_projects: &HashMap<String, PathBuf>,
    bots: &[BotRegistration],
) -> Gateway {
    let (default_slug, default_dir) = bots
        .first()
        .map(|bot| {
            (
                bot.workflow_slug.clone(),
                bot.project_root_with_config(projects_root, config_projects),
            )
        })
        .or_else(|| {
            config_projects
                .iter()
                .next()
                .map(|(slug, path)| (slug.clone(), path.clone()))
        })
        .unwrap_or_else(|| ("default".to_string(), projects_root.join("default")));

    let mut gateway = Gateway::new_with_factory(factory, default_slug, default_dir);
    for (slug, path) in config_projects {
        gateway.register_project(slug.clone(), path.clone());
    }
    for bot in bots {
        gateway.register_bot_template(
            bot,
            bot.project_root_with_config(projects_root, config_projects),
        );
    }
    if let Err(err) = gateway.enable_persistence(
        crate::default_ccteam_root_public()
            .join("imd")
            .join("gateway-state.json"),
    ) {
        tracing::warn!(
            error = %err,
            "ccteam-im: failed to load gateway state; starting with empty route table"
        );
    }
    gateway
}

/// Drain the mpsc receiving from every listener and route each accepted
/// `ChannelMessage` directly through the v8.1 gateway.
fn spawn_inbound_consumer(
    mut rx: tokio::sync::mpsc::Receiver<ChannelMessage>,
    channels: ChannelMap,
    sec: Arc<Mutex<ThreeLayerSec>>,
    gateway: Arc<Mutex<Gateway>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let cid = msg.id.clone();
            let route_t0 = std::time::Instant::now();
            tracing::info!(
                event = "latency",
                stage = "imd.route.begin",
                cid = %cid,
                channel = %msg.channel,
                "latency imd.route.begin"
            );
            let Some(channel) = channels.get(&msg.channel).cloned() else {
                tracing::debug!(
                    channel = %msg.channel,
                    sender = %msg.sender,
                    "imd: no Channel for inbound msg.channel; dropping"
                );
                continue;
            };

            let clean_payload =
                match sec
                    .lock()
                    .await
                    .evaluate(&msg.channel, &msg.sender, &msg.content)
                {
                    SecOutcome::Accept { payload } => payload,
                    other => {
                        tracing::warn!(
                            cid = %cid,
                            outcome = ?other,
                            "ccteam-im: gateway inbound rejected by security layer"
                        );
                        continue;
                    }
                };

            let replies = gateway
                .lock()
                .await
                .handle_text(&msg.channel, &msg.reply_target, &msg.sender, &clean_payload)
                .await;
            match replies {
                Ok(replies) => {
                    for (seq, reply) in replies.into_iter().enumerate() {
                        let out = SendMessage::new(reply, msg.reply_target.clone())
                            .in_thread(msg.thread_ts.clone());
                        send_gateway_outbound(&cid, seq, &msg.channel, channel.as_ref(), out).await;
                    }
                    tracing::info!(
                        event = "latency",
                        stage = "imd.gateway.done",
                        cid = %cid,
                        elapsed_ms = route_t0.elapsed().as_millis() as u64,
                        "latency imd.gateway.done"
                    );
                }
                Err(err) => {
                    let out =
                        SendMessage::new(format!("gateway error: {err}"), msg.reply_target.clone())
                            .in_thread(msg.thread_ts.clone());
                    send_gateway_outbound(&cid, 0, &msg.channel, channel.as_ref(), out).await;
                    tracing::warn!(
                        event = "latency",
                        stage = "imd.gateway.err",
                        cid = %cid,
                        elapsed_ms = route_t0.elapsed().as_millis() as u64,
                        error = %err,
                        "latency imd.gateway.err"
                    );
                }
            }
        }
        tracing::debug!("imd: inbound consumer exited (all senders closed)");
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableOutboundRow {
    ts_ms: u64,
    id: String,
    inbound_id: String,
    channel: String,
    state: DurableOutboundState,
    message: SendMessage,
    platform_message_id: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DurableOutboundState {
    Queued,
    Sent,
    Failed,
}

fn durable_outbox_path() -> PathBuf {
    crate::default_ccteam_root_public()
        .join("imd")
        .join("outbound.jsonl")
}

async fn send_gateway_outbound(
    inbound_id: &str,
    seq: usize,
    channel_name: &str,
    channel: &(dyn Channel + Send + Sync),
    message: SendMessage,
) {
    let id = format!("{inbound_id}-{seq}");
    append_durable_outbound(DurableOutboundRow {
        ts_ms: now_unix_ms_u64(),
        id: id.clone(),
        inbound_id: inbound_id.to_string(),
        channel: channel_name.to_string(),
        state: DurableOutboundState::Queued,
        message: message.clone(),
        platform_message_id: None,
        error: None,
    });
    finish_durable_outbound_send(id, inbound_id, channel_name, channel, message).await;
}

async fn finish_durable_outbound_send(
    id: String,
    inbound_id: &str,
    channel_name: &str,
    channel: &(dyn Channel + Send + Sync),
    message: SendMessage,
) {
    match channel.send(&message).await {
        Ok(platform_message_id) => {
            append_durable_outbound(DurableOutboundRow {
                ts_ms: now_unix_ms_u64(),
                id,
                inbound_id: inbound_id.to_string(),
                channel: channel_name.to_string(),
                state: DurableOutboundState::Sent,
                message,
                platform_message_id,
                error: None,
            });
        }
        Err(err) => {
            append_durable_outbound(DurableOutboundRow {
                ts_ms: now_unix_ms_u64(),
                id,
                inbound_id: inbound_id.to_string(),
                channel: channel_name.to_string(),
                state: DurableOutboundState::Failed,
                message,
                platform_message_id: None,
                error: Some(err.to_string()),
            });
            tracing::warn!(
                inbound_id,
                channel = %channel_name,
                error = %err,
                "ccteam-im: gateway outbound send failed"
            );
        }
    }
}

async fn replay_durable_outbox(channels: &ChannelMap) {
    let path = durable_outbox_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut latest: HashMap<String, DurableOutboundRow> = HashMap::new();
    for (line_idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<DurableOutboundRow>(line) {
            Ok(row) => {
                latest.insert(row.id.clone(), row);
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    line = line_idx + 1,
                    error = %err,
                    "ccteam-im: ignoring malformed durable outbound row"
                );
            }
        }
    }
    for row in latest
        .into_values()
        .filter(|row| row.state != DurableOutboundState::Sent)
    {
        let Some(channel) = channels.get(&row.channel) else {
            append_durable_outbound(DurableOutboundRow {
                ts_ms: now_unix_ms_u64(),
                id: row.id,
                inbound_id: row.inbound_id,
                channel: row.channel,
                state: DurableOutboundState::Failed,
                message: row.message,
                platform_message_id: None,
                error: Some("replay failed: channel is not configured".to_string()),
            });
            continue;
        };
        finish_durable_outbound_send(
            row.id,
            &row.inbound_id,
            &row.channel,
            channel.as_ref(),
            row.message,
        )
        .await;
    }
}

fn append_durable_outbound(row: DurableOutboundRow) {
    if let Err(err) = append_durable_outbound_inner(&row) {
        tracing::warn!(
            id = %row.id,
            state = ?row.state,
            error = %err,
            "ccteam-im: durable outbound append failed"
        );
    }
}

fn append_durable_outbound_inner(row: &DurableOutboundRow) -> Result<()> {
    let path = durable_outbox_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    serde_json::to_writer(&mut file, row)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn now_unix_ms_u64() -> u64 {
    now_unix_ms().min(u128::from(u64::MAX)) as u64
}

/// Pure variant of [`build_handle_map`] — operates on a borrowed bot
/// slice so unit tests can probe the collision resolution rule without
/// touching the on-disk registry.
pub fn build_handle_map_from_bots(bots: &[BotRegistration]) -> HandleMap {
    let mut sorted: Vec<&BotRegistration> = bots.iter().collect();
    sorted.sort_by(|a, b| {
        a.workflow_slug
            .cmp(&b.workflow_slug)
            .then_with(|| a.role.cmp(&b.role))
    });

    let mut map = HandleMap::new();
    let mut claimed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for b in sorted {
        let base = b.effective_handle().to_string();
        let handle = if claimed.contains(&base) {
            crate::router::collision_suffix(&base, &b.workflow_slug)
        } else {
            base.clone()
        };
        claimed.insert(handle.clone());
        // Bare-name claim is also reserved so a later identical base
        // can't sneak in via a numerically-smaller slug suffix.
        claimed.insert(base);
        map.insert(&handle, &b.workflow_slug, &b.role);
    }
    map
}

/// Outcome of one cross-bot @mention scan. Exposed mainly so tests can
/// assert on the routing decision without inspecting tracing output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrossBotDispatch {
    /// Reply had no `@<handle>` mention.
    NoMention,
    /// Handle parsed but no registered bot answers to it.
    UnknownHandle {
        /// The literal handle parsed (without `@`).
        handle: String,
    },
    /// Handle resolved to the sender itself — dropped to avoid loops.
    SelfMention,
    /// Resolved + within budget + target mpsc wired — pushed.
    Dispatched {
        /// Target slug the synthetic InboxItem was sent to.
        to_slug: String,
        /// Target role the synthetic InboxItem was sent to.
        to_role: String,
        /// `sender_hop + 1`.
        hop: u8,
    },
    /// Resolved + budget exhausted (`hop + 1 >= MAX_HOPS`).
    HopExceeded {
        /// Target slug that would have received the InboxItem.
        to_slug: String,
        /// Target role that would have received the InboxItem.
        to_role: String,
        /// The hop value that exceeded the budget (`sender_hop + 1`).
        hop: u8,
    },
    /// Resolved but the target bot's mpsc isn't wired in
    /// `bot_channels` yet (race: target registered after sender's
    /// dispatcher spawned, supervisor tick hasn't run `ensure_bot_channels`
    /// for it yet). Dropped — no envelope file exists so safety-net
    /// drain doesn't cover this.
    TargetNotWired {
        /// Target slug the lookup resolved to.
        to_slug: String,
        /// Target role the lookup resolved to.
        to_role: String,
    },
    /// Resolved + budget OK but the target inbox is full (try_send
    /// returned Err). Dropped — same reason as TargetNotWired.
    InboxFull {
        /// Target slug whose inbox was full.
        to_slug: String,
        /// Target role whose inbox was full.
        to_role: String,
    },
}

/// V0.6.8 F193 — scan an OutboundItem's content for a `@<otherbot>`
/// mention; if it resolves to a registered bot AND we have hop budget,
/// synthesize a fresh `InboxItem` (hop = sender_hop + 1) and `try_send`
/// it directly into that bot's `inbox_tx`. Returns the dispatch outcome
/// for tests / structured logging.
///
/// Called from `spawn_outbound_dispatcher` immediately after a
/// successful `channel.send` (assistant rows only — non-assistant rows
/// short-circuit before the scan). Cursor-skip + non-assistant paths
/// don't fire cross-mention by design: the safety-net drain doesn't
/// emit synthetic mpsc items, so duplicating the scan there would
/// double-route on overlap.
///
/// Self-mention guard uses the resolved `(slug, role)` tuple, NOT the
/// handle string — a bot's `chat_handle` may override its role, and two
/// slugs can legitimately share a role name (e.g. two squads with a
/// `reporter`).
pub async fn dispatch_cross_bot_mention(
    item: &OutboundItem,
    sender_slug: &str,
    sender_role: &str,
    bots: &[BotRegistration],
    bot_channels: &BotChannelMap,
) -> CrossBotDispatch {
    let Some((handle, rest)) = router::parse_first_mention(&item.content) else {
        return CrossBotDispatch::NoMention;
    };
    let handles = build_handle_map_from_bots(bots);
    let Some((target_slug, target_role)) = handles.lookup(&handle) else {
        return CrossBotDispatch::UnknownHandle { handle };
    };
    if target_slug.as_str() == sender_slug && target_role.as_str() == sender_role {
        return CrossBotDispatch::SelfMention;
    }
    let next_hop = item.hop.saturating_add(1);
    if !router::within_hop_budget(next_hop) {
        tracing::info!(
            event = "cross_bot_mention_hop_exceeded",
            from_slug = %sender_slug,
            from_role = %sender_role,
            to_slug = %target_slug,
            to_role = %target_role,
            hop = next_hop,
            max = router::MAX_HOPS,
            "F193 mention dropped (hop budget exceeded)"
        );
        return CrossBotDispatch::HopExceeded {
            to_slug: target_slug,
            to_role: target_role,
            hop: next_hop,
        };
    }
    let guard = bot_channels.lock().await;
    let Some(ch) = guard.get(&bot_key(&target_slug, &target_role)) else {
        tracing::debug!(
            from_slug = %sender_slug,
            from_role = %sender_role,
            to_slug = %target_slug,
            to_role = %target_role,
            "F193 target mpsc not yet wired; cross-mention dropped"
        );
        return CrossBotDispatch::TargetNotWired {
            to_slug: target_slug,
            to_role: target_role,
        };
    };
    // Synthetic InboxItem: `path = PathBuf::new()` — there is no
    // envelope file on disk. The inbox dispatcher's `remove_file` will
    // silently fail (correct: nothing to unlink).
    let item_synth = InboxItem {
        cid: format!("cross-{}", item.turn_id),
        slug: target_slug.clone(),
        role: target_role.clone(),
        payload: rest,
        path: PathBuf::new(),
        enqueue_unix_ms: now_unix_ms(),
        hop: next_hop,
    };
    match ch.inbox_tx.try_send(item_synth) {
        Ok(_) => {
            tracing::info!(
                event = "cross_bot_mention",
                from_slug = %sender_slug,
                from_role = %sender_role,
                to_slug = %target_slug,
                to_role = %target_role,
                hop = next_hop,
                turn_id = %item.turn_id,
                "F193 cross-bot @mention routed via mpsc"
            );
            CrossBotDispatch::Dispatched {
                to_slug: target_slug,
                to_role: target_role,
                hop: next_hop,
            }
        }
        Err(err) => {
            tracing::warn!(
                event = "cross_bot_mention_drop",
                from_slug = %sender_slug,
                from_role = %sender_role,
                to_slug = %target_slug,
                to_role = %target_role,
                error = %err,
                "F193 target inbox saturated; dropped"
            );
            CrossBotDispatch::InboxFull {
                to_slug: target_slug,
                to_role: target_role,
            }
        }
    }
}

/// Run the daemon with the default SIGINT (ctrl-C) shutdown trigger.
///
/// Preserved as the lib-level entry point used by integration tests
/// that don't supply their own shutdown future. V0.6.1 F130 folded the
/// `ccteam-im` binary into `ccteam start`, so production now goes via
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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn tick_supervisors(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    projects_root_override: Option<&Path>,
    factory: &AdapterFactory,
    bot_channels: Option<&BotChannelMap>,
) {
    tick_supervisors_with_config(
        bots,
        registry,
        projects_root_override,
        factory,
        bot_channels,
        &HashMap::new(),
    )
    .await
}

/// V0.6.8 F190 — config-yaml-aware companion to [`tick_supervisors`].
/// Daemon main loop reads `~/.ccteam/config.yaml::projects[]` once at
/// startup and passes the slug → path map through here so legacy bots
/// (no `reg.project_dir`) whose project lives outside the
/// projects_root tree resolve to the right bot_dir for signal /
/// heartbeat decisions.
pub(crate) async fn tick_supervisors_with_config(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    projects_root_override: Option<&Path>,
    factory: &AdapterFactory,
    bot_channels: Option<&BotChannelMap>,
    config_projects: &HashMap<String, PathBuf>,
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

    // First pass: ensure a supervisor exists per bot. F190 — pass the
    // config-yaml map so the new supervisor's `project_dir()` /
    // `bot_dir()` honors the slug → path lookup for legacy
    // registrations.
    let supervisors: Vec<(BotRegistration, Arc<BotSupervisor>)> = {
        let mut reg = registry.lock().await;
        bots.iter()
            .map(|b| {
                let sup = reg.ensure_with_config(b, projects_root, factory, config_projects);
                (b.clone(), sup)
            })
            .collect()
    };

    // Second pass: decide + apply per bot (drop the registry lock for
    // each adapter call so a slow start_thread doesn't stall other
    // bots' decisions). F190 — use `decide_with_config` so signal /
    // heartbeat path resolution mirrors the supervisor's spawn path.
    for (bot, sup) in supervisors {
        let state = sup.state_snapshot().await;
        let action = supervisor::decide_with_config(
            projects_root,
            &bot,
            &state,
            SystemTime::now(),
            config_projects,
        );
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
    use ccteam_harness::{
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
            chat_handle: None,
            project_dir: None,
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
            chat_handle: None,
            project_dir: None,
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

    /// `default_adapter_factory` must route the Codex arm to the mode-3
    /// app-server adapter, not the legacy exec path or Claude fallback.
    #[test]
    fn default_adapter_factory_codex_arm_returns_app_server_adapter() {
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
        assert_eq!(codex.name(), "codex-app-server");
    }
}
