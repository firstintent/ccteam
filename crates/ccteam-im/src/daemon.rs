//! Main daemon event loop.
//!
//! Composes credentials → Channel listeners → gateway routing. The loop
//! is `tokio::select`-driven across:
//!
//! - one inbound mpsc receiver (multiplexed across active Channels),
//! - a SIGTERM future for graceful shutdown,
//! - an optional max-runtime watchdog (test-only — production is `0`).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use ccteam_harness::execution::{ClaudeStreamJsonAdapter, ClaudeTuiAdapter, CodexAppServerAdapter};
use ccteam_harness::{AgentVendor, HarnessAdapter, SessionProtocol};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::acl::AclPolicy;
use crate::credentials::{self, Credentials};
use crate::gateway::{Gateway, GatewayEvent, GatewayEventKind};
use crate::latency::now_unix_ms;
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
pub type AdapterFactory = Arc<
    dyn Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync,
>;

/// Pick the canonical production adapter for `(vendor, protocol)`.
///
/// - `Claude` + `StreamJson` (the default) → [`ClaudeStreamJsonAdapter`] —
///   the lightweight NDJSON chat path (no PTY / pane / hook chain).
/// - `Claude` + `Terminal` → [`ClaudeTuiAdapter`] — the advanced tmux-PTY
///   path (byte-faithful terminal mirror / attach / screenshot).
/// - `Codex` → [`CodexAppServerAdapter`] regardless of protocol (codex
///   always drives via its app-server JSON-RPC control plane).
///
/// F10: **per-(vendor,protocol) singleton.** Exactly ONE of each adapter is
/// constructed here; every factory call `.clone()`s the matching `Arc` so a
/// vendor's memoised child (codex app-server) / live-session registry
/// (stream-json) is shared across all that vendor's chat sessions.
///
/// v0.8.22 P0-2: the stream-json adapter's HITL resolver is wired
/// POST-construction (see [`default_adapter_factory_with_stream_json_handle`]
/// and `run_daemon_with_shutdown`) — a bare call to this fn returns an
/// adapter with no resolver, so a `hitl` stream-json session default-denies
/// until something calls `ClaudeStreamJsonAdapter::set_resolver` on the
/// handle.
pub fn default_adapter_factory() -> AdapterFactory {
    default_adapter_factory_with_stream_json_handle().0
}

/// Like [`default_adapter_factory`], but also returns a direct handle to the
/// stream-json Claude adapter singleton the factory captured.
///
/// The factory closure only hands out type-erased `Arc<dyn HarnessAdapter>`s
/// (so the gateway can stay adapter-agnostic), which cannot be downcast back
/// to the concrete `ClaudeStreamJsonAdapter` — but wiring the production HITL
/// `CanUseToolResolver` (v0.8.22 P0-2, review §3.1-1) needs exactly that
/// concrete type's `set_resolver`. This fn hands back BOTH: the
/// `Arc<ClaudeStreamJsonAdapter>` and the type-erased `Arc<dyn HarnessAdapter>`
/// wrapping it are clones of the SAME inner state (the adapter's `live`
/// registry + its interior-mutable resolver cell), so calling
/// `.set_resolver(..)` on the handle is visible to every session the factory
/// spawns through the type-erased clone — no matter which was constructed
/// first.
///
/// `run_daemon_with_shutdown` calls this (not the plain
/// [`default_adapter_factory`]) so it can wire the resolver once the
/// gateway, pending registry, and event sink all exist; the composition
/// root (`ccteam start`) gets its handle from [`build_gateway_for_daemon`],
/// which also routes through here.
pub fn default_adapter_factory_with_stream_json_handle(
) -> (AdapterFactory, Arc<ClaudeStreamJsonAdapter>) {
    let claude_tui: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(ClaudeTuiAdapter::new());
    let claude_stream_json = Arc::new(ClaudeStreamJsonAdapter::new());
    let claude_stream: Arc<dyn HarnessAdapter + Send + Sync> = claude_stream_json.clone();
    let codex: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(CodexAppServerAdapter::new());
    let grok: Arc<dyn HarnessAdapter + Send + Sync> =
        Arc::new(ccteam_harness::GrokAcpAdapter::new());
    let opencode: Arc<dyn HarnessAdapter + Send + Sync> =
        Arc::new(ccteam_harness::OpencodeAcpAdapter::new());
    // Vendor-first factory (v0.8.23 §3.E + v0.8.24 OpenCode): protocol is
    // Claude-only for stream-json/terminal; Grok/OpenCode always ACP.
    let factory: AdapterFactory = Arc::new(
        move |vendor: AgentVendor, protocol: SessionProtocol| match vendor {
            AgentVendor::Claude => match protocol {
                SessionProtocol::StreamJson => Arc::clone(&claude_stream),
                SessionProtocol::Terminal => Arc::clone(&claude_tui),
                // Claude has no ACP arm — fall back to default stream-json.
                SessionProtocol::Acp => Arc::clone(&claude_stream),
            },
            AgentVendor::Codex => Arc::clone(&codex),
            AgentVendor::Grok => Arc::clone(&grok),
            AgentVendor::Opencode => Arc::clone(&opencode),
        },
    );
    (factory, claude_stream_json)
}

fn format_gateway_user_error(err: &anyhow::Error) -> String {
    let raw = err.to_string();
    if let Some(rest) = raw.strip_prefix("spawn failed: ") {
        return format!(
            "会话启动失败: {rest}。下一步: 请检查项目和角色后重试 /new；如果仍失败，重启 ccteam start 后再试。"
        );
    }
    if let Some(rest) = raw.strip_prefix("submit failed: ") {
        return format!(
            "发送失败: {rest}。下一步: 请重试；如果仍失败，发送 /sessions 确认会话还在，或重新 /new。"
        );
    }
    if let Some(project) = raw.strip_prefix("unknown project: ") {
        return format!(
            "项目不存在: {project}。下一步: 发送 /projects 查看可用项目，或先运行 ccteam init 注册项目。"
        );
    }
    format!(
        "操作失败: {raw}。下一步: 请重试；如果仍失败，发送 /projects 检查项目，或重启 ccteam start。"
    )
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
    /// V0.8.6 W5b — caller-provided gateway handle. `ccteam start` is the
    /// composition root: it builds the `Arc<Mutex<Gateway>>` once (via
    /// [`build_gateway_for_daemon`]) and hands the SAME handle to both this
    /// daemon and the web `AppState`, so the resource API and the IM router
    /// drive one in-memory session map. When `Some`, the daemon runs its
    /// post-build wiring (pending registry, restored-session resume, event
    /// sink) on this handle instead of constructing its own. `None`
    /// (standalone `ccteam-im run`, no web) → the daemon builds + owns its
    /// gateway exactly as before.
    pub gateway: Option<Arc<Mutex<Gateway>>>,
    /// v0.8.22 P0-2 — the stream-json Claude adapter singleton PAIRED with
    /// `gateway` (both built together by [`build_gateway_for_daemon`] via
    /// [`default_adapter_factory_with_stream_json_handle`]). The daemon calls
    /// `set_resolver` on this handle once `pending` + the event sink are
    /// wired, so a `hitl` stream-json session's `can_use_tool` reverse-RPCs
    /// route to the SAME approval machinery IM/web already use. `None` on the
    /// standalone path (`ccteam-im run`) — the daemon then builds + wires its
    /// own via `default_adapter_factory_with_stream_json_handle`, UNLESS
    /// `adapter_factory` is test-overridden (no production adapter to wire).
    pub claude_stream_json_adapter: Option<Arc<ClaudeStreamJsonAdapter>>,
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
            .field("gateway", &self.gateway.is_some())
            .field(
                "claude_stream_json_adapter",
                &self.claude_stream_json_adapter.is_some(),
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

    // v0.8.22 P0-2 — when no test-injected `adapter_factory` is present, build
    // the production factory via the handle-returning variant so the
    // standalone path (`args.gateway` is `None`) can wire the HITL resolver
    // onto the stream-json adapter it is ABOUT to bake into the gateway
    // below. A test-injected factory means test doubles, not the production
    // `ClaudeStreamJsonAdapter` — nothing to wire.
    let (factory, standalone_stream_json_handle) = match args.adapter_factory.clone() {
        Some(f) => (f, None),
        None => {
            let (f, handle) = default_adapter_factory_with_stream_json_handle();
            (f, Some(handle))
        }
    };
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
    let menu_specs = crate::gateway::menu_command_specs();
    {
        for (name, ch) in channels.iter() {
            if let Err(err) = ch.register_commands(&menu_specs).await {
                tracing::warn!(
                    channel = %name,
                    error = %err,
                    "imd: register_commands (menu) failed"
                );
            }
        }
    }
    replay_durable_outbox(&channels).await;
    // In-place IM hot-reload: share the channel map so the inbound + event
    // consumers read the live set while a reload swaps credential-driven
    // entries underneath them. `last_creds` lets a reload no-op when
    // `credentials.json` is byte-identical (a pref-only `ccteam config` must
    // not blip Telegram). `menu_specs` is captured once and re-published to
    // each rebuilt channel on reload.
    let shared_channels: Arc<RwLock<ChannelMap>> = Arc::new(RwLock::new(channels));
    let last_creds: Arc<StdMutex<Credentials>> = Arc::new(StdMutex::new(creds.clone()));
    // v0.8.20 F2 — track the last tenants.json bytes so a reload rebuilds only
    // the CHANGED scope: a tenant-only change must not blip the owner's live
    // global bot, and a creds-only change must not blip the tenant bots.
    let last_tenants: Arc<StdMutex<String>> = Arc::new(StdMutex::new(tenants_fingerprint(&args)));
    // V0.8.6 W5b — use the caller-provided gateway handle when `ccteam start`
    // built one (so the web `AppState` and this daemon drive the SAME session
    // map); else build + own one (standalone `ccteam-im run`). The post-build
    // wiring below runs identically on either, under a brief lock.
    let gateway = args.gateway.clone().unwrap_or_else(|| {
        Arc::new(Mutex::new(build_gateway(
            factory.clone(),
            &projects_root,
            &config_projects,
            &initial,
        )))
    });
    // V0.8.4 P2b — use the externally-supplied channel when `ccteam start`
    // provided one (so the mcp.sock handler shares this sender); else make
    // our own (standalone `ccteam-im run`).
    let (gateway_event_tx, gateway_event_rx) =
        match (args.gateway_event_tx.take(), args.gateway_event_rx.take()) {
            (Some(tx), Some(rx)) => (tx, rx),
            _ => tokio::sync::mpsc::unbounded_channel::<GatewayEvent>(),
        };
    // In-place IM hot-reload signal: `ccteam config` → mcp.sock `ccteam/reload`
    // → `gateway.request_im_reload()` → `try_send(())` here → the reload arm of
    // the daemon select-loop rebuilds the credential-driven channels. A small
    // buffer (4) coalesces bursts; a full buffer just means a reload is already
    // pending. Always wired here because a gateway always exists by this point
    // (caller-supplied or daemon-built); on the standalone path with no mcp.sock
    // the trigger simply never fires.
    let (reload_tx, mut reload_rx) = tokio::sync::mpsc::channel::<()>(4);
    // v0.9.0 W2 (F2) — the delegation notifier channel is created OUTSIDE the
    // locked setup block (its receiver is moved into the notifier task after
    // setup); the sender is wired in BEFORE `set_event_sink` so every pump it
    // spawns captures it.
    let (delegation_tx, delegation_rx) = tokio::sync::mpsc::unbounded_channel();
    {
        // Hold the gateway lock only for synchronous setup. Restored-session
        // resume can wait on vendor startup (stream-json `system:init`), and
        // web session creation shares this same mutex, so resume is kicked off
        // below without holding the lock.
        let mut g = gateway.lock().await;
        // v0.8.5 D6 — inject the shared pending-interaction registry when one
        // was supplied (`ccteam start` hands the same `Arc` to the mcp.sock
        // handler so the D6 `interaction/ask` ingress and the gateway resolve
        // through one registry). Standalone runs leave the gateway's own fresh
        // registry.
        if let Some(pending) = args.pending.clone() {
            g.set_pending(pending);
        }
        log_orphan_chat_sessions(&g).await;
        g.set_delegation_notifier_tx(delegation_tx);
        g.set_event_sink(gateway_event_tx.clone());
        g.set_im_reload_trigger(reload_tx);
        // v0.8.22 P0-2 — wire the production HITL resolver onto the
        // stream-json Claude adapter singleton, now that the gateway +
        // pending registry + event sink all exist. Composition root
        // (`ccteam start`) supplies its handle via
        // `args.claude_stream_json_adapter` (built alongside `args.gateway`
        // by `build_gateway_for_daemon`, so both name the SAME adapter this
        // gateway spawns stream-json sessions through); the standalone path
        // uses the one it just built above (`standalone_stream_json_handle`).
        // `None` (a test-injected `adapter_factory`) ⇒ skip: nothing to wire
        // (tests inject their own fake harness / deterministic resolver
        // stub, never the production `ClaudeStreamJsonAdapter`).
        if let Some(adapter) = args
            .claude_stream_json_adapter
            .clone()
            .or(standalone_stream_json_handle)
        {
            adapter.set_resolver(Arc::new(crate::hitl::GatewayCanUseToolResolver::new(
                Arc::clone(&gateway),
                g.pending_handle(),
                gateway_event_tx.clone(),
            )));
        }
    }
    let restore_gateway = Arc::clone(&gateway);
    tokio::spawn(async move {
        Gateway::resume_restored_sessions_shared(restore_gateway).await;
    });
    // v0.9.0 W2 (F2/F7) — the delegation notifier: startup reconcile (deliver
    // notifications missed while the daemon was down) + live delivery of every
    // completed watched child turn. Owns its own gateway handle + the pump
    // signal receiver.
    let notifier_gateway = Arc::clone(&gateway);
    tokio::spawn(async move {
        Gateway::run_delegation_notifier(notifier_gateway, delegation_rx).await;
    });

    // V0.6.1 F132 — spawn one `Channel::listen` task per active
    // channel. Each listener pushes ChannelMessages into a shared mpsc
    // that the inbound consumer drains.
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel::<ChannelMessage>(INBOUND_BUF);
    // Listener registry keyed by channel name so an in-place reload can abort
    // exactly the credential-driven listener it rebuilds (was a flat `Vec`).
    let listeners: Arc<StdMutex<HashMap<String, tokio::task::JoinHandle<()>>>> =
        Arc::new(StdMutex::new(HashMap::new()));
    {
        // Clone out the (name, ch) pairs under the read lock, then drop the
        // guard before spawning (never hold a std lock across spawn/await).
        let pairs: Vec<(String, Arc<dyn Channel + Send + Sync>)> = {
            let g = shared_channels.read().unwrap();
            g.iter().map(|(n, c)| (n.clone(), c.clone())).collect()
        };
        let mut reg = listeners.lock().unwrap();
        for (name, ch) in pairs {
            let h = spawn_channel_listener(name.clone(), ch, inbound_tx.clone());
            reg.insert(name.clone(), h);
            tracing::info!(
                channel = %name,
                bots = initial.len(),
                "imd: {} channel listener spawned bots={}",
                name,
                initial.len()
            );
        }
    }
    // Keep a separate clone alive across the whole daemon lifetime so a reload
    // can spawn fresh listeners onto the same inbound channel. The original
    // `inbound_tx` is still dropped below to end the consumer on shutdown; this
    // clone holds the channel open until daemon teardown (the consumer just
    // drains until then — exactly the desired behavior).
    let inbound_tx_for_reload = inbound_tx.clone();
    // Drop our extra clone so the consumer's `recv()` returns `None`
    // once every listener exits.
    drop(inbound_tx);

    // Shared inbound security state. v8.1 routes accepted messages
    // directly through the gateway; mailbox/admin/supervisor tick paths
    // are legacy helpers and are not part of the daemon hot path.
    let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));

    let inbound_consumer = spawn_inbound_consumer(
        inbound_rx,
        shared_channels.clone(),
        sec.clone(),
        gateway.clone(),
    );
    let gateway_event_consumer =
        spawn_gateway_event_consumer(gateway_event_rx, shared_channels.clone());

    tracing::info!(
        channels = shared_channels.read().unwrap().len(),
        bots = initial.len(),
        "ccteam-im: gateway router started (no supervisor tick)"
    );

    let mut shutdown: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(shutdown);
    let mut max_runtime: Pin<Box<dyn Future<Output = ()> + Send>> = match args.max_runtime {
        Some(max) => Box::pin(tokio::time::sleep(max)),
        None => Box::pin(std::future::pending()),
    };

    // Select-LOOP (not a one-shot select) so the IM-reload signal can fire
    // repeatedly over the daemon's life. Reload rebuilds ONLY the
    // credential-driven channel listeners in place — agent sessions, the
    // gateway, the consumers, the event pumps, the web/`extra_channels`/`mock`
    // channels are all untouched.
    let result: Result<()> = loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("ccteam-im: shutdown signalled; exiting cleanly");
                break Ok(());
            }
            _ = &mut max_runtime => {
                tracing::info!("max_runtime reached; exiting");
                break Ok(());
            }
            Some(()) = reload_rx.recv() => {
                reload_im_channels(
                    &shared_channels,
                    &listeners,
                    &inbound_tx_for_reload,
                    &args,
                    &last_creds,
                    &last_tenants,
                    &menu_specs,
                )
                .await;
            }
        }
    };

    // V0.6.1 F132 — abort listener + consumer tasks on shutdown so the
    // daemon doesn't leak background tokio tasks. `JoinHandle::abort`
    // is best-effort but matches the rest of the F130 supervisor's
    // shutdown semantics.
    {
        let mut reg = listeners.lock().unwrap();
        for (_name, h) in reg.drain() {
            h.abort();
        }
    }
    inbound_consumer.abort();
    gateway_event_consumer.abort();
    result
}

/// Spawn one `Channel::listen` task. Factored out of startup so the in-place
/// IM reload can spawn a fresh listener for a rebuilt channel with identical
/// semantics (log on error / clean exit). The returned handle is stored in the
/// per-name listener registry so a later reload can `abort()` exactly it.
fn spawn_channel_listener(
    name: String,
    ch: Arc<dyn Channel + Send + Sync>,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(err) = ch.listen(tx).await {
            tracing::warn!(
                channel = %name,
                error = %err,
                "imd: channel listener exited with error"
            );
        } else {
            tracing::debug!(channel = %name, "imd: channel listener exited cleanly");
        }
    })
}

/// In-place IM channel reload (no daemon restart, no agent-session restart).
///
/// Triggered by `ccteam config` over the mcp.sock `ccteam/reload` control call
/// → [`Gateway::request_im_reload`] → the reload arm of the daemon select-loop.
/// Re-reads `credentials.json` and rebuilds ONLY the credential-driven channels
/// ([`CHANNEL_BUILDERS`]) — `extra_channels` (web), `mock`, the gateway, the
/// consumers, and all agent sessions are left untouched. A byte-identical
/// credentials doc is a no-op (a pref-only `ccteam config` must not blip the
/// live IM listeners).
async fn reload_im_channels(
    shared: &Arc<RwLock<ChannelMap>>,
    listeners: &Arc<StdMutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    inbound_tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    args: &DaemonArgs,
    last_creds: &Arc<StdMutex<Credentials>>,
    last_tenants: &Arc<StdMutex<String>>,
    menu_specs: &[crate::transport::CommandSpec],
) {
    // Re-read credentials (global/admin bot) + tenants.json (per-tenant bots).
    let new_creds = match credentials::load(args.credentials.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "imd: reload could not read credentials");
            return;
        }
    };
    let new_tenants_raw = tenants_fingerprint(args);
    let creds_changed = *last_creds.lock().unwrap() != new_creds;
    let tenants_changed = *last_tenants.lock().unwrap() != new_tenants_raw;
    // No-op when nothing changed (e.g. a pref-only `ccteam config`).
    if !creds_changed && !tenants_changed {
        tracing::debug!("imd: reload — credentials + tenants unchanged, no-op");
        return;
    }
    // Rebuild ONLY the CHANGED scope, so a tenant-only change never blips the
    // owner's live global bot (and a creds-only change never blips the tenant
    // bots). Globals use the SAME table the startup path walks + current bot
    // registrations; per-tenant bots come from tenants.json.
    let bots = list_bots().unwrap_or_default();
    let mut rebuilt: ChannelMap = HashMap::new();
    let lark_probe_path = lark_open_id_probe_path_for(args);
    if creds_changed {
        for (name, builder) in CHANNEL_BUILDERS {
            if let Some(ch) = builder(&new_creds, &bots, Some(lark_probe_path.as_path())) {
                rebuilt.insert((*name).to_string(), ch);
            }
        }
    }
    if tenants_changed {
        for (name, ch) in
            build_tenant_channels(&daemon_tenants(args), Some(lark_probe_path.as_path()))
        {
            rebuilt.insert(name, ch);
        }
    }
    // Apply: for each rebuilt channel, abort its old listener, spawn a new one,
    // and (re)publish its command menu.
    for (name, ch) in rebuilt.iter() {
        if let Some(old) = listeners.lock().unwrap().remove(name) {
            old.abort();
        }
        let h = spawn_channel_listener(name.clone(), ch.clone(), inbound_tx.clone());
        listeners.lock().unwrap().insert(name.clone(), h);
        if let Err(e) = ch.register_commands(menu_specs).await {
            tracing::warn!(channel = %name, error = %e, "imd: reload register_commands failed");
        }
    }
    // Remove managed channels that vanished — but only within the CHANGED scope
    // (a creds-driven global iff creds changed; a per-tenant bot iff tenants
    // changed), so the unchanged dimension's listeners are never touched.
    {
        let creds_driven: HashSet<&str> = CHANNEL_BUILDERS.iter().map(|(n, _)| *n).collect();
        let mut map = shared.write().unwrap();
        let vanished: Vec<String> = map
            .keys()
            .filter(|k| {
                if rebuilt.contains_key(*k) {
                    return false;
                }
                let is_global = creds_driven.contains(k.as_str());
                let is_tenant = crate::transport::is_tenant_bot_channel(k);
                (creds_changed && is_global) || (tenants_changed && is_tenant)
            })
            .cloned()
            .collect();
        for k in vanished {
            map.remove(&k);
            if let Some(h) = listeners.lock().unwrap().remove(&k) {
                h.abort();
            }
        }
        for (name, ch) in rebuilt {
            map.insert(name, ch);
        }
    }
    if creds_changed {
        *last_creds.lock().unwrap() = new_creds;
    }
    if tenants_changed {
        *last_tenants.lock().unwrap() = new_tenants_raw;
    }
    tracing::info!(
        creds_changed,
        tenants_changed,
        "imd: IM channels reloaded (changed scope only; sessions untouched)"
    );
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
    fn(&Credentials, &[BotRegistration], Option<&Path>) -> Option<Arc<dyn Channel + Send + Sync>>;

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
    let lark_probe_path = lark_open_id_probe_path_for(args);
    for (name, builder) in CHANNEL_BUILDERS {
        if let Some(ch) = builder(creds, bots, Some(lark_probe_path.as_path())) {
            out.insert((*name).to_string(), ch);
            tracing::info!(channel = %name, "imd: provider channel built from credentials");
        }
    }
    // v0.8.20 F2 — one listener per tenant bot (tenants.json), additive to the
    // global/admin bot above. Keyed "<platform>@<tenant_id>" so reply routing
    // and the inbound→tenant binding work without colliding on the platform.
    for (name, ch) in build_tenant_channels(&daemon_tenants(args), Some(lark_probe_path.as_path()))
    {
        tracing::info!(channel = %name, "imd: per-tenant bot channel built");
        out.insert(name, ch);
    }
    if let Some(extra) = args.extra_channels.clone() {
        out.extend(extra); // web-chat WS merge still last, unchanged
    }
    out
}

/// Telegram effective inbound allowlist. An EMPTY user-configured list
/// means "open mode" and MUST stay open — registry `im_chat_id`s only
/// ADD authorization on top of an explicit list, never flip the channel
/// from open to allowlist-only. (A single stale/fixture registration
/// would otherwise silently lock out every real chat: drops log at
/// DEBUG while the getUpdates offset still advances — an unobservable
/// black hole.)
#[cfg(feature = "telegram")]
fn telegram_effective_allowlist(user_allowed: &[String], bots: &[BotRegistration]) -> Vec<String> {
    if user_allowed.is_empty() {
        return Vec::new();
    }
    let mut allowed = user_allowed.to_vec();
    for b in bots.iter().filter(|b| b.im_platform == "telegram") {
        allowed.push(b.im_chat_id.clone());
    }
    allowed.sort();
    allowed.dedup();
    allowed
}

/// Telegram: user-configured allowlist unioned with registered telegram
/// bots' `im_chat_id`s (both live in `reply_target` chat-id space) via
/// [`telegram_effective_allowlist`] — open mode is preserved.
#[cfg(feature = "telegram")]
fn build_telegram_channel(
    creds: &Credentials,
    bots: &[BotRegistration],
    _lark_probe_path: Option<&Path>,
) -> Option<Arc<dyn Channel + Send + Sync>> {
    let tg = creds.telegram.as_ref()?;
    Some(Arc::new(TelegramChannel::new(
        tg.bot_token.clone(),
        telegram_effective_allowlist(&tg.allowed_chat_ids, bots),
    )))
}

/// Slack: HTTP `chat.postMessage` + channel polling. Discharges the old
/// `TODO(V0.7-im-providers)` — the row was dark only because no creds
/// block existed, not because the provider was missing.
#[cfg(feature = "slack")]
fn build_slack_channel(
    creds: &Credentials,
    _bots: &[BotRegistration],
    _lark_probe_path: Option<&Path>,
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
    _lark_probe_path: Option<&Path>,
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
    lark_probe_path: Option<&Path>,
) -> Option<Arc<dyn Channel + Send + Sync>> {
    let lark = creds.lark.as_ref()?;
    let mut allowed = lark.allowed_user_ids.clone();
    for b in bots.iter().filter(|b| b.im_platform == "lark") {
        allowed.push(b.im_chat_id.clone());
    }
    allowed.sort();
    allowed.dedup();
    let mut ch = crate::transport::providers::lark::LarkChannel::new(
        lark.app_id.clone(),
        lark.app_secret.clone(),
        allowed,
        lark.use_feishu,
    );
    if let Some(path) = lark_probe_path {
        ch = ch.with_open_id_probe_path(path.to_path_buf());
    }
    Some(Arc::new(ch))
}

/// JSONL file where Lark channels record unauthorized sender `open_id`s for
/// the web self-serve setup flow. When tests override the credentials path,
/// derive the matching fake `~/.ccteam` root from that path instead of touching
/// the real home.
fn lark_open_id_probe_path_for(args: &DaemonArgs) -> PathBuf {
    if let Some(creds) = &args.credentials {
        if let Some(secrets) = creds.parent() {
            if let Some(root) = secrets.parent() {
                return root
                    .join("state")
                    .join("im")
                    .join("lark-open-id-probes.jsonl");
            }
        }
    }
    ccteam_core::CcteamPaths::from_env()
        .map(|p| p.im_state_dir().join("lark-open-id-probes.jsonl"))
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".ccteam")
                .join("state")
                .join("im")
                .join("lark-open-id-probes.jsonl")
        })
}

/// v0.8.20 — the per-user tenant registry DIR (`~/.ccteam/secrets/users/`, one
/// `<id>.json` per tenant). When the creds path is overridden (tests / non-
/// default home), derive the sibling `users/` from its `secrets/` parent so a
/// test home is honored; otherwise the canonical env-aware path.
fn users_dir_for(args: &DaemonArgs) -> PathBuf {
    if let Some(creds) = &args.credentials {
        // creds = .../secrets/im-credentials.json → .../secrets/users
        if let Some(secrets) = creds.parent() {
            return secrets.join("users");
        }
    }
    ccteam_core::CcteamPaths::from_env()
        .map(|p| p.users_dir())
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".ccteam")
                .join("secrets")
                .join("users")
        })
}

/// v0.8.20 — load the tenant registry for the daemon (best-effort: a missing
/// dir is an empty registry, so the daemon always starts).
fn daemon_tenants(args: &DaemonArgs) -> ccteam_core::tenants::TenantRegistry {
    ccteam_core::tenants::TenantRegistry::load(&users_dir_for(args))
}

/// v0.8.20 — a cheap fingerprint of the per-user tenant files so a reload can
/// no-op when tenants are unchanged (replaces the single-file byte compare now
/// that tenants live in a directory of `<id>.json` files).
fn tenants_fingerprint(args: &DaemonArgs) -> String {
    serde_json::to_string(&daemon_tenants(args).tenants).unwrap_or_default()
}

/// v0.8.20 F2 — one IM [`Channel`] per tenant bot (from `tenants.json`). Each is
/// keyed `"<platform>@<tenant_id>"` so its inbound stamps that name (→ routes to
/// the tenant + replies return through THIS bot, not a colliding shared
/// `"telegram"`). The global/admin bot keeps its bare platform name. Only
/// tenants WITH IM creds get a channel.
fn build_tenant_channels(
    reg: &ccteam_core::tenants::TenantRegistry,
    lark_probe_path: Option<&Path>,
) -> Vec<(String, Arc<dyn Channel + Send + Sync>)> {
    let mut out: Vec<(String, Arc<dyn Channel + Send + Sync>)> = Vec::new();
    for t in reg.list() {
        #[cfg(feature = "telegram")]
        {
            if let Some(tg) = &t.telegram {
                let name = format!("telegram@{}", t.id);
                let ch = TelegramChannel::new(tg.bot_token.clone(), tg.allowed_chat_ids.clone())
                    .with_name(name.clone());
                out.push((name, Arc::new(ch)));
            }
        }
        #[cfg(feature = "lark")]
        {
            if let Some(lk) = &t.lark {
                let name = format!("lark@{}", t.id);
                let mut ch = crate::transport::providers::lark::LarkChannel::new(
                    lk.app_id.clone(),
                    lk.app_secret.clone(),
                    lk.allowed_user_ids.clone(),
                    lk.use_feishu,
                );
                if let Some(path) = lark_probe_path {
                    ch = ch.with_open_id_probe_path(path.to_path_buf());
                }
                let ch = ch.with_name(name.clone());
                out.push((name, Arc::new(ch)));
            }
        }
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
    if let Err(err) = gateway.enable_persistence(crate::default_ccteam_root_public()) {
        tracing::warn!(
            error = %err,
            "ccteam-im: failed to load gateway state; starting with empty route table"
        );
    }
    gateway
}

/// V0.8.6 W5b — build the gateway the daemon would, for the composition
/// root (`ccteam start`). Derives the same inputs the daemon's startup
/// computes (default adapter factory, `projects_root` from the optional
/// `registry` override, `config.yaml::projects[]`, and the persisted bot
/// list), then returns a [`Gateway`] in the *pre-wiring* state — exactly
/// what [`build_gateway`] returns. The caller wraps it in
/// `Arc<Mutex<…>>`, clones the handle into both the web `AppState`
/// (`AppState::with_gateway`) and [`DaemonArgs::gateway`], and the daemon
/// then runs its identical post-build wiring (pending registry, restored
/// session resume, event sink, and — v0.8.22 P0-2 — the stream-json HITL
/// resolver) on the shared handle. Building it here — instead of after the
/// daemon spawns — eliminates the spawn-order race between the web task and
/// the IM task: the handle exists before either runs. `registry` mirrors
/// [`DaemonArgs::registry`] (the projects_root override; `None` →
/// `~/projects`).
///
/// Returns the [`Gateway`] PAIRED with the stream-json Claude adapter
/// singleton `default_adapter_factory_with_stream_json_handle` built it
/// with — the composition root threads this handle into
/// [`DaemonArgs::claude_stream_json_adapter`] so `run_daemon_with_shutdown`
/// can wire the production HITL resolver onto the SAME adapter this gateway
/// spawns stream-json sessions through (a fresh, unrelated adapter singleton
/// would silently never receive the wiring).
pub fn build_gateway_for_daemon(
    registry: Option<PathBuf>,
) -> Result<(Gateway, Arc<ClaudeStreamJsonAdapter>)> {
    let (factory, claude_stream_json) = default_adapter_factory_with_stream_json_handle();
    let projects_root: PathBuf = registry.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join("projects")
    });
    let ccteam_root = crate::default_ccteam_root_public();
    let config_projects = crate::load_config_projects_map(&ccteam_root).unwrap_or_default();
    let bots = list_bots()?;
    let gateway = build_gateway(factory, &projects_root, &config_projects, &bots);
    Ok((gateway, claude_stream_json))
}

/// Best-effort: (re)publish the gateway command menu to Telegram's
/// `setMyCommands`, reading the configured bot token from
/// `credentials.json` (or `credentials_path` when overridden).
///
/// `ccteam start` calls this on **every** invocation, *before* the
/// single-instance pidfile guard — so the menu refreshes even when a
/// daemon is already running (the second `ccteam start` otherwise aborts
/// at `write_pidfile` before the daemon-startup registration in
/// [`run_daemon_with_shutdown`] could fire) and even when Telegram was
/// configured *after* the daemon first started. `setMyCommands` is
/// idempotent (it replaces the whole menu), so refreshing every start is
/// safe and runs no risk to live sessions/daemon — it is one HTTPS POST
/// to the Bot API, touching nothing in the running process.
///
/// Reuses the SAME path the daemon uses: [`menu_command_specs`] +
/// [`TelegramChannel::register_commands`] (→ `setMyCommands`), so there is
/// no second copy of the request body/POST. `Ok(())` when no Telegram
/// credential is configured (nothing to refresh) or the menu published
/// successfully; `Err` only when the Bot API call itself failed — the CLI
/// logs that warn and never aborts `ccteam start`.
///
/// [`menu_command_specs`]: crate::gateway::menu_command_specs
pub async fn refresh_telegram_command_menu(credentials_path: Option<&Path>) -> Result<()> {
    let creds = credentials::load(credentials_path)?;
    let Some(tg) = creds.telegram.as_ref() else {
        return Ok(()); // no Telegram configured → nothing to refresh
    };
    let specs = crate::gateway::menu_command_specs();
    // The allowlist is irrelevant to setMyCommands (it gates inbound reads,
    // not this outbound publish), so an empty list keeps the channel minimal.
    let channel = TelegramChannel::new(tg.bot_token.clone(), Vec::new());
    channel.register_commands(&specs).await
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
/// A richer operator surface — orphans in `/sessions` / a
/// `ccteam session ls --all` command, plus an explicit reclaim verb — can reuse
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
            // v0.8.8 F1 — orphan names carry the sid (not a role) post-F1.
            sid = %orphan.sid,
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
    channels: Arc<RwLock<ChannelMap>>,
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
            // Clone the channel out under the read lock, then DROP the guard
            // before any `.await` (never hold a std RwLock guard across await).
            let channel = {
                let g = channels.read().unwrap();
                g.get(&msg.channel).cloned()
            };
            let Some(channel) = channel else {
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
            // v0.8.20 F2 — a per-tenant bot's channel is "<platform>@<tenant>";
            // the ACL is keyed by PLATFORM (fail-closed on unknown), so strip the
            // tenant suffix before the gate (reply routing still uses the full
            // channel name elsewhere).
            let outcome = sec.lock().await.evaluate(
                crate::transport::platform_of(&msg.channel),
                &msg.sender,
                &msg.content,
            );
            let has_nontext_payload = msg.selection.is_some() || !msg.attachments.is_empty();
            let Some(clean_payload) = sec_gate_payload(outcome.clone(), has_nontext_payload) else {
                tracing::warn!(
                    cid = %cid,
                    outcome = ?outcome,
                    "ccteam-im: gateway inbound rejected by security layer"
                );
                continue;
            };

            // v0.8.x (concurrency review §4.1 P1) — LOCKING PROTOCOL. Before
            // this fix, `gateway.lock().await.handle_message(...).await` held
            // the gateway's global lock for the ENTIRE call, and this loop
            // awaited it inline before pulling the next message off `rx` — so
            // one chat spawning a session (tmux/subprocess spawn, stream-json
            // `system:init`) queued every other chat's message behind it,
            // both on the lock AND on this loop never reaching `rx.recv()`
            // again.
            //
            // `inbound_may_spawn` is a cheap, synchronous, always-safe-either-
            // way hint: a message that can only ever SUBMIT to an
            // already-live session (no gateway command, no `@mention`, the
            // chat already has a current session) is still processed INLINE
            // exactly as before — its own lock section is bounded by the
            // adapter's submit timeout, never a spawn, so keeping it on this
            // loop costs nothing. A message that might need the implicit
            // first-message spawn is instead handed to
            // `Gateway::handle_message_shared` on its OWN task: that entry
            // point plans under a short lock, drops the lock for the slow
            // spawn, then re-locks briefly to apply the result — freeing this
            // loop to `rx.recv()` the next chat's message immediately rather
            // than queuing behind the spawn.
            //
            // SCOPE: explicit spawning commands (`/new` `/role` `/use`
            // `/clear` `@mention`-to-a-template) are NOT decomposed in this
            // pass — `inbound_may_spawn` returns `false` for them, so they
            // still run inline, holding the gateway lock across their own
            // spawn exactly as before (a pre-existing, documented tradeoff —
            // same as `start_session`'s inline compose). Dead-child resume
            // is three-phase (v0.9 T2); `resume_dead_session_shared` is the
            // lock-free form when a shared handle is available. A scoped,
            // incremental step, not a full fix.
            let may_spawn = {
                let g = gateway.lock().await;
                g.inbound_may_spawn(
                    &msg.channel,
                    &msg.reply_target,
                    &msg.sender,
                    &clean_payload,
                    msg.selection.is_some(),
                )
            };
            if may_spawn {
                let gateway = Arc::clone(&gateway);
                let channel = Arc::clone(&channel);
                let msg = msg.clone();
                let cid = cid.clone();
                tokio::spawn(async move {
                    let replies = Gateway::handle_message_shared(
                        gateway,
                        &msg.channel,
                        &msg.reply_target,
                        &msg.sender,
                        &msg.id,
                        &clean_payload,
                        &msg.attachments,
                        msg.selection.as_ref(),
                    )
                    .await;
                    deliver_gateway_replies(&cid, route_t0, &msg, channel.as_ref(), replies).await;
                });
                continue;
            }

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
            deliver_gateway_replies(&cid, route_t0, &msg, channel.as_ref(), replies).await;
        }
        tracing::debug!("imd: inbound consumer exited (all senders closed)");
    })
}

/// Send the outcome of one `handle_message`/`handle_message_shared` call to
/// the originating channel — shared by `spawn_inbound_consumer`'s inline and
/// backgrounded-spawn branches so the reply-delivery + latency logging can
/// never drift between them.
async fn deliver_gateway_replies(
    cid: &str,
    route_t0: std::time::Instant,
    msg: &ChannelMessage,
    channel: &(dyn Channel + Send + Sync),
    replies: Result<Vec<String>>,
) {
    match replies {
        Ok(replies) => {
            for (seq, reply) in replies.into_iter().enumerate() {
                let out = SendMessage::new(reply, msg.reply_target.clone())
                    .in_thread(msg.thread_ts.clone());
                send_gateway_outbound(cid, seq, &msg.channel, channel, out).await;
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
            let out = SendMessage::new(format_gateway_user_error(&err), msg.reply_target.clone())
                .in_thread(msg.thread_ts.clone());
            send_gateway_outbound(cid, 0, &msg.channel, channel, out).await;
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
    channels: Arc<RwLock<ChannelMap>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // One editable status message per `status_key` (a turn's progress
        // epoch). Bounded: entries are inserted on the first progress of a
        // turn and removed when the turn finalizes (`done`).
        let mut status_messages: HashMap<String, StatusHandle> = HashMap::new();
        // 👀 ack reaction handle map (v0.8.19). Keyed `"{channel}:{message_id}"`;
        // the value is the provider's reaction handle (`Some(reaction_id)` for
        // Lark, `None` for Telegram which clears by message_id alone). An add
        // records the handle; the matching remove pops it. Bounded by the
        // in-flight turn count (each turn adds one entry, removes it on its
        // first event), and any orphan is harmless (a stale handle never
        // dereferences). ALL reaction calls are fire-and-forget — a failure is
        // logged + swallowed, NEVER propagated, so it can't affect delivery.
        let mut reaction_handles: HashMap<String, Option<String>> = HashMap::new();
        while let Some(evt) = rx.recv().await {
            // v0.9.0 W4 (F4) — a delegation lifecycle event is broadcast-only
            // (the team view's global SSE); it has no IM channel representation
            // and no bound `evt.channel` to resolve, so skip it BEFORE the
            // channel lookup (avoids a spurious "channel not configured"
            // warning firing for every delegation transition).
            if matches!(evt.kind, GatewayEventKind::Delegation { .. }) {
                continue;
            }
            // Clone the channel out under the read lock, then DROP the guard
            // before any `.await` (never hold a std RwLock guard across await).
            let channel = {
                let g = channels.read().unwrap();
                g.get(&evt.channel).cloned()
            };
            let Some(channel) = channel else {
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
                // v0.8.19 — structured per-step activity is WEB-ONLY. IM's
                // status is fully driven by the folded `Progress` event above,
                // so this is a strict no-op (no send / no edit): IM delivery
                // stays byte-identical to before the Activity event existed.
                GatewayEventKind::Activity { .. } => {}
                // v0.9.0 W4 — unreachable in practice (the early `continue`
                // above already skips every `Delegation` event before this
                // match), but the arm must exist for exhaustiveness and stays
                // correct if that early skip is ever removed.
                GatewayEventKind::Delegation { .. } => {}
                // v0.8.19 — the 👀 ack reaction (IM-only; web/discord/slack keep
                // the trait's no-op `add_reaction`/`remove_reaction`). Mirror the
                // Activity arm's discipline: ALL fire-and-forget — log + swallow,
                // never propagate, so a reaction can't break/delay the turn.
                GatewayEventKind::Reaction { message_id, on } => {
                    let key = format!("{}:{}", evt.channel, message_id);
                    if on {
                        match channel.add_reaction(&evt.chat_id, &message_id).await {
                            Ok(handle) => {
                                reaction_handles.insert(key, handle);
                            }
                            Err(err) => {
                                tracing::warn!(
                                    channel = %evt.channel,
                                    message_id = %message_id,
                                    error = %err,
                                    "ccteam-im: add_reaction failed (ack skipped)"
                                );
                            }
                        }
                    } else {
                        let handle = reaction_handles.remove(&key).flatten();
                        if let Err(err) = channel
                            .remove_reaction(&evt.chat_id, &message_id, handle.as_deref())
                            .await
                        {
                            tracing::warn!(
                                channel = %evt.channel,
                                message_id = %message_id,
                                error = %err,
                                "ccteam-im: remove_reaction failed (ack lingers)"
                            );
                        }
                    }
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
        .join("state")
        .join("im")
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

    /// v0.8.20 F2 — one channel per tenant bot, keyed `"<platform>@<tenant_id>"`
    /// (the unique routing key); a tenant with no IM creds yields no channel.
    #[cfg(feature = "telegram")]
    #[test]
    fn build_tenant_channels_keys_one_per_tenant_bot() {
        let mut reg = ccteam_core::tenants::TenantRegistry::default();
        let a = reg.add("alice");
        reg.set_telegram(
            &a.id,
            Some(ccteam_core::tenants::TenantTelegram {
                bot_token: "123:abc".into(),
                allowed_chat_ids: vec![],
            }),
        );
        let _bob = reg.add("bob"); // no IM creds → no channel
        let chans = build_tenant_channels(&reg, None);
        let names: Vec<String> = chans.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            names,
            vec![format!("telegram@{}", a.id)],
            "one channel for alice's telegram bot, none for bob",
        );
        // The Channel reports the SAME unique name → inbound stamps it + replies
        // route back through this bot (not a colliding shared `"telegram"`).
        assert_eq!(chans[0].1.name(), format!("telegram@{}", a.id).as_str());
    }
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

    /// v0.8.19 — the daemon egress 👀-reaction handle-map round-trips. A
    /// `Reaction{on:true}` calls `add_reaction` and STORES the returned handle
    /// (here the stateful Lark `reaction_id` shape); the matching
    /// `Reaction{on:false}` POPS it and passes it to `remove_reaction`. Drives
    /// the real `spawn_gateway_event_consumer` end-to-end through a recording
    /// `MockChannel`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gateway_egress_reaction_handle_round_trips() {
        use crate::transport::providers::mock::MockChannel;

        let mock = MockChannel::new()
            .with_name("telegram")
            .with_reaction_handle("rid-123");
        let mut channels: ChannelMap = HashMap::new();
        channels.insert("telegram".to_string(), Arc::new(mock.clone()));
        let channels = Arc::new(RwLock::new(channels));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        let consumer = spawn_gateway_event_consumer(rx, channels);

        let reaction_event = |on: bool| GatewayEvent {
            id: format!("gateway-reaction-{on}"),
            channel: "telegram".to_string(),
            chat_id: "chat-7".to_string(),
            thread_ts: None,
            content: String::new(),
            kind: GatewayEventKind::Reaction {
                message_id: "tg-555".to_string(),
                on,
            },
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some("s1".to_string()),
            slug: None,
        };
        tx.send(reaction_event(true)).unwrap();
        tx.send(reaction_event(false)).unwrap();

        // Poll the recording mock until both calls land (the consumer is async).
        let mut calls = Vec::new();
        for _ in 0..200 {
            calls = mock.reactions().await;
            if calls.len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        consumer.abort();

        assert_eq!(calls.len(), 2, "add + remove must both fire, got {calls:?}");
        // add: op=add, on the inbound (chat, message_id), no handle passed in.
        assert_eq!(calls[0].0, "add");
        assert_eq!(calls[0].1, "chat-7");
        assert_eq!(calls[0].2, "tg-555");
        assert_eq!(calls[0].3, None);
        // remove: the SAME (chat, message_id), with the stored handle replayed.
        assert_eq!(calls[1].0, "remove");
        assert_eq!(calls[1].1, "chat-7");
        assert_eq!(calls[1].2, "tg-555");
        assert_eq!(
            calls[1].3.as_deref(),
            Some("rid-123"),
            "the add handle must be replayed to remove"
        );
    }

    /// v0.8.19 — the stateless (Telegram) shape: `add_reaction` returns `None`,
    /// so the egress stores `None` and `remove_reaction` is called with `None`
    /// (Telegram clears by message_id alone). The handle map still round-trips
    /// (the key is present), just with a `None` value.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gateway_egress_reaction_stateless_handle_is_none() {
        use crate::transport::providers::mock::MockChannel;

        // No `with_reaction_handle` → add_reaction returns None (Telegram shape).
        let mock = MockChannel::new().with_name("telegram");
        let mut channels: ChannelMap = HashMap::new();
        channels.insert("telegram".to_string(), Arc::new(mock.clone()));
        let channels = Arc::new(RwLock::new(channels));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        let consumer = spawn_gateway_event_consumer(rx, channels);

        let ev = |on: bool| GatewayEvent {
            id: format!("r-{on}"),
            channel: "telegram".to_string(),
            chat_id: "chat-7".to_string(),
            thread_ts: None,
            content: String::new(),
            kind: GatewayEventKind::Reaction {
                message_id: "tg-9".to_string(),
                on,
            },
            attachments: Vec::new(),
            options: Vec::new(),
            sid: None,
            slug: None,
        };
        tx.send(ev(true)).unwrap();
        tx.send(ev(false)).unwrap();

        let mut calls = Vec::new();
        for _ in 0..200 {
            calls = mock.reactions().await;
            if calls.len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        consumer.abort();

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, "remove");
        assert_eq!(
            calls[1].3, None,
            "stateless channel clears with a None handle"
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

    /// `refresh_telegram_command_menu` is a no-op (returns `Ok`) when no
    /// Telegram credential is configured: `ccteam start` calls it
    /// unconditionally (gated only on `--no-imd`), so the common
    /// "no Telegram set up" path must not error / must not touch the network.
    /// A missing credentials file loads as empty creds → `telegram = None`.
    #[tokio::test]
    async fn refresh_menu_is_noop_without_telegram_creds() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("no-such-credentials.json");
        // No file → empty credentials → no telegram block → Ok, no HTTP call.
        refresh_telegram_command_menu(Some(&missing))
            .await
            .expect("missing creds → Ok no-op");

        // A credentials file present but WITHOUT a telegram block is also a
        // no-op (only the telegram arm publishes setMyCommands).
        let only_lark = tmp.path().join("lark-only.json");
        std::fs::write(&only_lark, r#"{"lark":{"app_id":"a","app_secret":"s"}}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&only_lark).unwrap().permissions();
            perm.set_mode(0o600);
            std::fs::set_permissions(&only_lark, perm).unwrap();
        }
        refresh_telegram_command_menu(Some(&only_lark))
            .await
            .expect("non-telegram creds → Ok no-op");
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

    #[cfg(feature = "telegram")]
    fn tg_bot_reg(im_chat_id: &str) -> BotRegistration {
        BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "telegram".into(),
            im_chat_id: im_chat_id.into(),
            chat_handle: None,
            project_dir: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// Empty user allowlist = open mode; a registered bot must NOT flip
    /// the channel into allowlist-only (regression guard for the
    /// fixture-registration lockout).
    #[cfg(feature = "telegram")]
    #[test]
    fn telegram_allowlist_empty_stays_open_despite_registered_bots() {
        let out = telegram_effective_allowlist(&[], &[tg_bot_reg("chat-1")]);
        assert!(out.is_empty(), "open mode must survive registry union");
    }

    /// Explicit user allowlist: registered telegram bots' chat ids are
    /// unioned in (sorted + deduped); other platforms are ignored.
    #[cfg(feature = "telegram")]
    #[test]
    fn telegram_allowlist_unions_registry_onto_explicit_list() {
        let mut slack_bot = tg_bot_reg("C99");
        slack_bot.im_platform = "slack".into();
        let out = telegram_effective_allowlist(
            &["339498819".to_string(), "chat-1".to_string()],
            &[tg_bot_reg("chat-1"), tg_bot_reg("chat-2"), slack_bot],
        );
        assert_eq!(out, vec!["339498819", "chat-1", "chat-2"]);
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
        let claude = factory(AgentVendor::Claude, SessionProtocol::default());
        assert_eq!(
            claude.vendor(),
            AgentVendor::Claude,
            "claude arm must return a Claude adapter"
        );
        let codex = factory(AgentVendor::Codex, SessionProtocol::default());
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
        let codex_a = factory(AgentVendor::Codex, SessionProtocol::default());
        let codex_b = factory(AgentVendor::Codex, SessionProtocol::default());
        assert!(
            Arc::ptr_eq(&codex_a, &codex_b),
            "F10: codex arm must memoise ONE adapter (one app-server child), got distinct Arcs"
        );
        let claude_a = factory(AgentVendor::Claude, SessionProtocol::default());
        let claude_b = factory(AgentVendor::Claude, SessionProtocol::default());
        assert!(
            Arc::ptr_eq(&claude_a, &claude_b),
            "F10: claude arm must also be a singleton"
        );
    }

    /// v0.8.11 E2 — the factory routes Claude by protocol: StreamJson →
    /// `claude-stream-json`, Terminal → `claude-tui`. Codex ignores protocol.
    #[test]
    fn default_adapter_factory_routes_claude_by_protocol() {
        let factory = default_adapter_factory();
        let stream = factory(AgentVendor::Claude, SessionProtocol::StreamJson);
        assert_eq!(stream.name(), "claude-stream-json");
        assert_eq!(stream.vendor(), AgentVendor::Claude);
        let terminal = factory(AgentVendor::Claude, SessionProtocol::Terminal);
        assert_eq!(terminal.name(), "claude-tui");
        // The two protocols select DIFFERENT adapter instances.
        assert!(!Arc::ptr_eq(&stream, &terminal));
        // Codex ignores protocol — same app-server adapter either way.
        let codex_s = factory(AgentVendor::Codex, SessionProtocol::StreamJson);
        let codex_t = factory(AgentVendor::Codex, SessionProtocol::Terminal);
        assert_eq!(codex_s.name(), "codex-app-server");
        assert!(Arc::ptr_eq(&codex_s, &codex_t));
    }
}
