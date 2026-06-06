//! v8.1 IM gateway route table.
//!
//! This module owns the chat-local `project ⇄ session` state that sits
//! above the older `@handle -> mailbox` router. It is deliberately
//! daemon-agnostic: tests drive it with a fake [`HarnessAdapter`], and
//! the daemon can wire the same state machine into real transports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use ccteam_core::config::{upsert_project, CcteamConfig, ProjectEntry};
use ccteam_core::projects::{bootstrap_project_at_dir, validate_slug_format};
use ccteam_core::{CcteamPaths, HotConfig};
use ccteam_harness::{
    chat_session_name, parse_chat_session_name, AgentSpecBrief, AgentVendor, ChoicePrompt,
    ChoiceSelection, Directive, DirectiveOutcome, HarnessAdapter, ProcessBackend, SpawnCtx,
    ThreadEvent, ThreadHandle, ThreadItemDetails, TurnInput,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::pending::InteractionOrigin;
use crate::transport::{AttachmentKind, ChannelAttachment, ChoiceReply, MessageOption};
use crate::BotRegistration;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ChatKey {
    channel: String,
    chat_id: String,
    user_id: String,
}

impl ChatKey {
    fn new(channel: &str, chat_id: &str, user_id: &str) -> Self {
        Self {
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
        }
    }
}

#[derive(Clone)]
struct GatewaySession {
    id: String,
    /// Chat that created the session.
    owner: ChatKey,
    project: String,
    role: String,
    vendor: AgentVendor,
    handle: String,
    thread: ThreadHandle,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    visible_events: Arc<AtomicU64>,
    /// Where this session's replies go (option ①: whoever last drove it).
    /// Starts at `owner`; updated on `/use` and on every submit so a turn
    /// sent from web replies to web, one from Telegram replies to Telegram.
    /// Shared with the detached event pump / watchdog so they route live.
    reply_to: Arc<std::sync::Mutex<ChatKey>>,
}

#[derive(Debug, Clone)]
struct GatewayRouteTemplate {
    channel: String,
    chat_id: String,
    project: String,
    role: String,
    vendor: AgentVendor,
    handle: String,
}

/// In-memory v8.1 route table for one daemon process.
pub struct Gateway {
    adapter_factory:
        Arc<dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync>,
    default_project: String,
    state_path: Option<PathBuf>,
    projects: BTreeMap<String, PathBuf>,
    current_project: BTreeMap<ChatKey, String>,
    current_session: BTreeMap<ChatKey, String>,
    sessions: BTreeMap<String, GatewaySession>,
    templates: Vec<GatewayRouteTemplate>,
    next_session: u64,
    event_sink: Option<GatewayEventSink>,
    /// Broadcast tee of every [`GatewayEvent`] the gateway emits (V0.8.6 —
    /// fix #2). The IM delivery path stays on the mpsc `event_sink`; this
    /// fan-out lets the web layer subscribe a per-session SSE stream
    /// (filtered by [`GatewayEvent::sid`]) without touching that path.
    /// Created up front (in [`new_with_factory`](Self::new_with_factory)) so
    /// [`subscribe_events`](Self::subscribe_events) works even before a sink
    /// is wired or on the standalone path. The pump tees through
    /// [`GatewayEventSink`]; the held sender is the source for new receivers.
    events_broadcast: tokio::sync::broadcast::Sender<GatewayEvent>,
    event_pumps: BTreeMap<String, tokio::task::JoinHandle<()>>,
    /// Outstanding choice prompts (v0.8.5 D3/D4/D6). Its own lock — held
    /// separately from the gateway so a long External await (D6, W2) never
    /// blocks gateway inbound. The daemon injects a shared `Arc` (also
    /// handed to the mcp.sock handler) via [`Gateway::set_pending`]; tests
    /// get the default fresh registry.
    pending: Arc<tokio::sync::Mutex<crate::pending::PendingInteractions>>,
    /// Path context for `/newproject` (scaffold + config-registry write).
    /// `None` in unit tests that don't exercise project creation; the
    /// daemon sets it via [`Gateway::enable_project_creation`].
    project_paths: Option<CcteamPaths>,
    /// Hot-reloaded view of `~/.ccteam/config.yaml` (re-parsed only on mtime
    /// change; pull-based, no watcher). `Some` once the daemon calls
    /// [`Gateway::enable_project_creation`]; lets `/cd` resolve a project
    /// registered after daemon start without a restart — config.yaml is the
    /// source of truth, `projects` is just a cache. `None` in unit tests.
    config: Option<HotConfig<CcteamConfig>>,
}

/// How the daemon should deliver a [`GatewayEvent`] (V0.8.4 P1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GatewayEventKind {
    /// A delivered reply — a **new** message (P0-split, durable-ledgered,
    /// pings the user). Today's behavior.
    #[default]
    Answer,
    /// A live progress update — the daemon keeps **one editable status
    /// message per `status_key`** (sends the first, edits the rest),
    /// folding rapid steps into a single message. `done` finalizes and
    /// forgets the status. Progress bypasses the durable ledger (it is a
    /// delivery-layer UX, not state SoT).
    Progress {
        /// Correlates edits within one turn's status epoch.
        status_key: String,
        /// Final update — finalize + forget after delivering.
        done: bool,
    },
}

/// User-visible text emitted asynchronously from a harness event stream.
///
/// The daemon owns delivery: it maps `channel` to a live [`Channel`],
/// appends the durable outbound ledger row (answers only), and sends to
/// `chat_id`.
#[derive(Debug, Clone)]
pub struct GatewayEvent {
    /// Stable outbound id prefix used by the durable ledger.
    pub id: String,
    /// IM channel name (`telegram`, `ws`, ...).
    pub channel: String,
    /// Platform chat/recipient id.
    pub chat_id: String,
    /// Optional platform thread id.
    pub thread_ts: Option<String>,
    /// User-visible message content.
    pub content: String,
    /// Answer (new message) vs. live progress (edited status message).
    pub kind: GatewayEventKind,
    /// Outbound file attachments (V0.8.4 P2b — `chat_send_file`). Empty
    /// for normal text answers / progress updates.
    pub attachments: Vec<crate::transport::OutboundFile>,
    /// Selectable options to render as buttons / chips (v0.8.5 D3). Empty
    /// for ordinary answers; non-empty when an adapter `NeedsChoice` (or a
    /// D6 hook prompt, W2) is delivered asynchronously.
    pub options: Vec<crate::transport::MessageOption>,
    /// Originating gateway session id (`s{n}`), when the event came from a
    /// tracked session's event pump / turn watchdog (V0.8.6 W5b). This is
    /// the **SSE filter key**: a per-session web SSE handler keeps only the
    /// events whose `sid` matches the session it is streaming. `None` for
    /// events not tied to a session (e.g. the `chat_send_file` MCP path and
    /// the D6 `interaction/ask` hook prompt, which have no gateway session).
    /// The IM delivery path ignores `sid` entirely — it routes by `channel`
    /// + `chat_id` as before — so this is additive.
    pub sid: Option<String>,
}

/// The gateway's emit endpoint (V0.8.6 — fix #2). Every [`GatewayEvent`] the
/// gateway produces (pump answers + progress, turn-timeout watchdog, choice
/// prompts) is sent through this, which **tees** to two consumers:
///
/// 1. the daemon's mpsc consumer (`event_sink`) — the IM/web delivery path,
///    routed by `channel` + `chat_id`, unchanged; and
/// 2. a broadcast fan-out — for per-session web SSE, filtered by `sid`.
///
/// Send semantics follow the mpsc: [`send`](Self::send) returns `false` only
/// when the mpsc receiver is gone (the daemon exited → the pump should stop),
/// matching the prior raw-`UnboundedSender` `is_err()`/`is_ok()` checks. A
/// broadcast send with no live receivers is the normal case and is ignored.
#[derive(Clone)]
struct GatewayEventSink {
    /// The IM/web delivery channel (the historical sink).
    mpsc: tokio::sync::mpsc::UnboundedSender<GatewayEvent>,
    /// Fan-out for per-session SSE subscribers.
    broadcast: tokio::sync::broadcast::Sender<GatewayEvent>,
}

impl GatewayEventSink {
    /// Tee one event to the mpsc delivery path **and** the broadcast fan-out.
    /// Returns `true` while the mpsc is live; `false` once it is closed (the
    /// pump's stop signal). The broadcast leg never affects the return — a
    /// lagging/absent SSE subscriber must not stop IM delivery.
    fn send(&self, event: GatewayEvent) -> bool {
        // Broadcast first (cheap clone); a `SendError` here just means no SSE
        // subscriber is attached, which is normal.
        let _ = self.broadcast.send(event.clone());
        self.mpsc.send(event).is_ok()
    }
}

/// A live `ccteam-chat-*` process with no matching tracked gateway session —
/// a survivor of a prior daemon. The process name carries only slug+role (not
/// the owning chat), so orphans are a global concern and are never attributed
/// to a single chat's `/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanSession {
    /// Full process/tmux session name (`ccteam-chat-<slug>-<role>`).
    pub name: String,
    /// Project slug parsed from the name.
    pub slug: String,
    /// Role parsed from the name.
    pub role: String,
}

/// Reconciliation of live chat-mode processes against this gateway's tracked
/// sessions. See [`Gateway::reconcile_chat_sessions`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionInventory {
    /// Live `ccteam-chat-*` names that map to a tracked gateway session.
    pub tracked: Vec<String>,
    /// Live `ccteam-chat-*` names with no tracked session (orphans).
    pub orphans: Vec<OrphanSession>,
}

/// A serializable, read-only snapshot of one tracked gateway session
/// (V0.8.6 W5b — the resource API spine). Produced by
/// [`Gateway::session_views`] under a brief lock (fields are cloned; no
/// `.await` is held), so the web layer can list live sessions without
/// reaching into the gateway's private maps. `vendor` is stringified
/// (`"claude"` / `"codex"`) to keep the wire shape stable independent of
/// the harness enum. `current` is true when the session is the active one
/// for *any* chat the gateway is routing (the web console is a global
/// operator view, so "current for someone" is the useful hint). `status`
/// is a cheap, synchronous liveness label — the async per-session model /
/// context detail stays in `/sessions` (which `.await`s `thread_status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    /// Gateway session id (`s{n}`) — the network-API session namespace.
    pub sid: String,
    /// Project slug the session runs in.
    pub project: String,
    /// Agent role (`.claude/agents/<role>.md`).
    pub role: String,
    /// Vendor, stringified (`"claude"` / `"codex"`).
    pub vendor: String,
    /// Whether this session is the active one for at least one chat.
    pub current: bool,
    /// Cheap synchronous liveness hint (`"live"` for any tracked session).
    pub status: String,
}

/// v0.8.7 W1 — what [`Gateway::session_resolve`] hands a collector so it can
/// tail a child session's `.ccteam/chat/<role>/turns.jsonl` without reaching
/// into the gateway's private session map. Pure data (no adapter handle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResolve {
    /// Gateway session id (`s{n}`).
    pub sid: String,
    /// Agent role — the `<bot>` segment of the transcript path.
    pub role: String,
    /// Project slug the session runs in.
    pub project: String,
    /// Absolute working dir hosting `.ccteam/chat/<role>/turns.jsonl`.
    pub project_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewayState {
    default_project: String,
    current_project: Vec<SavedGatewayRoute>,
    current_session: Vec<SavedGatewayRoute>,
    sessions: Vec<SavedGatewaySession>,
    next_session: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewayRoute {
    chat: ChatKey,
    value: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewaySession {
    id: String,
    owner: ChatKey,
    project: String,
    role: String,
    vendor: AgentVendor,
    handle: String,
    thread: ThreadHandle,
}

/// A gateway-owned command (v0.8.5 P1): the single source of truth for the
/// command names the gateway handles itself + their menu/help metadata.
/// `is_gateway_command`, `/help`, and the channel menu registration all
/// derive from this table — no hand-copied lists drifting apart.
pub struct GatewayCommandSpec {
    /// Command name including the leading `/` (e.g. `"/new"`).
    pub name: &'static str,
    /// Short argument hint for `/help` (e.g. `"<id>"`); `None` for zero-arg.
    pub arg_hint: Option<&'static str>,
    /// One-line description for `/help` and the channel menu.
    pub help: &'static str,
    /// Show in the channel command menu. Zero-arg commands read well in a
    /// menu; arg-bearing ones still work but the menu only types the name.
    pub in_menu: bool,
}

/// The gateway's own commands. Everything else `/…` is forwarded to the
/// current session's agent via `handle_directive`.
pub const GATEWAY_COMMANDS: &[GatewayCommandSpec] = &[
    GatewayCommandSpec {
        name: "/new",
        arg_hint: Some("[vendor] [role]"),
        help: "start a new session",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/use",
        arg_hint: Some("<id>"),
        help: "switch to a session",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/role",
        arg_hint: Some("<role>"),
        help: "switch the current session to a fresh agent role",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/cd",
        arg_hint: Some("<project>"),
        help: "switch project",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/sessions",
        arg_hint: None,
        help: "list sessions + status",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/projects",
        arg_hint: None,
        help: "list projects",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/newproject",
        arg_hint: Some("<slug> <path>"),
        help: "scaffold + register a project",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/pair",
        arg_hint: Some("<code>"),
        help: "pair this chat",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/help",
        arg_hint: None,
        help: "show gateway commands",
        in_menu: true,
    },
];

/// The [`GATEWAY_COMMANDS`] entries flagged `in_menu`, mapped to the
/// channel-facing [`CommandSpec`] shape (v0.8.5 P1). The daemon calls this
/// once at startup and hands the result to each channel's
/// [`Channel::register_commands`]. Passthrough vendor slashes (`/compact`,
/// `/model`, …) deliberately stay out of the menu — they are vendor-relative
/// and would mislead. `name` keeps the leading `/` (each channel strips it
/// if its API wants bare names, e.g. Telegram `setMyCommands`).
pub fn menu_command_specs() -> Vec<crate::transport::CommandSpec> {
    GATEWAY_COMMANDS
        .iter()
        .filter(|c| c.in_menu)
        .map(|c| crate::transport::CommandSpec {
            name: c.name.to_string(),
            description: c.help.to_string(),
        })
        .collect()
}

impl Gateway {
    /// Create a gateway with one default project.
    pub fn new(
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
        default_project: impl Into<String>,
        default_dir: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_factory(
            {
                let adapter = Arc::clone(&adapter);
                Arc::new(move |_vendor| Arc::clone(&adapter))
            },
            default_project,
            default_dir,
        )
    }

    /// Create a gateway with per-vendor adapter selection.
    pub fn new_with_factory(
        adapter_factory: Arc<
            dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync,
        >,
        default_project: impl Into<String>,
        default_dir: impl Into<PathBuf>,
    ) -> Self {
        let default_project = default_project.into();
        let mut projects = BTreeMap::new();
        projects.insert(default_project.clone(), default_dir.into());
        // Capacity covers a burst of answers + progress edits for one turn; SSE
        // subscribers that fall behind get a `Lagged` and the SSE handler emits
        // a reconnect hint (the SPA's `EventSource` then re-subscribes).
        let (events_broadcast, _) = tokio::sync::broadcast::channel(256);
        Self {
            adapter_factory,
            default_project,
            state_path: None,
            projects,
            current_project: BTreeMap::new(),
            current_session: BTreeMap::new(),
            sessions: BTreeMap::new(),
            templates: Vec::new(),
            next_session: 0,
            event_sink: None,
            events_broadcast,
            event_pumps: BTreeMap::new(),
            pending: Arc::new(tokio::sync::Mutex::new(
                crate::pending::PendingInteractions::new(),
            )),
            project_paths: None,
            config: None,
        }
    }

    /// Enable async delivery of [`HarnessAdapter::events`] back to IM.
    ///
    /// When enabled, `handle_text` returns a quick submit ACK and the
    /// daemon sends later assistant/error events via this sink. Calling
    /// this after `enable_persistence` also re-subscribes restored
    /// sessions, which is the daemon-restart path.
    pub fn set_event_sink(&mut self, tx: tokio::sync::mpsc::UnboundedSender<GatewayEvent>) {
        // Wrap the IM/web mpsc with the always-present broadcast tee so every
        // emitted event also reaches per-session SSE subscribers (fix #2).
        self.event_sink = Some(GatewayEventSink {
            mpsc: tx,
            broadcast: self.events_broadcast.clone(),
        });
        let ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            self.spawn_event_pump(&id);
        }
    }

    /// Subscribe to the broadcast tee of every [`GatewayEvent`] this gateway
    /// emits (V0.8.6 — fix #2). The per-session web SSE handler filters the
    /// stream by [`GatewayEvent::sid`]. Works regardless of whether a delivery
    /// sink has been wired ([`set_event_sink`](Self::set_event_sink)); a fresh
    /// receiver only sees events emitted after it subscribes. Cheap: clones an
    /// existing broadcast `Sender` and registers a new `Receiver`.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<GatewayEvent> {
        self.events_broadcast.subscribe()
    }

    /// Inject the shared pending-interaction registry (v0.8.5). The daemon
    /// hands the same `Arc` to the mcp.sock handler so D6 (W2) can register
    /// External-origin prompts there while the gateway resolves them on
    /// inbound — a single shared registry, two wiring points.
    pub fn set_pending(
        &mut self,
        pending: Arc<tokio::sync::Mutex<crate::pending::PendingInteractions>>,
    ) {
        self.pending = pending;
    }

    /// Enable `/newproject <slug> <path>` by giving the gateway the path
    /// context it needs to scaffold + register a project. The daemon
    /// wires this; unit tests that don't create projects leave it unset
    /// (the command then reports it's unavailable).
    pub fn enable_project_creation(&mut self, paths: CcteamPaths) {
        // Build the hot-reloaded config.yaml view: stat-on-read, re-parse only
        // when its mtime advances (pull-based; respects the "no file-watch"
        // red line). `/cd` resolves projects through this, so a project added
        // by `ccteam init` after startup is picked up without a daemon restart.
        let root = paths.root.clone();
        self.config = Some(HotConfig::new(
            ccteam_core::config::config_path(&root),
            move || ccteam_core::config::load(&root),
        ));
        self.project_paths = Some(paths);
    }

    /// Load and persist route/session state at `path`.
    ///
    /// The daemon uses this for v8.1 spawn-on-demand continuity across
    /// restarts. Unit tests keep the default in-memory mode.
    pub fn enable_persistence(&mut self, path: impl Into<PathBuf>) -> Result<()> {
        self.state_path = Some(path.into());
        self.load_state()
    }

    /// Reconnect persisted sessions after daemon restart.
    ///
    /// Claude TUI sessions first use the live tmux `resume_thread`
    /// path, then merge persisted transcript-tail context back in.
    /// If the pane is gone, real Claude handles fall through to
    /// `start_thread` so the adapter can reattach/recreate. Codex
    /// app-server sessions use the native `thread/resume` RPC.
    pub async fn resume_restored_sessions(&mut self) {
        let ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let Some(snapshot) = self.sessions.get(&id).cloned() else {
                continue;
            };
            let Some(cwd) = self.projects.get(&snapshot.project).cloned() else {
                tracing::warn!(
                    session = %id,
                    project = %snapshot.project,
                    "ccteam-im: restored gateway session skipped; project root missing"
                );
                continue;
            };
            let adapter = (self.adapter_factory)(snapshot.vendor);
            let resumed = match snapshot.vendor {
                AgentVendor::Claude => {
                    match adapter.resume_thread(&snapshot.thread.identity).await {
                        Ok(mut thread) => {
                            thread.raw_extras = merge_thread_extras(
                                snapshot.thread.raw_extras.clone(),
                                thread.raw_extras,
                            );
                            Ok(thread)
                        }
                        Err(err) if is_real_claude_tui_handle(&snapshot.thread) => {
                            tracing::warn!(
                                session = %id,
                                error = %err,
                                "ccteam-im: Claude restored-session resume failed; trying start_thread reattach/recreate"
                            );
                            adapter
                                .start_thread(
                                    &AgentSpecBrief {
                                        role: snapshot.role.clone(),
                                    },
                                    &SpawnCtx {
                                        slug: snapshot.project.clone(),
                                        sid: snapshot.id.clone(),
                                        cwd: cwd.clone(),
                                        project_dir: cwd,
                                        extra_args: vec![],
                                        model_id: None,
                                    },
                                )
                                .await
                        }
                        Err(err) => Err(err),
                    }
                }
                AgentVendor::Codex => adapter.resume_thread(&snapshot.thread.identity).await,
            };
            match resumed {
                Ok(thread) => {
                    if let Some(session) = self.sessions.get_mut(&id) {
                        session.thread = thread;
                        session.adapter = adapter;
                        session.visible_events = Arc::new(AtomicU64::new(0));
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session = %id,
                        vendor = ?snapshot.vendor,
                        error = %err,
                        "ccteam-im: restored gateway session resume failed; keeping persisted handle"
                    );
                }
            }
        }
        if let Err(err) = self.persist_state() {
            tracing::warn!(
                error = %err,
                "ccteam-im: failed to persist resumed gateway sessions"
            );
        }
    }

    /// Register or update a project root addressable by `/cd <slug>`.
    pub fn register_project(&mut self, slug: impl Into<String>, dir: impl Into<PathBuf>) {
        self.projects.insert(slug.into(), dir.into());
    }

    /// Dynamically load one project from `config.yaml` into the in-memory map
    /// if it is missing. The map is otherwise a startup-time snapshot (+ bot
    /// templates + `/newproject`), so a project registered by `ccteam init`
    /// *after* the daemon started would be invisible to `/cd` ("unknown
    /// project") even though the project list reads the config registry live.
    /// This lazily syncs the gateway with the registry — the source of truth —
    /// on demand, so no daemon restart is needed to pick up a fresh project.
    /// No-op when `project_paths` isn't wired (unit tests) or the slug isn't
    /// registered.
    fn ensure_project_loaded(&mut self, slug: &str) {
        if self.projects.contains_key(slug) {
            return;
        }
        // Read the hot-reloaded config.yaml (re-parsed only on mtime change);
        // resolve the Arc before touching self.projects so the borrow on
        // self.config is released first.
        let cfg = match self.config.as_ref().map(|c| c.get()) {
            Some(Ok(cfg)) => cfg,
            _ => return,
        };
        if let Some(entry) = cfg.projects.iter().find(|p| p.slug == slug) {
            self.projects.insert(slug.to_string(), entry.path.clone());
        }
    }

    /// Register a persisted bot as a spawn-on-demand gateway session template.
    pub fn register_bot_template(
        &mut self,
        bot: &BotRegistration,
        project_dir: impl Into<PathBuf>,
    ) {
        self.register_project(bot.workflow_slug.clone(), project_dir);
        let template = GatewayRouteTemplate {
            channel: bot.im_platform.clone(),
            chat_id: bot.im_chat_id.clone(),
            project: bot.workflow_slug.clone(),
            role: bot.role.clone(),
            vendor: bot.vendor,
            handle: bot.effective_handle().to_string(),
        };
        if let Some(existing) = self.templates.iter_mut().find(|entry| {
            entry.channel == template.channel
                && entry.chat_id == template.chat_id
                && entry.project == template.project
                && entry.role == template.role
        }) {
            *existing = template;
        } else {
            self.templates.push(template);
        }
    }

    /// True when `text` is one of the gateway-owned slash commands.
    pub fn is_gateway_command(text: &str) -> bool {
        match text.split_whitespace().next() {
            Some(first) => GATEWAY_COMMANDS.iter().any(|c| c.name == first),
            None => false,
        }
    }

    /// True when this chat/user already has a current gateway session.
    pub fn has_current_session(&self, channel: &str, chat_id: &str, user_id: &str) -> bool {
        self.current_session
            .contains_key(&ChatKey::new(channel, chat_id, user_id))
    }

    /// Route one inbound text message and return outbound replies. Thin
    /// wrapper over [`handle_message`](Self::handle_message) with no
    /// attachments — preserves the historic call shape for tests and
    /// non-Telegram callers.
    pub async fn handle_text(
        &mut self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        text: &str,
    ) -> Result<Vec<String>> {
        self.handle_message(channel, chat_id, user_id, "", text, &[], None)
            .await
    }

    /// Route one inbound message (text + optional attachments) and return
    /// outbound replies. V0.8.4 P2a: when `attachments` is non-empty, the
    /// submitted turn text is wrapped in a `<channel …>` tag naming each
    /// file's on-disk path, so the agent `Read`s it — the load-bearing
    /// Read convention is taught by the daemon's MCP server instructions
    /// (`ccteam mcp-serve` `initialize`).
    // v0.8.5 D3 added the `selection` arg (inbound option click); the
    // per-field inbound signature is the established shape (same as the
    // daemon's `deliver_progress`), so allow the arg count.
    #[allow(clippy::too_many_arguments)]
    pub async fn handle_message(
        &mut self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        message_id: &str,
        text: &str,
        attachments: &[ChannelAttachment],
        selection: Option<&ChoiceReply>,
    ) -> Result<Vec<String>> {
        let chat = ChatKey::new(channel, chat_id, user_id);
        // (v0.8.5 D3) An inbound option click (Telegram callback / web chip)
        // resolves the session's pending choice — never treated as text.
        if let Some(reply) = selection {
            return self.resolve_selection(&chat, &reply.data).await;
        }
        // (v0.8.5 D3) A bare number is a short-reply to a pending choice, but
        // only when one is outstanding for the current session; otherwise
        // it's ordinary text for the agent.
        if let Some(n) = numeric_choice(text) {
            if self.has_pending_for_current(&chat).await {
                return self.resolve_numeric(&chat, n).await;
            }
        }
        // Commands parse on the raw text; attachments don't apply to them.
        if let Some(reply) = self.handle_command(&chat, text).await? {
            return Ok(vec![reply]);
        }
        if let Some((handle, payload)) = crate::router::parse_first_mention(text) {
            if let Some(session_id) = self.session_by_handle(&chat, &handle) {
                self.current_session.insert(chat.clone(), session_id);
                if payload.is_empty() && attachments.is_empty() {
                    return Ok(vec![format!("using @{handle}")]);
                }
                let turn =
                    wrap_inbound(channel, chat_id, user_id, message_id, &payload, attachments);
                return self.submit_to_current(&chat, turn).await;
            }
            if let Some(template) = self.template_by_handle(&chat, &handle) {
                let session_id = self.start_template_session(chat.clone(), template).await?;
                self.current_session.insert(chat.clone(), session_id);
                if payload.is_empty() && attachments.is_empty() {
                    return Ok(vec![format!("using @{handle}")]);
                }
                let turn =
                    wrap_inbound(channel, chat_id, user_id, message_id, &payload, attachments);
                return self.submit_to_current(&chat, turn).await;
            }
        }
        let templates = self.templates_for_chat(&chat);
        if templates.len() > 1 {
            let mut handles: Vec<String> = templates.iter().map(|t| t.handle.clone()).collect();
            handles.sort();
            handles.dedup();
            return Ok(vec![crate::inbound::format_ambiguous_dm_reply(&handles)]);
        }
        self.ensure_current_session(&chat).await?;
        let turn = wrap_inbound(channel, chat_id, user_id, message_id, text, attachments);
        self.submit_to_current(&chat, turn).await
    }

    async fn handle_command(&mut self, chat: &ChatKey, text: &str) -> Result<Option<String>> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return Ok(None);
        }
        let mut parts = trimmed.split_whitespace();
        let cmd = parts.next().unwrap_or_default();
        match cmd {
            "/pair" => {
                let code = parts
                    .next()
                    .ok_or_else(|| anyhow!("/pair requires a code"))?;
                self.ensure_current_session(chat).await?;
                self.persist_state()?;
                Ok(Some(format!("paired {code}")))
            }
            "/new" => {
                let vendor = parse_vendor(parts.next().unwrap_or("claude"))?;
                let role = parts.next().unwrap_or("cto").to_string();
                let project = self.current_project_for(chat);
                let handle = role.clone();
                let session_id = self
                    .start_session(chat.clone(), project, vendor, role, handle)
                    .await?;
                Ok(Some(format!("created session {session_id}")))
            }
            "/role" => {
                let role = parts
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("用法: /role <role>"))?
                    .to_string();
                let sid = self.switch_current_role(chat, role.clone()).await?;
                Ok(Some(format!("switched session {sid} to role {role}")))
            }
            "/use" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("/use requires a session id"))?;
                // The web console drives any session (cross-entry sharing);
                // IM channels stay scoped to sessions they own.
                let session = self
                    .sessions
                    .get(id)
                    .filter(|s| s.owner == *chat || chat.channel == "web")
                    .ok_or_else(|| anyhow!("unknown session for this chat: {id}"))?;
                let sid = session.id.clone();
                if let Ok(mut target) = session.reply_to.lock() {
                    *target = chat.clone();
                }
                self.current_session.insert(chat.clone(), sid.clone());
                self.persist_state()?;
                Ok(Some(format!("using session {sid}")))
            }
            "/cd" => {
                let project = parts
                    .next()
                    .ok_or_else(|| anyhow!("/cd requires a project"))?;
                // (v0.8.5) Pick up a project registered in config.yaml after the
                // daemon started — the in-memory map is a cache, config.yaml is
                // the source of truth — so /cd needs no daemon restart.
                self.ensure_project_loaded(project);
                if !self.projects.contains_key(project) {
                    return Err(anyhow!("unknown project: {project}"));
                }
                self.current_project
                    .insert(chat.clone(), project.to_string());
                // The active session must follow the project switch, otherwise
                // messages keep landing in the previous project's session while
                // the receipt claims we moved. Adopt an existing session owned by
                // this chat in the target project (deterministic: smallest id);
                // otherwise clear the active session so the next message spawns
                // one on demand in the target project via `ensure_current_session`.
                let adopted = self.adopt_session_in_project(chat, project);
                self.persist_state()?;
                Ok(Some(match adopted {
                    Some(sid) => format!("project set to {project} (switched to {sid})"),
                    None => {
                        format!("project set to {project} (next message starts a session there)")
                    }
                }))
            }
            "/newproject" => {
                // `/newproject <slug> <path>` — the path is the remainder
                // of the line so it may contain spaces. Splitting on the
                // first two whitespace runs keeps the path intact.
                let mut it = trimmed.splitn(3, char::is_whitespace);
                let _cmd = it.next();
                let slug = it
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("用法: /newproject <slug> <项目路径>"))?;
                let path = it
                    .next()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| anyhow!("用法: /newproject <slug> <项目路径>"))?;
                self.create_project(slug, path).map(Some)
            }
            "/sessions" => Ok(Some(self.render_sessions(chat).await)),
            "/projects" => Ok(Some(self.render_projects())),
            "/help" => Ok(Some(render_help())),
            _ => Ok(None),
        }
    }

    /// Scaffold a ccteam project at `raw_path`, register it in
    /// `config.yaml`, and make it addressable by `/cd <slug>` in this
    /// running daemon. `raw_path` may be `~`-relative; it must resolve to
    /// an absolute directory (existing repos are adopted in place, empty
    /// dirs are created — `bootstrap_project_at_dir` leaves user files
    /// alone). Requires [`Gateway::enable_project_creation`].
    fn create_project(&mut self, slug: &str, raw_path: &str) -> Result<String> {
        let paths = self
            .project_paths
            .clone()
            .ok_or_else(|| anyhow!("project creation is not configured on this daemon"))?;
        let slug = validate_slug_format(slug)?;
        // (v0.8.5) Detect a slug already registered in config.yaml even if it's
        // not yet in our in-memory cache, so /newproject can't clobber it.
        self.ensure_project_loaded(&slug);
        if self.projects.contains_key(&slug) {
            return Err(anyhow!("project already exists: {slug}"));
        }
        let abs = expand_project_path(raw_path)?;
        bootstrap_project_at_dir(&paths, &abs, &slug, "(created from web/IM chat)", "dev")
            .with_context(|| format!("scaffold project {slug} at {}", abs.display()))?;
        upsert_project(
            &paths.root,
            ProjectEntry {
                slug: slug.clone(),
                path: abs.clone(),
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .with_context(|| format!("register project {slug} in config.yaml"))?;
        self.register_project(slug.clone(), abs.clone());
        if let Err(err) = self.persist_state() {
            tracing::warn!(error = %err, "ccteam-im: persist after /newproject failed");
        }
        Ok(format!("created project {slug} at {}", abs.display()))
    }

    async fn ensure_current_session(&mut self, chat: &ChatKey) -> Result<()> {
        if self.current_session.contains_key(chat) {
            return Ok(());
        }
        let templates = self.templates_for_chat(chat);
        if templates.len() == 1 {
            // A single registered bot template spawns on demand — UNLESS the
            // user explicitly `/cd`'d to a different project. An explicit `/cd`
            // target wins over the template so the project switch is honoured:
            // fall through to a default `cto` agent in the requested project
            // rather than silently dragging the message back into the bot's
            // project. (Tradeoff: the bot's role/vendor are not reused once you
            // `/cd` off its project.)
            let template = &templates[0];
            let cd_elsewhere = self
                .current_project
                .get(chat)
                .is_some_and(|p| *p != template.project);
            if !cd_elsewhere {
                self.start_template_session(chat.clone(), template.clone())
                    .await?;
                return Ok(());
            }
        }
        if templates.len() > 1 {
            let mut handles: Vec<String> = templates.iter().map(|t| t.handle.clone()).collect();
            handles.sort();
            handles.dedup();
            return Err(anyhow!(crate::inbound::format_ambiguous_dm_reply(&handles)));
        }
        let project = self.current_project_for(chat);
        self.start_session(
            chat.clone(),
            project,
            AgentVendor::Claude,
            "cto".to_string(),
            "cto".to_string(),
        )
        .await?;
        Ok(())
    }

    async fn start_template_session(
        &mut self,
        owner: ChatKey,
        template: GatewayRouteTemplate,
    ) -> Result<String> {
        self.current_project
            .insert(owner.clone(), template.project.clone());
        self.start_session(
            owner,
            template.project,
            template.vendor,
            template.role,
            template.handle,
        )
        .await
    }

    async fn start_session(
        &mut self,
        owner: ChatKey,
        project: String,
        vendor: AgentVendor,
        role: String,
        handle: String,
    ) -> Result<String> {
        // A chat session's tmux pane is named `ccteam-chat-<project>-<role>`,
        // so one (project, role) == one pane + transcript. Reuse an existing
        // record instead of spawning a duplicate that would share the pane and
        // run a second event pump over the same transcript (which doubles every
        // reply and clutters the session list). Point it at the new driver.
        if let Some(existing) = self
            .sessions
            .values()
            .find(|s| s.project == project && s.role == role)
        {
            let id = existing.id.clone();
            if let Ok(mut target) = existing.reply_to.lock() {
                *target = owner.clone();
            }
            self.current_session.insert(owner, id.clone());
            self.persist_state()?;
            return Ok(id);
        }
        self.next_session += 1;
        let id = format!("s{}", self.next_session);
        let cwd = self
            .projects
            .get(&project)
            .cloned()
            .ok_or_else(|| anyhow!("unknown project: {project}"))?;
        let adapter = (self.adapter_factory)(vendor);
        let thread = adapter
            .start_thread(
                &AgentSpecBrief { role: role.clone() },
                &SpawnCtx {
                    slug: project.clone(),
                    sid: id.clone(),
                    cwd: cwd.clone(),
                    project_dir: cwd,
                    extra_args: vec![],
                    model_id: None,
                },
            )
            .await?;
        self.sessions.insert(
            id.clone(),
            GatewaySession {
                id: id.clone(),
                owner: owner.clone(),
                project,
                role,
                vendor,
                handle,
                thread,
                adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(owner.clone())),
            },
        );
        self.current_session.insert(owner, id.clone());
        self.persist_state()?;
        self.spawn_event_pump(&id);
        Ok(id)
    }

    /// Switch the chat's CURRENT session to run `role` (W1 `/role`). Start-time
    /// binding: the pane is `(project, role)`, so a role change is a *different*
    /// pane — close the old one and re-spawn a fresh `--agent <role>` thread,
    /// reusing the SAME gateway session id so `/use <sid>` keeps resolving. No
    /// dedup here (unlike `start_session`): the new pane never collides with the
    /// old role's pane, and an explicit `/role` always wants a fresh agent.
    ///
    /// The target role is validated (name charset + `.claude/agents/<role>.md`
    /// existence under the session's project dir) BEFORE any teardown, so a bad
    /// or missing role is rejected with the live session left untouched rather
    /// than destroying the user's working pane on a failed re-spawn.
    async fn switch_current_role(&mut self, chat: &ChatKey, role: String) -> Result<String> {
        let sid = self
            .current_session
            .get(chat)
            .cloned()
            .ok_or_else(|| anyhow!("/role 需要一个活动会话:先 /new 或发条消息再切换。"))?;
        let old = self
            .sessions
            .get(&sid)
            .ok_or_else(|| anyhow!("current session missing: {sid}"))?;
        // Already this role → no-op (a fresh re-spawn would needlessly drop
        // the live pane's context for no behavioral change).
        if old.role == role {
            return Ok(sid);
        }
        let project = old.project.clone();
        let vendor = old.vendor;
        let owner = old.owner.clone();
        let old_thread = old.thread.clone();
        let old_adapter = Arc::clone(&old.adapter);
        let cwd = self
            .projects
            .get(&project)
            .cloned()
            .ok_or_else(|| anyhow!("unknown project: {project}"))?;

        // Validate the target role BEFORE touching the live session: a typo or
        // a role that has no `.claude/agents/<role>.md` would otherwise tear the
        // working pane down here and then fail to spawn `claude --agent <role>`,
        // silently destroying the user's chat. `read_role` resolves the role
        // file under the session's project dir (`cwd`, the same path the spawn
        // uses) and validates the name charset ([a-z0-9_-]); it returns `Err`
        // on a bad name (path-traversal etc.) and `Ok(None)` when the file is
        // absent — both mean "no such role", so we bail with a clear hint and
        // leave the session completely intact.
        if ccteam_core::read_role(&cwd, &role).ok().flatten().is_none() {
            return Err(anyhow!(
                "role 不存在:.claude/agents/{role}.md 未找到;用 /role <已存在的角色>"
            ));
        }

        // Tear down the old pane + its event pump before re-spawning so the
        // single (project, role) pane invariant holds and no stale pump keeps
        // draining the retired transcript.
        if let Some(pump) = self.event_pumps.remove(&sid) {
            pump.abort();
        }
        let _ = old_adapter.close_thread(&old_thread).await;

        let adapter = (self.adapter_factory)(vendor);
        let thread = adapter
            .start_thread(
                &AgentSpecBrief { role: role.clone() },
                &SpawnCtx {
                    slug: project.clone(),
                    sid: sid.clone(),
                    cwd: cwd.clone(),
                    project_dir: cwd,
                    extra_args: vec![],
                    model_id: None,
                },
            )
            .await?;
        // Replace the record in place: same sid, new role/handle/thread, fresh
        // pane counters; replies route back to the owner (the chat that drives
        // it), matching `start_session`.
        self.sessions.insert(
            sid.clone(),
            GatewaySession {
                id: sid.clone(),
                owner: owner.clone(),
                project,
                role: role.clone(),
                vendor,
                handle: role,
                thread,
                adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(owner)),
            },
        );
        self.current_session.insert(chat.clone(), sid.clone());
        self.persist_state()?;
        self.spawn_event_pump(&sid);
        Ok(sid)
    }

    fn spawn_event_pump(&mut self, session_id: &str) {
        if self.event_pumps.contains_key(session_id) {
            return;
        }
        let Some(tx) = self.event_sink.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(session_id).cloned() else {
            return;
        };
        let session_id = session.id.clone();
        let pump_key = session_id.clone();
        let handle = tokio::spawn(async move {
            use std::time::Instant;
            // V0.8.4 P1: split the event stream into ANSWER (new message)
            // and PROGRESS (one live, edited status message per turn).
            let progress_on = progress_enabled();
            let throttle = progress_throttle();
            let mut events = session.adapter.events(&session.thread);
            let mut seq: u64 = 0; // answer sequence → message ids
            let mut epoch: u64 = 0; // status epoch → one per turn
            let mut fold = crate::progress::ProgressFold::new();
            let mut dirty = false;
            let mut last_emit: Option<Instant> = None;
            let mut last_sent: Option<String> = None;

            loop {
                // Flush timer, armed only while a throttled update waits.
                let flush = async {
                    if let Some(t) = last_emit {
                        let deadline = t + throttle;
                        let now = Instant::now();
                        if now < deadline {
                            tokio::time::sleep(deadline - now).await;
                        }
                    }
                };

                tokio::select! {
                    biased;
                    maybe = events.next() => {
                        let Some(evt) = maybe else { break; };
                        if let Some(text) = async_event_text(&evt) {
                            // ----- ANSWER (or error) -----
                            // Finalize this turn's status epoch first.
                            if progress_on && fold.has_activity() && !fold.done() {
                                fold.mark_done();
                                if !flush_progress(
                                    &tx, &session, &session_id, epoch, &fold,
                                    &mut last_sent, &mut last_emit, &mut dirty,
                                ) {
                                    break;
                                }
                                epoch = epoch.saturating_add(1);
                            }
                            fold = crate::progress::ProgressFold::new();
                            dirty = false;
                            last_emit = None;
                            last_sent = None;

                            seq = seq.saturating_add(1);
                            session.visible_events.fetch_add(1, Ordering::SeqCst);
                            let (channel, chat_id) = pump_target(&session);
                            // `GatewayEventSink::send` returns false only when the
                            // mpsc consumer is gone (daemon exited) → stop the pump.
                            if !tx.send(GatewayEvent {
                                id: format!("gateway-event-{session_id}-{seq}"),
                                channel,
                                chat_id,
                                thread_ts: None,
                                content: text,
                                kind: GatewayEventKind::Answer,
                                attachments: Vec::new(),
                                options: Vec::new(),
                                sid: Some(session_id.clone()),
                            }) {
                                break;
                            }
                        } else if progress_on && fold.apply(&evt) {
                            // ----- PROGRESS -----
                            dirty = true;
                            let ready = last_emit.map(|t| t.elapsed() >= throttle).unwrap_or(true);
                            if ready
                                && !flush_progress(
                                    &tx, &session, &session_id, epoch, &fold,
                                    &mut last_sent, &mut last_emit, &mut dirty,
                                )
                            {
                                break;
                            }
                        }
                    }
                    _ = flush, if progress_on && dirty => {
                        if !flush_progress(
                            &tx, &session, &session_id, epoch, &fold,
                            &mut last_sent, &mut last_emit, &mut dirty,
                        ) {
                            break;
                        }
                    }
                }
            }
            // Stream ended — flush any pending progress as a last update.
            if progress_on && dirty {
                let _ = flush_progress(
                    &tx,
                    &session,
                    &session_id,
                    epoch,
                    &fold,
                    &mut last_sent,
                    &mut last_emit,
                    &mut dirty,
                );
            }
        });
        self.event_pumps.insert(pump_key, handle);
    }

    fn load_state(&mut self) -> Result<()> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)?;
        let saved: SavedGatewayState = serde_json::from_str(&raw)?;
        self.default_project = saved.default_project;
        self.current_project = saved
            .current_project
            .into_iter()
            .map(|route| (route.chat, route.value))
            .collect();
        self.current_session = saved
            .current_session
            .into_iter()
            .map(|route| (route.chat, route.value))
            .collect();
        self.next_session = saved.next_session;
        self.sessions.clear();
        // Collapse legacy duplicate records that share a tmux pane (same
        // project+role) — keep the first, drop the rest. Without this, each
        // duplicate would resume its own pump over the same transcript and
        // re-deliver every reply.
        let mut seen_panes = std::collections::HashSet::new();
        for saved_session in saved.sessions {
            if !seen_panes.insert((saved_session.project.clone(), saved_session.role.clone())) {
                continue;
            }
            let adapter = (self.adapter_factory)(saved_session.vendor);
            self.sessions.insert(
                saved_session.id.clone(),
                GatewaySession {
                    id: saved_session.id,
                    owner: saved_session.owner.clone(),
                    project: saved_session.project,
                    role: saved_session.role,
                    vendor: saved_session.vendor,
                    handle: saved_session.handle,
                    thread: saved_session.thread,
                    adapter,
                    visible_events: Arc::new(AtomicU64::new(0)),
                    reply_to: Arc::new(std::sync::Mutex::new(saved_session.owner)),
                },
            );
        }
        // Drop current-session routes that pointed at a dropped duplicate; the
        // next message re-resolves to the kept record via start_session's dedup.
        let live: std::collections::HashSet<String> = self.sessions.keys().cloned().collect();
        self.current_session.retain(|_, sid| live.contains(sid));
        Ok(())
    }

    fn persist_state(&self) -> Result<()> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let saved = SavedGatewayState {
            default_project: self.default_project.clone(),
            current_project: self
                .current_project
                .iter()
                .map(|(chat, value)| SavedGatewayRoute {
                    chat: chat.clone(),
                    value: value.clone(),
                })
                .collect(),
            current_session: self
                .current_session
                .iter()
                .map(|(chat, value)| SavedGatewayRoute {
                    chat: chat.clone(),
                    value: value.clone(),
                })
                .collect(),
            sessions: self
                .sessions
                .values()
                .map(|session| SavedGatewaySession {
                    id: session.id.clone(),
                    owner: session.owner.clone(),
                    project: session.project.clone(),
                    role: session.role.clone(),
                    vendor: session.vendor,
                    handle: session.handle.clone(),
                    thread: session.thread.clone(),
                })
                .collect(),
            next_session: self.next_session,
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    async fn submit_to_current(&self, chat: &ChatKey, payload: String) -> Result<Vec<String>> {
        let session_id = self
            .current_session
            .get(chat)
            .ok_or_else(|| anyhow!("no current session for chat"))?
            .clone();
        // (v0.8.5 D1) A single-line `/command` is a session directive — the
        // owning adapter (the only thing that knows its vendor's command
        // surface) interprets it. Multi-line text starting with `/` is
        // ordinary content (a pasted path / code block), never a directive.
        if let Some(directive) = parse_session_directive(&payload) {
            return self.dispatch_directive(chat, &session_id, directive).await;
        }
        let session = self
            .sessions
            .get(&session_id)
            .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
        // Option ① — replies for this turn go back to whoever sent it.
        if let Ok(mut target) = session.reply_to.lock() {
            *target = chat.clone();
        }
        let start_visible_events = session.visible_events.load(Ordering::SeqCst);
        let submit_wait = gateway_submit_timeout_duration();
        let turn_id = tokio::time::timeout(
            submit_wait,
            session
                .adapter
                .submit_turn(&session.thread, TurnInput::UserText(payload)),
        )
        .await
        .map_err(|_| anyhow!("submit timed out after {submit_wait:?} for {session_id}"))??;
        self.after_turn_submitted(session, start_visible_events, &turn_id.0)
            .await
    }

    /// Interpret a session directive through the owning adapter, then render
    /// the [`DirectiveOutcome`] back into outbound replies (v0.8.5 D1).
    async fn dispatch_directive(
        &self,
        chat: &ChatKey,
        session_id: &str,
        directive: Directive,
    ) -> Result<Vec<String>> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
        if let Ok(mut target) = session.reply_to.lock() {
            *target = chat.clone();
        }
        let start_visible_events = session.visible_events.load(Ordering::SeqCst);
        let submit_wait = gateway_submit_timeout_duration();
        // Keep a copy so a `NeedsChoice` can re-enter with the same directive
        // once the user picks.
        let original = directive.clone();
        let outcome = tokio::time::timeout(
            submit_wait,
            session.adapter.handle_directive(&session.thread, directive),
        )
        .await
        .map_err(|_| anyhow!("directive timed out after {submit_wait:?} for {session_id}"))??;
        match outcome {
            DirectiveOutcome::Turn(turn_id) => {
                self.after_turn_submitted(session, start_visible_events, &turn_id.0)
                    .await
            }
            DirectiveOutcome::Done { receipt } => Ok(vec![receipt]),
            DirectiveOutcome::Rejected { reason } => Ok(vec![reason]),
            DirectiveOutcome::Redirect { hint } => Ok(vec![hint]),
            DirectiveOutcome::NeedsChoice(prompt) => {
                self.offer_choice(chat, session_id, prompt, original).await
            }
        }
    }

    /// Wire a freshly-submitted turn into the async pump (production) or
    /// drain its first answer synchronously (sink-less unit tests). Shared
    /// by the plain-text submit path and the directive `Turn` outcome.
    async fn after_turn_submitted(
        &self,
        session: &GatewaySession,
        start_visible_events: u64,
        turn_id: &str,
    ) -> Result<Vec<String>> {
        if let Some(tx) = self.event_sink.clone() {
            spawn_turn_timeout_watchdog(tx, session, start_visible_events, turn_id);
            Ok(Vec::new())
        } else {
            let mut replies = Vec::new();
            let mut events = session.adapter.events(&session.thread);
            let wait = gateway_reply_wait_duration();
            while let Ok(Some(evt)) = tokio::time::timeout(wait, events.next()).await {
                if let Some(text) = event_text(&evt) {
                    replies.push(text);
                    break;
                }
            }
            if replies.is_empty() {
                replies.push(format!("submitted turn {turn_id}"));
            }
            Ok(replies)
        }
    }

    /// Register an adapter `NeedsChoice` as the session's pending interaction
    /// and deliver the prompt: inline buttons via the async sink, or a
    /// numbered-text fallback on the sink-less path (v0.8.5 D3/D4).
    async fn offer_choice(
        &self,
        chat: &ChatKey,
        session_id: &str,
        prompt: ChoicePrompt,
        original: Directive,
    ) -> Result<Vec<String>> {
        let key = pending_key(chat, session_id);
        let expires_at = Instant::now() + gateway_pending_ttl();
        // Single-flight: a new prompt for the same (chat, session) evicts the
        // old one. (W2 resolves an evicted External origin with
        // deny-with-reason; a Directive origin is simply dropped.)
        let _evicted = self.pending.lock().await.register(
            key,
            prompt.clone(),
            InteractionOrigin::Directive {
                session_id: session_id.to_string(),
                directive: original,
            },
            expires_at,
        );
        let body = render_choice_text(&prompt);
        if let Some(tx) = self.event_sink.clone() {
            let _ = tx.send(GatewayEvent {
                id: format!("gateway-choice-{session_id}-{}", prompt.token),
                channel: chat.channel.clone(),
                chat_id: chat.chat_id.clone(),
                thread_ts: None,
                content: body,
                kind: GatewayEventKind::Answer,
                attachments: Vec::new(),
                options: to_message_options(&prompt),
                sid: Some(session_id.to_string()),
            });
            Ok(Vec::new())
        } else {
            // Sink-less (unit test) path: deliver the numbered-text form; the
            // pending registry still holds the prompt for selection resolve.
            Ok(vec![body])
        }
    }

    /// Resolve an inbound option click (`"{token}:{idx}"`) by TOKEN, scanning
    /// the whole registry (v0.8.5 D3 + D6). Token-global so the one path
    /// resolves both origins: a Directive prompt (registered under
    /// `pending_key(chat, session)`) and a D6 External prompt (registered by
    /// the mcp.sock handler under the token itself, with no gateway session).
    /// The callback `data` (`"{token}:{idx}"`) carries the token + positional
    /// index; the real option id is reverse-resolved from the taken prompt and
    /// never leaves the gateway.
    async fn resolve_selection(&self, chat: &ChatKey, data: &str) -> Result<Vec<String>> {
        let Some((token, idx)) = split_callback(data) else {
            return Ok(vec!["invalid selection".to_string()]);
        };
        let taken = {
            let mut pend = self.pending.lock().await;
            // (v0.8.5 S2) Lapse any prompt past its TTL before resolving, so a
            // late click on an expired choice reads as absent ("expired")
            // instead of re-entering dispatch. Dropping a lapsed External
            // entry's oneshot also unblocks its waiting hook (deny) — matching
            // that path's own tokio timeout.
            pend.drain_expired(Instant::now());
            pend.take_by_token(&token)
        };
        let Some(p) = taken else {
            return Ok(vec!["this choice has expired".to_string()]);
        };
        let Some(opt) = p.prompt.options.get(idx) else {
            return Ok(vec!["invalid choice".to_string()]);
        };
        let selection = ChoiceSelection {
            token,
            ids: vec![opt.id.clone()],
            free_text: None,
        };
        self.apply_pending(chat, p, selection).await
    }

    /// Resolve a numeric short-reply (1-based) against the current session's
    /// pending choice (v0.8.5 D3).
    async fn resolve_numeric(&self, chat: &ChatKey, n: usize) -> Result<Vec<String>> {
        let Some(session_id) = self.current_session.get(chat).cloned() else {
            return Ok(vec!["no choice to answer".to_string()]);
        };
        let key = pending_key(chat, &session_id);
        let mut pend = self.pending.lock().await;
        // (v0.8.5 S2) Drop lapsed prompts first: a number typed after the TTL
        // is ordinary text for the agent, not a stale choice re-entry.
        pend.drain_expired(Instant::now());
        let (token, id) = {
            let Some(prompt) = pend.prompt_for(&key) else {
                return Ok(vec!["no choice to answer".to_string()]);
            };
            let Some(idx) = n.checked_sub(1) else {
                return Ok(vec!["invalid choice".to_string()]);
            };
            match prompt.options.get(idx) {
                Some(opt) => (prompt.token.clone(), opt.id.clone()),
                None => return Ok(vec![format!("please reply 1–{}", prompt.options.len())]),
            }
        };
        let p = pend.take(&key).expect("pending present under lock");
        drop(pend);
        let selection = ChoiceSelection {
            token,
            ids: vec![id],
            free_text: None,
        };
        self.apply_pending(chat, p, selection).await
    }

    /// Dispatch a resolved selection per the pending interaction's origin
    /// (v0.8.5). Directive origin re-enters `handle_directive` with the
    /// choice; External origin (D6, W2) delivers over the oneshot.
    async fn apply_pending(
        &self,
        chat: &ChatKey,
        pending: crate::pending::PendingInteraction,
        selection: ChoiceSelection,
    ) -> Result<Vec<String>> {
        match pending.origin {
            InteractionOrigin::Directive {
                session_id,
                mut directive,
            } => {
                directive.choice = Some(selection);
                self.dispatch_directive(chat, &session_id, directive).await
            }
            InteractionOrigin::External { reply } => {
                let _ = reply.send(selection);
                Ok(Vec::new())
            }
        }
    }

    /// True when the current session has an outstanding pending choice.
    async fn has_pending_for_current(&self, chat: &ChatKey) -> bool {
        let Some(session_id) = self.current_session.get(chat) else {
            return false;
        };
        let key = pending_key(chat, session_id);
        let mut pend = self.pending.lock().await;
        // (v0.8.5 S2) A lapsed prompt no longer counts as pending, so a bare
        // number after the TTL falls through to ordinary agent text.
        pend.drain_expired(Instant::now());
        pend.has(&key)
    }

    fn current_project_for(&self, chat: &ChatKey) -> String {
        self.current_project
            .get(chat)
            .cloned()
            .unwrap_or_else(|| self.default_project.clone())
    }

    /// Point the chat's active session at an existing session owned by this
    /// chat in `project` (deterministic: smallest session index), returning its
    /// id. When none exists, clear the active session so the next message spawns
    /// one on demand in `project`. Backs `/cd` so the project switch is real.
    fn adopt_session_in_project(&mut self, chat: &ChatKey, project: &str) -> Option<String> {
        let adopted = self
            .sessions
            .values()
            .filter(|s| s.owner == *chat && s.project == project)
            .min_by_key(|s| session_index(&s.id))
            .map(|s| s.id.clone());
        match &adopted {
            Some(id) => {
                self.current_session.insert(chat.clone(), id.clone());
            }
            None => {
                self.current_session.remove(chat);
            }
        }
        adopted
    }

    fn session_by_handle(&self, chat: &ChatKey, handle: &str) -> Option<String> {
        self.sessions
            .values()
            .find(|s| s.owner == *chat && s.handle == handle)
            .map(|s| s.id.clone())
    }

    fn template_by_handle(&self, chat: &ChatKey, handle: &str) -> Option<GatewayRouteTemplate> {
        self.templates
            .iter()
            .find(|t| t.channel == chat.channel && t.chat_id == chat.chat_id && t.handle == handle)
            .cloned()
    }

    fn templates_for_chat(&self, chat: &ChatKey) -> Vec<GatewayRouteTemplate> {
        self.templates
            .iter()
            .filter(|t| t.channel == chat.channel && t.chat_id == chat.chat_id)
            .cloned()
            .collect()
    }

    async fn render_sessions(&self, chat: &ChatKey) -> String {
        // The web console is a global operator view and lists every chat
        // session (cross-entry sharing); IM channels stay scoped to their own.
        let global = chat.channel == "web";
        let visible: Vec<&GatewaySession> = self
            .sessions
            .values()
            .filter(|s| global || s.owner == *chat)
            .collect();
        if visible.is_empty() {
            return "no sessions".to_string();
        }
        let mut rows: Vec<String> = Vec::with_capacity(visible.len());
        for s in visible {
            // P3 — append model + ctx from the owning adapter's
            // `thread_status`. Statusless adapters (bg / default) report
            // `ThreadStatus::default()` → `status_suffix() == None` → the
            // legacy `id:project:vendor:role` row is unchanged. Per-session
            // failures degrade to the bare row (never break the listing).
            let base = format!("{}:{}:{:?}:{}", s.id, s.project, s.vendor, s.role);
            let suffix = match s.adapter.thread_status(&s.thread).await {
                Ok(status) => status.status_suffix(),
                Err(_) => None,
            };
            match suffix {
                Some(sfx) => rows.push(format!("{base} — {sfx}")),
                None => rows.push(base),
            }
        }
        rows.join("\n")
    }

    fn render_projects(&self) -> String {
        self.projects.keys().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Reconcile live `ccteam-chat-*` process names against tracked sessions.
    ///
    /// A live name equal to some tracked session's canonical name
    /// ([`chat_session_name`]) is `tracked`; the rest are `orphans` — processes
    /// that outlived a prior daemon and were never recorded by this one.
    /// Matching is by *computed* canonical name (not by parsing the live name),
    /// so dash-containing slugs are unambiguous; parsing is only used to
    /// describe orphans for display.
    pub fn reconcile_chat_sessions(&self, live_chat_names: &[String]) -> SessionInventory {
        let tracked_names: std::collections::BTreeSet<String> = self
            .sessions
            .values()
            .map(|s| chat_session_name(&s.project, &s.role))
            .collect();
        // Bare-path call binds to the free function below, not this method.
        reconcile_chat_sessions(&tracked_names, live_chat_names)
    }

    /// Enumerate live chat sessions from `backend` and reconcile them against
    /// tracked sessions. Production entry for daemon startup / a global session
    /// view. Read-only — never kills (the "never auto-kill a long session"
    /// redline; reclaim stays an explicit, opt-in action).
    pub async fn inventory_via_backend(
        &self,
        backend: &dyn ProcessBackend,
    ) -> Result<SessionInventory> {
        let live = ccteam_harness::list_chat_sessions(backend).await?;
        Ok(self.reconcile_chat_sessions(&live))
    }

    /// Render a global session inventory for an operator: every tracked session
    /// (`id:project:vendor:role`) plus any orphaned `ccteam-chat-*` processes,
    /// each flagged for explicit reclaim. Global (not per-chat): orphan names
    /// don't carry an owning chat, so this is intentionally not part of the
    /// per-chat `/sessions` view.
    pub fn render_all_sessions(&self, live_chat_names: &[String]) -> String {
        let inventory = self.reconcile_chat_sessions(live_chat_names);
        let mut lines: Vec<String> = self
            .sessions
            .values()
            .map(|s| format!("{}:{}:{:?}:{}", s.id, s.project, s.vendor, s.role))
            .collect();
        lines.sort();
        for orphan in &inventory.orphans {
            lines.push(format!(
                "orphan {} (slug={} role={}) — untracked, reclaim explicitly",
                orphan.name, orphan.slug, orphan.role
            ));
        }
        if lines.is_empty() {
            "no sessions".to_string()
        } else {
            lines.join("\n")
        }
    }

    // =================================================================
    // V0.8.6 W5b — resource-API spine. Public, web-facing accessors that
    // compose the existing private internals so `ccteam-web` can drive
    // sessions over HTTP without reaching into the gateway's private maps.
    // =================================================================

    /// Snapshot every tracked session as a [`SessionView`] (W5b). Holds the
    /// gateway only long enough to clone scalar fields — no `.await` runs
    /// under any lock — so an SSE/list handler can call this cheaply. A
    /// session is `current` when it is the active session for at least one
    /// routed chat. Ordered by `s{n}` index for stable rendering.
    pub fn session_views(&self) -> Vec<SessionView> {
        let current: std::collections::HashSet<&String> = self.current_session.values().collect();
        let mut views: Vec<SessionView> = self
            .sessions
            .values()
            .map(|s| SessionView {
                sid: s.id.clone(),
                project: s.project.clone(),
                role: s.role.clone(),
                vendor: vendor_str(s.vendor).to_string(),
                current: current.contains(&s.id),
                status: "live".to_string(),
            })
            .collect();
        views.sort_by_key(|v| session_index(&v.sid));
        views
    }

    /// Resolve a session id to the data a collector needs to tail its
    /// transcript (v0.8.7 W1 — cto `session_collect`). Returns the role
    /// (the `<bot>` segment of `.ccteam/chat/<bot>/turns.jsonl`) and the
    /// session's project slug + absolute working dir, or `None` for an
    /// unknown sid. Read-only: clones scalar fields under no `.await`, so a
    /// collect handler can call it cheaply while holding the gateway lock.
    pub fn session_resolve(&self, sid: &str) -> Option<SessionResolve> {
        let session = self.sessions.get(sid)?;
        let project_dir = self.projects.get(&session.project).cloned()?;
        Some(SessionResolve {
            sid: session.id.clone(),
            role: session.role.clone(),
            project: session.project.clone(),
            project_dir,
        })
    }

    /// Create a session from the network API (W5b). Thin wrapper over
    /// [`start_session`](Self::start_session): the caller supplies the
    /// project + role + vendor; the handle defaults to the role name (the
    /// established convention from `/new`). Returns the new `s{n}` id. The
    /// `owner` is a synthetic `web` chat key so replies route to the web
    /// console; an SSE handler then filters the outbound stream by `sid`.
    /// Reuses an existing (project, role) pane if one is already tracked
    /// (same dedup as `/new`), so a duplicate API call is idempotent.
    pub async fn create_session_api(
        &mut self,
        project: String,
        role: String,
        vendor: AgentVendor,
    ) -> Result<String> {
        let owner = web_api_chat();
        let handle = role.clone();
        self.start_session(owner, project, vendor, role, handle)
            .await
    }

    /// Submit a user-text turn to a session addressed by `sid` (W5b).
    /// Looks the session up by id (not by current-chat routing), points its
    /// `reply_to` at the web console so the async answer/progress events
    /// route back to a web SSE subscriber, then submits via the owning
    /// adapter. The lock is held only across the (fast) `submit_turn`
    /// send-keys / RPC; the long turn streams asynchronously through the
    /// event pump. Returns the submitted [`TurnId`]'s inner string.
    pub async fn submit_to_sid(&mut self, sid: &str, text: String) -> Result<String> {
        let session = self
            .sessions
            .get(sid)
            .ok_or_else(|| anyhow!("unknown session: {sid}"))?;
        // Route this turn's async replies to the web console (mirrors the
        // per-turn `reply_to` retarget the inbound submit path does).
        if let Ok(mut target) = session.reply_to.lock() {
            *target = web_api_chat();
        }
        let start_visible_events = session.visible_events.load(Ordering::SeqCst);
        let submit_wait = gateway_submit_timeout_duration();
        let turn_id = tokio::time::timeout(
            submit_wait,
            session
                .adapter
                .submit_turn(&session.thread, TurnInput::UserText(text)),
        )
        .await
        .map_err(|_| anyhow!("submit timed out after {submit_wait:?} for {sid}"))??;
        // Arm the same async-turn machinery the inbound path uses (turn
        // watchdog when a sink is wired; otherwise this is a no-op drain).
        let _ = self
            .after_turn_submitted(session, start_visible_events, &turn_id.0)
            .await?;
        Ok(turn_id.0)
    }

    /// Stop the session addressed by `sid` (W5b). Mirrors the
    /// [`switch_current_role`](Self::switch_current_role) teardown: abort
    /// the event pump, `close_thread` on the owning adapter, drop the
    /// session record, clear any `current_session` route that pointed at
    /// it, then persist. Idempotent-ish: an unknown `sid` is an error so
    /// the API can surface a 404, but a missing tmux pane inside
    /// `close_thread` is tolerated (adapter close is idempotent). Never
    /// file-purges — deregister-only, per the locked W5b decision.
    pub async fn stop_session(&mut self, sid: &str) -> Result<()> {
        let session = self
            .sessions
            .get(sid)
            .ok_or_else(|| anyhow!("unknown session: {sid}"))?;
        let thread = session.thread.clone();
        let adapter = Arc::clone(&session.adapter);
        // Abort the pump before closing so no stale pump keeps draining the
        // retired transcript (mirrors switch_current_role).
        if let Some(pump) = self.event_pumps.remove(sid) {
            pump.abort();
        }
        let _ = adapter.close_thread(&thread).await;
        self.sessions.remove(sid);
        // Drop every current-session route that pointed at this sid so a
        // chat doesn't keep addressing a dead session.
        self.current_session.retain(|_, v| v != sid);
        self.persist_state()?;
        Ok(())
    }
}

/// Stringify a vendor for the [`SessionView`] wire shape. Kept local so
/// the web layer never depends on the harness enum's serde rename.
fn vendor_str(v: AgentVendor) -> &'static str {
    match v {
        AgentVendor::Claude => "claude",
        AgentVendor::Codex => "codex",
    }
}

/// The synthetic chat key used by the network resource API (W5b) as the
/// `owner` / `reply_to` for sessions it creates or drives. `channel ==
/// "web"` so it matches the web console's existing cross-entry sharing
/// rules (e.g. global `/sessions`, `/use` any session), and the
/// per-`sid` SSE filter keys on `chat_id == sid` via [`pump_target`].
fn web_api_chat() -> ChatKey {
    ChatKey::new("web", "web-api", "web-api")
}

/// Reconcile live `ccteam-chat-*` process names against a set of *tracked*
/// canonical session names. A live name present in `tracked_names` is
/// `tracked`; any other (parseable) live name is an `orphan` — a process that
/// outlived the daemon that spawned it. Matching is by the *computed* canonical
/// name, so dash-containing slugs stay unambiguous; the live name is only
/// parsed to describe an orphan for display.
///
/// This is the daemon-independent core behind [`Gateway::reconcile_chat_sessions`].
/// The read-only `ccteam sessions` CLI view calls it directly, passing tracked
/// names loaded from the persisted registry via [`tracked_chat_session_names`].
pub fn reconcile_chat_sessions(
    tracked_names: &std::collections::BTreeSet<String>,
    live_chat_names: &[String],
) -> SessionInventory {
    let mut inventory = SessionInventory::default();
    for name in live_chat_names {
        if tracked_names.contains(name) {
            inventory.tracked.push(name.clone());
        } else if let Some((slug, role)) = parse_chat_session_name(name) {
            inventory.orphans.push(OrphanSession {
                name: name.clone(),
                slug,
                role,
            });
        }
    }
    inventory.tracked.sort();
    inventory.tracked.dedup();
    inventory.orphans.sort_by(|a, b| a.name.cmp(&b.name));
    inventory
}

/// Load the set of canonical chat-session names (`ccteam-chat-<slug>-<role>`)
/// the gateway has tracked, from its persisted route table at `state_path`
/// (see [`default_gateway_state_path`](crate::default_gateway_state_path)).
///
/// Returns an empty set when the file is absent — no daemon has persisted a
/// registry yet, so every live chat session is by definition an orphan. This
/// is the daemon-independent registry source the `ccteam sessions` CLI view
/// reconciles against; it is strictly read-only and never mutates the file.
pub fn tracked_chat_session_names(state_path: &Path) -> Result<std::collections::BTreeSet<String>> {
    if !state_path.exists() {
        return Ok(std::collections::BTreeSet::new());
    }
    let raw = std::fs::read_to_string(state_path)
        .with_context(|| format!("read gateway state {}", state_path.display()))?;
    let saved: SavedGatewayState = serde_json::from_str(&raw)
        .with_context(|| format!("parse gateway state {}", state_path.display()))?;
    Ok(saved
        .sessions
        .into_iter()
        .map(|s| chat_session_name(&s.project, &s.role))
        .collect())
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for (_, handle) in std::mem::take(&mut self.event_pumps) {
            handle.abort();
        }
    }
}

fn spawn_turn_timeout_watchdog(
    tx: GatewayEventSink,
    session: &GatewaySession,
    start_visible_events: u64,
    turn_id: &str,
) {
    let timeout = gateway_turn_timeout_duration();
    if timeout.is_zero() {
        return;
    }
    let visible_events = Arc::clone(&session.visible_events);
    let session_id = session.id.clone();
    let reply_to = Arc::clone(&session.reply_to);
    let owner = session.owner.clone();
    let turn_id = turn_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        if visible_events.load(Ordering::SeqCst) != start_visible_events {
            return;
        }
        let (channel, chat_id) = match reply_to.lock() {
            Ok(target) => (target.channel.clone(), target.chat_id.clone()),
            Err(_) => (owner.channel.clone(), owner.chat_id.clone()),
        };
        let _ = tx.send(GatewayEvent {
            id: format!("gateway-timeout-{session_id}-{turn_id}"),
            channel,
            chat_id,
            thread_ts: None,
            content: format!(
                "gateway error: turn timed out after {timeout:?} for {session_id} turn {turn_id}"
            ),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(session_id.clone()),
        });
    });
}

/// Build the turn text for an inbound message (V0.8.4 P2a). With no
/// attachments it is the payload unchanged (today's behavior). With
/// attachments, wrap it in a `<channel …>` provenance tag whose
/// `image_path` / `file_path` attribute names each staged file, so the
/// agent `Read`s it. The Read convention itself is taught by the daemon's
/// MCP server instructions (a bare `claude` won't auto-Read otherwise).
fn wrap_inbound(
    channel: &str,
    chat_id: &str,
    user_id: &str,
    message_id: &str,
    payload: &str,
    attachments: &[ChannelAttachment],
) -> String {
    if attachments.is_empty() {
        return payload.to_string();
    }
    let mut attrs = format!(
        "source=\"{channel}\" chat_id=\"{chat_id}\" user=\"{user_id}\" message_id=\"{message_id}\""
    );
    let mut extra_lines = Vec::new();
    for (i, att) in attachments.iter().enumerate() {
        let key = match att.kind {
            AttachmentKind::Image => "image_path",
            AttachmentKind::File => "file_path",
        };
        if i == 0 {
            attrs.push_str(&format!(" {key}=\"{}\"", att.local_path));
        } else {
            extra_lines.push(format!("[attachment {key}=\"{}\"]", att.local_path));
        }
    }
    let body = if extra_lines.is_empty() {
        payload.to_string()
    } else {
        format!("{payload}\n{}", extra_lines.join("\n"))
    };
    format!("<channel {attrs}>\n{body}\n</channel>")
}

/// Resolve a session pump's live reply target `(channel, chat_id)`,
/// honoring a `/cd`-updated `reply_to` and falling back to the owner.
fn pump_target(session: &GatewaySession) -> (String, String) {
    match session.reply_to.lock() {
        Ok(target) => (target.channel.clone(), target.chat_id.clone()),
        Err(_) => (session.owner.channel.clone(), session.owner.chat_id.clone()),
    }
}

/// Send one `Progress` gateway event with the given rendered `content`.
/// Returns `false` only if the sink is closed (pump should stop). Sync
/// (unbounded send), so it never holds a lock across an await.
fn emit_progress(
    tx: &GatewayEventSink,
    session: &GatewaySession,
    session_id: &str,
    epoch: u64,
    content: &str,
    done: bool,
) -> bool {
    let (channel, chat_id) = pump_target(session);
    let status_key = format!("{session_id}-{epoch}");
    // `send` returns false only when the mpsc consumer is gone; surface that as
    // emit_progress's "sink closed → pump should stop" signal.
    tx.send(GatewayEvent {
        id: format!("gateway-progress-{status_key}"),
        channel,
        chat_id,
        thread_ts: None,
        content: content.to_string(),
        sid: Some(session_id.to_string()),
        kind: GatewayEventKind::Progress { status_key, done },
        attachments: Vec::new(),
        options: Vec::new(),
    })
}

/// Render the fold and flush it as a progress update, **skipping** a
/// redundant edit whose text is unchanged (avoids Telegram's "message is
/// not modified" 400). Updates the pump's throttle bookkeeping
/// (`last_sent` / `last_emit` / `dirty`). Returns `false` if the sink
/// closed.
#[allow(clippy::too_many_arguments)]
fn flush_progress(
    tx: &GatewayEventSink,
    session: &GatewaySession,
    session_id: &str,
    epoch: u64,
    fold: &crate::progress::ProgressFold,
    last_sent: &mut Option<String>,
    last_emit: &mut Option<std::time::Instant>,
    dirty: &mut bool,
) -> bool {
    *last_emit = Some(std::time::Instant::now());
    *dirty = false;
    let content = fold.render();
    if !fold.done() && last_sent.as_deref() == Some(content.as_str()) {
        return true; // no visible change → don't spend an edit
    }
    let ok = emit_progress(tx, session, session_id, epoch, &content, fold.done());
    if ok {
        *last_sent = Some(content);
    }
    ok
}

/// Whether IM progress status messages are enabled (default on). Set
/// `CCTEAM_IM_PROGRESS=off` to fall back to answers-only.
fn progress_enabled() -> bool {
    !matches!(
        std::env::var("CCTEAM_IM_PROGRESS").ok().as_deref(),
        Some("off") | Some("0") | Some("false")
    )
}

/// Minimum interval between status-message edits (default 1500ms — TG
/// soft-limits edits to ~1/s). `CCTEAM_IM_PROGRESS_THROTTLE_MS=0` makes
/// every step emit, for deterministic tests that don't rely on sleeps.
fn progress_throttle() -> std::time::Duration {
    let ms = std::env::var("CCTEAM_IM_PROGRESS_THROTTLE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1500);
    std::time::Duration::from_millis(ms)
}

fn async_event_text(evt: &ThreadEvent) -> Option<String> {
    match evt {
        // Codex streams the agent message as `ItemUpdated` deltas *and* a
        // final `ItemCompleted` carrying the full text (verified against a
        // live `codex app-server`: a delta "391" followed by a completed
        // "391"). Forward ONLY the final, else every codex chat turn is
        // doubled — a fragment per delta plus a duplicate final message.
        // Claude emits its reply solely as `ItemCompleted{AgentMessage}`
        // (its `ItemUpdated` carries `Reasoning`), so dropping the delta
        // arm is a no-op for Claude.
        ThreadEvent::ItemCompleted { item } => match &item.details {
            ThreadItemDetails::AgentMessage(text) if !text.is_empty() => Some(text.clone()),
            _ => None,
        },
        ThreadEvent::TurnFailed { err, .. } | ThreadEvent::Error(err) => Some(err.message.clone()),
        ThreadEvent::ItemUpdated { .. }
        | ThreadEvent::ThreadStarted { .. }
        | ThreadEvent::TurnStarted { .. }
        | ThreadEvent::TurnCompleted { .. }
        | ThreadEvent::ItemStarted { .. } => None,
    }
}

fn gateway_reply_wait_duration() -> std::time::Duration {
    const DEFAULT_MS: u64 = 5;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_REPLY_WAIT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn gateway_submit_timeout_duration() -> std::time::Duration {
    const DEFAULT_MS: u64 = 5_000;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn gateway_turn_timeout_duration() -> std::time::Duration {
    const DEFAULT_MS: u64 = 120_000;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn is_real_claude_tui_handle(thread: &ThreadHandle) -> bool {
    thread
        .raw_extras
        .get("tmux_session")
        .and_then(|v| v.as_str())
        .is_some()
        && thread
            .raw_extras
            .get("cwd")
            .and_then(|v| v.as_str())
            .is_some()
        && thread
            .raw_extras
            .get("project_dir")
            .and_then(|v| v.as_str())
            .is_some()
}

fn merge_thread_extras(
    persisted: serde_json::Value,
    resumed: serde_json::Value,
) -> serde_json::Value {
    let mut merged = persisted.as_object().cloned().unwrap_or_default();
    if let Some(resumed) = resumed.as_object() {
        for (key, value) in resumed {
            merged.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(merged)
}

/// Parse a single-line `/command [args]` into a neutral [`Directive`].
/// Returns `None` for non-slash text and for multi-line input (a pasted
/// path / code block that merely starts with `/` is ordinary content, not
/// a command — v0.8.5 §2.1).
fn parse_session_directive(payload: &str) -> Option<Directive> {
    let trimmed = payload.trim();
    if trimmed.contains('\n') {
        return None;
    }
    let rest = trimmed.strip_prefix('/')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    let args = it.next().unwrap_or("").trim().to_string();
    Some(Directive {
        name,
        args,
        choice: None,
    })
}

/// Compose the pending-interaction key for a (chat, session) pair. Built
/// here (not in `pending`) so that module stays decoupled from `ChatKey`.
fn pending_key(chat: &ChatKey, session_id: &str) -> String {
    format!(
        "{}\u{1}{}\u{1}{}\u{1}{}",
        chat.channel, chat.chat_id, chat.user_id, session_id
    )
}

/// Map a harness [`ChoicePrompt`] to channel-local [`MessageOption`]s. The
/// callback payload is `"{token}:{idx}"` — short, opaque, within Telegram's
/// 64-byte `callback_data` cap; the real option id never leaves the gateway
/// (idx is reverse-resolved from the pending registry).
fn to_message_options(prompt: &ChoicePrompt) -> Vec<MessageOption> {
    prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{}:{}", prompt.token, i),
            label: opt.label.clone(),
        })
        .collect()
}

/// Numbered-text rendering of a choice prompt — the universal fallback for
/// channels without buttons, and the sink-less unit-test form.
fn render_choice_text(prompt: &ChoicePrompt) -> String {
    let mut s = prompt.title.clone();
    for (i, opt) in prompt.options.iter().enumerate() {
        s.push_str(&format!("\n{}) {}", i + 1, opt.label));
    }
    s
}

/// Render the `/help` body from [`GATEWAY_COMMANDS`].
fn render_help() -> String {
    let mut s = String::from("Gateway commands:");
    for c in GATEWAY_COMMANDS {
        match c.arg_hint {
            Some(hint) => s.push_str(&format!("\n{} {} — {}", c.name, hint, c.help)),
            None => s.push_str(&format!("\n{} — {}", c.name, c.help)),
        }
    }
    s.push_str("\n\nAny other /command is forwarded to the current session's agent.");
    s
}

/// Split an inbound callback payload `"{token}:{idx}"` (v0.8.5 D3).
fn split_callback(data: &str) -> Option<(String, usize)> {
    let (token, idx) = data.split_once(':')?;
    let idx: usize = idx.parse().ok()?;
    Some((token.to_string(), idx))
}

/// A bare positive integer is a candidate numeric short-reply to a pending
/// choice (resolved only when one is actually outstanding).
fn numeric_choice(text: &str) -> Option<usize> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<usize>().ok().filter(|n| *n >= 1)
}

/// TTL for a pending choice prompt (v0.8.5). Shares its default with the D6
/// hook timeout (10 min); env-overridable for tests.
fn gateway_pending_ttl() -> std::time::Duration {
    const DEFAULT_MS: u64 = 600_000;
    let ms = std::env::var("CCTEAM_IM_PENDING_TTL_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn parse_vendor(raw: &str) -> Result<AgentVendor> {
    match raw {
        "claude" => Ok(AgentVendor::Claude),
        "codex" => Ok(AgentVendor::Codex),
        other => Err(anyhow!("unknown vendor: {other}")),
    }
}

/// Resolve a chat-supplied project path: expand a leading `~`, then
/// require the result to be absolute (the daemon's cwd is not a
/// meaningful base for a path typed into a chat / web form).
fn expand_project_path(raw: &str) -> Result<PathBuf> {
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("cannot resolve home directory for ~"))?
            .join(rest)
    } else if raw == "~" {
        dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve home directory for ~"))?
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_absolute() {
        return Err(anyhow!("项目路径必须是绝对路径(或 ~ 开头): {raw}"));
    }
    Ok(expanded)
}

/// Numeric ordering key for a `s{n}` session id; unparseable ids sort last so
/// session adoption stays deterministic.
fn session_index(id: &str) -> u64 {
    id.strip_prefix('s')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

fn event_text(evt: &ThreadEvent) -> Option<String> {
    match evt {
        // Same codex delta-vs-final dedup as `async_event_text`: only the
        // final `ItemCompleted` agent message is a reply. The sync reply
        // path breaks on the first hit, so forwarding a delta here would
        // return a partial fragment instead of the full text.
        ThreadEvent::ItemCompleted { item } => match &item.details {
            ThreadItemDetails::AgentMessage(text) if !text.is_empty() => Some(text.clone()),
            _ => None,
        },
        ThreadEvent::TurnCompleted { turn_id, .. } => Some(format!("turn completed {turn_id}")),
        ThreadEvent::TurnFailed { err, .. } | ThreadEvent::Error(err) => Some(err.message.clone()),
        ThreadEvent::ItemUpdated { .. }
        | ThreadEvent::ThreadStarted { .. }
        | ThreadEvent::TurnStarted { .. }
        | ThreadEvent::ItemStarted { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::{
        ChoiceOption, ContextUsage, ExecutionMode, HarnessError, ThreadItem, ThreadStatus, TurnId,
    };
    use futures::stream::BoxStream;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex as StdMutex, OnceLock};
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn agent_msg(kind: fn(ThreadItem) -> ThreadEvent, text: &str) -> ThreadEvent {
        kind(ThreadItem {
            id: "i1".into(),
            details: ThreadItemDetails::AgentMessage(text.into()),
        })
    }

    /// Seed a `.claude/agents/<role>.md` under `project_dir` so the `/role`
    /// existence check (`ccteam_core::read_role`) resolves it. Minimal frontmatter
    /// is enough; the gateway only checks the file exists, not its contents.
    fn seed_role(project_dir: &std::path::Path, role: &str) {
        let agents = project_dir.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join(format!("{role}.md")),
            format!("---\nname: {role}\n---\n{role} role.\n"),
        )
        .unwrap();
    }

    /// Regression: a real `codex app-server` turn emits the agent message
    /// as a streaming `ItemUpdated` delta AND a final `ItemCompleted`
    /// carrying the full text (verified live: delta "391" + completed
    /// "391"). The chat pump (`async_event_text`) must forward ONLY the
    /// final — else every codex reply is doubled. Claude's reply arrives
    /// solely as `ItemCompleted{AgentMessage}` (its `ItemUpdated` is
    /// `Reasoning`), so the streaming-delta drop is a no-op for Claude.
    #[test]
    fn async_event_text_forwards_final_not_codex_streaming_delta() {
        // The streaming delta must NOT become a chat message.
        assert_eq!(
            async_event_text(&agent_msg(|item| ThreadEvent::ItemUpdated { item }, "391")),
            None,
            "codex streaming ItemUpdated delta must be suppressed"
        );
        // The final completed message is the one reply.
        assert_eq!(
            async_event_text(&agent_msg(
                |item| ThreadEvent::ItemCompleted { item },
                "391"
            )),
            Some("391".to_string()),
        );
        // The empty placeholder `item/completed` codex emits first is not a
        // reply.
        assert_eq!(
            async_event_text(&agent_msg(|item| ThreadEvent::ItemCompleted { item }, "")),
            None,
        );
        // Claude's reasoning rides ItemUpdated too — also dropped (no chat
        // noise), confirming the fix doesn't leak thinking into chat.
        let reasoning = ThreadEvent::ItemUpdated {
            item: ThreadItem {
                id: "r".into(),
                details: ThreadItemDetails::Reasoning("thinking".into()),
            },
        };
        assert_eq!(async_event_text(&reasoning), None);
    }

    // ----- fix #2: GatewayEvent broadcast tee (per-session SSE source) -----

    fn fake_event(sid: Option<&str>) -> GatewayEvent {
        GatewayEvent {
            id: "e1".into(),
            channel: "web".into(),
            chat_id: "web-api".into(),
            thread_ts: None,
            content: "hi".into(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: sid.map(str::to_string),
        }
    }

    /// The sink tees one event to BOTH the mpsc delivery path (IM/web) and the
    /// broadcast fan-out (per-session SSE) — neither leg is skipped.
    #[tokio::test]
    async fn gateway_event_sink_tees_to_mpsc_and_broadcast() {
        let (mtx, mut mrx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        let (btx, mut brx) = tokio::sync::broadcast::channel::<GatewayEvent>(8);
        let sink = GatewayEventSink {
            mpsc: mtx,
            broadcast: btx,
        };
        assert!(sink.send(fake_event(Some("s1"))), "mpsc live ⇒ send ok");
        assert_eq!(mrx.recv().await.unwrap().sid.as_deref(), Some("s1"));
        assert_eq!(brx.recv().await.unwrap().sid.as_deref(), Some("s1"));
    }

    /// A broadcast send with no live SSE subscriber must NOT stop delivery: the
    /// mpsc leg still carries the event and `send` stays true.
    #[tokio::test]
    async fn gateway_event_sink_send_ok_with_no_subscriber() {
        let (mtx, mut mrx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        let (btx, _) = tokio::sync::broadcast::channel::<GatewayEvent>(8);
        let sink = GatewayEventSink {
            mpsc: mtx,
            broadcast: btx,
        };
        assert!(sink.send(fake_event(None)), "no SSE subscriber ⇒ still ok");
        assert!(mrx.recv().await.is_some());
    }

    /// `subscribe_events` hands out a live receiver off the gateway's held
    /// broadcast, so an event published through the tee reaches an SSE
    /// subscriber. (The per-session SSE handler then filters by `sid`.)
    #[tokio::test]
    async fn subscribe_events_receives_emitted_event() {
        let adapter: Arc<dyn HarnessAdapter + Send + Sync> = Arc::new(FakeAdapter::default());
        let mut gw = Gateway::new(adapter, "demo", std::env::temp_dir());
        let mut sub = gw.subscribe_events();
        // Wire a delivery sink (production path); set_event_sink reuses the
        // gateway's held broadcast, so `sub` sees what the tee publishes.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(tx);
        let sink = gw.event_sink.clone().expect("sink wired");
        assert!(sink.send(fake_event(Some("s7"))));
        assert_eq!(sub.recv().await.unwrap().sid.as_deref(), Some("s7"));
    }

    // ----- P2a wrap_inbound (turn-text + attachment paths) ----------

    fn img(path: &str) -> ChannelAttachment {
        ChannelAttachment {
            kind: AttachmentKind::Image,
            file_name: "shot.png".into(),
            local_path: path.into(),
            mime: Some("image/png".into()),
            size: Some(10),
        }
    }

    #[test]
    fn wrap_inbound_no_attachments_is_unchanged() {
        // The text-only path must be byte-identical to today's behavior
        // so every existing handle_text test stays valid.
        assert_eq!(
            wrap_inbound("telegram", "c1", "alice", "tg-9", "hello", &[]),
            "hello"
        );
    }

    #[test]
    fn wrap_inbound_names_image_path_in_channel_tag() {
        let turn = wrap_inbound(
            "telegram",
            "c1",
            "alice",
            "tg-9",
            "这是报错",
            &[img("/abs/inbound/tg-9-shot.png")],
        );
        assert!(turn.contains("<channel "), "missing channel tag: {turn}");
        assert!(turn.contains("source=\"telegram\""));
        assert!(turn.contains("chat_id=\"c1\""));
        assert!(turn.contains("image_path=\"/abs/inbound/tg-9-shot.png\""));
        assert!(turn.contains("这是报错"));
        assert!(turn.trim_end().ends_with("</channel>"));
    }

    #[test]
    fn wrap_inbound_file_uses_file_path_and_lists_extras() {
        let atts = vec![
            img("/abs/a.png"),
            ChannelAttachment {
                kind: AttachmentKind::File,
                file_name: "log.txt".into(),
                local_path: "/abs/b.log".into(),
                mime: None,
                size: None,
            },
        ];
        let turn = wrap_inbound("telegram", "c1", "alice", "tg-9", "see these", &atts);
        // First attachment becomes a tag attribute, extras become body lines.
        assert!(turn.contains("image_path=\"/abs/a.png\""));
        assert!(turn.contains("[attachment file_path=\"/abs/b.log\"]"));
    }

    #[derive(Debug)]
    struct FakeAdapter {
        vendor: AgentVendor,
        starts: AtomicUsize,
        submissions: Arc<Mutex<Vec<(String, String)>>>,
        events: Arc<Mutex<VecDeque<(String, ThreadEvent)>>>,
        event_delay: std::time::Duration,
        /// Recorded `handle_directive` calls (thread id + directive) for
        /// routing + choice-reentry assertions (v0.8.5 D1).
        directives: Arc<Mutex<Vec<(String, Directive)>>>,
        /// Scripted outcomes popped in order by `handle_directive` (e.g. a
        /// `NeedsChoice`); empty ⇒ a `Done` echo.
        directive_script: Arc<Mutex<VecDeque<DirectiveOutcome>>>,
        /// Status returned by `thread_status` (v0.8.5 P3).
        status: Arc<Mutex<ThreadStatus>>,
    }

    impl Default for FakeAdapter {
        fn default() -> Self {
            Self::new(AgentVendor::Claude)
        }
    }

    impl FakeAdapter {
        fn new(vendor: AgentVendor) -> Self {
            Self {
                vendor,
                starts: AtomicUsize::new(0),
                submissions: Arc::new(Mutex::new(Vec::new())),
                events: Arc::new(Mutex::new(VecDeque::new())),
                event_delay: std::time::Duration::ZERO,
                directives: Arc::new(Mutex::new(Vec::new())),
                directive_script: Arc::new(Mutex::new(VecDeque::new())),
                status: Arc::new(Mutex::new(ThreadStatus::default())),
            }
        }

        /// Set the status this fake reports from `thread_status` (P3).
        async fn set_status(&self, status: ThreadStatus) {
            *self.status.lock().await = status;
        }

        fn new_with_event_delay(vendor: AgentVendor, event_delay: std::time::Duration) -> Self {
            Self {
                event_delay,
                ..Self::new(vendor)
            }
        }
    }

    #[async_trait::async_trait]
    impl HarnessAdapter for FakeAdapter {
        fn name(&self) -> &'static str {
            "fake-gateway"
        }

        fn vendor(&self) -> AgentVendor {
            self.vendor
        }

        async fn start_thread(
            &self,
            spec: &AgentSpecBrief,
            ctx: &SpawnCtx,
        ) -> Result<ThreadHandle, HarnessError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(ThreadHandle {
                vendor: self.vendor,
                mode: ExecutionMode::Chat,
                identity: format!("{}-{}-{}", ctx.slug, spec.role, ctx.sid),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({}),
            })
        }

        async fn submit_turn(
            &self,
            h: &ThreadHandle,
            input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            let text = match input {
                TurnInput::UserText(text) => text,
                _ => String::new(),
            };
            self.submissions
                .lock()
                .await
                .push((h.identity.clone(), text.clone()));
            self.events.lock().await.push_back((
                h.identity.clone(),
                ThreadEvent::ItemCompleted {
                    item: ThreadItem {
                        id: "msg-1".to_string(),
                        details: ThreadItemDetails::AgentMessage(format!(
                            "{} echo: {text}",
                            h.identity
                        )),
                    },
                },
            ));
            Ok(TurnId::new(format!("turn-{}", h.identity)))
        }

        fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
            let events = Arc::clone(&self.events);
            let wanted = h.identity.clone();
            let delay = self.event_delay;
            Box::pin(futures::stream::unfold((), move |_| {
                let events = Arc::clone(&events);
                let wanted = wanted.clone();
                let delay = delay;
                async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let mut guard = events.lock().await;
                    let idx = guard.iter().position(|(thread, _)| thread == &wanted)?;
                    let (_, evt) = guard.remove(idx)?;
                    Some((evt, ()))
                }
            }))
        }

        async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
            Err(HarnessError::NotImplemented {
                reason: "fake".to_string(),
            })
        }

        async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
            Ok(())
        }

        async fn handle_directive(
            &self,
            h: &ThreadHandle,
            d: Directive,
        ) -> Result<DirectiveOutcome, HarnessError> {
            self.directives
                .lock()
                .await
                .push((h.identity.clone(), d.clone()));
            if let Some(outcome) = self.directive_script.lock().await.pop_front() {
                return Ok(outcome);
            }
            Ok(DirectiveOutcome::Done {
                receipt: format!("{} directive: {}", h.identity, d.name),
            })
        }

        async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
            Ok(self.status.lock().await.clone())
        }
    }

    #[tokio::test]
    async fn gateway_plain_message_submits_to_current_session_and_echoes() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let created = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(created, vec!["created session s1"]);

        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "hi")
            .await
            .unwrap();
        assert_eq!(replies, vec!["alpha-reviewer-s1 echo: hi"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[("alpha-reviewer-s1".to_string(), "hi".to_string())]
        );
    }

    /// V0.8.6 W5b — the resource-API spine: create a session via
    /// `create_session_api`, see it in `session_views`, submit a turn by
    /// `sid` (the sink-less drain returns the fake echo), then `stop_session`
    /// removes it. Confirms the web layer can drive a session purely by id.
    #[tokio::test]
    async fn gateway_resource_api_create_view_submit_stop() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        // Create by API → s1, tracked, role/vendor/project as supplied.
        let sid = gateway
            .create_session_api("alpha".into(), "reviewer".into(), AgentVendor::Claude)
            .await
            .unwrap();
        assert_eq!(sid, "s1");

        let views = gateway.session_views();
        assert_eq!(views.len(), 1);
        let v = &views[0];
        assert_eq!(v.sid, "s1");
        assert_eq!(v.project, "alpha");
        assert_eq!(v.role, "reviewer");
        assert_eq!(v.vendor, "claude", "vendor is stringified for the wire");
        assert_eq!(v.status, "live");
        assert!(
            v.current,
            "the API session is current for its synthetic web chat"
        );

        // Submit by sid → sink-less drain returns the fake's echo; the
        // adapter recorded the UserText against the right thread.
        let turn = gateway.submit_to_sid("s1", "hello".into()).await.unwrap();
        assert!(turn.starts_with("turn-alpha-reviewer-s1"));
        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[("alpha-reviewer-s1".to_string(), "hello".to_string())]
        );

        // Submit to an unknown sid is an error (→ 404 at the API edge).
        assert!(gateway.submit_to_sid("s99", "x".into()).await.is_err());

        // Stop by sid → session gone, view list empty; idempotent-not (a
        // second stop is an error so the API can 404).
        gateway.stop_session("s1").await.unwrap();
        assert!(gateway.session_views().is_empty());
        assert!(gateway.stop_session("s1").await.is_err());
    }

    /// V0.8.6 W5b — `create_session_api` is idempotent on (project, role):
    /// a duplicate call reuses the existing pane / session id rather than
    /// spawning a second thread over the same transcript (same dedup `/new`
    /// relies on).
    #[tokio::test]
    async fn gateway_resource_api_create_dedups_pane() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        let a = gateway
            .create_session_api("alpha".into(), "reviewer".into(), AgentVendor::Claude)
            .await
            .unwrap();
        let b = gateway
            .create_session_api("alpha".into(), "reviewer".into(), AgentVendor::Claude)
            .await
            .unwrap();
        assert_eq!(a, b, "same (project, role) reuses the session id");
        assert_eq!(gateway.session_views().len(), 1);
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "the pane is started exactly once"
        );
    }

    /// v0.8.7 W1 — `session_resolve` is the collect-side accessor: it maps a
    /// gateway sid to the role + absolute project_dir a collector tails for
    /// `.ccteam/chat/<role>/turns.jsonl`. End-to-end with the real fake: spawn
    /// a session, submit a turn, then resolve the sid and read back the child's
    /// answer from a turns.jsonl mirror (the exact pipeline `session_collect`
    /// runs daemon-side). Unknown sid → None (→ tool error at the edge).
    #[tokio::test]
    async fn gateway_session_resolve_then_collect_child_turns() {
        use ccteam_harness::execution::turns_mirror::{append_turn, read_all_turns, TurnRecord};

        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // The default project "alpha" points at the sandbox dir so the
        // collect-side turns.jsonl write stays inside the tempdir.
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());

        // cto spawns a work-role session + dispatches a task (gateway-driven
        // half of session_spawn / session_dispatch).
        let sid = gateway
            .create_session_api("alpha".into(), "reviewer".into(), AgentVendor::Claude)
            .await
            .unwrap();
        assert_eq!(sid, "s1");
        let _turn = gateway
            .submit_to_sid(&sid, "review the diff".into())
            .await
            .unwrap();

        // session_collect resolves the sid → role + project_dir, then tails
        // the ccteam-owned mirror. Unknown sid is None.
        assert!(gateway.session_resolve("s99").is_none());
        let resolved = gateway.session_resolve(&sid).expect("known sid resolves");
        assert_eq!(resolved.sid, "s1");
        assert_eq!(resolved.role, "reviewer");
        assert_eq!(resolved.project, "alpha");
        assert_eq!(resolved.project_dir, project_dir);

        // Simulate the child's answer being mirrored to turns.jsonl (in
        // production the event pump + turns_mirror consumer write this; the
        // collect tool only READS it).
        append_turn(
            &resolved.project_dir,
            &resolved.role,
            &TurnRecord {
                turn_id: "t1".into(),
                ts: chrono::Utc::now(),
                vendor: "claude".into(),
                role: resolved.role.clone(),
                user: "review the diff".into(),
                assistant: "LGTM, two nits inline.".into(),
                usage: serde_json::Value::Null,
                tool_calls: vec![],
            },
        )
        .unwrap();

        let turns = read_all_turns(&resolved.project_dir, &resolved.role).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].assistant, "LGTM, two nits inline.");
        assert_eq!(turns[0].turn_id, "t1");
    }

    /// P3 — `/sessions` appends each session's model + ctx from
    /// `thread_status`. With a `[1m]` model the window is 1M; with no
    /// status reported the legacy `id:project:vendor:role` row is unchanged.
    #[tokio::test]
    async fn gateway_sessions_shows_model_and_context() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // Default status (all-None) → no suffix, legacy row verbatim.
        let bare = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(bare, vec!["s1:alpha:Claude:reviewer"]);

        // Now report a model + usage → suffix appears, rendered the same
        // way Codex /status renders (shared helper).
        fake.set_status(ThreadStatus {
            model: Some("claude-opus-4-8[1m]".into()),
            context: Some(ContextUsage {
                used_tokens: 188_000,
                window_tokens: 1_000_000,
            }),
        })
        .await;
        let with_status = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            with_status,
            vec!["s1:alpha:Claude:reviewer — claude-opus-4-8[1m] · ctx 188k / 1M (19%)"]
        );

        // A non-[1m] model renders against the 200k baseline.
        fake.set_status(ThreadStatus {
            model: Some("claude-sonnet-4-5".into()),
            context: Some(ContextUsage {
                used_tokens: 188_000,
                window_tokens: 200_000,
            }),
        })
        .await;
        let baseline = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            baseline,
            vec!["s1:alpha:Claude:reviewer — claude-sonnet-4-5 · ctx 188k / 200k (94%)"]
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn gateway_reply_wait_can_capture_realistic_delayed_event() {
        let _guard = env_lock();
        std::env::set_var("CCTEAM_IM_GATEWAY_REPLY_WAIT_MS", "100");
        let fake = Arc::new(FakeAdapter::new_with_event_delay(
            AgentVendor::Claude,
            std::time::Duration::from_millis(25),
        ));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "hi after delay")
            .await
            .unwrap();
        std::env::remove_var("CCTEAM_IM_GATEWAY_REPLY_WAIT_MS");

        assert_eq!(replies, vec!["alpha-reviewer-s1 echo: hi after delay"]);
    }

    #[tokio::test]
    async fn gateway_pair_starts_default_session() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let paired = gateway
            .handle_text("mock", "chat-1", "alice", "/pair 4821-77")
            .await
            .unwrap();
        assert_eq!(paired, vec!["paired 4821-77"]);

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "after pair")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-cto-s1 echo: after pair"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gateway_commands_switch_project_and_session() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        let projects = gateway
            .handle_text("mock", "chat-1", "alice", "/projects")
            .await
            .unwrap();
        assert_eq!(projects, vec!["alpha\nbeta"]);

        let cd = gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        assert_eq!(
            cd,
            vec!["project set to beta (next message starts a session there)"]
        );

        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let sessions = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["s1:beta:Codex:api\ns2:beta:Claude:reviewer"]);

        let use_first = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(use_first, vec!["using session s1"]);
        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "ping")
            .await
            .unwrap();
        assert_eq!(replies, vec!["beta-api-s1 echo: ping"]);
    }

    #[tokio::test]
    async fn gateway_routes_two_projects_and_sessions_matrix() {
        let claude = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let codex = Arc::new(FakeAdapter::new(AgentVendor::Codex));
        let factory = {
            let claude = Arc::clone(&claude);
            let codex = Arc::clone(&codex);
            Arc::new(move |vendor| -> Arc<dyn HarnessAdapter + Send + Sync> {
                match vendor {
                    AgentVendor::Claude => claude.clone(),
                    AgentVendor::Codex => codex.clone(),
                }
            })
        };
        let mut gateway = Gateway::new_with_factory(factory, "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex docs")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude qa")
            .await
            .unwrap();

        let sessions = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            sessions,
            vec![
                "s1:alpha:Claude:reviewer\ns2:alpha:Codex:docs\ns3:beta:Codex:api\ns4:beta:Claude:qa"
            ]
        );
        let projects = gateway
            .handle_text("mock", "chat-1", "alice", "/projects")
            .await
            .unwrap();
        assert_eq!(projects, vec!["alpha\nbeta"]);

        let alpha_reply = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer alpha ping")
            .await
            .unwrap();
        assert_eq!(alpha_reply, vec!["alpha-reviewer-s1 echo: alpha ping"]);
        let beta_reply = gateway
            .handle_text("mock", "chat-1", "alice", "@api beta ping")
            .await
            .unwrap();
        assert_eq!(beta_reply, vec!["beta-api-s3 echo: beta ping"]);

        // chat-2 uses a distinct role → its own pane/session (a same
        // (project, role) would dedup onto the shared pane).
        gateway
            .handle_text("mock", "chat-2", "bob", "/new claude security")
            .await
            .unwrap();
        let isolated = gateway
            .handle_text("mock", "chat-2", "bob", "same text")
            .await
            .unwrap();
        assert_eq!(isolated, vec!["alpha-security-s5 echo: same text"]);
    }

    #[tokio::test]
    async fn gateway_dual_vendor_sessions_route_directives_by_vendor() {
        let claude = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let codex = Arc::new(FakeAdapter::new(AgentVendor::Codex));
        let factory = {
            let claude = Arc::clone(&claude);
            let codex = Arc::clone(&codex);
            Arc::new(move |vendor| -> Arc<dyn HarnessAdapter + Send + Sync> {
                match vendor {
                    AgentVendor::Claude => claude.clone(),
                    AgentVendor::Codex => codex.clone(),
                }
            })
        };
        let mut gateway = Gateway::new_with_factory(factory, "alpha", "/tmp/alpha");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();

        // Directives route to the CURRENT session's adapter via
        // `handle_directive` (no more gateway vendor branch / SystemDirective).
        // The FakeAdapter echoes `<id> directive: <name>` as a `Done` receipt.
        let compact = gateway
            .handle_text("mock", "chat-1", "alice", "/compact")
            .await
            .unwrap();
        assert_eq!(compact, vec!["alpha-api-s2 directive: compact"]);
        let review = gateway
            .handle_text("mock", "chat-1", "alice", "/review")
            .await
            .unwrap();
        assert_eq!(review, vec!["alpha-api-s2 directive: review"]);
        // `@reviewer /clear` explicitly routes the directive to the claude
        // session. (The real Codex adapter would `Redirect` /clear; that is
        // asserted in the codex adapter tests, not against this fake.)
        let claude_clear = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer /clear")
            .await
            .unwrap();
        assert_eq!(claude_clear, vec!["alpha-reviewer-s1 directive: clear"]);

        // Each vendor's adapter saw only its own directives (by id + name).
        let codex_dirs: Vec<(String, String)> = codex
            .directives
            .lock()
            .await
            .iter()
            .map(|(id, d)| (id.clone(), d.name.clone()))
            .collect();
        assert_eq!(
            codex_dirs,
            vec![
                ("alpha-api-s2".to_string(), "compact".to_string()),
                ("alpha-api-s2".to_string(), "review".to_string()),
            ]
        );
        let claude_dirs: Vec<(String, String)> = claude
            .directives
            .lock()
            .await
            .iter()
            .map(|(id, d)| (id.clone(), d.name.clone()))
            .collect();
        assert_eq!(
            claude_dirs,
            vec![("alpha-reviewer-s1".to_string(), "clear".to_string())]
        );
    }

    /// Concept lock (arch-refactor §8 / T1): the gateway renders each
    /// non-choice `DirectiveOutcome` straight back as the reply text.
    #[tokio::test]
    async fn gateway_renders_each_directive_outcome() {
        for (outcome, expected) in [
            (
                DirectiveOutcome::Done {
                    receipt: "done!".into(),
                },
                "done!",
            ),
            (
                DirectiveOutcome::Rejected {
                    reason: "nope".into(),
                },
                "nope",
            ),
            (
                DirectiveOutcome::Redirect {
                    hint: "use /new".into(),
                },
                "use /new",
            ),
        ] {
            let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
            fake.directive_script.lock().await.push_back(outcome);
            let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap();
            let reply = gateway
                .handle_text("mock", "chat-1", "alice", "/anything")
                .await
                .unwrap();
            assert_eq!(reply, vec![expected.to_string()]);
        }
    }

    /// Concept lock (arch-refactor §8-4 + §8-5): a `NeedsChoice` registers a
    /// pending interaction + renders numbered options; a callback
    /// `"{token}:{idx}"` resolves to the real option id and re-enters
    /// `handle_directive` with the choice (the Telegram-callback inbound form).
    #[tokio::test]
    async fn gateway_needschoice_resolves_by_callback() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        fake.directive_script
            .lock()
            .await
            .push_back(DirectiveOutcome::NeedsChoice(ChoicePrompt {
                token: "t1".into(),
                title: "Pick a model".into(),
                options: vec![
                    ChoiceOption {
                        id: "opt-a".into(),
                        label: "Model A".into(),
                    },
                    ChoiceOption {
                        id: "opt-b".into(),
                        label: "Model B".into(),
                    },
                ],
                multi: false,
            }));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // Bare directive → NeedsChoice → numbered-text prompt (sink-less).
        let prompt = gateway
            .handle_text("mock", "chat-1", "alice", "/model")
            .await
            .unwrap();
        assert_eq!(prompt.len(), 1);
        assert!(prompt[0].contains("Pick a model"));
        assert!(prompt[0].contains("1) Model A"));
        assert!(prompt[0].contains("2) Model B"));

        // Telegram callback for option index 1 (`Model B`).
        gateway
            .handle_message(
                "mock",
                "chat-1",
                "alice",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: "t1:1".into(),
                }),
            )
            .await
            .unwrap();

        // The directive re-entered with the resolved real option id.
        let dirs = fake.directives.lock().await;
        assert_eq!(dirs.len(), 2, "first call + choice re-entry");
        assert_eq!(dirs[0].1.choice, None);
        assert_eq!(
            dirs[1].1.choice,
            Some(ChoiceSelection {
                token: "t1".into(),
                ids: vec!["opt-b".into()],
                free_text: None,
            })
        );
    }

    /// A numeric short-reply resolves the same pending choice (1-based).
    #[tokio::test]
    async fn gateway_needschoice_resolves_by_numeric() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        fake.directive_script
            .lock()
            .await
            .push_back(DirectiveOutcome::NeedsChoice(ChoicePrompt {
                token: "tok".into(),
                title: "Pick".into(),
                options: vec![
                    ChoiceOption {
                        id: "first".into(),
                        label: "First".into(),
                    },
                    ChoiceOption {
                        id: "second".into(),
                        label: "Second".into(),
                    },
                ],
                multi: false,
            }));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/personality")
            .await
            .unwrap();
        // Reply "2" → the second option.
        gateway
            .handle_text("mock", "chat-1", "alice", "2")
            .await
            .unwrap();
        let dirs = fake.directives.lock().await;
        assert_eq!(
            dirs.last().unwrap().1.choice,
            Some(ChoiceSelection {
                token: "tok".into(),
                ids: vec!["second".into()],
                free_text: None,
            })
        );
    }

    /// Concept lock (arch-refactor §8-9): a multi-line message that merely
    /// starts with `/` is ordinary text, never a directive.
    #[tokio::test]
    async fn gateway_multiline_slash_is_user_text() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/path/to/file\nplease read this")
            .await
            .unwrap();
        // Routed as UserText (submit_turn), not as a directive.
        assert!(fake.directives.lock().await.is_empty());
        let subs = fake.submissions.lock().await;
        assert_eq!(subs.len(), 1);
        assert!(subs[0].1.starts_with("/path/to/file"));
    }

    #[tokio::test]
    async fn gateway_at_bot_switches_session_without_cross_chat_leakage() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        // chat-2 uses a DISTINCT role → a distinct tmux pane / session. (Same
        // (project, role) would be the same pane, so it dedups to one session;
        // isolation between chats comes from distinct roles, not duplicate
        // records over a shared pane.)
        gateway
            .handle_text("mock", "chat-2", "bob", "/new claude qa")
            .await
            .unwrap();

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer check this")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-reviewer-s1 echo: check this"]);

        let other = gateway
            .handle_text("mock", "chat-2", "bob", "same text")
            .await
            .unwrap();
        assert_eq!(other, vec!["alpha-qa-s3 echo: same text"]);
    }

    #[tokio::test]
    async fn gateway_persistence_restores_routes_and_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("gateway-state.json");
        let fake = Arc::new(FakeAdapter::default());

        {
            let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
            gateway.register_project("beta", "/tmp/beta");
            gateway.enable_persistence(&state_path).unwrap();
            gateway
                .handle_text("mock", "chat-1", "alice", "/cd beta")
                .await
                .unwrap();
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap();
        }

        let mut restored = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        restored.register_project("beta", "/tmp/beta");
        restored.enable_persistence(&state_path).unwrap();

        let sessions = restored
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["s1:beta:Claude:reviewer"]);

        let reply = restored
            .handle_text("mock", "chat-1", "alice", "after restart")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta-reviewer-s1 echo: after restart"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reconcile_chat_sessions_free_fn_splits_tracked_and_orphans() {
        let tracked: std::collections::BTreeSet<String> =
            [ccteam_harness::chat_session_name("dev-foo", "alice")]
                .into_iter()
                .collect();
        let live = vec![
            ccteam_harness::chat_session_name("dev-foo", "alice"), // tracked
            ccteam_harness::chat_session_name("ghost-proj", "zombie"), // orphan
            "ccteam-chat-".to_string(),                            // unparseable → dropped
        ];
        let inv = reconcile_chat_sessions(&tracked, &live);
        assert_eq!(inv.tracked, vec!["ccteam-chat-dev-foo-alice".to_string()]);
        assert_eq!(inv.orphans.len(), 1);
        assert_eq!(inv.orphans[0].slug, "ghost-proj");
        assert_eq!(inv.orphans[0].role, "zombie");
        assert_eq!(inv.orphans[0].name, "ccteam-chat-ghost-proj-zombie");
    }

    #[test]
    fn tracked_chat_session_names_empty_when_state_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.json");
        assert!(tracked_chat_session_names(&missing).unwrap().is_empty());
    }

    #[tokio::test]
    async fn tracked_chat_session_names_reads_persisted_canonical_names() {
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("gateway-state.json");
        let fake = Arc::new(FakeAdapter::default());

        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");
        gateway.enable_persistence(&state_path).unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        let names = tracked_chat_session_names(&state_path).unwrap();
        assert!(
            names.contains("ccteam-chat-beta-reviewer"),
            "expected canonical chat-session name, got {names:?}"
        );
    }

    #[tokio::test]
    async fn gateway_registered_bot_template_spawns_on_demand() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "default", "/tmp/default");
        gateway.register_bot_template(
            &BotRegistration {
                workflow_slug: "alpha".to_string(),
                role: "lead".to_string(),
                vendor: AgentVendor::Claude,
                persona_id: None,
                im_platform: "mock".to_string(),
                im_chat_id: "chat-1".to_string(),
                chat_handle: None,
                project_dir: None,
                created_at: chrono::Utc::now(),
            },
            "/tmp/alpha",
        );

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();

        assert_eq!(reply, vec!["alpha-lead-s1 echo: hello"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gateway_registered_bot_templates_keep_ambiguous_dm_out_of_sessions() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "default", "/tmp/default");
        for role in ["lead", "reviewer"] {
            gateway.register_bot_template(
                &BotRegistration {
                    workflow_slug: format!("alpha-{role}"),
                    role: role.to_string(),
                    vendor: AgentVendor::Claude,
                    persona_id: None,
                    im_platform: "mock".to_string(),
                    im_chat_id: "chat-1".to_string(),
                    chat_handle: None,
                    project_dir: None,
                    created_at: chrono::Utc::now(),
                },
                format!("/tmp/alpha-{role}"),
            );
        }

        let ambiguous = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();
        assert_eq!(
            ambiguous,
            vec!["Multiple bots in this chat. Specify one: @lead @reviewer"]
        );

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer hello")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-reviewer-reviewer-s1 echo: hello"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    /// (v0.8.5) A project registered in config.yaml AFTER the gateway started
    /// — e.g. `ccteam init` while the daemon was already running — must be
    /// addressable by /cd, not just the startup snapshot. Reproduces the
    /// "gateway error: unknown project" bug.
    #[tokio::test]
    async fn gateway_cd_dynamically_loads_project_from_config() {
        use ccteam_core::config::{upsert_project, ProjectEntry};
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        // A project the daemon never snapshotted — written straight to the
        // config registry, as `ccteam init` would after startup.
        let gamma_dir = paths.projects_root.join("dev-gamma");
        upsert_project(
            &paths.root,
            ProjectEntry {
                slug: "dev-gamma".to_string(),
                path: gamma_dir.clone(),
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        gateway.enable_project_creation(paths);

        // Not pre-registered in the in-memory map; /cd must reload it from
        // config.yaml on the miss (before the fix this returned
        // "unknown project: dev-gamma").
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "/cd dev-gamma")
            .await
            .unwrap();
        assert!(
            reply.iter().any(|r| r.contains("project set to dev-gamma")),
            "expected /cd to resolve the config-only project, got {reply:?}"
        );
    }

    #[tokio::test]
    async fn gateway_cd_switches_active_session_to_target_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        // Active session s1 lives in project alpha.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let before = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(before, vec!["s1:alpha:Claude:reviewer"]);

        // /cd to beta, where no session exists yet, clears the active session.
        let cd = gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        assert_eq!(
            cd,
            vec!["project set to beta (next message starts a session there)"]
        );

        // The next plain message must route into a beta session, not back s1.
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "where am i")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta-cto-s2 echo: where am i"]);

        let after = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(after, vec!["s1:alpha:Claude:reviewer\ns2:beta:Claude:cto"]);
    }

    #[tokio::test]
    async fn gateway_cd_adopts_existing_session_in_target_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        // s1 in alpha; then /cd beta + /new makes s2 in beta.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();

        // /cd back to alpha must deterministically re-adopt the existing s1.
        let cd_back = gateway
            .handle_text("mock", "chat-1", "alice", "/cd alpha")
            .await
            .unwrap();
        assert_eq!(cd_back, vec!["project set to alpha (switched to s1)"]);

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "ping")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-reviewer-s1 echo: ping"]);
    }

    #[tokio::test]
    async fn gateway_cd_overrides_single_template_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "default", "/tmp/default");
        gateway.register_project("beta", "/tmp/beta");
        gateway.register_bot_template(
            &BotRegistration {
                workflow_slug: "alpha".to_string(),
                role: "lead".to_string(),
                vendor: AgentVendor::Claude,
                persona_id: None,
                im_platform: "mock".to_string(),
                im_chat_id: "chat-1".to_string(),
                chat_handle: None,
                project_dir: None,
                created_at: chrono::Utc::now(),
            },
            "/tmp/alpha",
        );

        // /cd to a different project than the bot's: the explicit target wins,
        // so the next message spawns a default `cto` agent in beta, not the bot.
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta-cto-s1 echo: hello"]);
    }

    #[tokio::test]
    async fn gateway_reconciles_orphan_chat_sessions() {
        use ccteam_harness::{InProcBackend, MuxSessionSpec};
        use std::path::PathBuf;

        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        // One tracked session: s1 = alpha/lead → ccteam-chat-alpha-lead.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude lead")
            .await
            .unwrap();

        // Two live ccteam-chat-* processes injected via a fake ProcessBackend:
        // one matches the tracked session, the other is an orphan that outlived
        // a prior daemon (dashed slug to exercise the parser).
        let backend = InProcBackend::new();
        let spec =
            |name: &str| MuxSessionSpec::new(name, vec!["true".into()], PathBuf::from("/tmp"));
        backend
            .spawn(spec(&chat_session_name("alpha", "lead")))
            .await
            .unwrap();
        backend
            .spawn(spec("ccteam-chat-ghost-proj-zombie"))
            .await
            .unwrap();

        let inventory = gateway.inventory_via_backend(&backend).await.unwrap();
        assert_eq!(
            inventory.tracked,
            vec!["ccteam-chat-alpha-lead".to_string()]
        );
        assert_eq!(
            inventory.orphans,
            vec![OrphanSession {
                name: "ccteam-chat-ghost-proj-zombie".to_string(),
                slug: "ghost-proj".to_string(),
                role: "zombie".to_string(),
            }]
        );

        // The global display entry lists the tracked session and flags the orphan.
        let live = ccteam_harness::list_chat_sessions(&backend).await.unwrap();
        let rendered = gateway.render_all_sessions(&live);
        assert!(
            rendered.contains("s1:alpha:Claude:lead"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains("orphan ccteam-chat-ghost-proj-zombie (slug=ghost-proj role=zombie)"),
            "rendered: {rendered}"
        );
    }

    #[tokio::test]
    async fn gateway_newproject_validates_args_and_requires_path_context() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        // Missing path → usage error (parsed before any path-context check).
        let usage = gateway
            .handle_text("mock", "chat-1", "alice", "/newproject demo")
            .await;
        assert!(format!("{:#}", usage.unwrap_err()).contains("用法"));
        // Valid args, but project creation is not configured on this gateway.
        let err = gateway
            .handle_text("mock", "chat-1", "alice", "/newproject demo /tmp/demo")
            .await
            .expect_err("expected not-configured error");
        assert!(format!("{err:#}").contains("not configured"));
        assert!(Gateway::is_gateway_command("/newproject demo /x"));
    }

    #[test]
    fn expand_project_path_requires_absolute_and_expands_tilde() {
        assert_eq!(
            expand_project_path("/srv/code/app").unwrap(),
            std::path::PathBuf::from("/srv/code/app")
        );
        assert!(expand_project_path("relative/dir").is_err());
        let home = expand_project_path("~/code/app").unwrap();
        assert!(home.is_absolute());
        assert!(home.ends_with("code/app"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gateway_web_is_global_view_and_drives_im_created_session() {
        // Sync reply path (no event_sink) — the async pump's reply routing is
        // covered by the web_chat_bridge harness (its adapter keeps the event
        // stream alive; this FakeAdapter ends it on an empty queue).
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");

        // A Telegram chat creates a session.
        gateway
            .handle_text("telegram", "tg-1", "rob", "/new claude assistant")
            .await
            .unwrap();

        // The web console is a global view: it sees the session it didn't create.
        let listing = gateway
            .handle_text("web", "web-chat", "web-user", "/sessions")
            .await
            .unwrap();
        assert!(
            listing
                .iter()
                .any(|r| r.contains("s1:alpha:Claude:assistant")),
            "web /sessions should list the Telegram session: {listing:?}"
        );

        // Web can /use it (cross-entry) and drive it; the reply comes back.
        let used = gateway
            .handle_text("web", "web-chat", "web-user", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["using session s1"]);
        let reply = gateway
            .handle_text("web", "web-chat", "web-user", "hello from web")
            .await
            .unwrap();
        assert!(
            reply.iter().any(|r| r.contains("echo: hello from web")),
            "web drive reply: {reply:?}"
        );

        // IM stays scoped: a different Telegram chat does NOT see tg-1's session.
        let other = gateway
            .handle_text("telegram", "tg-2", "bob", "/sessions")
            .await
            .unwrap();
        assert_eq!(other, vec!["no sessions"]);
    }

    #[tokio::test]
    async fn gateway_dedupes_sessions_by_project_and_role() {
        // One (project, role) == one tmux pane, so a repeat /new reuses the
        // record instead of spawning a duplicate pump over the same transcript.
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");

        let first = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude assistant")
            .await
            .unwrap();
        assert_eq!(first, vec!["created session s1"]);
        // Same project + role → reuse s1, not a new sid.
        let again = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude assistant")
            .await
            .unwrap();
        assert_eq!(again, vec!["created session s1"]);
        // A different role → a genuinely distinct pane/session.
        let other_role = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(other_role, vec!["created session s2"]);

        // Exactly two sessions tracked (one per project+role pane).
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            listing[0].lines().count(),
            2,
            "expected 2 deduped sessions: {}",
            listing[0]
        );
    }

    /// W1 `/role` — switch the chat's CURRENT session to a fresh `--agent
    /// <role>` while keeping the SAME gateway session id, so a follow-up turn
    /// routes to the new role and `/use <sid>` still resolves. Also covers the
    /// no-active-session error path and that `/help` advertises `/role`.
    /// Sink-less (sync reply path), matching the other `*_echoes` tests.
    #[tokio::test]
    async fn gateway_role_switches_current_session_in_place() {
        let fake = Arc::new(FakeAdapter::default());
        // `/role` validates `.claude/agents/<role>.md` under the project dir
        // before re-spawning, so point the project at a real temp dir and seed
        // the target role there.
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "reviewer");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());

        // No active session yet → `/role` returns the helpful Chinese error.
        let no_session = gateway
            .handle_text("mock", "chat-1", "alice", "/role reviewer")
            .await
            .expect_err("/role with no active session should error");
        assert!(
            format!("{no_session:#}").contains("活动会话"),
            "expected the no-active-session hint: {no_session:#}"
        );

        // Start a default `cto` session (s1) and confirm the role binding.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new")
            .await
            .unwrap();
        let cto_reply = gateway
            .handle_text("mock", "chat-1", "alice", "hi")
            .await
            .unwrap();
        assert_eq!(cto_reply, vec!["alpha-cto-s1 echo: hi"]);

        // /role flips s1 to `reviewer` in place — same sid, fresh agent.
        let switched = gateway
            .handle_text("mock", "chat-1", "alice", "/role reviewer")
            .await
            .unwrap();
        assert_eq!(switched, vec!["switched session s1 to role reviewer"]);

        // A follow-up turn now routes to the reviewer pane under the SAME sid.
        let after = gateway
            .handle_text("mock", "chat-1", "alice", "still here?")
            .await
            .unwrap();
        assert_eq!(after, vec!["alpha-reviewer-s1 echo: still here?"]);

        // The session list shows the new role bound to the same sid (no s2).
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(listing, vec!["s1:alpha:Claude:reviewer"]);

        // `/use s1` still resolves the same (now-reviewer) session.
        let used = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["using session s1"]);

        // /help advertises /role.
        assert!(
            render_help().contains("/role"),
            "render_help should list /role: {}",
            render_help()
        );
    }

    /// V0.8.6 fix #5 — `/role` to a missing role must NOT destroy the live
    /// session: it validates `.claude/agents/<role>.md` (under the session's
    /// project dir) before any teardown, so a typo / absent role is rejected
    /// with a clear hint and the working pane stays intact (same sid + role,
    /// no re-spawn). A switch to a role that DOES exist still works.
    #[tokio::test]
    async fn gateway_role_missing_role_keeps_session_intact() {
        let fake = Arc::new(FakeAdapter::default());
        let tmp = tempfile::tempdir().unwrap();
        // `reviewer` exists; `ghost` deliberately does not.
        seed_role(tmp.path(), "reviewer");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());

        // Start a default `cto` session (s1) and drive it once so the pane is
        // demonstrably live.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new")
            .await
            .unwrap();
        let cto_reply = gateway
            .handle_text("mock", "chat-1", "alice", "hi")
            .await
            .unwrap();
        assert_eq!(cto_reply, vec!["alpha-cto-s1 echo: hi"]);
        // One spawn so far (the `cto` start); the bad switch must not add another.
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        // `/role ghost` → Err with the missing-role hint; session UNCHANGED.
        let err = gateway
            .handle_text("mock", "chat-1", "alice", "/role ghost")
            .await
            .expect_err("/role to a missing role should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("role 不存在") && msg.contains("ghost.md"),
            "expected the missing-role hint naming the file: {msg}"
        );
        // No teardown + re-spawn happened on the bad role.
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "a bad /role must not re-spawn (no teardown of the live pane)"
        );

        // The session is still resolvable, still s1, still `cto`.
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(listing, vec!["s1:alpha:Claude:cto"]);
        // And a follow-up turn still routes to the SAME live `cto` pane.
        let still_cto = gateway
            .handle_text("mock", "chat-1", "alice", "still here?")
            .await
            .unwrap();
        assert_eq!(still_cto, vec!["alpha-cto-s1 echo: still here?"]);

        // A switch to a role that DOES exist still works (same sid, fresh agent).
        let switched = gateway
            .handle_text("mock", "chat-1", "alice", "/role reviewer")
            .await
            .unwrap();
        assert_eq!(switched, vec!["switched session s1 to role reviewer"]);
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            2,
            "a valid /role re-spawns exactly once"
        );
        let after = gateway
            .handle_text("mock", "chat-1", "alice", "now?")
            .await
            .unwrap();
        assert_eq!(after, vec!["alpha-reviewer-s1 echo: now?"]);
    }

    // ===== v0.8.5 D6 — AskUserQuestion → IM round-trip (External origin) =====

    /// Build a 3-option single-select prompt with distinct real ids vs labels,
    /// so the idx→id reverse mapping is actually exercised (a click on index 1
    /// must resolve to id `m-sonnet`, not the label or the index).
    fn ask_prompt(token: &str) -> ChoicePrompt {
        ChoicePrompt {
            token: token.to_string(),
            title: "Which model?".to_string(),
            options: vec![
                ChoiceOption {
                    id: "m-opus".into(),
                    label: "Opus".into(),
                },
                ChoiceOption {
                    id: "m-sonnet".into(),
                    label: "Sonnet".into(),
                },
                ChoiceOption {
                    id: "m-haiku".into(),
                    label: "Haiku".into(),
                },
            ],
            multi: false,
        }
    }

    /// §8-8 D6 e2e (no live socket): the mcp.sock handler registers an
    /// External-origin pending in the SHARED registry; an inbound option click
    /// delivered through the gateway resolves it token-globally and the waiting
    /// oneshot receives the resolved `ChoiceSelection` with the correct REAL
    /// option id (idx 1 → `m-sonnet`). This is exactly the D6 ingress contract
    /// minus the UnixStream transport.
    #[tokio::test]
    async fn d6_external_origin_inbound_click_resolves_oneshot() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");

        // The shared registry the daemon hands to BOTH the gateway and the
        // mcp.sock handler. Here the test plays the handler: register the
        // External pending under the token (mcp.sock keys by token; the
        // gateway resolves token-globally).
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        let token = "habc123";
        let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            ask_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );

        // The user clicks option index 1 ("Sonnet"). Delivered as a callback
        // selection, never as text — note there is NO current gateway session
        // for this chat, proving resolution is purely token-based.
        let replies = gateway
            .handle_message(
                "telegram",
                "chat-9",
                "bob",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: format!("{token}:1"),
                }),
            )
            .await
            .unwrap();
        // External delivery produces no chat reply (the answer flows over the
        // oneshot back to the blocked hook, not back to the chat).
        assert!(
            replies.is_empty(),
            "External resolve must not emit a chat reply: {replies:?}"
        );

        // The blocked hook task receives the resolved selection with the REAL
        // id mapped from the positional index — not the label, not the index.
        let got = rx.await.expect("oneshot delivered");
        assert_eq!(got.token, token);
        assert_eq!(got.ids, vec!["m-sonnet".to_string()]);
        assert_eq!(got.free_text, None);

        // Single-flight: the pending was removed on resolve.
        assert!(
            shared.lock().await.is_empty(),
            "pending consumed on resolve"
        );
    }

    /// A click whose token matches no live pending (expired / already-answered)
    /// must not panic and must leave the registry untouched; the user sees a
    /// benign "expired" notice. Mirrors the daemon timeout path where the
    /// handler `take_by_token`s a now-absent entry.
    #[tokio::test]
    async fn d6_external_stale_token_click_is_benign() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        // No registration — simulate a click after the pending already lapsed.
        let replies = gateway
            .handle_message(
                "telegram",
                "chat-9",
                "bob",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: "hdead:0".to_string(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(replies, vec!["this choice has expired".to_string()]);
        assert!(shared.lock().await.is_empty());
    }

    /// Timeout/drain: an External pending that lapses is returned by
    /// `drain_expired` (so the daemon can forget it) and, once dropped, the
    /// blocked hook's oneshot receiver observes a `RecvError` — which the
    /// daemon maps to the `{"timeout":true}` response. Exercised at the
    /// registry layer (the gateway holds the same `Arc`).
    #[tokio::test]
    async fn d6_external_pending_timeout_drops_oneshot() {
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        let token = "hstale1";
        // Register already-expired (expires_at in the past).
        shared.lock().await.register(
            token.to_string(),
            ask_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() - std::time::Duration::from_secs(1),
        );
        let drained = shared.lock().await.drain_expired(Instant::now());
        assert_eq!(drained.len(), 1, "lapsed External pending is drained");
        // Dropping the drained pending drops its sender; the blocked hook's
        // receiver then errors (daemon → {"timeout":true}).
        drop(drained);
        assert!(rx.await.is_err(), "dropped sender ⇒ receiver RecvError");
        assert!(shared.lock().await.is_empty());
    }

    /// (v0.8.5 S2) A click on a prompt past its TTL must be treated as absent.
    /// `resolve_selection` drains lapsed entries first, so `take_by_token`
    /// finds nothing and the user sees the benign "expired" notice — the
    /// lapsed Directive is NOT re-entered into dispatch. Without the drain the
    /// stale prompt would resolve and re-run the directive long after the TTL.
    #[tokio::test]
    async fn expired_pending_is_drained_before_resolve() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        let token = "texpired";
        // A Directive-origin prompt registered already past its TTL.
        shared.lock().await.register(
            "telegram:chat-1:bob::sess".to_string(),
            ask_prompt(token),
            InteractionOrigin::Directive {
                session_id: "sess".to_string(),
                directive: Directive {
                    name: "model".into(),
                    args: String::new(),
                    choice: None,
                },
            },
            Instant::now() - std::time::Duration::from_secs(1),
        );

        let replies = gateway
            .handle_message(
                "telegram",
                "chat-1",
                "bob",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: format!("{token}:0"),
                }),
            )
            .await
            .unwrap();
        assert_eq!(replies, vec!["this choice has expired".to_string()]);
        assert!(
            shared.lock().await.is_empty(),
            "lapsed pending drained, not resolved"
        );
    }
}
