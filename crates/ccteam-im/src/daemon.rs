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
use std::time::Duration;

use anyhow::Result;
use ccteam_harness::execution::{ClaudeTuiAdapter, CodexAppServerAdapter};
use ccteam_harness::{AgentVendor, HarnessAdapter};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::acl::AclPolicy;
use crate::bot_mpsc::{bot_key, BotChannelMap, InboxItem, OutboundItem};
use crate::credentials::{self, Credentials};
use crate::gateway::{Gateway, GatewayEvent};
use crate::latency::now_unix_ms;
use crate::router::{self, HandleMap};
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
#[derive(Clone, Default)]
pub struct DaemonArgs {
    /// Override credentials path (`None` → default).
    pub credentials: Option<PathBuf>,
    /// Override registry root.
    pub registry: Option<PathBuf>,
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
    /// Additional channels supplied by the embedding process. `ccteam start`
    /// uses this to add the browser web-chat transport while preserving
    /// credential-driven IM channels.
    pub extra_channels: Option<ChannelMap>,
}

impl std::fmt::Debug for DaemonArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonArgs")
            .field("credentials", &self.credentials)
            .field("registry", &self.registry)
            .field("max_runtime", &self.max_runtime)
            .field("adapter_factory", &self.adapter_factory.is_some())
            .field(
                "channels_override",
                &self.channels_override.as_ref().map(|m| m.len()),
            )
            .field(
                "extra_channels",
                &self.extra_channels.as_ref().map(|m| m.len()),
            )
            .finish()
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

    // V0.6.1 F132 — projects_root used for gateway project fallback
    // and legacy mailbox path resolution.
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
    let mut gateway_inner =
        build_gateway(factory.clone(), &projects_root, &config_projects, &initial);
    gateway_inner.resume_restored_sessions().await;
    log_orphan_chat_sessions(&gateway_inner).await;
    let (gateway_event_tx, gateway_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
    gateway_inner.set_event_sink(gateway_event_tx);
    let gateway = Arc::new(Mutex::new(gateway_inner));

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
    let gateway_event_consumer = spawn_gateway_event_consumer(gateway_event_rx, channels.clone());

    tracing::info!(
        channels = channels.len(),
        bots = initial.len(),
        "ccteam-im: gateway router started (no supervisor tick)"
    );

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
    gateway_event_consumer.abort();
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
    if let Some(extra) = args.extra_channels.clone() {
        out.extend(extra);
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
    // Enable `/newproject <slug> <path>`: config.yaml lives under the
    // ccteam root; new projects are scaffolded at the caller's path.
    gateway.enable_project_creation(ccteam_core::CcteamPaths {
        root: crate::default_ccteam_root_public(),
        projects_root: projects_root.to_path_buf(),
    });
    if let Err(err) = gateway.enable_persistence(crate::default_gateway_state_path()) {
        tracing::warn!(
            error = %err,
            "ccteam-im: failed to load gateway state; starting with empty route table"
        );
    }
    gateway
}

/// Surface `ccteam-chat-*` processes that outlived a prior daemon but are not
/// in the restored route table (orphans). Read-only control-plane enumeration:
/// it only LOGS — reclaim stays explicit and opt-in (the "never auto-kill a
/// long session" redline).
///
/// Scoped to the tmux backend: tmux sessions outlive the daemon, whereas the
/// bundled rmux backend is daemon-tracked (its sessions die with the daemon, so
/// there is nothing to orphan). Enumerating only on an explicit
/// `CCTEAM_MUX_BACKEND=tmux` also keeps daemon startup side-effect-free on the
/// default backend. Timeout-guarded so a stale tmux server never blocks boot.
///
/// A richer operator surface — orphans in `@ccteam list` / a
/// `ccteam sessions --all` command, plus an explicit reclaim verb — can reuse
/// [`Gateway::render_all_sessions`], which already renders tracked + orphan
/// rows; this startup hook is the read-only visibility half.
async fn log_orphan_chat_sessions(gateway: &Gateway) {
    if std::env::var("CCTEAM_MUX_BACKEND").ok().as_deref() != Some("tmux") {
        return;
    }
    let backend = ccteam_harness::TmuxBackend::new();
    let inventory = match tokio::time::timeout(
        Duration::from_secs(2),
        gateway.inventory_via_backend(&backend),
    )
    .await
    {
        Ok(Ok(inventory)) => inventory,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "ccteam-im: orphan reconcile: backend enumeration unavailable");
            return;
        }
        Err(_) => {
            tracing::debug!("ccteam-im: orphan reconcile: backend enumeration timed out");
            return;
        }
    };
    for orphan in &inventory.orphans {
        tracing::warn!(
            session = %orphan.name,
            slug = %orphan.slug,
            role = %orphan.role,
            "ccteam-im: orphaned chat session (untracked; survived a prior daemon) — reclaim explicitly, never auto-killed"
        );
    }
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

fn spawn_gateway_event_consumer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<GatewayEvent>,
    channels: ChannelMap,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            let Some(channel) = channels.get(&evt.channel).cloned() else {
                tracing::warn!(
                    channel = %evt.channel,
                    event_id = %evt.id,
                    "ccteam-im: gateway event dropped because channel is not configured"
                );
                continue;
            };
            let out = SendMessage::new(evt.content, evt.chat_id).in_thread(evt.thread_ts);
            send_gateway_outbound(&evt.id, 0, &evt.channel, channel.as_ref(), out).await;
        }
        tracing::debug!("imd: gateway event consumer exited");
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
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _guard = LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|err| err.into_inner());
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
    /// dispatcher spawned). Dropped — no envelope file exists so
    /// safety-net drain doesn't cover this.
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

/// Compatibility shim — `lib.rs` re-exports this so the existing
/// `pub use daemon::run_daemon;` keeps working without forcing
/// callers to depend on this module path directly.
pub fn _link_check(_c: &Credentials) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex as StdMutex, OnceLock};
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread", start_paused = false)]
    async fn daemon_boots_and_exits_on_max_runtime() {
        let _guard = env_lock();
        // Point HOME at a tempdir so no real credentials are read.
        let tmp = TempDir::new().unwrap();
        let old_home = std::env::var_os("HOME");
        let old_ccteam_home = std::env::var_os("CCTEAM_HOME");
        std::env::set_var("HOME", tmp.path());
        std::env::set_var("CCTEAM_HOME", tmp.path().join(".ccteam"));
        let args = DaemonArgs {
            credentials: None,
            registry: None,
            max_runtime: Some(Duration::from_millis(120)),
            adapter_factory: None,
            channels_override: None,
            extra_channels: None,
        };
        run_daemon(args).await.unwrap();
        restore_env("CCTEAM_HOME", old_ccteam_home);
        restore_env("HOME", old_home);
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
