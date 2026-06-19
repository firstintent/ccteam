//! v8.1 IM gateway route table.
//!
//! This module owns the chat-local `project ⇄ session` state that sits
//! above the older `@handle -> mailbox` router. It is deliberately
//! daemon-agnostic: tests drive it with a fake [`HarnessAdapter`], and
//! the daemon can wire the same state machine into real transports.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use ccteam_core::config::{upsert_project, CcteamConfig, ProjectEntry};
use ccteam_core::projects::{bootstrap_project_at_dir, validate_slug_format};
use ccteam_core::{CcteamPaths, HotConfig, RoleDetail};
use ccteam_harness::{
    chat_session_name, parse_chat_session_name, AgentSpecBrief, AgentVendor, ChoicePrompt,
    ChoiceSelection, Directive, DirectiveOutcome, HarnessAdapter, HarnessError, PermissionMode,
    ProcessBackend, SessionProtocol, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItemDetails,
    TurnInput,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::pending::InteractionOrigin;
use crate::transport::{AttachmentKind, ChannelAttachment, ChoiceReply, MessageOption};
use crate::BotRegistration;

/// v0.8.7 review-fix (R-M6) — a typed "the named role has no
/// `.claude/agents/<role>.md`" error so create paths can distinguish a
/// caller mistake (bad/unseeded role ⇒ a 4xx in the web API) from a genuine
/// internal failure (adapter spawn error, fs error ⇒ 500). `start_session`
/// returns `anyhow::Result`, so this is surfaced via `anyhow::Error` and the
/// web handler recovers it with `downcast_ref::<RoleNotFound>()`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("role 不存在:.claude/agents/{role}.md 未找到;先用 /role <已存在的角色> 或 `ccteam role add {role}` 创建")]
pub struct RoleNotFound {
    /// The role stem that was requested but has no persona file.
    pub role: String,
}

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
    /// v0.8.11 E2 — the session's protocol axis (`stream-json` default /
    /// `terminal`). Selects the adapter via the factory; remembered so a
    /// `/role` re-spawn and a daemon-restart resume re-bind the same adapter.
    protocol: SessionProtocol,
    /// v0.8.11 §七 ② — the host axis, reserved for v0.9 (`local` only today;
    /// not exposed via UI/CLI). Carried so the schema is forward-shaped.
    host: String,
    /// v0.8.7 W2 (DB.1) — per-session permission posture (`skip` default /
    /// `hitl`). Remembered so a `/role` re-spawn and a daemon-restart resume
    /// re-apply the same mode (and the same hook install).
    permission_mode: PermissionMode,
    /// v0.8.7 review-fix (R-M1) — per-session secret minted at first spawn and
    /// injected into the pane env as `CCTEAM_CHAT_SECRET`. The cto-gate
    /// authenticates a forwarded `session_*` caller by matching the secret it
    /// presents against this stored value (see [`Gateway::verify_session_caller`]).
    /// Persisted across daemon restarts so the live pane's env still matches.
    /// HONEST SCOPE: only raises the bar under the single-uid full-trust model
    /// — not a hard boundary (see `ccteam_core::session_secret`).
    secret: String,
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
    adapter_factory: Arc<
        dyn Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync,
    >,
    default_project: String,
    state_path: Option<PathBuf>,
    projects: BTreeMap<String, PathBuf>,
    current_project: BTreeMap<ChatKey, String>,
    /// chat → its current/focused session id. Shared (`Arc<RwLock>`) so the
    /// detached event pumps can read it to label *out-of-band* answers/errors
    /// — i.e. async events from a session that is no longer the chat's focus,
    /// which otherwise masquerade as the current session in the single IM
    /// stream (v0.8.10 routing-isolation fix).
    current_session: Arc<std::sync::RwLock<BTreeMap<ChatKey, String>>>,
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
    /// v0.8.10 D9 — warn once per Claude-routed non-Claude model family.
    /// This is an honesty label only: it never blocks spawn and never changes
    /// adapter/model behavior.
    model_warned: HashSet<(AgentVendor, String)>,
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
/// a survivor of a prior daemon. The process name carries only slug+sid (not
/// the owning chat), so orphans are a global concern and are never attributed
/// to a single chat's `/sessions`.
///
/// v0.8.8 F1 — the name's trailing segment is now the gateway `sid` (`s<N>`),
/// not a role: an orphan/untracked pane cannot recover a role attribute from
/// its name, so display shows the sid (accepted UX cost of sid-keyed panes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanSession {
    /// Full process/tmux session name (`ccteam-chat-<slug>-<sid>`).
    pub name: String,
    /// Project slug parsed from the name.
    pub slug: String,
    /// Gateway session id (`s<N>`) parsed from the name (post-F1 the trailing
    /// segment is the sid, not a role).
    pub sid: String,
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
    /// v0.8.7 W2 (DB.1) — permission posture, stringified (`"skip"` /
    /// `"hitl"`) so the UI / API can show whether a session prompts for
    /// non-allowlist tools. `#[serde(default)]` keeps older clients tolerant.
    #[serde(default)]
    pub permission_mode: String,
    /// v0.8.11 E2 — protocol axis, stringified (`"stream-json"` /
    /// `"terminal"`). The SPA hides the terminal tab for `stream-json`
    /// (paneless) sessions. `#[serde(default)]` keeps older clients tolerant.
    #[serde(default)]
    pub protocol: String,
    /// v0.8.11 §七 ② — host axis (reserved; `"local"` today).
    #[serde(default)]
    pub host: String,
    /// Whether this session is the active one for at least one chat.
    pub current: bool,
    /// Cheap synchronous liveness hint (`"live"` for any tracked session).
    pub status: String,
    /// Seconds since this session's latest progress event when known.
    #[serde(default)]
    pub last_activity_seconds: Option<u64>,
}

/// What [`Gateway::start_session`] reports back so a receipt can name the
/// session it created and its posture.
///
/// v0.8.8 F1 — sessions are keyed by sid (one `(project, role)` can host many),
/// so every `start_session` is a FRESH spawn: there is no longer a reuse path
/// that could silently downgrade a requested `hitl` to a live pane's `skip`.
/// `permission_mode` is therefore always exactly the requested mode (the prior
/// R-M2 "actual vs requested" divergence + the `reused` flag are gone with the
/// dedup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOutcome {
    /// The gateway session id (`s{n}`) — always freshly minted.
    pub id: String,
    /// The session's permission posture (always the requested mode on a fresh
    /// spawn).
    pub permission_mode: PermissionMode,
    /// Optional model-support warning emitted when the role declares a
    /// Claude-routed model outside the verified Claude family.
    pub model_warning: Option<String>,
}

/// Result returned by the web/resource session creation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSessionOutcome {
    /// The freshly minted gateway session id (`s{n}`).
    pub sid: String,
    /// Optional human warning for a Claude-routed role model that ccteam has
    /// not verified as part of the Claude model family.
    pub model_warning: Option<String>,
}

impl CreateSessionOutcome {
    /// Borrow the session id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.sid
    }
}

impl std::ops::Deref for CreateSessionOutcome {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.sid
    }
}

impl AsRef<str> for CreateSessionOutcome {
    fn as_ref(&self) -> &str {
        &self.sid
    }
}

impl std::fmt::Display for CreateSessionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sid)
    }
}

impl PartialEq<&str> for CreateSessionOutcome {
    fn eq(&self, other: &&str) -> bool {
        self.sid == *other
    }
}

impl PartialEq<CreateSessionOutcome> for &str {
    fn eq(&self, other: &CreateSessionOutcome) -> bool {
        *self == other.sid
    }
}

/// v0.8.7 W1 — what [`Gateway::session_resolve`] hands a collector so it can
/// tail a child session's transcript without reaching into the gateway's
/// private session map. Pure data (no adapter handle).
///
/// v0.8.8 F1 — the transcript path is now `.ccteam/chat/<sid>/turns.jsonl`
/// (keyed by `sid`, not role), so collectors read via `sid`; `role` stays as
/// a content/display label, and `vendor` is added for the collect acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResolve {
    /// Gateway session id (`s{n}`) — the transcript directory key.
    pub sid: String,
    /// Agent role — display/content label only (no longer the transcript key).
    pub role: String,
    /// Vendor, stringified (`"claude"` / `"codex"`).
    pub vendor: String,
    /// Project slug the session runs in.
    pub project: String,
    /// Absolute working dir hosting `.ccteam/chat/<sid>/turns.jsonl`.
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
    /// v0.8.11 E2 — persisted protocol axis so a daemon restart re-binds the
    /// same adapter. `#[serde(default)]` ⇒ pre-existing state files (no field)
    /// restore as `StreamJson` (the new default). NOTE: such an old session's
    /// live handle is a tmux handle; resume re-spawns it under the default
    /// protocol — acceptable for pre-v0.8.11 state (dev-stage, no migration).
    #[serde(default)]
    protocol: SessionProtocol,
    /// v0.8.11 §七 ② — persisted host axis (reserved). `#[serde(default)]` ⇒
    /// empty restores; normalized to `local` on load.
    #[serde(default)]
    host: String,
    /// v0.8.7 W2 (DB.1) — persisted permission posture so a daemon restart
    /// re-spawns a hitl session as hitl. `#[serde(default)]` ⇒ already-saved
    /// state files (no field) restore as `Skip`, matching prior behavior.
    #[serde(default)]
    permission_mode: PermissionMode,
    /// v0.8.7 review-fix (R-M1) — persisted per-session cto-gate secret so the
    /// restored in-memory map still matches the live pane's `CCTEAM_CHAT_SECRET`
    /// (and the recreate-fallback re-spawn re-injects the SAME value).
    /// `#[serde(default)]` ⇒ pre-existing state files (no field) restore as
    /// `""`; such a session simply can't pass the secret check until re-spawned
    /// — fail-closed, never fail-open.
    #[serde(default)]
    secret: String,
    handle: String,
    thread: ThreadHandle,
}

#[derive(Clone)]
struct RestoredSessionSnapshot {
    id: String,
    project: String,
    role: String,
    vendor: AgentVendor,
    permission_mode: PermissionMode,
    secret: String,
    thread: ThreadHandle,
    cwd: PathBuf,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
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
        arg_hint: Some("[vendor] [role] [hitl]"),
        help: "start a new session (trailing `hitl` = approve tools in IM)",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/use",
        arg_hint: Some("<id>"),
        help: "switch to a session",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/stop",
        arg_hint: Some("<id>"),
        help: "stop (destroy) a session by id",
        in_menu: false,
    },
    GatewayCommandSpec {
        name: "/screen",
        arg_hint: Some("[id]"),
        help: "screenshot a session's pane (bare = current)",
        in_menu: true,
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
                Arc::new(move |_vendor, _protocol| Arc::clone(&adapter))
            },
            default_project,
            default_dir,
        )
    }

    /// Create a gateway with per-(vendor, protocol) adapter selection.
    pub fn new_with_factory(
        adapter_factory: Arc<
            dyn Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter + Send + Sync>
                + Send
                + Sync,
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
            current_session: Arc::new(std::sync::RwLock::new(BTreeMap::new())),
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
            model_warned: HashSet::new(),
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
            let Some(snapshot) = self.restored_session_snapshot(&id) else {
                continue;
            };
            let resumed = Self::resume_restored_snapshot(&snapshot).await;
            match resumed {
                Ok(thread) => {
                    self.apply_resumed_restored_session(&id, snapshot.adapter, thread);
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

    /// Resume restored sessions without holding the shared web/IM gateway lock
    /// across adapter startup. Stream-json may wait for `system:init` and time
    /// out; keeping that await outside the mutex prevents stale restored
    /// sessions from blocking fresh web `POST /sessions` requests.
    pub async fn resume_restored_sessions_shared(gateway: Arc<tokio::sync::Mutex<Self>>) {
        let ids = {
            let g = gateway.lock().await;
            g.sessions.keys().cloned().collect::<Vec<_>>()
        };
        for id in ids {
            let Some(snapshot) = ({
                let g = gateway.lock().await;
                g.restored_session_snapshot(&id)
            }) else {
                continue;
            };
            let resumed = Self::resume_restored_snapshot(&snapshot).await;
            let mut g = gateway.lock().await;
            match resumed {
                Ok(thread) => {
                    g.apply_resumed_restored_session(&id, snapshot.adapter, thread);
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
            if let Err(err) = g.persist_state() {
                tracing::warn!(
                    error = %err,
                    "ccteam-im: failed to persist resumed gateway session"
                );
            }
        }
    }

    fn restored_session_snapshot(&self, id: &str) -> Option<RestoredSessionSnapshot> {
        let snapshot = self.sessions.get(id)?;
        let Some(cwd) = self.projects.get(&snapshot.project).cloned() else {
            tracing::warn!(
                session = %id,
                project = %snapshot.project,
                "ccteam-im: restored gateway session skipped; project root missing"
            );
            return None;
        };
        Some(RestoredSessionSnapshot {
            id: snapshot.id.clone(),
            project: snapshot.project.clone(),
            role: snapshot.role.clone(),
            vendor: snapshot.vendor,
            permission_mode: snapshot.permission_mode,
            secret: snapshot.secret.clone(),
            thread: snapshot.thread.clone(),
            cwd,
            adapter: (self.adapter_factory)(snapshot.vendor, snapshot.protocol),
        })
    }

    async fn resume_restored_snapshot(
        snapshot: &RestoredSessionSnapshot,
    ) -> Result<ThreadHandle, HarnessError> {
        match snapshot.vendor {
            AgentVendor::Claude => match snapshot
                .adapter
                .resume_thread(&snapshot.thread.identity)
                .await
            {
                Ok(mut thread) => {
                    thread.raw_extras =
                        merge_thread_extras(snapshot.thread.raw_extras.clone(), thread.raw_extras);
                    Ok(thread)
                }
                Err(err) if is_real_claude_tui_handle(&snapshot.thread) => {
                    tracing::warn!(
                        session = %snapshot.id,
                        error = %err,
                        "ccteam-im: Claude restored-session resume failed; trying start_thread reattach/recreate"
                    );
                    snapshot
                        .adapter
                        .start_thread(
                            &AgentSpecBrief {
                                role: snapshot.role.clone(),
                            },
                            &SpawnCtx {
                                slug: snapshot.project.clone(),
                                sid: snapshot.id.clone(),
                                cwd: snapshot.cwd.clone(),
                                project_dir: snapshot.cwd.clone(),
                                extra_args: vec![],
                                model_id: None,
                                permission_mode: snapshot.permission_mode,
                                secret: snapshot.secret.clone(),
                            },
                        )
                        .await
                }
                Err(err) => Err(err),
            },
            AgentVendor::Codex => {
                snapshot
                    .adapter
                    .resume_thread(&snapshot.thread.identity)
                    .await
            }
        }
    }

    fn apply_resumed_restored_session(
        &mut self,
        id: &str,
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
        thread: ThreadHandle,
    ) {
        if let Some(session) = self.sessions.get_mut(id) {
            session.thread = thread;
            session.adapter = adapter;
            session.visible_events = Arc::new(AtomicU64::new(0));
        }
        if let Some(pump) = self.event_pumps.remove(id) {
            pump.abort();
        }
        self.spawn_event_pump(id);
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
            .read()
            .unwrap()
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
                self.current_session
                    .write()
                    .unwrap()
                    .insert(chat.clone(), session_id);
                if payload.is_empty() && attachments.is_empty() {
                    return Ok(vec![format!("using @{handle}")]);
                }
                let turn =
                    wrap_inbound(channel, chat_id, user_id, message_id, &payload, attachments);
                return self.submit_to_current(&chat, turn).await;
            }
            if let Some(template) = self.template_by_handle(&chat, &handle) {
                let session_id = self.start_template_session(chat.clone(), template).await?;
                self.current_session
                    .write()
                    .unwrap()
                    .insert(chat.clone(), session_id);
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
        // v0.8.10 — codex `/clear` = recycle + recreate at the gateway, so its
        // user-facing EFFECT matches Claude Code's in-thread `/clear` (a brand-new
        // empty conversation). Codex cannot clear a thread in place (its native
        // `/clear` is itself a new thread), so we model it as stop-old + start-new.
        // Claude keeps its native in-thread `/clear` (the passthrough below) — only
        // a codex current session + an exact `/clear` is intercepted here.
        if text.trim() == "/clear" {
            let current = self.current_session.read().unwrap().get(&chat).cloned();
            if let Some(sid) = current {
                let is_codex = self
                    .sessions
                    .get(&sid)
                    .map(|s| s.vendor == AgentVendor::Codex)
                    .unwrap_or(false);
                if is_codex {
                    return self.recycle_codex_session(chat.clone(), &sid).await;
                }
            }
        }
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
                // v0.8.7 W2 (DB.1) — optional trailing `hitl` token enables
                // human-in-the-loop approval. v0.8.11 E2 — optional `terminal`
                // token selects the tmux/terminal protocol. Both are order-
                // independent (`/new claude cto hitl terminal` ≡
                // `/new claude cto terminal hitl`); defaults = skip + stream-json.
                let mut permission_mode = PermissionMode::Skip;
                let mut protocol = SessionProtocol::StreamJson;
                for tok in parts {
                    match tok {
                        "hitl" | "skip" => {
                            permission_mode =
                                PermissionMode::parse_opt(Some(tok)).map_err(|e| anyhow!(e))?;
                        }
                        "terminal" | "tmux" | "stream-json" | "streamjson" | "stream_json" => {
                            protocol =
                                SessionProtocol::parse_opt(Some(tok)).map_err(|e| anyhow!(e))?;
                        }
                        other => {
                            return Err(anyhow!(
                                "/new: unknown option `{other}` (expected hitl / terminal)"
                            ));
                        }
                    }
                }
                let project = self.current_project_for(chat);
                let handle = role.clone();
                let outcome = self
                    .start_session(
                        chat.clone(),
                        project,
                        vendor,
                        role,
                        handle,
                        permission_mode,
                        protocol,
                    )
                    .await?;
                Ok(Some(Self::new_session_receipt(&outcome)))
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
                // Cross-frontend sharing: a chat may drive a session it owns,
                // the web operator console drives any, and ANY chat may drive a
                // session in its current project (so IM can take over a
                // web-created session in the same project). Replies still follow
                // the per-turn submitter (`reply_to` retarget below).
                let cur_project = self.current_project_for(chat);
                let session = self
                    .sessions
                    .get(id)
                    .filter(|s| Self::chat_can_access(chat, s, &cur_project))
                    .ok_or_else(|| anyhow!("unknown session for this chat: {id}"))?;
                let sid = session.id.clone();
                // v0.8.10 — capture the target session's project so the switch can
                // also move the chat's project context (below).
                let project = session.project.clone();
                if let Ok(mut target) = session.reply_to.lock() {
                    *target = chat.clone();
                }
                // Switching INTO a session moves the chat's "current project" to
                // that session's project, so a following /new (and /cd's default)
                // lands in the same project you just switched into — not the stale
                // prior one.
                self.current_project.insert(chat.clone(), project);
                self.current_session
                    .write()
                    .unwrap()
                    .insert(chat.clone(), sid.clone());
                self.persist_state()?;
                Ok(Some(format!("using session {sid}")))
            }
            "/stop" => {
                // v0.8.10 — stop (destroy) a session BY ID. Completes the session
                // lifecycle: /new (create) · /clear (recycle) · /use (switch) ·
                // /stop (destroy). Uses the SAME `stop_session` the web API's
                // `POST /sessions/{sid}/stop` calls, so the verb is unified across
                // IM and web. A session id is REQUIRED — a bare `/stop` is rejected
                // because silently destroying the current session is too easy to
                // fat-finger. `stop_session` aborts the pump, closes the vendor
                // thread, drops the record, and clears any `current_session` route
                // pointing at it (so stopping the current session leaves the next
                // message to spawn a fresh default).
                let sid = parts
                    .next()
                    .ok_or_else(|| {
                        anyhow!("/stop 必须带 session id:/stop <sid>(安全起见不支持裸 /stop)")
                    })?
                    .to_string();
                // Same access scope as /use — own session, web operator, or any
                // session in the chat's current project (cross-frontend sharing).
                let cur_project = self.current_project_for(chat);
                let accessible = self
                    .sessions
                    .get(&sid)
                    .map(|s| Self::chat_can_access(chat, s, &cur_project))
                    .unwrap_or(false);
                if !accessible {
                    return Ok(Some(format!("unknown session for this chat: {sid}")));
                }
                self.stop_session(&sid).await?;
                Ok(Some(format!("stopped session {sid}")))
            }
            "/screen" => {
                // v0.8.10 — capture a session's pane to a PNG and send it as an
                // image, so the IM user can SEE the live claude/codex TUI state
                // (e.g. a /model "Switch model?" confirmation or picker that has
                // no hook and so can't be forwarded otherwise). This is the
                // read-only screenshot path (ccteam-core render_screenshot: tmux
                // capture → vt100 → imageproc PNG) — it shows the user a picture,
                // it does NOT parse the pane for control flow (the no-scrape red
                // line). `/screen <sid>` targets a session; bare `/screen` shoots
                // the current one.
                let sid = match parts.next() {
                    Some(id) => id.to_string(),
                    None => self
                        .current_session
                        .read()
                        .unwrap()
                        .get(chat)
                        .cloned()
                        .ok_or_else(|| anyhow!("/screen 需要一个活动会话(或 /screen <sid>)"))?,
                };
                let cur_project = self.current_project_for(chat);
                let slug = self
                    .sessions
                    .get(&sid)
                    .filter(|s| Self::chat_can_access(chat, s, &cur_project))
                    .map(|s| s.project.clone())
                    .ok_or_else(|| anyhow!("unknown session for this chat: {sid}"))?;
                // v0.8.11 E2 — a stream-json session has no pane to capture;
                // refuse with a human message instead of a generic degrade.
                if self
                    .sessions
                    .get(&sid)
                    .map(|s| s.protocol.is_stream_json())
                    .unwrap_or(false)
                {
                    return Ok(Some(format!(
                        "会话 {sid} 是 stream-json 通道(无终端 pane),没法截图 —— 它的回复直接走聊天。要终端镜像/截图,用 `/new … terminal` 起一个终端通道会话。"
                    )));
                }
                let paths = self
                    .project_paths
                    .clone()
                    .ok_or_else(|| anyhow!("screenshot 暂不可用(daemon 缺少 paths 上下文)"))?;
                match ccteam_core::render_screenshot(&paths, &slug, Some(sid.as_str()), 50) {
                    Ok(Some(png)) => {
                        let nanos = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0);
                        self.emit_user_signal(GatewayEvent {
                            id: format!("gateway-screenshot-{sid}-{nanos}"),
                            channel: chat.channel.clone(),
                            chat_id: chat.chat_id.clone(),
                            thread_ts: None,
                            content: String::new(),
                            kind: GatewayEventKind::Answer,
                            attachments: vec![crate::transport::OutboundFile {
                                path: png.to_string_lossy().to_string(),
                                caption: Some(format!("📸 {sid} ({slug})")),
                                kind: crate::transport::OutboundFileKind::Photo,
                            }],
                            options: Vec::new(),
                            sid: Some(sid.clone()),
                        });
                        Ok(None)
                    }
                    Ok(None) => Ok(Some(
                        "截图降级失败(tmux 缺失 / session 未找到 / 字体失败 / IO)—— 看 daemon stderr。"
                            .to_string(),
                    )),
                    Err(err) => Ok(Some(format!("截图出错: {err}"))),
                }
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
        if self.current_session.read().unwrap().contains_key(chat) {
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
            // Implicit default-cto spawn (first message, no `/new`) stays
            // skip — HITL is opt-in via `/new … hitl` / API / cto tool.
            PermissionMode::Skip,
            // v0.8.11 E2 — cto defaults to the stream-json protocol (a pure
            // chat role with no terminal needs).
            SessionProtocol::StreamJson,
        )
        .await?;
        Ok(())
    }

    /// Build the `/new` receipt. v0.8.8 F1 — every `/new` mints a fresh sid
    /// (no more `(project, role)` reuse), so the posture is always exactly the
    /// requested one; the receipt just names the new session + flags hitl.
    fn new_session_receipt(outcome: &StartOutcome) -> String {
        let id = &outcome.id;
        let suffix = if outcome.permission_mode.is_hitl() {
            " (hitl: non-allowlist tools need IM approval)"
        } else {
            ""
        };
        format!("created session {id}{suffix}")
    }

    /// v0.8.10 — codex `/clear`: recycle the current codex session and start a
    /// fresh one in its place, so the user-facing effect matches Claude's
    /// in-thread `/clear` (a brand-new empty conversation). Codex has no in-place
    /// thread-wipe (its native `/clear` is a new thread), so this is modeled at
    /// the gateway. The replacement is spawned FIRST (same project / role / vendor
    /// / permission posture); only on success is the old session stopped — so a
    /// spawn failure leaves the user's existing session intact rather than
    /// session-less. `start_session` repoints `current_session` at the new sid, so
    /// the subsequent `stop_session` of the old sid leaves the chat on the fresh
    /// session.
    async fn recycle_codex_session(&mut self, chat: ChatKey, old_sid: &str) -> Result<Vec<String>> {
        let (project, vendor, role, handle, permission_mode, protocol) = {
            let s = self
                .sessions
                .get(old_sid)
                .ok_or_else(|| anyhow!("session vanished: {old_sid}"))?;
            (
                s.project.clone(),
                s.vendor,
                s.role.clone(),
                s.handle.clone(),
                s.permission_mode,
                s.protocol,
            )
        };
        // Spawn the replacement first; propagate the error WITHOUT touching the
        // old session if it fails.
        let outcome = self
            .start_session(
                chat,
                project,
                vendor,
                role,
                handle,
                permission_mode,
                protocol,
            )
            .await?;
        let new_sid = outcome.id.clone();
        // Replacement is live + current; retire the old session. A stop failure
        // is non-fatal — the fresh session already serves the chat.
        if let Err(err) = self.stop_session(old_sid).await {
            tracing::warn!(
                old_sid,
                new_sid = %new_sid,
                error = %err,
                "codex /clear: fresh session started but stopping the old one failed"
            );
            return Ok(vec![format!(
                "/clear: started fresh codex session {new_sid} (old {old_sid} could not be stopped: {err})"
            )]);
        }
        Ok(vec![format!(
            "/clear: recycled codex session {old_sid}; started fresh {new_sid}"
        )])
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
            // Template-spawned sessions are skip (the route template has no
            // mode field; HITL is opt-in per session, not per route).
            PermissionMode::Skip,
            // Template sessions default to the stream-json protocol.
            SessionProtocol::StreamJson,
        )
        .await
        .map(|o| o.id)
    }

    // v0.8.11 E2 — the spawn axes (vendor / role / permission_mode /
    // protocol) are all independent session attributes; a param-bag struct
    // would just move the same 7 fields behind one name. Keep the flat
    // signature (the 4 callers pass them positionally).
    #[allow(clippy::too_many_arguments)]
    async fn start_session(
        &mut self,
        owner: ChatKey,
        project: String,
        vendor: AgentVendor,
        role: String,
        handle: String,
        permission_mode: PermissionMode,
        protocol: SessionProtocol,
    ) -> Result<StartOutcome> {
        // v0.8.8 F1 — sessions are now keyed by sid, NOT (project, role): the
        // pane/--name/turns/marker all key on `s<N>`, so one (project, role) can
        // host multiple INDEPENDENT sessions (each its own pane + transcript).
        // `/new` (and the API / cto-spawn) therefore ALWAYS mints a fresh sid —
        // no (project, role) dedup here. (The spawn-storm guard is NOT this
        // dedup: it is `ensure_current_session`'s `contains_key` early-return,
        // which only spawns a session for a plain message when the chat has none
        // — that is untouched, so plain-message reuse is preserved while `/new`
        // mints fresh.) `permission_mode` is honored as requested on every fresh
        // spawn (no reuse path that could silently downgrade hitl→skip).
        //
        // (v0.8.8 bug-fix) A project registered via REST `POST /projects` (or
        // `ccteam init`) AFTER the daemon started lands in config.yaml but not
        // yet in the in-memory `projects` cache, so sync from the registry SoT
        // before the lookup — same `ensure_project_loaded` `/cd` uses. Without
        // it the web "new project → new session" flow fails "unknown project"
        // immediately after a successful project create.
        self.ensure_project_loaded(&project);
        let cwd = self
            .projects
            .get(&project)
            .cloned()
            .ok_or_else(|| anyhow!("unknown project: {project}"))?;
        // v0.8.7 (FIX-2) — reject a role with no `.claude/agents/<role>.md`
        // BEFORE allocating a session id or spawning, so any create path (web /
        // API / IM `/new` / cto-dispatch) that names an unseeded persona fails
        // fast with a clear hint instead of spawning `claude --agent <undefined>`
        // → a live-but-brainless pane that never produces a forwardable turn
        // (the original `assistant` web-default bug). Done before the
        // `next_session += 1` bump so a rejected create doesn't burn an `s{n}`.
        // This is the same `read_role` existence check `/role`
        // (switch_current_role) already applies; here it guards creation. See
        // `ensure_role_exists` for the test-dir exemption.
        let role_detail = ensure_role_exists(&cwd, &role)?;
        let model_id = role_model_id(role_detail.as_ref());
        self.next_session += 1;
        let id = format!("s{}", self.next_session);
        // v0.8.8 F2 — roleless(空 role)session 的 handle 默认会随 role 一起变空,
        // 而空 handle 会让 @handle 路由(session_by_handle / template_by_handle)
        // 误匹配/互撞。空 handle 时回退到 sid(全局唯一,绝不撞),保证 @handle
        // 寻址始终非空且确定。非空 handle(常规 role)原样保留。
        let handle = if handle.is_empty() {
            id.clone()
        } else {
            handle
        };
        // v0.8.7 review-fix (R-M1) — mint the per-session cto-gate secret and
        // inject it into the pane env (`CCTEAM_CHAT_SECRET`) at spawn so the
        // in-pane stdio forwarder can authenticate `session_*` calls against
        // this session's stored secret instead of a spoofable plaintext role.
        let secret = ccteam_core::session_secret::mint();
        let adapter = (self.adapter_factory)(vendor, protocol);
        let thread = adapter
            .start_thread(
                &AgentSpecBrief { role: role.clone() },
                &SpawnCtx {
                    slug: project.clone(),
                    sid: id.clone(),
                    cwd: cwd.clone(),
                    project_dir: cwd,
                    extra_args: vec![],
                    model_id: model_id.clone(),
                    permission_mode,
                    secret: secret.clone(),
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
                protocol,
                host: "local".to_string(),
                permission_mode,
                secret,
                handle,
                thread,
                adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(owner.clone())),
            },
        );
        let model_warning =
            self.maybe_emit_model_support_warning(&owner, &id, vendor, model_id.as_deref());
        self.current_session
            .write()
            .unwrap()
            .insert(owner, id.clone());
        self.persist_state()?;
        self.spawn_event_pump(&id);
        Ok(StartOutcome {
            id,
            // Fresh spawn ran with exactly the requested posture.
            permission_mode,
            model_warning,
        })
    }

    /// Switch the chat's CURRENT session to run `role` (W1 `/role`). Role binds
    /// the persona (`--agent <role>`), so a role change re-spawns a fresh thread
    /// — close the old pane and start a new `--agent <role>` one, reusing the
    /// SAME gateway session id so `/use <sid>` keeps resolving. v0.8.8 F1 — the
    /// pane/--name key on the sid (not the role), so the re-spawn reuses the
    /// identical pane name; `start_session` no longer dedups, so the only reuse
    /// here is the deliberate same-sid identity.
    ///
    /// The target role is validated (name charset + `.claude/agents/<role>.md`
    /// existence under the session's project dir) BEFORE any teardown, so a bad
    /// or missing role is rejected with the live session left untouched rather
    /// than destroying the user's working pane on a failed re-spawn.
    async fn switch_current_role(&mut self, chat: &ChatKey, role: String) -> Result<String> {
        let sid = self
            .current_session
            .read()
            .unwrap()
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
        // v0.8.7 W2 (DB.1) — preserve the session's permission posture across a
        // `/role` re-spawn (the new pane re-applies the same hitl/skip spawn
        // flag + hook install). Without capturing it here the fresh SpawnCtx
        // would default the session back to skip.
        let permission_mode = old.permission_mode;
        // v0.8.11 E2 — preserve the protocol axis across a `/role` re-spawn so
        // the same adapter is re-selected (a terminal session stays terminal,
        // a stream-json session stays stream-json).
        let protocol = old.protocol;
        let host = old.host.clone();
        let owner = old.owner.clone();
        let old_thread = old.thread.clone();
        let old_adapter = Arc::clone(&old.adapter);
        // (v0.8.8 bug-fix) sync a possibly-registered-after-start project from
        // the config.yaml SoT before the lookup (mirrors start_session / `/cd`).
        self.ensure_project_loaded(&project);
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
        let role_detail = match ccteam_core::read_role(&cwd, &role) {
            Ok(Some(detail)) => detail,
            Ok(None) | Err(_) => {
                return Err(anyhow!(
                    "role 不存在:.claude/agents/{role}.md 未找到;用 /role <已存在的角色>"
                ));
            }
        };
        let model_id = role_model_id(Some(&role_detail));

        // Tear down the old pane + its event pump before re-spawning so the
        // same-sid pane is recreated cleanly and no stale pump keeps draining
        // the retired transcript.
        if let Some(pump) = self.event_pumps.remove(&sid) {
            pump.abort();
        }
        let _ = old_adapter.close_thread(&old_thread).await;

        let adapter = (self.adapter_factory)(vendor, protocol);
        // v0.8.7 review-fix (R-M1) — a `/role` switch closes the old pane and
        // spawns a brand-new one, so mint a FRESH secret: the new pane's env
        // gets it, and the in-place record below stores the same value, keeping
        // pane-env and gate-map in lockstep.
        let secret = ccteam_core::session_secret::mint();
        let thread = adapter
            .start_thread(
                &AgentSpecBrief { role: role.clone() },
                &SpawnCtx {
                    slug: project.clone(),
                    sid: sid.clone(),
                    cwd: cwd.clone(),
                    project_dir: cwd,
                    extra_args: vec![],
                    model_id: model_id.clone(),
                    permission_mode,
                    secret: secret.clone(),
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
                protocol,
                host,
                permission_mode,
                secret,
                handle: role,
                thread,
                adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(owner)),
            },
        );
        self.current_session
            .write()
            .unwrap()
            .insert(chat.clone(), sid.clone());
        self.persist_state()?;
        self.spawn_event_pump(&sid);
        let _ = self.maybe_emit_model_support_warning(chat, &sid, vendor, model_id.as_deref());
        Ok(sid)
    }

    fn maybe_emit_model_support_warning(
        &mut self,
        chat: &ChatKey,
        sid: &str,
        vendor: AgentVendor,
        model: Option<&str>,
    ) -> Option<String> {
        if vendor != AgentVendor::Claude {
            return None;
        }
        let model = model.map(str::trim).filter(|m| !m.is_empty())?;
        if ccteam_core::is_claude_family(model) {
            return None;
        }
        let key = (vendor, model_warn_key(model));
        if !self.model_warned.insert(key) {
            return None;
        }
        let content = format!(
            "模型提示: 这个 Claude session 的角色声明了 model `{model}`。ccteam 目前只验证 Claude 家族模型；如果会话长时间空转，请改用 sonnet/opus/haiku，或在角色文件里调整 model 后重新 /new。"
        );
        self.emit_user_signal(GatewayEvent {
            id: format!("gateway-model-warn-{sid}-{}", model_warn_key(model)),
            channel: chat.channel.clone(),
            chat_id: chat.chat_id.clone(),
            thread_ts: None,
            content: content.clone(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(sid.to_string()),
        });
        Some(content)
    }

    fn emit_user_signal(&self, event: GatewayEvent) {
        if let Some(tx) = self.event_sink.clone() {
            let _ = tx.send(event);
        } else {
            let _ = self.events_broadcast.send(event);
        }
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
        // v0.8.8 F1 — 捕获项目根供 detached pump 写 turns.jsonl(生产唯一 live
        // writer:历史 read 侧从 `.ccteam/chat/<sid>/turns.jsonl` 取,没有这个写
        // 入 SPA/collect 历史永远为空)。spawn-on-demand 时项目已 register,缺失
        // 才退化(pump 仍跑、只是不落盘),所以 None 不阻断 ANSWER 投递。
        let project_dir = self.projects.get(&session.project).cloned();
        // v0.8.11 E4 — stream-json sessions have NO chat-progress hooks, so the
        // pump is their only progress.jsonl writer (the E1 "直写 progress" intent).
        // Capture the path so the pump can mirror turn boundaries for them; tmux
        // sessions get these from their Stop hook (gated on protocol below).
        let progress_path = self
            .project_paths
            .as_ref()
            .map(|paths| paths.progress_jsonl(&session.project));
        let session_id = session.id.clone();
        let pump_key = session_id.clone();
        // v0.8.10 routing-isolation — read handle to the chat→focus map so the
        // detached pump can label out-of-band answers/errors (events from a
        // session that is no longer the chat's current focus).
        let current_session = Arc::clone(&self.current_session);
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
                        // v0.8.11 E4 — for a stream-json session (no hooks), the
                        // pump mirrors each completed turn to progress.jsonl with
                        // the sid, so the session-list activity classifier (which
                        // keys off the latest sid-tagged event) sees it as active
                        // and `last_activity_seconds` tracks. Tmux sessions get
                        // this from their Stop hook → gate on protocol to avoid a
                        // double-write.
                        if session.protocol.is_stream_json() {
                            if let (ThreadEvent::TurnCompleted { turn_id, usage }, Some(ppath)) =
                                (&evt, progress_path.as_ref())
                            {
                                let ev = ccteam_core::progress::build_chat_turn_completed_event(
                                    &session.role,
                                    &session_id,
                                    turn_id,
                                    usage,
                                );
                                if let Err(err) =
                                    ccteam_core::progress::append_event(ppath, &ev)
                                {
                                    tracing::warn!(
                                        session = %session_id,
                                        error = %err,
                                        "stream-json pump: failed to mirror chat_turn_completed"
                                    );
                                }
                            }
                        }
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
                            // v0.8.8 F1 — 落盘这条 assistant 回复到
                            // `.ccteam/chat/<sid>/turns.jsonl`(生产唯一 live
                            // writer)。这是 SPA / cto session_collect 历史读侧的
                            // 数据源,缺它历史永远为空。`append_turn` 是 O_APPEND
                            // 原子单写(PIPE_BUF-atomic)、不持 gateway 锁,放热路
                            // 径安全;目录已按 sid 隔离,故 TurnRecord 不带
                            // session_id 字段。turn_id 用 pump 内单调 `seq`(每条
                            // ANSWER +1)+ sid 派生,稳定且可 grep,非随机。失败
                            // 只 warn,绝不阻断回复投递。
                            if let Some(dir) = project_dir.as_ref() {
                                let record = ccteam_harness::execution::turns_mirror::TurnRecord {
                                    turn_id: format!("{session_id}-{seq}"),
                                    ts: chrono::Utc::now(),
                                    vendor: vendor_str(session.vendor).to_string(),
                                    // 仅内容标签(state-SoT 的 role 维度在
                                    // progress.jsonl,这里只为历史可读)。
                                    role: session.role.clone(),
                                    user: String::new(),
                                    assistant: text.clone(),
                                    usage: serde_json::Value::Null,
                                    tool_calls: Vec::new(),
                                };
                                if let Err(err) = ccteam_harness::execution::turns_mirror::append_turn(
                                    dir,
                                    &session_id,
                                    &record,
                                ) {
                                    tracing::warn!(
                                        session = %session_id,
                                        error = %err,
                                        "ccteam-im: failed to mirror turn to turns.jsonl"
                                    );
                                }
                            }
                            // Resolve the live reply target ONCE (reply_to → owner
                            // fallback, same as pump_target) and reuse it for the
                            // focus check below.
                            let chat_key = session
                                .reply_to
                                .lock()
                                .map(|k| k.clone())
                                .unwrap_or_else(|_| session.owner.clone());
                            let channel = chat_key.channel.clone();
                            let chat_id = chat_key.chat_id.clone();
                            // v0.8.10 routing-isolation — when this session is NOT the
                            // chat's current focus (the user has since /use'd or
                            // messaged a different sid), its async answer/error still
                            // lands in the same single IM stream. Prefix the sid +
                            // context so an old session can't masquerade as the current
                            // one; the focused session's own replies stay unlabeled.
                            // turns.jsonl already captured the RAW `text` above — only
                            // the IM `content` is prefixed.
                            let is_focused = current_session
                                .read()
                                .map(|m| m.get(&chat_key).map(|s| s == &session_id).unwrap_or(false))
                                .unwrap_or(true);
                            let content = if is_focused {
                                text
                            } else {
                                format!(
                                    "[{} {} {} {}] {}",
                                    session_id,
                                    session.project,
                                    vendor_str(session.vendor),
                                    session.role,
                                    text
                                )
                            };
                            // `GatewayEventSink::send` returns false only when the
                            // mpsc consumer is gone (daemon exited) → stop the pump.
                            if !tx.send(GatewayEvent {
                                id: format!("gateway-event-{session_id}-{seq}"),
                                channel,
                                chat_id,
                                thread_ts: None,
                                content,
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
        *self.current_session.write().unwrap() = saved
            .current_session
            .into_iter()
            .map(|route| (route.chat, route.value))
            .collect();
        self.next_session = saved.next_session;
        self.sessions.clear();
        // v0.8.8 F1 — restore ALL saved sessions: each is keyed by its own sid
        // (pane/--name/turns all key on `s<N>`), so multiple same-(project,role)
        // records are now legitimately independent sessions, NOT duplicates to
        // collapse. The prior seen-panes collapse is gone with the dedup.
        for saved_session in saved.sessions {
            let adapter = (self.adapter_factory)(saved_session.vendor, saved_session.protocol);
            let host = if saved_session.host.is_empty() {
                "local".to_string()
            } else {
                saved_session.host
            };
            self.sessions.insert(
                saved_session.id.clone(),
                GatewaySession {
                    id: saved_session.id,
                    owner: saved_session.owner.clone(),
                    project: saved_session.project,
                    role: saved_session.role,
                    vendor: saved_session.vendor,
                    protocol: saved_session.protocol,
                    host,
                    permission_mode: saved_session.permission_mode,
                    // R-M1 — restore the persisted secret so the gate-map matches
                    // the live pane's `CCTEAM_CHAT_SECRET` after a daemon restart.
                    secret: saved_session.secret,
                    handle: saved_session.handle,
                    thread: saved_session.thread,
                    adapter,
                    visible_events: Arc::new(AtomicU64::new(0)),
                    reply_to: Arc::new(std::sync::Mutex::new(saved_session.owner)),
                },
            );
        }
        // Defensive dead-route cleanup: drop current-session routes that point
        // at a sid with no restored session record (e.g. a state file edited or
        // truncated out-of-band). With sid-keying nothing is collapsed here, but
        // a dangling route would otherwise address a non-existent session.
        let live: std::collections::HashSet<String> = self.sessions.keys().cloned().collect();
        self.current_session
            .write()
            .unwrap()
            .retain(|_, sid| live.contains(sid));
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
                .read()
                .unwrap()
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
                    protocol: session.protocol,
                    host: session.host.clone(),
                    permission_mode: session.permission_mode,
                    secret: session.secret.clone(),
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

    /// v0.8.8 bug-fix — persist the USER side of a turn to
    /// `.ccteam/chat/<sid>/turns.jsonl`. The event pump only observes ANSWER
    /// events, so it writes assistant-only records; without this the user's
    /// prompts never land in the mirror and a session reopened from history
    /// (`GET /sessions/{sid}`) shows only the agent's replies (the user's
    /// messages "disappear" on session switch). Appended at submit time as a
    /// user-only record; the pump later appends the assistant-only record for
    /// the same turn, and `historyToRows` renders them as a user row then an
    /// assistant row in append order. Best-effort: warns on failure, never
    /// blocks the turn; holds no gateway lock (O_APPEND atomic write).
    fn mirror_user_turn(&self, session: &GatewaySession, user_text: &str, turn_id: &str) {
        if user_text.is_empty() {
            return;
        }
        let Some(project_dir) = self.projects.get(&session.project).cloned() else {
            return;
        };
        let record = ccteam_harness::execution::turns_mirror::TurnRecord {
            turn_id: turn_id.to_string(),
            ts: chrono::Utc::now(),
            vendor: vendor_str(session.vendor).to_string(),
            role: session.role.clone(),
            user: user_text.to_string(),
            assistant: String::new(),
            usage: serde_json::Value::Null,
            tool_calls: Vec::new(),
        };
        if let Err(err) =
            ccteam_harness::execution::turns_mirror::append_turn(&project_dir, &session.id, &record)
        {
            tracing::warn!(
                session = %session.id,
                error = %err,
                "ccteam-im: failed to mirror user turn to turns.jsonl"
            );
        }
    }

    async fn submit_to_current(&self, chat: &ChatKey, payload: String) -> Result<Vec<String>> {
        let session_id = self
            .current_session
            .read()
            .unwrap()
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
        // v0.8.8 bug-fix — keep the user's prompt so it can be mirrored into
        // turns.jsonl after a successful submit (the pump records only the
        // assistant side, so without this the user's message is lost on a
        // history reseed / session switch).
        let user_text = payload.clone();
        let submit_wait = gateway_submit_timeout_duration();
        let turn_id = tokio::time::timeout(
            submit_wait,
            session
                .adapter
                .submit_turn(&session.thread, TurnInput::UserText(payload)),
        )
        .await
        .map_err(|_| anyhow!("submit timed out after {submit_wait:?} for {session_id}"))??;
        self.mirror_user_turn(session, &user_text, &turn_id.0);
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
            let progress_path = self
                .project_paths
                .as_ref()
                .map(|paths| paths.progress_jsonl(&session.project));
            spawn_turn_timeout_watchdog(tx, session, start_visible_events, turn_id, progress_path);
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
        let Some(session_id) = self.current_session.read().unwrap().get(chat).cloned() else {
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
        let Some(session_id) = self.current_session.read().unwrap().get(chat).cloned() else {
            return false;
        };
        let key = pending_key(chat, &session_id);
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

    /// Session access scope — visibility (`/sessions`) AND addressing
    /// (`/use` / `/stop` / `/screen`). A chat may reach a session it OWNS,
    /// any session when it is the web operator console (`channel == "web"`),
    /// or **any session in the chat's current project** (cross-frontend
    /// sharing: IM can see + drive sessions a web chat created in the same
    /// project, and vice versa). Reply routing is unaffected — it follows the
    /// per-turn submitter via `reply_to`, so a session driven from Telegram
    /// answers Telegram and one driven from web answers web, regardless of who
    /// created it. `cur_project` is the caller's [`current_project_for`] (passed
    /// in so this stays a borrow-free predicate usable inside `sessions`
    /// iteration).
    fn chat_can_access(chat: &ChatKey, session: &GatewaySession, cur_project: &str) -> bool {
        session.owner == *chat || chat.channel == "web" || session.project == cur_project
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
                self.current_session
                    .write()
                    .unwrap()
                    .insert(chat.clone(), id.clone());
            }
            None => {
                self.current_session.write().unwrap().remove(chat);
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
        // session; an IM channel sees sessions in its CURRENT project
        // (cross-frontend sharing — incl. web-created ones), plus any it owns.
        let global = chat.channel == "web";
        let cur_project = self.current_project_for(chat);
        let visible: Vec<&GatewaySession> = self
            .sessions
            .values()
            .filter(|s| global || Self::chat_can_access(chat, s, &cur_project))
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
    ///
    /// v0.8.8 F1 — the canonical name keys on the `sid` (`ccteam-chat-<slug>-
    /// <sid>`), so the tracked set MUST be computed from `(project, id)` to
    /// match the pane name the adapter actually spawns; using role here would
    /// misjudge every live pane as an orphan.
    pub fn reconcile_chat_sessions(&self, live_chat_names: &[String]) -> SessionInventory {
        let tracked_names: std::collections::BTreeSet<String> = self
            .sessions
            .values()
            .map(|s| chat_session_name(&s.project, &s.id))
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
                "orphan {} (slug={} sid={}) — untracked, reclaim explicitly",
                orphan.name, orphan.slug, orphan.sid
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
        let current: std::collections::HashSet<String> = self
            .current_session
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let mut views: Vec<SessionView> = self
            .sessions
            .values()
            .map(|s| SessionView {
                sid: s.id.clone(),
                project: s.project.clone(),
                role: s.role.clone(),
                vendor: vendor_str(s.vendor).to_string(),
                permission_mode: s.permission_mode.as_str().to_string(),
                protocol: s.protocol.as_str().to_string(),
                host: if s.host.is_empty() {
                    "local".to_string()
                } else {
                    s.host.clone()
                },
                current: current.contains(&s.id),
                status: "live".to_string(),
                last_activity_seconds: None,
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
            vendor: vendor_str(session.vendor).to_string(),
            project: session.project.clone(),
            project_dir,
        })
    }

    /// Resolve a sid to its `(adapter, thread)` so the caller can query
    /// [`HarnessAdapter::thread_status`] (model + context-window usage for the
    /// web statusline bar) **after dropping the gateway lock**. Returns clones
    /// (the adapter is an `Arc`, the handle is cheap) for the same lock-drop
    /// discipline the history endpoint uses — `thread_status` does fs/transport
    /// I/O and must never run under the gateway mutex. `None` for an unknown
    /// sid (the handler's 404 gate). Sync, holds no `.await`.
    pub fn session_status_handle(
        &self,
        sid: &str,
    ) -> Option<(Arc<dyn HarnessAdapter + Send + Sync>, ThreadHandle)> {
        self.sessions
            .get(sid)
            .map(|s| (Arc::clone(&s.adapter), s.thread.clone()))
    }

    /// v0.8.7 review-fix (R-M1) — authenticate a forwarded `session_*` caller
    /// by matching the `(role, secret)` PAIR it presents against a tracked
    /// session, instead of trusting a plaintext `_caller_role` arg. Returns
    /// `true` iff some live session both runs `claimed_role` AND holds a secret
    /// equal (constant-time) to `presented_secret`. An empty secret is always
    /// `false` (fail-closed): a pre-secret restored session or a forger with no
    /// secret can never authenticate. Read-only, holds no `.await`.
    ///
    /// HONEST SCOPE: this only RAISES THE BAR. Under the single-OS-uid
    /// full-trust model any agent can read another's `/proc/<pid>/environ`,
    /// files, or ptrace it and recover the secret, so this is best-effort
    /// defense-in-depth, NOT a hard boundary. Real isolation = per-agent OS
    /// user / sandbox (v0.8.8-deferred). See `ccteam_core::session_secret`.
    pub fn verify_session_caller(&self, claimed_role: &str, presented_secret: &str) -> bool {
        if presented_secret.is_empty() {
            return false;
        }
        self.sessions.values().any(|s| {
            s.role == claimed_role
                && !s.secret.is_empty()
                && ccteam_core::session_secret::ct_eq(&s.secret, presented_secret)
        })
    }

    /// v0.8.8 F1 — confirm a HITL-firing `sid` maps to a live tracked session,
    /// returning its canonical id for the approval prompt label ("session sX
    /// wants to run …"). The firing session reports its own sid via the
    /// `CCTEAM_CHAT_SID` pane env (post-dedup `(project, role)` is no longer a
    /// unique key, so the sid is the only safe identity). Returns `None` when
    /// the sid is not tracked (the prompt then falls back to a sid-less label).
    /// Read-only, holds no `.await`.
    pub fn session_sid_for(&self, sid: &str) -> Option<String> {
        self.sessions.get(sid).map(|s| s.id.clone())
    }

    /// v0.8.7 (FIX-1) — resolve the live reply target `(channel, chat_id)` for
    /// the session addressed by `sid`. This is the outbound addressing the IM
    /// `chat_send_file` / `interaction/ask` paths need so an agent the user is
    /// actively chatting with can push a file back to that same chat WITHOUT a
    /// prior `chat_register_bot` (the on-disk registry is only written by an
    /// explicit register, so the inbound spawn path never populates it).
    ///
    /// v0.8.8 F1 — keyed by `sid` (the firing session reports its own
    /// `CCTEAM_CHAT_SID`): post-dedup `(project, role)` is no longer unique, so
    /// resolving by sid is the only way to reach the SPECIFIC session's reply
    /// target. Resolves the session's `reply_to` (whoever last drove it) →
    /// `owner` fallback exactly like the private [`pump_target`] free fn.
    /// Returns `None` when the sid is not tracked (the caller then falls back to
    /// the on-disk `resolve_home_chat` registry). Read-only, holds no `.await`.
    pub fn reply_target_for(&self, sid: &str) -> Option<(String, String)> {
        let session = self.sessions.get(sid)?;
        Some(pump_target(session))
    }

    /// Create a session from the network API (W5b). Thin wrapper over
    /// [`start_session`](Self::start_session): the caller supplies the
    /// project + role + vendor + permission mode; the handle defaults to the
    /// role name (the established convention from `/new`). Returns the new
    /// `s{n}` id. The `owner` is a synthetic `web` chat key so replies route
    /// to the web console; an SSE handler then filters the outbound stream by
    /// `sid`. v0.8.8 F1 — always mints a NEW sid (sessions are sid-keyed, so a
    /// repeat call for the same `(project, role)` is a distinct session, NOT a
    /// reuse), consistent with `/new`.
    pub async fn create_session_api(
        &mut self,
        project: String,
        role: String,
        vendor: AgentVendor,
        permission_mode: PermissionMode,
    ) -> Result<CreateSessionOutcome> {
        // Default protocol = stream-json (the薄/default channel); the REST
        // route uses `create_session_api_proto` to honor an explicit choice.
        self.create_session_api_proto(
            project,
            role,
            vendor,
            permission_mode,
            SessionProtocol::default(),
        )
        .await
    }

    /// v0.8.11 E2 — like [`Self::create_session_api`] but with an explicit
    /// `protocol` (the REST `POST …/sessions` path threads the request's
    /// `protocol` field here; omitted → caller passes the default).
    pub async fn create_session_api_proto(
        &mut self,
        project: String,
        role: String,
        vendor: AgentVendor,
        permission_mode: PermissionMode,
        protocol: SessionProtocol,
    ) -> Result<CreateSessionOutcome> {
        let owner = web_api_chat();
        // v0.8.8 F2 — handle 默认 = role;空 role(roleless)→ 空 handle,由
        // `start_session` 统一回退到 sid(避免空 handle 撞 @handle 路由)。
        let handle = role.clone();
        self.start_session(
            owner,
            project,
            vendor,
            role,
            handle,
            permission_mode,
            protocol,
        )
        .await
        .map(|o| CreateSessionOutcome {
            sid: o.id,
            model_warning: o.model_warning,
        })
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
        // v0.8.8 bug-fix — mirror the user's prompt to turns.jsonl (the pump
        // records only the assistant side); without it a web session reopened
        // from history shows the agent's replies but not the user's messages.
        let user_text = text.clone();
        let submit_wait = gateway_submit_timeout_duration();
        let turn_id = tokio::time::timeout(
            submit_wait,
            session
                .adapter
                .submit_turn(&session.thread, TurnInput::UserText(text)),
        )
        .await
        .map_err(|_| anyhow!("submit timed out after {submit_wait:?} for {sid}"))??;
        self.mirror_user_turn(session, &user_text, &turn_id.0);
        // Arm the same async-turn machinery the inbound path uses (turn
        // watchdog when a sink is wired; otherwise this is a no-op drain).
        let _ = self
            .after_turn_submitted(session, start_visible_events, &turn_id.0)
            .await?;
        Ok(turn_id.0)
    }

    /// v0.8.7 review-fix (R-H1) — resolve a token-keyed pending choice from the
    /// network API (the web HITL approve/deny path), by TOKEN + the chosen
    /// option `id`. This is the web peer of an IM option click: it routes
    /// through the EXACT same machinery (`take_by_token` → `apply_pending`),
    /// resolving the SAME External-origin pending the blocked
    /// `permission/ask` / `interaction/ask` hook is waiting on — it is NOT a
    /// turn. A web `[Approve]` therefore makes the hook return `allow` (the
    /// tool runs); `[Deny]` returns `deny` immediately (no 600s timeout).
    ///
    /// Returns `Ok(())` once the pending is consumed and its waiter delivered.
    /// An unknown / expired token, or an `id` not in the prompt's option set,
    /// is an `Err` the HTTP layer maps to a clean 4xx (never a turn, never a
    /// 5xx). Drains lapsed prompts first so a late click on an expired choice
    /// reads as absent. The `chat` passed to `apply_pending` is the synthetic
    /// web key — only consulted for a Directive-origin re-entry; an External
    /// (hook) origin delivers over its oneshot and ignores the chat.
    pub async fn resolve_web_selection(&self, token: &str, option_id: &str) -> Result<()> {
        let taken = {
            let mut pend = self.pending.lock().await;
            pend.drain_expired(Instant::now());
            pend.take_by_token(token)
        };
        let Some(p) = taken else {
            return Err(anyhow!("unknown or expired token"));
        };
        // Validate the chosen id against the prompt's real option set, so a
        // bogus selection is rejected rather than silently denied. (If the id
        // is absent we've already taken the pending; re-register would race, so
        // we instead resolve it as the rejection — but the cleaner contract is
        // a 4xx, and the pending is single-flight + about to time out anyway.)
        if !p.prompt.options.iter().any(|o| o.id == option_id) {
            // Put the waiter out of its misery deterministically: an unknown id
            // is treated as no valid choice → drop, which makes an External
            // hook observe RecvError → its own fail-safe deny. We surface a
            // 4xx to the web caller.
            return Err(anyhow!("invalid option id for this prompt"));
        }
        let selection = ChoiceSelection {
            token: token.to_string(),
            ids: vec![option_id.to_string()],
            free_text: None,
        };
        self.apply_pending(&web_api_chat(), p, selection).await?;
        Ok(())
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
        self.current_session
            .write()
            .unwrap()
            .retain(|_, v| v != sid);
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
        } else if let Some((slug, sid)) = parse_chat_session_name(name) {
            // v0.8.8 F1 — `parse_chat_session_name` now yields (slug, sid).
            inventory.orphans.push(OrphanSession {
                name: name.clone(),
                slug,
                sid,
            });
        }
    }
    inventory.tracked.sort();
    inventory.tracked.dedup();
    inventory.orphans.sort_by(|a, b| a.name.cmp(&b.name));
    inventory
}

/// Load the set of canonical chat-session names (`ccteam-chat-<slug>-<sid>`)
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
        // v0.8.8 F1 — canonical name keys on the sid (`s<N>`), matching the
        // pane name the adapter spawns; computing from role here would make
        // every live pane reconcile as an orphan.
        .map(|s| chat_session_name(&s.project, &s.id))
        .collect())
}

/// v0.8.8 B4/F3 — one tracked gateway session, flattened for out-of-process
/// readers (the `ccteam session ls` / `ccteam status` CLI). The gateway's
/// in-memory [`SessionView`] lives inside the daemon process; the CLI is a
/// separate process and can only reach the persisted [`SavedGatewayState`]
/// file. This projection exposes exactly the columns those views render
/// (sid · project · role · vendor · permission_mode) without leaking the
/// persisted struct's private fields (`secret` / `handle` / `thread`).
#[derive(Debug, Clone)]
pub struct TrackedSessionRow {
    /// Gateway session id (`s<N>`) — the unique session key (F1).
    pub sid: String,
    /// Project slug the session runs in.
    pub project: String,
    /// Agent role (display label).
    pub role: String,
    /// Vendor, stringified (`"claude"` / `"codex"`).
    pub vendor: String,
    /// Permission posture wire string (`"skip"` / `"hitl"`).
    pub permission_mode: String,
}

/// Load the gateway's tracked sessions as flat [`TrackedSessionRow`]s from the
/// persisted route table at `state_path` (see
/// [`default_gateway_state_path`](crate::default_gateway_state_path)).
///
/// Shares the exact read path of [`tracked_chat_session_names`] (same
/// [`SavedGatewayState`] file; **absent ⇒ empty `Vec`**, never an error) so
/// the two daemon-independent CLI views (`session ls` reconcile + `status`
/// nesting) never drift on what the daemon has persisted. Strictly read-only.
///
/// The sub-second drift between the in-memory gateway map and this on-disk
/// snapshot is accepted for the status / ls views (they're a glance, not a
/// liveness gate).
pub fn tracked_chat_sessions(state_path: &Path) -> Result<Vec<TrackedSessionRow>> {
    if !state_path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(state_path)
        .with_context(|| format!("read gateway state {}", state_path.display()))?;
    let saved: SavedGatewayState = serde_json::from_str(&raw)
        .with_context(|| format!("parse gateway state {}", state_path.display()))?;
    Ok(saved
        .sessions
        .into_iter()
        .map(|s| TrackedSessionRow {
            sid: s.id,
            project: s.project,
            role: s.role,
            vendor: vendor_str(s.vendor).to_string(),
            permission_mode: s.permission_mode.as_str().to_string(),
        })
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
    progress_path: Option<PathBuf>,
) {
    let timeout = gateway_turn_timeout_duration();
    if timeout.is_zero() {
        return;
    }
    let visible_events = Arc::clone(&session.visible_events);
    let session_id = session.id.clone();
    let project = session.project.clone();
    let role = session.role.clone();
    let reply_to = Arc::clone(&session.reply_to);
    let owner = session.owner.clone();
    let turn_id = turn_id.to_string();
    // v0.8.9 (owner request) — the watchdog now TERMINATES a runaway turn, not
    // just notifies. Clone the session's adapter + thread so the spawned task
    // can send the `esc` directive (Esc to a Claude pane) when the turn stalls.
    let adapter = Arc::clone(&session.adapter);
    let thread = session.thread.clone();
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        if visible_events.load(Ordering::SeqCst) != start_visible_events {
            return; // the turn produced a visible answer → not stuck.
        }
        if let Some(progress_path) = progress_path.as_ref() {
            let ev = ccteam_core::progress::build_chat_turn_timeout_event(
                &role,
                &session_id,
                &project,
                &turn_id,
                timeout.as_secs(),
            );
            if let Err(err) = ccteam_core::progress::append_event(progress_path, &ev) {
                tracing::warn!(
                    session = %session_id,
                    path = %progress_path.display(),
                    error = %err,
                    "turn-watchdog: failed to append chat_turn_timeout progress event"
                );
            }
        }
        // No answer within the timeout = a stalled / infinitely-looping turn
        // (e.g. a roleless model spinning on tool calls). INTERRUPT it via the
        // adapter's vendor-agnostic `esc` directive (Claude → Esc keystroke).
        // This terminates the TURN, NOT the session — the pane stays alive
        // ("never auto-kill a long session"); the user can simply send again.
        // Best-effort: on failure we still notify (the user can `/stop`).
        let esc = Directive {
            name: "esc".to_string(),
            args: String::new(),
            choice: None,
        };
        let interrupted = match adapter.handle_directive(&thread, esc).await {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(
                    session = %session_id,
                    error = %err,
                    "turn-watchdog: failed to interrupt the runaway turn"
                );
                false
            }
        };
        let (channel, chat_id) = match reply_to.lock() {
            Ok(target) => (target.channel.clone(), target.chat_id.clone()),
            Err(_) => (owner.channel.clone(), owner.chat_id.clone()),
        };
        let content = if interrupted {
            format!(
                "⏱️ turn {turn_id} produced no reply for {timeout:?} — the watchdog \
                 interrupted it (the session is still alive; just send again to retry). \
                 If this recurs the model may be looping: bind a role (e.g. cto) or give a \
                 clearer task. Tune the limit via CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS."
            )
        } else {
            format!(
                "⏱️ turn {turn_id} timed out after {timeout:?} for {session_id}; the watchdog \
                 could not interrupt it — you may need to /stop the session."
            )
        };
        let _ = tx.send(GatewayEvent {
            id: format!("gateway-timeout-{session_id}-{turn_id}"),
            channel,
            chat_id,
            thread_ts: None,
            content,
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

/// v0.8.7 (FIX-2) — fail fast when a create path names a role that has no
/// `.claude/agents/<role>.md` under the project dir, so we never spawn
/// `claude --agent <undefined>` (a live-but-brainless pane that never produces
/// a forwardable turn). Mirrors the `read_role` existence check `/role`
/// already applies on a role switch.
///
/// Exemption: when the project's `.claude/agents/` directory does NOT exist at
/// all, validation is SKIPPED. A real ccteam project always has that dir
/// (`ccteam init` seeds `cto.md` into it), so in production the dir is present
/// and the check is strict. The skip exists for the gateway's many unit /
/// integration tests that spawn against bare fake project dirs (e.g.
/// `/tmp/alpha`) with a `FakeAdapter` and no seeded agents — those exercise
/// routing, not personas, and shouldn't be forced to scaffold a role tree.
fn ensure_role_exists(cwd: &std::path::Path, role: &str) -> Result<Option<RoleDetail>> {
    // v0.8.8 F2 — 空 role = 显式 roleless(裸 claude 自读项目 CLAUDE.md):跳过
    // 存在性校验。必须在 `read_role` 之前(`read_role("")` 会因 charset 校验
    // bail → `.ok().flatten()` 折成 None → 误报 RoleNotFound)。
    if role.is_empty() {
        return Ok(None);
    }
    // No agents dir → uninitialized / test project; skip (see doc comment).
    if !ccteam_core::agents_dir(cwd).exists() {
        return Ok(None);
    }
    // `read_role` returns Err on a bad name (charset / traversal) and Ok(None)
    // when the file is absent — both mean "no such role" here.
    // v0.8.7 review-fix (R-M6): surface a typed `RoleNotFound` (via anyhow) so
    // the web create handler can map it to a 4xx instead of a blanket 500.
    match ccteam_core::read_role(cwd, role) {
        Ok(Some(detail)) => Ok(Some(detail)),
        Ok(None) | Err(_) => Err(RoleNotFound {
            role: role.to_string(),
        }
        .into()),
    }
}

fn role_model_id(detail: Option<&RoleDetail>) -> Option<String> {
    detail
        .and_then(|d| d.frontmatter.get("model"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn model_warn_key(model: &str) -> String {
    model
        .trim()
        .split_once('[')
        .map_or_else(|| model.trim(), |(head, _)| head.trim())
        .to_ascii_lowercase()
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

/// True when a restored Claude `ThreadHandle` carries enough context
/// (`cwd` + `project_dir`) to rebuild from the persisted `SpawnCtx` via
/// `start_thread` after a `resume_thread` failure. Covers BOTH Claude spawn
/// paths: the tmux/terminal handle (keyed by `tmux_session`) and the
/// v0.8.11 stream-json handle (`adapter = "claude-stream-json"`, resumed via
/// the deterministic per-sid uuid + `--resume`). A handle missing the cwd
/// pair can't be rebuilt, so the resume keeps the stale handle instead.
fn is_real_claude_tui_handle(thread: &ThreadHandle) -> bool {
    let has = |k: &str| thread.raw_extras.get(k).and_then(|v| v.as_str()).is_some();
    let is_tmux = has("tmux_session");
    let is_stream_json =
        thread.raw_extras.get("adapter").and_then(|v| v.as_str()) == Some("claude-stream-json");
    (is_tmux || is_stream_json) && has("cwd") && has("project_dir")
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
/// IM callback payload is `"{token}:{idx}"` — short, opaque, within Telegram's
/// 64-byte `callback_data` cap; the IM click resolves by idx (reverse-resolved
/// from the pending registry). v0.8.7 review-fix (R-H1): the stable option `id`
/// is also carried so a tokenless web SSE consumer can resolve the same pending
/// by `{token, selection=id}` (the IM path ignores `id`).
fn to_message_options(prompt: &ChoicePrompt) -> Vec<MessageOption> {
    prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{}:{}", prompt.token, i),
            label: opt.label.clone(),
            id: opt.id.clone(),
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
        seed_role_with_model(project_dir, role, None);
    }

    fn seed_role_with_model(project_dir: &std::path::Path, role: &str, model: Option<&str>) {
        let agents = project_dir.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let model = model.map(|m| format!("model: {m}\n")).unwrap_or_default();
        std::fs::write(
            agents.join(format!("{role}.md")),
            format!("---\nname: {role}\n{model}---\n{role} role.\n"),
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

    #[tokio::test]
    async fn claude_non_family_role_model_warns_once_to_event_stream() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role_with_model(tmp.path(), "reviewer", Some("deepseek-via-claude"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let mut events = gateway.subscribe_events();

        assert_eq!(
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap(),
            vec!["created session s1"]
        );
        let warn = events.recv().await.unwrap();
        assert_eq!(warn.sid.as_deref(), Some("s1"));
        assert!(warn.content.contains("模型提示"), "{}", warn.content);
        assert!(
            warn.content.contains("deepseek-via-claude"),
            "{}",
            warn.content
        );
        assert!(
            warn.content.contains("sonnet/opus/haiku"),
            "{}",
            warn.content
        );
        assert!(warn.content.contains("/new"), "{}", warn.content);

        assert_eq!(
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap(),
            vec!["created session s2"]
        );
        assert!(
            matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "same model family should warn once"
        );
    }

    #[tokio::test]
    async fn create_session_api_returns_model_warning_once_in_band() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role_with_model(tmp.path(), "reviewer", Some("deepseek-via-claude"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());

        let first = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!(first.sid, "s1");
        assert!(
            first
                .model_warning
                .as_deref()
                .is_some_and(|msg| msg.contains("deepseek-via-claude")),
            "first API create must return the warning in-band: {first:?}"
        );

        let second = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!(second.sid, "s2");
        assert_eq!(second.model_warning, None);
    }

    #[tokio::test]
    async fn claude_family_role_models_do_not_warn() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role_with_model(tmp.path(), "sonnetrole", Some("sonnet[1m]"));
        seed_role_with_model(tmp.path(), "future", Some("claude-future-99"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let mut events = gateway.subscribe_events();

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude sonnetrole")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude future")
            .await
            .unwrap();
        assert!(
            matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "Claude-family role models must not warn"
        );
    }

    #[tokio::test]
    async fn codex_route_with_non_claude_role_model_does_not_warn() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role_with_model(tmp.path(), "api", Some("deepseek-via-claude"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Codex));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let mut events = gateway.subscribe_events();

        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        assert!(
            matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "non-Claude vendor must not emit Claude model warning"
        );
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
        resume_delay: std::time::Duration,
        resume_started: Arc<AtomicUsize>,
        /// Recorded `handle_directive` calls (thread id + directive) for
        /// routing + choice-reentry assertions (v0.8.5 D1).
        directives: Arc<Mutex<Vec<(String, Directive)>>>,
        /// Scripted outcomes popped in order by `handle_directive` (e.g. a
        /// `NeedsChoice`); empty ⇒ a `Done` echo.
        directive_script: Arc<Mutex<VecDeque<DirectiveOutcome>>>,
        /// Status returned by `thread_status` (v0.8.5 P3).
        status: Arc<Mutex<ThreadStatus>>,
        /// v0.8.7 W2 — `SpawnCtx::permission_mode` captured per start_thread,
        /// in spawn order, so a test can assert the gateway threaded the right
        /// posture (skip vs hitl) down to the adapter.
        spawn_modes: Arc<Mutex<Vec<PermissionMode>>>,
        /// v0.8.7 review-fix (R-M1) — `SpawnCtx::secret` captured per
        /// start_thread so a test can assert the minted per-session secret was
        /// threaded into the spawn env.
        spawn_secrets: Arc<Mutex<Vec<String>>>,
        /// v0.8.11 E4 — when set, `submit_turn` ALSO enqueues a `TurnCompleted`
        /// after the `AgentMessage` (mirrors a real adapter's turn boundary).
        /// Off by default so the sync-drain tests (which only take the first
        /// text-bearing event) don't leave a stale `TurnCompleted` queued.
        emit_turn_boundary: bool,
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
                resume_delay: std::time::Duration::ZERO,
                resume_started: Arc::new(AtomicUsize::new(0)),
                directives: Arc::new(Mutex::new(Vec::new())),
                directive_script: Arc::new(Mutex::new(VecDeque::new())),
                status: Arc::new(Mutex::new(ThreadStatus::default())),
                spawn_modes: Arc::new(Mutex::new(Vec::new())),
                spawn_secrets: Arc::new(Mutex::new(Vec::new())),
                emit_turn_boundary: false,
            }
        }

        /// Opt into emitting a `TurnCompleted` boundary after the answer
        /// (v0.8.11 E4 — drives the stream-json pump's progress.jsonl mirror).
        fn with_turn_boundary(mut self) -> Self {
            self.emit_turn_boundary = true;
            self
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

        fn with_resume_delay(mut self, resume_delay: std::time::Duration) -> Self {
            self.resume_delay = resume_delay;
            self
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
            self.spawn_modes.lock().await.push(ctx.permission_mode);
            self.spawn_secrets.lock().await.push(ctx.secret.clone());
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
            // A real adapter also emits a turn boundary (carrying usage); the
            // stream-json pump mirrors it to progress.jsonl for paneless
            // sessions (v0.8.11 E4). Opt-in (`with_turn_boundary`) so the
            // sync-drain tests don't leave a stale TurnCompleted queued.
            if self.emit_turn_boundary {
                self.events.lock().await.push_back((
                    h.identity.clone(),
                    ThreadEvent::TurnCompleted {
                        turn_id: format!("turn-{}", h.identity),
                        usage: ccteam_harness::UnifiedTokenUsage::default(),
                    },
                ));
            }
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
            self.resume_started.fetch_add(1, Ordering::SeqCst);
            if !self.resume_delay.is_zero() {
                tokio::time::sleep(self.resume_delay).await;
            }
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
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
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

    /// v0.8.7 review-fix (R-M1) — `create_session_api` mints a per-session
    /// secret, stores it on the session, and injects it into the spawn env so
    /// the pane (and its in-pane stdio forwarder) can present it. Two sessions
    /// get DIFFERENT secrets.
    #[tokio::test]
    async fn create_session_mints_unique_secret_and_injects_into_env() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-secret");
        let s1 = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let s2 = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let sec1 = gateway.sessions.get(s1.as_str()).unwrap().secret.clone();
        let sec2 = gateway.sessions.get(s2.as_str()).unwrap().secret.clone();
        assert_eq!(sec1.len(), 32, "secret is 128-bit hex");
        assert_ne!(sec1, sec2, "each session gets its own secret");
        // The secret reached the spawn env (FakeAdapter records SpawnCtx).
        let envs = fake.spawn_secrets.lock().await;
        assert!(
            envs.contains(&sec1) && envs.contains(&sec2),
            "both minted secrets must have been injected into the spawn ctx: {envs:?}"
        );
    }

    /// v0.8.7 review-fix (R-M1) — the gate authenticates the `(role, secret)`
    /// PAIR, not a plaintext role. Right pair → ok; wrong/empty secret, or the
    /// right secret with a non-cto claimed role, → reject (fail-closed).
    #[tokio::test]
    async fn verify_session_caller_requires_matching_role_and_secret() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-verify");
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let secret = gateway.sessions.get(sid.as_str()).unwrap().secret.clone();

        // Correct (role, secret) pair authenticates.
        assert!(gateway.verify_session_caller("cto", &secret));
        // Wrong secret → reject.
        assert!(!gateway.verify_session_caller("cto", "deadbeefdeadbeefdeadbeefdeadbeef"));
        // Empty secret → reject (fail-closed; never fall-open).
        assert!(!gateway.verify_session_caller("cto", ""));
        // Right secret but a role no session runs → reject (pair must match).
        assert!(!gateway.verify_session_caller("reviewer", &secret));
        // A role that is not even spawned → reject.
        assert!(!gateway.verify_session_caller("ghost", &secret));
    }

    /// v0.8.8 F1 — with the (project, role) dedup removed, two sessions can run
    /// the SAME role; each is minted its own per-session secret. The gate's
    /// `(role, secret)` pair STILL isolates correctly: each session's secret
    /// authenticates as that role, and a bogus secret is rejected even though
    /// the role is live (twice). This proves verify_session_caller stays sound
    /// — it was NOT weakened to role-only when dedup was dropped.
    #[tokio::test]
    async fn verify_session_caller_isolates_two_same_role_secrets() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-verify2");
        let sid1 = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let sid2 = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_ne!(sid1, sid2, "two same-role sessions are distinct sids");
        let secret1 = gateway.sessions.get(sid1.as_str()).unwrap().secret.clone();
        let secret2 = gateway.sessions.get(sid2.as_str()).unwrap().secret.clone();
        assert_ne!(secret1, secret2, "each session mints its own secret");

        // Each session's secret authenticates as the (live) cto role.
        assert!(gateway.verify_session_caller("cto", &secret1));
        assert!(gateway.verify_session_caller("cto", &secret2));
        // A bogus secret is rejected even though cto is live (twice).
        assert!(!gateway.verify_session_caller("cto", "deadbeefdeadbeefdeadbeefdeadbeef"));
    }

    /// v0.8.8 F1 (acceptance a) — two same-role `create_session_api` calls yield
    /// distinct sids, two tracked SessionViews, and INDEPENDENT per-sid turns
    /// mirrors (a turn written under sid1 is invisible under sid2). This is the
    /// keystone "a chat can run multiple same-role sessions" guarantee.
    #[tokio::test]
    async fn two_same_role_sessions_have_distinct_sids_and_independent_turns() {
        use ccteam_harness::execution::turns_mirror::{append_turn, read_all_turns, TurnRecord};
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());

        let sid1 = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let sid2 = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_ne!(sid1, sid2, "same (project, role) → distinct sids");
        assert_eq!(
            gateway.session_views().len(),
            2,
            "two independent same-role sessions tracked"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            2,
            "each session spawns its own pane"
        );

        // Per-sid turns mirrors are independent: a turn written under sid1 is
        // not visible under sid2 (BUG-3 root: role-keyed reads bled history).
        let mk = |id: &str, who: &str| TurnRecord {
            turn_id: id.into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: "reviewer".into(),
            user: String::new(),
            assistant: who.into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
        };
        append_turn(&project_dir, &sid1, &mk("t1", "from-sid1")).unwrap();
        append_turn(&project_dir, &sid2, &mk("t2", "from-sid2")).unwrap();

        let turns1 = read_all_turns(&project_dir, &sid1).unwrap();
        let turns2 = read_all_turns(&project_dir, &sid2).unwrap();
        assert_eq!(turns1.len(), 1);
        assert_eq!(turns1[0].assistant, "from-sid1");
        assert_eq!(turns2.len(), 1);
        assert_eq!(turns2[0].assistant, "from-sid2");
    }

    /// v0.8.10 D6 — same-role sessions must route user turns by sid, not by
    /// role/current-session fallback. This is the lowest-level guard for the
    /// "two reviewers in one cwd do not cross-talk" notification invariant:
    /// each submit hits its own harness thread and each user mirror lands under
    /// the addressed sid.
    #[tokio::test]
    async fn same_role_submit_to_sid_routes_to_each_thread() {
        use ccteam_harness::execution::turns_mirror::read_all_turns;

        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());

        let sid1 = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let sid2 = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!((sid1.as_str(), sid2.as_str()), ("s1", "s2"));

        gateway
            .submit_to_sid(&sid1, "first reviewer prompt".into())
            .await
            .unwrap();
        gateway
            .submit_to_sid(&sid2, "second reviewer prompt".into())
            .await
            .unwrap();

        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[
                (
                    "alpha-reviewer-s1".to_string(),
                    "first reviewer prompt".to_string()
                ),
                (
                    "alpha-reviewer-s2".to_string(),
                    "second reviewer prompt".to_string()
                )
            ],
            "same-role submits must address each sid's own thread"
        );

        let turns1 = read_all_turns(&project_dir, &sid1).unwrap();
        let turns2 = read_all_turns(&project_dir, &sid2).unwrap();
        assert!(
            turns1
                .iter()
                .any(|turn| turn.user == "first reviewer prompt"),
            "sid1 mirror must contain only its prompt: {turns1:?}"
        );
        assert!(
            !turns1
                .iter()
                .any(|turn| turn.user == "second reviewer prompt"),
            "sid1 mirror must not receive sid2 prompt: {turns1:?}"
        );
        assert!(
            turns2
                .iter()
                .any(|turn| turn.user == "second reviewer prompt"),
            "sid2 mirror must contain only its prompt: {turns2:?}"
        );
        assert!(
            !turns2
                .iter()
                .any(|turn| turn.user == "first reviewer prompt"),
            "sid2 mirror must not receive sid1 prompt: {turns2:?}"
        );
    }

    /// v0.8.8 bug-fix (bug3) — `submit_to_sid` MIRRORS THE USER'S PROMPT to
    /// `.ccteam/chat/<sid>/turns.jsonl`. The event pump records only the
    /// assistant side, so without this user-side mirror a session reopened from
    /// history (`GET /sessions/{sid}` → `historyToRows`) showed only the agent's
    /// replies — the user's own messages "disappeared" on a session switch.
    #[tokio::test]
    async fn submit_to_sid_mirrors_the_user_prompt() {
        use ccteam_harness::execution::turns_mirror::read_all_turns;
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", project_dir.clone());

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        gateway
            .submit_to_sid(&sid, "what does this repo do?".into())
            .await
            .unwrap();

        let turns = read_all_turns(&project_dir, &sid).unwrap();
        assert!(
            turns.iter().any(|t| t.user == "what does this repo do?"),
            "the user's prompt must be persisted to turns.jsonl, got {turns:?}"
        );
    }

    /// v0.8.8 F1 (regression — closes the BUG-3 blind spot) — the gateway's
    /// live event pump WRITES `.ccteam/chat/<sid>/turns.jsonl`. Before F1 the
    /// ONLY non-test `append_turn` caller was the dead BotSupervisor (never
    /// wired by daemon.rs), so the SPA / cto-collect history read by sid found a
    /// PERMANENTLY EMPTY file even after the read-side BUG-3 fix. The absence of
    /// THIS test is exactly why that root hid. FakeAdapter emits one
    /// AgentMessage on submit → assert the row lands under the sid's mirror.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn event_pump_writes_turns_to_sid_mirror() {
        use ccteam_harness::execution::turns_mirror::read_all_turns;
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());
        // The pump only runs (and only writes) when an event sink is wired —
        // mirror the production daemon path.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        // Create a session (spawns the detached pump) and drive one turn; the
        // FakeAdapter enqueues an AgentMessage event the pump will fold + write.
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        gateway
            .submit_to_sid(&sid, "review the diff".into())
            .await
            .unwrap();

        // The pump is a detached task — poll the sid-keyed mirror until the row
        // appears (bounded, so a real failure still fails the test).
        // v0.8.8 bug3 — submit_to_sid mirrors the USER side synchronously; the
        // detached pump then appends the ASSISTANT side. Poll until the
        // assistant record lands (the user record appears first, at submit).
        let mut found = None;
        for _ in 0..100 {
            let turns = read_all_turns(&project_dir, &sid).unwrap_or_default();
            if turns.iter().any(|t| !t.assistant.is_empty()) {
                found = Some(turns);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let turns = found.expect("event pump must mirror the assistant turn to turns.jsonl");
        assert!(
            turns.iter().any(|t| t.user == "review the diff"),
            "the user's prompt is mirrored to turns.jsonl: {turns:?}"
        );
        let assistant = turns
            .iter()
            .find(|t| !t.assistant.is_empty())
            .expect("the pump mirrors the assistant reply");
        assert!(
            assistant.assistant.contains("echo: review the diff"),
            "the mirrored turn carries the assistant reply text: {:?}",
            assistant.assistant
        );
        assert_eq!(assistant.vendor, "claude");
        assert_eq!(
            assistant.role, "reviewer",
            "content-label role on the record"
        );
    }

    /// v0.8.11 E4 — a stream-json session has no chat-progress hooks, so the
    /// pump must be its progress.jsonl writer: a completed turn lands a
    /// `chat_turn_completed` event carrying the sid, so the session-list
    /// activity classifier sees it as active. (Tmux sessions get this from
    /// their Stop hook; the pump gates on protocol to avoid a double-write.)
    #[tokio::test]
    async fn stream_json_pump_mirrors_turn_to_progress_jsonl() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake, "alpha", project_dir.clone());
        gateway.enable_project_creation(paths.clone());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        // create_session_api defaults to the stream-json protocol.
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap();
        gateway
            .submit_to_sid(&sid, "do a thing".into())
            .await
            .unwrap();

        // The detached pump appends chat_turn_completed (carrying the sid) once
        // the FakeAdapter's TurnCompleted flows through. Poll the progress file.
        let progress = paths.progress_jsonl("alpha");
        let mut found = false;
        for _ in 0..100 {
            if let Ok(body) = std::fs::read_to_string(&progress) {
                if body.contains(ccteam_core::progress::CHAT_TURN_COMPLETED)
                    && body.contains(&format!("\"{sid}\""))
                {
                    found = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            found,
            "stream-json pump must mirror a chat_turn_completed carrying the sid to {}",
            progress.display()
        );
    }

    /// Startup restore can be slow (e.g. stream-json waits for `system:init`).
    /// The web API shares the same Gateway mutex, so restore work must not hold
    /// that lock while awaiting the adapter; otherwise `POST /sessions` blocks
    /// behind every stale restored session.
    #[tokio::test]
    async fn restored_session_resume_does_not_hold_gateway_lock_while_adapter_waits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_path = tmp.path().join("gateway.json");
        let project_dir = tmp.path().join("alpha");
        std::fs::create_dir_all(&project_dir).unwrap();

        let seed = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(seed, "alpha", project_dir.clone());
        gateway.enable_persistence(&state_path).unwrap();
        let sid = gateway
            .create_session_api_proto(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
            )
            .await
            .unwrap();
        assert_eq!(sid, "s1");

        let slow = Arc::new(
            FakeAdapter::new(AgentVendor::Claude)
                .with_resume_delay(std::time::Duration::from_millis(250)),
        );
        let mut restored = Gateway::new(slow.clone(), "alpha", project_dir);
        restored.enable_persistence(&state_path).unwrap();
        let gateway = Arc::new(tokio::sync::Mutex::new(restored));

        let resume_task = tokio::spawn(Gateway::resume_restored_sessions_shared(Arc::clone(
            &gateway,
        )));
        for _ in 0..50 {
            if slow.resume_started.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(slow.resume_started.load(Ordering::SeqCst), 1);

        let guard = tokio::time::timeout(std::time::Duration::from_millis(50), gateway.lock())
            .await
            .expect("gateway lock must stay available while restored resume awaits adapter");
        assert_eq!(guard.session_views().len(), 1);
        drop(guard);

        resume_task.await.unwrap();
    }

    /// v0.8.7 review-fix (R-M3) — `session_resolve(sid).project` is the value
    /// the daemon gate uses to enforce same-project scope. Confirm it reports
    /// the project a session was created in (so a project-A caller can be told
    /// a project-B sid is out of scope).
    #[tokio::test]
    async fn session_resolve_reports_owning_project_for_scope_check() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-scope");
        gateway.register_project("beta", "/tmp/beta-scope");
        let sa = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let sb = gateway
            .create_session_api(
                "beta".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let ra = gateway.session_resolve(&sa).unwrap();
        let rb = gateway.session_resolve(&sb).unwrap();
        assert_eq!(ra.project, "alpha");
        assert_eq!(rb.project, "beta");
        // v0.8.8 F1 (acceptance c) — the resolve also carries the vendor.
        assert_eq!(ra.vendor, "claude");
        assert_eq!(rb.vendor, "claude");
    }

    /// v0.8.7 review-fix (R-M6) — creating a session for a role with no
    /// `.claude/agents/<role>.md` (in a project whose agents dir DOES exist, so
    /// the test-dir exemption doesn't apply) fails with a typed
    /// [`RoleNotFound`], NOT a generic error. The web create handler downcasts
    /// this to a 422 instead of a 500.
    #[tokio::test]
    async fn create_session_unknown_role_is_role_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Real ccteam project shape: a `.claude/agents/` dir that holds `cto`
        // but NOT the role we'll request. The dir's existence flips
        // `ensure_role_exists` into strict mode.
        let agents = tmp.path().join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("cto.md"),
            "---\nname: cto\ndescription: x\n---\nbody\n",
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        let err = gateway
            .create_session_api(
                "alpha".into(),
                "no-such-role".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .expect_err("unknown role must error");
        let typed = err.downcast_ref::<RoleNotFound>();
        assert!(
            typed.is_some(),
            "error must downcast to RoleNotFound (so the web layer can 422), got: {err:#}"
        );
        assert_eq!(typed.unwrap().role, "no-such-role");
        // A seeded role in the SAME project still creates fine (not a blanket
        // reject of every create once the agents dir exists).
        assert!(gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .is_ok());
    }

    /// V0.8.6 W5b — `create_session_api` is idempotent on (project, role):
    /// a duplicate call reuses the existing pane / session id rather than
    /// spawning a second thread over the same transcript (same dedup `/new`
    /// relies on).
    #[tokio::test]
    async fn gateway_resource_api_create_mints_distinct_sids() {
        // v0.8.8 F1 — create_session_api no longer dedups on (project, role):
        // two creates of the same role mint two DISTINCT sids, each backed by
        // its own pane/pump. The web/cto "spawn another reviewer" flow relies
        // on this (and the session_spawn tool description was updated to
        // "always mints a new sid").
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        let a = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let b = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_ne!(
            a, b,
            "F1: same (project, role) must mint distinct sids, not reuse one"
        );
        assert_eq!(
            gateway.session_views().len(),
            2,
            "two independent sessions tracked"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            2,
            "each session spawns its own pane"
        );
    }

    /// v0.8.7 W2 (DB.1) — `/new claude reviewer hitl` parses the trailing
    /// token and threads `PermissionMode::Hitl` all the way to the adapter's
    /// SpawnCtx; the SessionView reports `hitl`. A plain `/new` stays skip.
    #[tokio::test]
    async fn new_command_parses_hitl_token_and_threads_mode() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let created = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer hitl")
            .await
            .unwrap();
        assert_eq!(created.len(), 1);
        assert!(
            created[0].contains("hitl"),
            "the /new receipt should mention hitl, got: {created:?}"
        );

        // The SpawnCtx that reached the adapter carried Hitl.
        assert_eq!(
            fake.spawn_modes.lock().await.as_slice(),
            &[PermissionMode::Hitl]
        );
        // The view reports the posture.
        let views = gateway.session_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].permission_mode, "hitl");
    }

    /// v0.8.8 F1 — the R-M2 "false-hitl on a reused skip pane" footgun is
    /// STRUCTURALLY GONE: there is no (project, role) reuse path anymore, so a
    /// `/new … hitl` after a live skip session ALWAYS mints a SECOND, genuinely
    /// hitl session (its own pane spawned with the hitl flag). The receipt is
    /// therefore honestly hitl — there is no live skip process being silently
    /// re-labeled. This supersedes the pre-F1
    /// `new_hitl_onto_live_skip_pane_receipt_does_not_claim_hitl` (whose
    /// premise — reuse — no longer exists).
    #[tokio::test]
    async fn new_hitl_after_live_skip_spawns_second_honest_hitl_session() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-rm2");

        // First message auto-spawns the default cto in SKIP mode (s1).
        gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();

        // Now `/new … cto hitl` — post-F1 this mints a SECOND, distinct hitl
        // session (s2), it does NOT reuse the live skip pane.
        let receipt = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto hitl")
            .await
            .unwrap();
        assert_eq!(receipt.len(), 1);
        assert_eq!(
            receipt[0], "created session s2 (hitl: non-allowlist tools need IM approval)",
            "F1: a fresh hitl session is spawned + honestly reported, got: {receipt:?}"
        );

        // TWO spawns: the original skip s1 + the new hitl s2 — the hitl request
        // genuinely started its own pane with the hitl flag (no false labeling
        // of a skip process).
        assert_eq!(
            fake.spawn_modes.lock().await.as_slice(),
            &[PermissionMode::Skip, PermissionMode::Hitl],
            "the hitl /new spawns a second, genuinely-hitl pane"
        );
        let views = gateway.session_views();
        assert_eq!(views.len(), 2, "two independent cto sessions (skip + hitl)");
        // The two same-role sessions carry their requested postures faithfully.
        let mut modes: Vec<&str> = views.iter().map(|v| v.permission_mode.as_str()).collect();
        modes.sort_unstable();
        assert_eq!(modes, vec!["hitl", "skip"]);
    }

    /// v0.8.7 review-fix (R-M2) — the inverse honest case: a FRESH `/new …
    /// hitl` (no prior pane) does spawn hitl and the receipt says so. Guards
    /// against the fix over-correcting into never reporting hitl.
    #[tokio::test]
    async fn new_hitl_fresh_spawn_receipt_reports_hitl() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-rm2b");
        let receipt = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto hitl")
            .await
            .unwrap();
        assert!(
            receipt[0].contains("non-allowlist tools need IM approval"),
            "a fresh hitl spawn must report hitl: {receipt:?}"
        );
        assert_eq!(
            fake.spawn_modes.lock().await.as_slice(),
            &[PermissionMode::Hitl]
        );
    }

    #[tokio::test]
    async fn new_command_defaults_to_skip() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(
            fake.spawn_modes.lock().await.as_slice(),
            &[PermissionMode::Skip],
            "absent trailing token ⇒ skip"
        );
        assert_eq!(gateway.session_views()[0].permission_mode, "skip");
    }

    #[tokio::test]
    async fn new_command_rejects_bad_permission_token() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        // A bad trailing token is a command error (surfaces the typo); no
        // session is spawned.
        let res = gateway
            .handle_command(
                &ChatKey::new("mock", "chat-1", "alice"),
                "/new claude reviewer bogus",
            )
            .await;
        assert!(res.is_err(), "a bad permission token must be rejected");
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            0,
            "no spawn on bad token"
        );
    }

    /// v0.8.7 W2 (DB.1) — a `/role` re-spawn preserves the session's hitl
    /// posture (the new pane re-applies the same spawn flag + hook install).
    #[tokio::test]
    async fn role_switch_preserves_hitl_mode() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // The role validation reads `.claude/agents/<role>.md` under the
        // project dir, so seed a target role on disk in a tempdir project.
        let proj = tempfile::TempDir::new().unwrap();
        let agents = proj.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("cto.md"), "---\nname: cto\n---\nx").unwrap();
        std::fs::write(agents.join("builder.md"), "---\nname: builder\n---\nx").unwrap();
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto hitl")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/role builder")
            .await
            .unwrap();

        // Two spawns: the cto (hitl) + the builder re-spawn — both hitl.
        assert_eq!(
            fake.spawn_modes.lock().await.as_slice(),
            &[PermissionMode::Hitl, PermissionMode::Hitl],
            "/role must preserve the hitl posture across the re-spawn"
        );
        // Same sid, still hitl in the view.
        let views = gateway.session_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].role, "builder");
        assert_eq!(views[0].permission_mode, "hitl");
    }

    /// v0.8.8 F1 — `session_sid_for(sid)` confirms a HITL-firing sid maps to a
    /// live tracked session (for the approval prompt label). Post-dedup the sid
    /// is the only safe identity (the firing session reports its own
    /// `CCTEAM_CHAT_SID`); an untracked sid is `None`.
    #[tokio::test]
    async fn session_sid_for_maps_project_role_to_sid() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Hitl,
            )
            .await
            .unwrap();
        assert_eq!(gateway.session_sid_for(&sid), Some(sid.sid.clone()));
        assert_eq!(gateway.session_sid_for("s99"), None);
    }

    /// v0.8.8 F1 — `reply_target_for(sid)` resolves a live session's reply
    /// target from the in-memory map, with NO on-disk registry involved. After
    /// a `/new` + a driving message the session's `reply_to` points at the chat
    /// that last drove it, so an outbound `chat_send_file` can deliver back to
    /// that chat without a prior `chat_register_bot`. Keyed by sid (the firing
    /// session reports its own `CCTEAM_CHAT_SID`).
    #[tokio::test]
    async fn reply_target_for_resolves_live_session_without_registry() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-fix1");
        // Create + drive a session from a specific chat (sets reply_to → chat).
        gateway
            .handle_text("telegram", "chat-99", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("telegram", "chat-99", "alice", "hello")
            .await
            .unwrap();
        // First (only) session is deterministically s1.
        let sid = gateway.session_views()[0].sid.clone();
        // The live target is the driving chat — resolved purely from memory.
        assert_eq!(
            gateway.reply_target_for(&sid),
            Some(("telegram".to_string(), "chat-99".to_string()))
        );
        // An untracked sid → None (caller falls back to registry).
        assert_eq!(gateway.reply_target_for("s99"), None);
    }

    /// v0.8.7 (FIX-2) — when the project HAS a `.claude/agents/` dir, a create
    /// path that names a role with no `<role>.md` is rejected fast (no session
    /// created, no dead pane), while a seeded role succeeds. This is the web
    /// `assistant`-default bug: an undefined agent must never spawn.
    #[tokio::test]
    async fn create_session_rejects_unseeded_role_when_agents_dir_present() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // Real project dir WITH an agents dir → strict validation applies.
        let proj = tempfile::TempDir::new().unwrap();
        seed_role(proj.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());

        // Unseeded role → fail fast, nothing spawned, no session tracked.
        let err = gateway
            .create_session_api(
                "alpha".into(),
                "assistant".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("assistant.md"),
            "clear hint expected; got: {msg}"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            0,
            "no spawn on bad role"
        );
        assert!(
            gateway.session_views().is_empty(),
            "rejected role must not appear in session ls"
        );

        // Seeded role → ok.
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!(sid, "s1");
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    /// v0.8.8 F2 — an EMPTY role is explicit "roleless" (bare claude reads the
    /// project's own `CLAUDE.md`), NOT a typo: `create_session_api` returns
    /// `Ok(sid)` even when the project HAS a `.claude/agents/` dir (the strict
    /// path that rejects a non-empty unseeded role). This is the mirror of
    /// `create_session_unknown_role_is_role_not_found` for the empty case.
    /// Also confirms the handle falls back to the sid (never empty, so @handle
    /// addressing can't collide / mis-match).
    #[tokio::test]
    async fn create_session_empty_role_is_ok() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // Real project dir WITH an agents dir → strict validation applies to a
        // non-empty role; an EMPTY role must still slip through (roleless).
        let proj = tempfile::TempDir::new().unwrap();
        seed_role(proj.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .expect("empty role (roleless) must create Ok, not RoleNotFound");
        assert_eq!(sid, "s1");
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "roleless still spawns"
        );

        // The session is tracked with an EMPTY role label …
        let views = gateway.session_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].role, "", "roleless session reports an empty role");
        // … but the handle fell back to the sid (non-empty), so @handle routing
        // resolves it deterministically and never collides on "".
        assert_eq!(
            gateway.session_by_handle(&web_api_chat(), &sid),
            Some(sid.sid.clone()),
            "roleless handle must fall back to the sid (addressable, non-empty)"
        );
    }

    /// v0.8.7 (FIX-2) — the test-dir exemption: a bare project dir with NO
    /// `.claude/agents/` dir (the gateway's many fake-adapter tests) skips
    /// persona validation so routing tests don't have to scaffold a role tree.
    #[tokio::test]
    async fn create_session_skips_validation_without_agents_dir() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-no-agents");
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "anything".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!(sid, "s1");
    }

    /// v0.8.7 W2 (DB.1) — a hitl session's mode survives a daemon restart:
    /// persist → reload → the restored session reports hitl. Uses a state file
    /// so the SavedGatewaySession serde round-trip is exercised.
    #[tokio::test]
    async fn hitl_mode_persists_across_reload() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = tmp.path().join("gateway-state.json");
        {
            let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
            let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
            gateway.enable_persistence(&state).unwrap();
            gateway
                .create_session_api(
                    "alpha".into(),
                    "reviewer".into(),
                    AgentVendor::Claude,
                    PermissionMode::Hitl,
                )
                .await
                .unwrap();
        }
        // Fresh gateway loading the same state file.
        let fake2 = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw2 = Gateway::new(fake2.clone(), "alpha", "/tmp/alpha");
        gw2.enable_persistence(&state).unwrap();
        let views = gw2.session_views();
        assert_eq!(views.len(), 1, "the session restored from disk");
        assert_eq!(
            views[0].permission_mode, "hitl",
            "the hitl posture must survive the persist/reload round-trip"
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
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
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
        // v0.8.8 F1 (acceptance c) — session_resolve now carries the vendor so
        // a collector / API can label the session without a second lookup.
        assert_eq!(resolved.vendor, "claude");

        // Simulate the child's answer being mirrored to turns.jsonl (in
        // production the event pump's turns writer does this; the collect tool
        // only READS it). v0.8.8 F1 — the mirror is keyed by the session sid,
        // NOT the role (BUG-3 root: reading by role bled same-role histories).
        append_turn(
            &resolved.project_dir,
            &resolved.sid,
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

        let turns = read_all_turns(&resolved.project_dir, &resolved.sid).unwrap();
        // v0.8.8 bug3 — submit_to_sid also mirrors the user prompt, so the
        // manually-simulated answer ("t1") is one of (at least) two records;
        // find it by turn_id rather than asserting a single-record file.
        let answer = turns
            .iter()
            .find(|t| t.turn_id == "t1")
            .expect("the simulated answer turn is present");
        assert_eq!(answer.assistant, "LGTM, two nits inline.");
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

    /// Task 3 — cross-frontend sharing by project: a session created by one
    /// chat (here `chat-1`; the web console / another frontend is the same
    /// owner≠querier case) is visible AND addressable from a DIFFERENT chat
    /// whose current project matches — not just its owner. So IM can see + `/use`
    /// a web-created session in the same project. (Reply routing is unchanged —
    /// it follows the per-turn submitter via `reply_to`.)
    #[tokio::test]
    async fn gateway_sessions_shared_across_frontends_by_project() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        // chat-1 creates a session in the default project "alpha".
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // chat-2 (a different frontend/user) owns no session, but its current
        // project defaults to "alpha" — so it now SEES the shared session
        // (pre-fix: owner mismatch → "no sessions").
        let seen = gateway
            .handle_text("mock", "chat-2", "bob", "/sessions")
            .await
            .unwrap();
        assert_eq!(seen, vec!["s1:alpha:Claude:reviewer"]);

        // …and can ADDRESS it: /use succeeds (addressing scope broadened too).
        let used = gateway
            .handle_text("mock", "chat-2", "bob", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["using session s1"]);

        // A chat in a DIFFERENT project does NOT see it (scope is the project,
        // not "everything"): chat-3 switches to beta, then /sessions is empty.
        gateway.register_project("beta", "/tmp/beta");
        gateway
            .handle_text("mock", "chat-3", "carol", "/cd beta")
            .await
            .unwrap();
        let other = gateway
            .handle_text("mock", "chat-3", "carol", "/sessions")
            .await
            .unwrap();
        assert_eq!(other, vec!["no sessions"]);
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
            Arc::new(
                move |vendor, _protocol| -> Arc<dyn HarnessAdapter + Send + Sync> {
                    match vendor {
                        AgentVendor::Claude => claude.clone(),
                        AgentVendor::Codex => codex.clone(),
                    }
                },
            )
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
            Arc::new(
                move |vendor, _protocol| -> Arc<dyn HarnessAdapter + Send + Sync> {
                    match vendor {
                        AgentVendor::Claude => claude.clone(),
                        AgentVendor::Codex => codex.clone(),
                    }
                },
            )
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
        // chat-2 spawns its own session (here a distinct role). v0.8.8 F1 — every
        // /new mints a fresh sid regardless of (project, role), so cross-chat
        // isolation is per-session by construction; this test happens to use
        // distinct roles for readable echo assertions.
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

        let original_secret_s1;
        let original_secret_s2;
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
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap();
            // R-M1 — the minted secret is non-empty and will be persisted.
            original_secret_s1 = gateway.sessions.get("s1").unwrap().secret.clone();
            original_secret_s2 = gateway.sessions.get("s2").unwrap().secret.clone();
            assert_eq!(original_secret_s1.len(), 32);
            assert_eq!(original_secret_s2.len(), 32);
            assert_ne!(original_secret_s1, original_secret_s2);
        }

        let mut restored = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        restored.register_project("beta", "/tmp/beta");
        restored.enable_persistence(&state_path).unwrap();

        // R-M1 — the per-session secret survives the daemon restart so the gate
        // map still matches the live pane's `CCTEAM_CHAT_SECRET`.
        assert_eq!(
            restored.sessions.get("s1").unwrap().secret,
            original_secret_s1,
            "s1 cto-gate secret must round-trip through persisted state"
        );
        assert_eq!(
            restored.sessions.get("s2").unwrap().secret,
            original_secret_s2,
            "s2 cto-gate secret must round-trip through persisted state"
        );

        let sessions = restored
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            sessions,
            vec!["s1:beta:Claude:reviewer\ns2:beta:Claude:reviewer"]
        );

        assert_eq!(
            restored
                .handle_text("mock", "chat-1", "alice", "/use s1")
                .await
                .unwrap(),
            vec!["using session s1"]
        );
        let reply_s1 = restored
            .handle_text("mock", "chat-1", "alice", "after restart")
            .await
            .unwrap();
        assert_eq!(reply_s1, vec!["beta-reviewer-s1 echo: after restart"]);

        assert_eq!(
            restored
                .handle_text("mock", "chat-1", "alice", "/use s2")
                .await
                .unwrap(),
            vec!["using session s2"]
        );
        let reply_s2 = restored
            .handle_text("mock", "chat-1", "alice", "after restart two")
            .await
            .unwrap();
        assert_eq!(reply_s2, vec!["beta-reviewer-s2 echo: after restart two"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 2);
    }

    /// v0.8.8 F1 (acceptance b) — sids are stable AND never reused across a
    /// daemon restart, even after a session is stopped. Concretely: spawn two
    /// SAME-role sessions (s1 + s2), stop s1, then rebuild the Gateway from the
    /// persisted state. The reload must (1) restore s2 (proving the load_state
    /// seen-panes collapse is gone — both same-role records survive) and (2)
    /// resume the monotonic counter so the NEXT create is s3, never recycling
    /// the freed s1. (next_session is persisted, so non-reuse holds by
    /// construction.)
    #[tokio::test]
    async fn sid_stable_and_not_reused_across_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("gateway-state.json");
        let fake = Arc::new(FakeAdapter::default());

        {
            let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-reuse");
            gateway.enable_persistence(&state_path).unwrap();
            // Two SAME-role sessions → s1 + s2 (no dedup post-F1).
            let s1 = gateway
                .create_session_api(
                    "alpha".into(),
                    "reviewer".into(),
                    AgentVendor::Claude,
                    ccteam_harness::PermissionMode::Skip,
                )
                .await
                .unwrap();
            let s2 = gateway
                .create_session_api(
                    "alpha".into(),
                    "reviewer".into(),
                    AgentVendor::Claude,
                    ccteam_harness::PermissionMode::Skip,
                )
                .await
                .unwrap();
            assert_eq!((s1.as_str(), s2.as_str()), ("s1", "s2"));
            // Free s1 (persists the removal + the bumped counter).
            gateway.stop_session("s1").await.unwrap();
            assert!(gateway.session_resolve("s1").is_none());
            assert!(gateway.session_resolve("s2").is_some());
        }

        // Rebuild from the same on-disk state.
        let mut restored = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-reuse");
        restored.enable_persistence(&state_path).unwrap();
        // s2 survived the restart (both same-role records were persisted; only
        // s1 was explicitly stopped).
        assert!(
            restored.session_resolve("s2").is_some(),
            "the surviving same-role session must restore after restart"
        );
        assert!(
            restored.session_resolve("s1").is_none(),
            "the stopped session must not resurrect"
        );
        // The NEXT create resumes the counter → s3, NEVER recycling s1.
        let s3 = restored
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!(
            s3, "s3",
            "the monotonic sid counter must persist — the freed s1 is never reused"
        );
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
        // v0.8.8 F1 — the trailing segment parses as the sid (not a role).
        assert_eq!(inv.orphans[0].sid, "zombie");
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

        // v0.8.8 F1 — the canonical pane name is keyed by the session sid
        // (`s1`), not the role: a same-role second session would otherwise
        // collide on one name. The first `/new` minted s1.
        let names = tracked_chat_session_names(&state_path).unwrap();
        assert!(
            names.contains("ccteam-chat-beta-s1"),
            "expected sid-keyed canonical chat-session name, got {names:?}"
        );
    }

    #[test]
    fn tracked_chat_sessions_empty_when_state_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.json");
        assert!(tracked_chat_sessions(&missing).unwrap().is_empty());
    }

    #[tokio::test]
    async fn tracked_chat_sessions_projects_vendor_role_sid() {
        // v0.8.8 B4/F3 — spawn one claude + one codex session through the
        // gateway, persist, then read the flat rows back out-of-process.
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("gateway-state.json");
        let fake = Arc::new(FakeAdapter::default());

        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.enable_persistence(&state_path).unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex builder")
            .await
            .unwrap();

        let rows = tracked_chat_sessions(&state_path).unwrap();
        assert_eq!(rows.len(), 2, "expected two tracked rows, got {rows:?}");

        let claude = rows
            .iter()
            .find(|r| r.role == "reviewer")
            .expect("reviewer row");
        assert_eq!(claude.vendor, "claude");
        assert_eq!(claude.permission_mode, "skip");
        assert!(claude.sid.starts_with('s'), "sid shape: {}", claude.sid);
        assert_eq!(claude.project, "alpha");

        let codex = rows
            .iter()
            .find(|r| r.role == "builder")
            .expect("builder row");
        assert_eq!(codex.vendor, "codex");
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
    /// stale startup-only project registry bug.
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

    /// v0.8.8 bug-fix — `create_session_api` (the web `POST /sessions` path)
    /// must resolve a project registered in config.yaml AFTER daemon start, the
    /// same way `/cd` does. Repro: a successful web `POST /projects` (registry
    /// write) immediately followed by `POST /sessions` used to fail "unknown
    /// project: <slug>" because `start_session` read only the stale in-memory
    /// cache. Mirrors `gateway_cd_dynamically_loads_project_from_config` but on
    /// the API create path (`ensure_project_loaded` now runs before the lookup).
    #[tokio::test]
    async fn gateway_create_session_api_loads_project_from_config() {
        use ccteam_core::config::{upsert_project, ProjectEntry};
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        // A bare project dir registered straight into config.yaml (as REST
        // `POST /projects` does) — NOT in the gateway's in-memory snapshot.
        let delta_dir = paths.projects_root.join("dev-delta");
        std::fs::create_dir_all(&delta_dir).unwrap();
        upsert_project(
            &paths.root,
            ProjectEntry {
                slug: "dev-delta".to_string(),
                path: delta_dir.clone(),
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        gateway.enable_project_creation(paths);

        // Before the fix this returned Err "unknown project: dev-delta".
        let sid = gateway
            .create_session_api(
                "dev-delta".to_string(),
                "cto".to_string(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .expect("create_session_api must resolve a config-registered project");
        assert!(sid.starts_with('s'), "expected an s<N> sid, got {sid}");
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

        // v0.8.8 F1 — the tracked pane is keyed by sid: s1 = alpha/lead →
        // ccteam-chat-alpha-s1 (NOT ...-lead). The first /new mints s1.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude lead")
            .await
            .unwrap();

        // Two live ccteam-chat-* processes injected via a fake ProcessBackend:
        // one matches the tracked session (by sid), the other is an orphan that
        // outlived a prior daemon (dashed slug to exercise the parser).
        let backend = InProcBackend::new();
        let spec =
            |name: &str| MuxSessionSpec::new(name, vec!["true".into()], PathBuf::from("/tmp"));
        backend
            .spawn(spec(&chat_session_name("alpha", "s1")))
            .await
            .unwrap();
        backend
            .spawn(spec("ccteam-chat-ghost-proj-zombie"))
            .await
            .unwrap();

        let inventory = gateway.inventory_via_backend(&backend).await.unwrap();
        assert_eq!(inventory.tracked, vec!["ccteam-chat-alpha-s1".to_string()]);
        assert_eq!(
            inventory.orphans,
            vec![OrphanSession {
                name: "ccteam-chat-ghost-proj-zombie".to_string(),
                slug: "ghost-proj".to_string(),
                // v0.8.8 F1 — trailing segment is the sid.
                sid: "zombie".to_string(),
            }]
        );

        // The global display entry lists the tracked session and flags the orphan.
        let live = ccteam_harness::list_chat_sessions(&backend).await.unwrap();
        let rendered = gateway.render_all_sessions(&live);
        assert!(
            rendered.contains("s1:alpha:Claude:lead"),
            "rendered: {rendered}"
        );
        // v0.8.8 F1 — the orphan render labels the trailing segment as the sid
        // (a daemon-outliving pane cannot recover a role from the sid-keyed
        // name, so the display shows sid — accepted UX cost).
        assert!(
            rendered.contains("orphan ccteam-chat-ghost-proj-zombie (slug=ghost-proj sid=zombie)"),
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

        // Cross-frontend sharing by project (task 3): a different Telegram chat
        // whose current project matches now SEES tg-1's session — sharing is by
        // project, not by creator. (Cross-PROJECT isolation still holds; see
        // gateway_sessions_shared_across_frontends_by_project. Reply routing is
        // unchanged — it follows the per-turn submitter via `reply_to`.)
        let other = gateway
            .handle_text("telegram", "tg-2", "bob", "/sessions")
            .await
            .unwrap();
        assert_eq!(other, vec!["s1:alpha:Claude:assistant"]);
    }

    #[tokio::test]
    async fn two_same_role_sessions_get_distinct_sids() {
        // v0.8.8 F1 — the (project, role) dedup is GONE: every `/new` mints a
        // fresh sid, so two `/new claude assistant` calls yield s1 + s2 (two
        // independent panes / pumps), NOT a reused s1. This is the keystone
        // behavior change — a chat can now run multiple same-role sessions
        // side by side. (The spawn-storm guard is ensure_current_session's
        // contains_key early-return for PLAIN messages, not this dedup — see
        // plain_messages_reuse_current_session_no_storm.)
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");

        let first = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude assistant")
            .await
            .unwrap();
        assert_eq!(first, vec!["created session s1"]);
        // Same project + role → a SECOND, distinct session s2 (no reuse).
        let again = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude assistant")
            .await
            .unwrap();
        assert_eq!(
            again,
            vec!["created session s2"],
            "F1: a repeat /new of the same role must mint a NEW sid, not reuse s1"
        );
        // A third /new (different role) → s3.
        let other_role = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(other_role, vec!["created session s3"]);

        // Three sessions tracked — two same-role (s1, s2) + one (s3).
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            listing[0].lines().count(),
            3,
            "expected 3 distinct sessions (no dedup): {}",
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
        // the target role there. v0.8.7 (FIX-2): the create path now validates
        // too, so the default `cto` (spawned by `/new`) must also be seeded.
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
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
        // `cto` (the `/new` default) + `reviewer` exist; `ghost` deliberately
        // does not. v0.8.7 (FIX-2): the create path validates the persona, so
        // the default `cto` must be seeded for `/new` to succeed.
        seed_role(tmp.path(), "cto");
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

    /// A HITL approve/deny ChoicePrompt (the exact 2-option shape the daemon's
    /// `permission/ask` handler mints): ids are the decision wire values.
    fn permission_prompt(token: &str) -> ChoicePrompt {
        ChoicePrompt {
            token: token.to_string(),
            title: "session s1 (cto) wants to run: Bash rm -rf /".to_string(),
            options: vec![
                ChoiceOption {
                    id: "allow".into(),
                    label: "✅ Approve".into(),
                },
                ChoiceOption {
                    id: "deny".into(),
                    label: "⛔ Deny".into(),
                },
            ],
            multi: false,
        }
    }

    /// v0.8.7 review-fix (R-H1) — the web HITL APPROVE path: the daemon's
    /// `permission/ask` handler registers a token-keyed External pending in the
    /// SHARED registry and blocks on its oneshot; the web `/resolve` endpoint
    /// calls `Gateway::resolve_web_selection(token, "allow")`, which routes
    /// through the SAME `take_by_token` → `apply_pending` machinery an IM click
    /// uses. The blocked hook then receives `allow` over the oneshot (→ the tool
    /// runs). This is NOT a turn; there is no gateway session for the chat,
    /// proving resolution is purely token-based. Single-flight: the pending is
    /// consumed.
    #[tokio::test]
    async fn web_resolve_approve_delivers_allow_over_oneshot() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        let token = "pcafef00d";
        let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            permission_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );

        // The web user clicks [Approve] → POST /resolve {token, selection:"allow"}.
        gateway
            .resolve_web_selection(token, "allow")
            .await
            .expect("approve resolves cleanly");

        // The blocked permission hook receives the decision (→ {behavior:allow}).
        let got = rx.await.expect("oneshot delivered");
        assert_eq!(got.ids, vec!["allow".to_string()]);
        assert_eq!(got.token, token);
        // Single-flight: pending consumed.
        assert!(
            shared.lock().await.is_empty(),
            "pending consumed on resolve"
        );
    }

    /// v0.8.7 review-fix (R-H1) — the web HITL DENY path: `[Deny]` resolves
    /// immediately to `deny` over the oneshot (no 600s timeout). Same machinery.
    #[tokio::test]
    async fn web_resolve_deny_delivers_deny_over_oneshot() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        let token = "pf00dbabe";
        let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            permission_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );

        gateway
            .resolve_web_selection(token, "deny")
            .await
            .expect("deny resolves cleanly");

        let got = rx.await.expect("oneshot delivered");
        assert_eq!(got.ids, vec!["deny".to_string()]);
        assert!(shared.lock().await.is_empty());
    }

    /// v0.8.7 review-fix (R-H1) — an unknown/expired token is a clean `Err`
    /// (the HTTP layer maps it to a 4xx), NOT a turn and NOT a panic; the
    /// registry is left untouched. Likewise a valid token with an option id
    /// that isn't in the prompt is rejected (and does NOT silently resolve).
    #[tokio::test]
    async fn web_resolve_unknown_token_and_bad_option_are_errors() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        // Unknown token → Err, registry untouched (it's empty here).
        assert!(
            gateway
                .resolve_web_selection("nope", "allow")
                .await
                .is_err(),
            "unknown token must be an Err (→ 4xx), never a turn"
        );

        // Register a real pending, then send a bogus option id → Err.
        let token = "pdeadbeef";
        let (tx, _rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            permission_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );
        assert!(
            gateway.resolve_web_selection(token, "maybe").await.is_err(),
            "an option id not in the prompt must be an Err"
        );
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
