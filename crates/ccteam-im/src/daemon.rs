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
use crate::gateway::{Gateway, GatewayEvent, GatewayEventKind};
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
///
/// F10: **per-vendor singleton.** Exactly ONE `ClaudeTuiAdapter` and ONE
/// `CodexAppServerAdapter` are constructed here; every factory call
/// `.clone()`s the matching `Arc`. Because `CodexAppServerAdapter`
/// memoises a single `codex app-server` child (stdio transport), one
/// shared adapter ⇒ one memoised client ⇒ one codex app-server child for
/// the whole daemon, instead of a fresh child per chat session.
pub fn default_adapter_factory() -> AdapterFactory {
    let claude: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(ClaudeTuiAdapter::new());
    let codex: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(CodexAppServerAdapter::new());
    Arc::new(move |vendor: AgentVendor| match vendor {
        AgentVendor::Claude => Arc::clone(&claude),
        AgentVendor::Codex => Arc::clone(&codex),
    })
}

/// CLI arguments forwarded from `main.rs`.
///
/// Not `Clone` — it owns a one-shot `gateway_event_rx` (V0.8.4 P2b).
#[derive(Default)]
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
    /// V0.8.4 P2b — externally-created gateway-event channel. When both
    /// halves are `Some`, the daemon uses them instead of creating its
    /// own, so `ccteam start` can clone the sender into the `mcp.sock`
    /// handler (`chat_send_file` reuses the same outbound funnel). `None`
    /// (standalone `ccteam-im run`) → the daemon makes its own channel.
    pub gateway_event_tx: Option<tokio::sync::mpsc::UnboundedSender<GatewayEvent>>,
    /// Receiver half paired with [`Self::gateway_event_tx`].
    pub gateway_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<GatewayEvent>>,
    /// v0.8.5 D6 — shared pending-interaction registry. When `Some`, the
    /// daemon injects it into the gateway via [`Gateway::set_pending`] so the
    /// gateway and the `mcp.sock` handler (which `ccteam start` hands the same
    /// `Arc`) share one registry: the handler registers External-origin
    /// `interaction/ask` prompts, the gateway resolves them on inbound. `None`
    /// (standalone `ccteam-im run`, no mcp.sock) → the gateway keeps its own
    /// fresh registry.
    pub pending: Option<Arc<Mutex<crate::pending::PendingInteractions>>>,
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
            .field("gateway_event_tx", &self.gateway_event_tx.is_some())
            .field("gateway_event_rx", &self.gateway_event_rx.is_some())
            .field("pending", &self.pending.is_some())
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
pub async fn run_daemon_with_shutdown<F>(mut args: DaemonArgs, shutdown: F) -> Result<()>
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
        has_lark = creds.lark.is_some(),
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
    // v0.8.5 P1 — advertise the gateway's own commands in each channel's
    // native menu (Telegram `setMyCommands`; default no-op elsewhere). Done
    // once at startup; passthrough vendor slashes are intentionally absent.
    {
        let specs = crate::gateway::menu_command_specs();
        for (name, ch) in channels.iter() {
            if let Err(err) = ch.register_commands(&specs).await {
                tracing::warn!(
                    channel = %name,
                    error = %err,
                    "imd: register_commands (menu) failed"
                );
            }
        }
    }
    replay_durable_outbox(&channels).await;
    let mut gateway_inner =
        build_gateway(factory.clone(), &projects_root, &config_projects, &initial);
    // v0.8.5 D6 — inject the shared pending-interaction registry when one was
    // supplied (`ccteam start` hands the same `Arc` to the mcp.sock handler so
    // the D6 `interaction/ask` ingress and the gateway resolve through one
    // registry). Standalone runs leave the gateway's own fresh registry.
    if let Some(pending) = args.pending.clone() {
        gateway_inner.set_pending(pending);
    }
    gateway_inner.resume_restored_sessions().await;
    log_orphan_chat_sessions(&gateway_inner).await;
    // V0.8.4 P2b — use the externally-supplied channel when `ccteam start`
    // provided one (so the mcp.sock handler shares this sender); else make
    // our own (standalone `ccteam-im run`).
    let (gateway_event_tx, gateway_event_rx) =
        match (args.gateway_event_tx.take(), args.gateway_event_rx.take()) {
            (Some(tx), Some(rx)) => (tx, rx),
            _ => tokio::sync::mpsc::unbounded_channel::<GatewayEvent>(),
        };
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

/// One row of the provider table [`build_channels`] walks. The builder
/// inspects credentials + registered bots and yields the live [`Channel`]
/// when its credential block is present, or `None` when unconfigured.
/// Adding a provider is one row + one `build_*` fn — the loop never
/// names a platform.
///
/// Builders are stateless free fns, so `fn`-pointers (not `Box<dyn Fn>`)
/// keep the table a zero-alloc `const` that's `#[cfg]`-gateable per-row.
type ChannelBuilder =
    fn(&Credentials, &[BotRegistration]) -> Option<Arc<dyn Channel + Send + Sync>>;

/// The platform-agnostic provider table. Each row pairs a channel key
/// with its builder; `#[cfg]` on const-array elements drops a row when
/// its feature is off. This is the single place a new IM provider is
/// registered for the daemon.
const CHANNEL_BUILDERS: &[(&str, ChannelBuilder)] = &[
    #[cfg(feature = "telegram")]
    ("telegram", build_telegram_channel),
    #[cfg(feature = "slack")]
    ("slack", build_slack_channel),
    #[cfg(feature = "discord")]
    ("discord", build_discord_channel),
    #[cfg(feature = "lark")]
    ("lark", build_lark_channel),
];

/// Assemble the Channel set the daemon listens on.
///
/// Resolution order:
/// 1. `args.channels_override` (tests inject `MockChannel`) — wins,
/// 2. each [`CHANNEL_BUILDERS`] row whose credential block is present,
/// 3. `args.extra_channels` (web-chat WS) — merged last.
fn build_channels(args: &DaemonArgs, creds: &Credentials, bots: &[BotRegistration]) -> ChannelMap {
    if let Some(ch) = args.channels_override.clone() {
        return ch; // test MockChannel injection still wins, unchanged
    }
    let mut out: ChannelMap = HashMap::new();
    for (name, builder) in CHANNEL_BUILDERS {
        if let Some(ch) = builder(creds, bots) {
            out.insert((*name).to_string(), ch);
            tracing::info!(channel = %name, "imd: provider channel built from credentials");
        }
    }
    if let Some(extra) = args.extra_channels.clone() {
        out.extend(extra); // web-chat WS merge still last, unchanged
    }
    out
}

/// Telegram: union the user-configured chat-id allowlist with every
/// registered telegram bot's `im_chat_id` (both live in `reply_target`
/// chat-id space, so the union authorizes those chats). Verbatim move
/// of the previous inline logic.
#[cfg(feature = "telegram")]
fn build_telegram_channel(
    creds: &Credentials,
    bots: &[BotRegistration],
) -> Option<Arc<dyn Channel + Send + Sync>> {
    let tg = creds.telegram.as_ref()?;
    let mut allowed = tg.allowed_chat_ids.clone();
    for b in bots.iter().filter(|b| b.im_platform == "telegram") {
        allowed.push(b.im_chat_id.clone());
    }
    allowed.sort();
    allowed.dedup();
    Some(Arc::new(TelegramChannel::new(
        tg.bot_token.clone(),
        allowed,
    )))
}

/// Slack: HTTP `chat.postMessage` + channel polling. Discharges the old
/// `TODO(V0.7-im-providers)` — the row was dark only because no creds
/// block existed, not because the provider was missing.
#[cfg(feature = "slack")]
fn build_slack_channel(
    creds: &Credentials,
    _bots: &[BotRegistration],
) -> Option<Arc<dyn Channel + Send + Sync>> {
    let slack = creds.slack.as_ref()?;
    Some(Arc::new(
        crate::transport::providers::slack::SlackChannel::new(
            slack.bot_token.clone(),
            slack.poll_channels.clone(),
        ),
    ))
}

/// Discord: REST messages API + per-channel polling. `DiscordCreds`
/// carries no poll list (the bound channel is discovered at runtime), so
/// the poll set starts empty; the user-id allowlist passes through.
#[cfg(feature = "discord")]
fn build_discord_channel(
    creds: &Credentials,
    _bots: &[BotRegistration],
) -> Option<Arc<dyn Channel + Send + Sync>> {
    let discord = creds.discord.as_ref()?;
    Some(Arc::new(
        crate::transport::providers::discord::DiscordChannel::new(
            discord.bot_token.clone(),
            Vec::new(),
            discord.authorized_user_ids.clone(),
        ),
    ))
}

/// Lark/Feishu: WSS long-connection + `im/v1/messages`.
///
/// ALLOWLIST-UNION SUBTLETY: telegram unions registered bot `im_chat_id`s
/// (chat-id space) into its chat-id allowlist, which authorizes those
/// chats. Lark's [`LarkChannel::is_user_allowed`] checks the SENDER
/// `open_id` (`ou_…`), but `im_chat_id` is a CHAT id (`oc_…`) — a
/// different namespace — so this union is **parity-only**: it never
/// authorizes anyone. Real auth comes from `LarkCreds.allowed_user_ids`.
/// The union is kept so every provider's builder is shaped identically.
#[cfg(feature = "lark")]
fn build_lark_channel(
    creds: &Credentials,
    bots: &[BotRegistration],
) -> Option<Arc<dyn Channel + Send + Sync>> {
    let lark = creds.lark.as_ref()?;
    let mut allowed = lark.allowed_user_ids.clone();
    for b in bots.iter().filter(|b| b.im_platform == "lark") {
        allowed.push(b.im_chat_id.clone());
    }
    allowed.sort();
    allowed.dedup();
    Some(Arc::new(
        crate::transport::providers::lark::LarkChannel::new(
            lark.app_id.clone(),
            lark.app_secret.clone(),
            allowed,
            lark.use_feishu,
        ),
    ))
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

/// Decide whether an inbound message clears the security layer, returning the
/// text payload to forward to the gateway (or `None` to drop it).
///
/// `Accept` forwards its sanitized payload. An `EmptyAfterSanitize` is normally
/// a drop, **except** when the message carries a non-text payload —
/// `has_nontext_payload` is true for a selection callback (inline-button / chip
/// click; B1) OR an attachment-only message (a file/photo sent with no caption;
/// B1b). Both legitimately have empty `content`: the real payload is the
/// structured `selection` (resolved in the gateway) or the staged `attachments`
/// (Read by the agent via the `<channel …>` tag), so an empty-after-sanitize
/// result there is expected, not hostile. ACL / rate-limit / bad-signature
/// rejections are always dropped; because they precede the sanitize check in
/// [`ThreeLayerSec::evaluate`], a non-text message still passes through them.
/// (v0.8.5 B1 / B1b)
pub fn sec_gate_payload(outcome: SecOutcome, has_nontext_payload: bool) -> Option<String> {
    match outcome {
        SecOutcome::Accept { payload } => Some(payload),
        SecOutcome::EmptyAfterSanitize if has_nontext_payload => Some(String::new()),
        _ => None,
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

            // (v0.8.5 B1 / B1b) A non-text message legitimately carries empty
            // `content`: a selection callback (inline-button / web-chip click —
            // its real payload is `msg.selection`, resolved in the gateway) OR
            // an attachment-only message (a file/photo with no caption — the
            // staged `msg.attachments` are Read by the agent via the `<channel>`
            // tag). The security layer's `EmptyAfterSanitize` is expected for
            // both, not an attack. ACL + rate-limit (which run *before* the
            // sanitize check in `evaluate`) still gate it; only the empty-text
            // rejection is waived. Without this, every D3/D6 button AND every
            // captionless inbound file is silently dropped here (on Telegram
            // *and* web chat — both feed this consumer).
            let outcome = sec
                .lock()
                .await
                .evaluate(&msg.channel, &msg.sender, &msg.content);
            let has_nontext_payload = msg.selection.is_some() || !msg.attachments.is_empty();
            let Some(clean_payload) = sec_gate_payload(outcome.clone(), has_nontext_payload) else {
                tracing::warn!(
                    cid = %cid,
                    outcome = ?outcome,
                    "ccteam-im: gateway inbound rejected by security layer"
                );
                continue;
            };

            let replies = gateway
                .lock()
                .await
                .handle_message(
                    &msg.channel,
                    &msg.reply_target,
                    &msg.sender,
                    &msg.id,
                    &clean_payload,
                    &msg.attachments,
                    msg.selection.as_ref(),
                )
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

/// A live, editable progress status message (V0.8.4 P1): the platform
/// message id plus where it lives, so later progress updates for the same
/// turn edit it in place instead of spamming new messages.
#[derive(Clone)]
struct StatusHandle {
    message_id: String,
    recipient: String,
}

fn spawn_gateway_event_consumer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<GatewayEvent>,
    channels: ChannelMap,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // One editable status message per `status_key` (a turn's progress
        // epoch). Bounded: entries are inserted on the first progress of a
        // turn and removed when the turn finalizes (`done`).
        let mut status_messages: HashMap<String, StatusHandle> = HashMap::new();
        while let Some(evt) = rx.recv().await {
            let Some(channel) = channels.get(&evt.channel).cloned() else {
                tracing::warn!(
                    channel = %evt.channel,
                    event_id = %evt.id,
                    "ccteam-im: gateway event dropped because channel is not configured"
                );
                continue;
            };
            match evt.kind {
                GatewayEventKind::Answer => {
                    let out = SendMessage::new(evt.content, evt.chat_id)
                        .in_thread(evt.thread_ts)
                        .with_attachments(evt.attachments)
                        .with_options(evt.options);
                    send_gateway_outbound(&evt.id, 0, &evt.channel, channel.as_ref(), out).await;
                }
                GatewayEventKind::Progress { status_key, done } => {
                    deliver_progress(
                        channel.as_ref(),
                        &mut status_messages,
                        status_key,
                        done,
                        &evt.channel,
                        evt.chat_id,
                        evt.thread_ts,
                        evt.content,
                    )
                    .await;
                }
            }
        }
        tracing::debug!("imd: gateway event consumer exited");
    })
}

/// Deliver one progress update: send a fresh status message the first
/// time a `status_key` is seen, then edit that same message for every
/// later update, finalizing + forgetting it on `done`. Progress bypasses
/// the durable ledger — it is delivery-layer UX, not state SoT.
#[allow(clippy::too_many_arguments)]
async fn deliver_progress(
    channel: &(dyn Channel + Send + Sync),
    status_messages: &mut HashMap<String, StatusHandle>,
    status_key: String,
    done: bool,
    channel_name: &str,
    chat_id: String,
    thread_ts: Option<String>,
    content: String,
) {
    if let Some(handle) = status_messages.get(&status_key).cloned() {
        if let Err(err) = channel
            .edit_message(&handle.recipient, &handle.message_id, &content)
            .await
        {
            tracing::warn!(
                channel = %channel_name,
                status_key = %status_key,
                error = %err,
                "ccteam-im: progress edit failed"
            );
        }
        if done {
            status_messages.remove(&status_key);
        }
        return;
    }
    // First progress for this turn — send a new status message (the seed).
    let seed = SendMessage::new(content, chat_id.clone()).in_thread(thread_ts.clone());
    match channel.send(&seed).await {
        Ok(Some(message_id)) if !done => {
            status_messages.insert(
                status_key,
                StatusHandle {
                    message_id,
                    recipient: chat_id,
                },
            );
        }
        Ok(_) => {} // no editable id, or already done → one-shot
        Err(err) => {
            tracing::warn!(
                channel = %channel_name,
                status_key = %status_key,
                error = %err,
                "ccteam-im: progress seed send failed"
            );
        }
    }
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
    // Channel-neutral splitting (V0.8.4 P0 / B2): when the channel
    // declares a per-message ceiling and the content overflows it, fan
    // one logical reply into ordered sub-messages. `None` (most channels,
    // incl. web `WsChannel`) keeps today's single-send path verbatim — no
    // `4096`/`"telegram"` branch lives here.
    // Attachment-bearing messages never split — the files + caption are
    // one logical send (splitting would duplicate the files across parts).
    let parts = match channel.max_message_len() {
        Some(limit) if message.attachments.is_empty() => {
            crate::sanitize::split_for_channel(&message.content, limit)
        }
        _ => vec![message.content.clone()],
    };

    if parts.len() <= 1 {
        // Unchanged single-message path: id = `{inbound_id}-{seq}`.
        let id = format!("{inbound_id}-{seq}");
        queue_and_send_durable_part(id, inbound_id, channel_name, channel, message).await;
        return;
    }

    // Multi-part: each part is its own durable row, id =
    // `{inbound_id}-{seq}-{part}`, sent in order (same logical message ⇒
    // serial). The ledger keeps one Queued+Sent/Failed pair per part.
    let total = parts.len();
    let mut failed_parts: Vec<usize> = Vec::new();
    for (part_idx, part) in parts.into_iter().enumerate() {
        let id = format!("{inbound_id}-{seq}-{part_idx}");
        let mut part_msg = message.clone();
        part_msg.content = part;
        let sent =
            queue_and_send_durable_part(id, inbound_id, channel_name, channel, part_msg).await;
        if !sent {
            failed_parts.push(part_idx + 1); // 1-based for the user notice
        }
    }

    // Failure visible: a partial split (some parts delivered, some not) is
    // confusing silence today — surface one line back to the chat. Sent
    // directly (not split / not laddered through the ledger) since it is a
    // best-effort UX notice.
    if !failed_parts.is_empty() {
        let body = if failed_parts.len() == 1 {
            format!("⚠️ 部分消息发送失败 (part {}/{total})", failed_parts[0])
        } else {
            format!("⚠️ 部分消息发送失败 ({}/{total} parts)", failed_parts.len())
        };
        let notice =
            SendMessage::new(body, message.recipient.clone()).in_thread(message.thread_ts.clone());
        if let Err(err) = channel.send(&notice).await {
            tracing::warn!(
                inbound_id,
                channel = %channel_name,
                error = %err,
                "ccteam-im: failed to deliver split-failure notice"
            );
        }
    }
}

/// Queue a single durable outbound row, then attempt delivery. Returns
/// `true` when the send succeeded. Shared by the single- and multi-part
/// branches of [`send_gateway_outbound`].
async fn queue_and_send_durable_part(
    id: String,
    inbound_id: &str,
    channel_name: &str,
    channel: &(dyn Channel + Send + Sync),
    message: SendMessage,
) -> bool {
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
    finish_durable_outbound_send(id, inbound_id, channel_name, channel, message).await
}

/// Send a single already-queued durable row and append its terminal
/// (`Sent`/`Failed`) ledger entry. Returns `true` on success.
async fn finish_durable_outbound_send(
    id: String,
    inbound_id: &str,
    channel_name: &str,
    channel: &(dyn Channel + Send + Sync),
    message: SendMessage,
) -> bool {
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
            true
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
            false
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

    /// (v0.8.5 B1 / B1b) The security gate must let a non-text message through
    /// even though it carries empty `content` (→ `EmptyAfterSanitize`) — a
    /// selection callback (B1) or a captionless file/photo (B1b) — while still
    /// dropping ACL / rate-limit / signature rejections and genuinely-empty
    /// *text* messages. The bool models `has_nontext_payload` (selection OR
    /// attachments); when it dropped these it killed every D3/D6 inline button
    /// AND every captionless inbound file in the daemon.
    #[test]
    fn sec_gate_payload_admits_nontext_payloads() {
        // Accepted text → forwarded payload (the flag is irrelevant).
        assert_eq!(
            sec_gate_payload(
                SecOutcome::Accept {
                    payload: "hi".into()
                },
                false
            ),
            Some("hi".to_string())
        );
        // Non-text payload (button/chip click OR a captionless attachment):
        // empty content + `has_nontext_payload` → admitted with an empty text
        // payload (gateway resolves the selection / agent Reads the file).
        assert_eq!(
            sec_gate_payload(SecOutcome::EmptyAfterSanitize, true),
            Some(String::new())
        );
        // Empty text with NO selection AND NO attachment → still dropped.
        assert_eq!(
            sec_gate_payload(SecOutcome::EmptyAfterSanitize, false),
            None
        );
        // ACL / rate-limit / signature denials are always dropped, even when a
        // selection is present — they precede the sanitize check in `evaluate`,
        // so a click can never bypass them.
        assert_eq!(sec_gate_payload(SecOutcome::AclDenied, true), None);
        assert_eq!(sec_gate_payload(SecOutcome::RateLimited, true), None);
        assert_eq!(
            sec_gate_payload(SecOutcome::BadSignature("x".into()), true),
            None
        );
    }

    /// (v0.8.5 S4) End-to-end through the *real* security layer: a button
    /// callback (empty content) yields `EmptyAfterSanitize`, and the gate then
    /// admits it because a selection is present — the composition the daemon
    /// inbound consumer runs. (Resolving the selection is covered by the
    /// gateway's `handle_message` selection tests.) No daemon-level test
    /// previously fed a non-`None` selection, which is how B1 shipped green.
    #[test]
    fn real_security_layer_admits_empty_selection_callback() {
        use crate::acl::AclPolicy;
        let mut sec = ThreeLayerSec::new(AclPolicy::default());
        // A Telegram button click: empty content, ACL-open, under rate limit.
        let outcome = sec.evaluate("telegram", "user-1", "");
        assert_eq!(outcome, SecOutcome::EmptyAfterSanitize);
        // With a selection present the gate admits it (empty text payload).
        assert_eq!(
            sec_gate_payload(outcome.clone(), true),
            Some(String::new()),
            "selection callback must clear the security gate"
        );
        // The same outcome without a selection is a real empty turn → dropped.
        assert_eq!(sec_gate_payload(outcome, false), None);
    }

    /// (v0.8.5 B1b) A captionless inbound file/photo arrives as
    /// `content="" + attachments=[…] + selection=None`. The consumer's gate
    /// input `has_nontext_payload = selection.is_some() || !attachments.is_empty()`
    /// must be true, so the security layer's `EmptyAfterSanitize` is admitted and
    /// the agent gets the `<channel … file_path>` turn. This is the exact path
    /// that logged `content_len=0 attachments=1 → EmptyAfterSanitize` and dropped
    /// every captionless file before the fix.
    #[test]
    fn captionless_attachment_message_clears_gate() {
        use crate::transport::{AttachmentKind, ChannelAttachment, ChannelMessage};
        let msg = ChannelMessage {
            id: "tg-1".into(),
            sender: "u1".into(),
            reply_target: "chat-1".into(),
            content: String::new(), // no caption
            channel: "telegram".into(),
            timestamp: 0,
            thread_ts: None,
            attachments: vec![ChannelAttachment {
                kind: AttachmentKind::File,
                file_name: "readme.txt".into(),
                local_path: "/tmp/stage/readme.txt".into(),
                mime: Some("text/plain".into()),
                size: Some(908),
            }],
            selection: None,
        };
        // The consumer's gate input: a captionless attachment counts as non-text.
        let has_nontext = msg.selection.is_some() || !msg.attachments.is_empty();
        assert!(has_nontext);
        // Real security layer: empty content → EmptyAfterSanitize, then admitted
        // because attachments are present (ACL + rate-limit already passed).
        let mut sec = ThreeLayerSec::new(crate::acl::AclPolicy::default());
        let outcome = sec.evaluate(&msg.channel, &msg.sender, &msg.content);
        assert_eq!(outcome, SecOutcome::EmptyAfterSanitize);
        assert_eq!(
            sec_gate_payload(outcome, has_nontext),
            Some(String::new()),
            "captionless attachment must clear the security gate"
        );
    }

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
            ..Default::default()
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

    /// F10 (arch §8-2): the factory is a **per-vendor singleton** — two
    /// Codex-arm calls return the SAME `Arc` instance (one codex
    /// app-server child for the whole daemon), and likewise for Claude.
    #[test]
    fn default_adapter_factory_is_per_vendor_singleton() {
        let factory = default_adapter_factory();
        let codex_a = factory(AgentVendor::Codex);
        let codex_b = factory(AgentVendor::Codex);
        assert!(
            Arc::ptr_eq(&codex_a, &codex_b),
            "F10: codex arm must memoise ONE adapter (one app-server child), got distinct Arcs"
        );
        let claude_a = factory(AgentVendor::Claude);
        let claude_b = factory(AgentVendor::Claude);
        assert!(
            Arc::ptr_eq(&claude_a, &claude_b),
            "F10: claude arm must also be a singleton"
        );
    }
}
