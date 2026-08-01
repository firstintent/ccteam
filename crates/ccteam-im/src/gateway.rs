//! v8.1 IM gateway route table.
//!
//! This module owns the chat-local `project ⇄ session` state that sits
//! above the older `@handle -> mailbox` router. It is deliberately
//! daemon-agnostic: tests drive it with a fake [`HarnessAdapter`], and
//! the daemon can wire the same state machine into real transports.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use ccteam_core::config::{upsert_project, CcteamConfig, ProjectEntry};
use ccteam_core::projects::{bootstrap_project_at_dir, validate_slug_format};
use ccteam_core::{CcteamPaths, HotConfig, RoleDetail};
use ccteam_harness::{
    apply_title, atomic_write_durable, chat_session_name, discover_external_claude_sessions,
    format_tokens, list_session_metas, parse_chat_session_name, read_session_meta, truncate_title,
    write_session_meta, AccountUsage, AgentSpecBrief, AgentVendor, ChoicePrompt, ChoiceSelection,
    Directive, DirectiveOutcome, ExternalClaudeSession, HarnessAdapter, HarnessError,
    PermissionMode, ProcessBackend, RunningTask, SessionMeta, SessionOrigin, SessionProtocol,
    SessionTitleTarget, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItemDetails, TitleSource,
    TitleSync, TurnDisposition, TurnInput, TurnRouting,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::pending::InteractionOrigin;
use crate::transport::{ChannelAttachment, ChoiceReply, MessageOption};
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

    /// v0.8.18 柱2 — canonical owner identity (`"channel:chat_id"`) recorded on
    /// a project this chat creates. The `chat_id` is the free per-chat identity
    /// (Telegram chat_id / Lark open_id); `user_id` is excluded so the project
    /// is owned by the CHAT, not a single member of it.
    fn identity(&self) -> String {
        format!("{}:{}", self.channel, self.chat_id)
    }

    /// Parse `"channel:chat_id"` back into a `ChatKey`. Returns `None` if the
    /// string doesn't contain a colon.
    fn from_identity(s: &str) -> Option<Self> {
        let (channel, chat_id) = s.split_once(':')?;
        Some(Self::new(channel, chat_id, chat_id))
    }
}

/// `slug -> tenant project principal` resolved once per ACL pass (`None` =
/// operator-owned / unowned, i.e. no principal to inherit). The session gate
/// runs per LIVE session, and each resolution is a `state.json` read, so the
/// filters that walk the whole fleet thread one of these through instead of
/// re-reading the same project N times. See
/// [`Gateway::memoized_tenant_project_owner`] for why it never becomes a field.
type ProjectPrincipalMemo = std::collections::HashMap<String, Option<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOrigin {
    User,
    Internal,
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
    /// injected into the session's MCP env as `CCTEAM_CHAT_SECRET`. The session
    /// gate authenticates a forwarded `session_*` caller by matching the secret
    /// it presents (with its sid) against this stored value (see
    /// [`Gateway::verify_session_principal`]).
    /// Persisted across daemon restarts so the live session's env still matches.
    /// HONEST SCOPE: only raises the bar under the single-uid full-trust model
    /// — not a hard boundary (see `ccteam_core::session_secret`).
    secret: String,
    handle: String,
    thread: ThreadHandle,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    /// Count of VISIBLE answers (final assistant text / error) produced. The
    /// turn-timeout watchdog reads it to know the turn ANSWERED (→ stop
    /// watching, never interrupt).
    visible_events: Arc<AtomicU64>,
    /// Count of ANY pump event (assistant delta, tool-use, progress, …) — a
    /// liveness signal. The watchdog resets its idle clock whenever this
    /// ticks, so a turn that is actively streaming work is never mistaken for
    /// a stall. Only TRUE silence (no event at all for the idle window) trips
    /// the interrupt. Distinct from `visible_events`, which counts only final
    /// answers.
    activity_events: Arc<AtomicU64>,
    /// Where this session's replies go (option ①: whoever last drove it).
    /// Starts at `owner`; updated on `/use` and on every submit so a turn
    /// sent from web replies to web, one from Telegram replies to Telegram.
    /// Shared with the detached event pump / watchdog so they route live.
    reply_to: Arc<std::sync::Mutex<ChatKey>>,
    /// The inbound IM `message_id` whose 👀 ack reaction is still pending
    /// removal for the in-flight turn, or `None` when no ack is outstanding.
    /// Set (= `Some(message_id)`) when an IM turn is dispatched (alongside
    /// emitting `Reaction{on:true}`); the detached event pump TAKEs it on the
    /// turn's first event and emits `Reaction{on:false}` to clear the 👀.
    /// Shared (`Arc<Mutex>`) so the pump reads/clears the same cell the submit
    /// path set. Web turns never set it (web has no reaction). Fires exactly
    /// once per turn (TAKE → None). Best-effort: a lost clear just leaves a
    /// stale 👀, never affects delivery.
    pending_reaction: Arc<std::sync::Mutex<Option<String>>>,
    /// v0.8.19 — `/status` fleet-health tracking. When a real **Turn** is
    /// submitted (the Turn branch of `submit_resolved`, NOT a directive) this
    /// is set to `Some(Instant::now())`; the event pump clears it to `None` on
    /// every terminal canonical boundary. `/status` reads it to know whether a turn
    /// is in flight (→ 🔵 working, showing `now - start`) or the session is
    /// idle (`None` → 🟢). Shared (`Arc<Mutex>`) so the detached pump clears
    /// the same cell the submit path set. PULL-only signal — nothing acts on it
    /// except the `/status` render.
    turn_started_at: Arc<std::sync::Mutex<Option<Instant>>>,
    /// v0.9 T5 — set when a user turn is mirrored while a prior turn is still
    /// in flight (mid-turn steer). Cleared on the terminal boundary after the
    /// experience writer reads it. Shared with `mirror_user_turn` + pump.
    steered_this_turn: Arc<AtomicBool>,
    /// v0.8.19 — timestamp of the most recent pump event for this session
    /// (set next to the `activity_events` tick, on EVERY event). `/status`
    /// derives the 🔴 stuck state the same way the turn-timeout watchdog does:
    /// a turn is in flight (`turn_started_at == Some`) yet the last event is
    /// older than the idle window (`gateway_turn_timeout_duration`) ⇒ silent ⇒
    /// STUCK. Shared so the pump and `/status` read the same cell.
    last_event_at: Arc<std::sync::Mutex<Option<Instant>>>,
    /// v0.8.19 — the latest compact activity summary (`read×16·bash×8` form,
    /// NOT the full progress text) for the in-flight turn, computed by the
    /// SAME [`crate::progress::ProgressFold`] the IM status message uses (so the
    /// two never drift). Updated by the pump whenever the fold changes; cleared
    /// to `None` on `TurnCompleted`. `/status` appends it on the 🔵 working line.
    latest_activity: Arc<std::sync::Mutex<Option<String>>>,
    /// v0.8.x (concurrency review §4.1 P1) — the in-flight turn's watchdog
    /// arm: `Some((turn_id, start_visible_events))` from the moment the turn is
    /// known to be RUNNING (`after_turn_submitted` for a `Started`/`Injected`
    /// submission; the pump's canonical `TurnStarted` for one that was queued
    /// behind a predecessor) until the session's own event pump either sees it
    /// answer (`visible_events` moves past `start_visible_events`) or fires the
    /// one-shot stall warning for it. A queued turn is never armed — it is not
    /// silent, it is waiting.
    /// Folds the old detached per-turn `spawn_turn_timeout_watchdog` task into
    /// the pump's own `tokio::select!` loop (one fewer task per turn); kept
    /// separate from `turn_started_at` (directive-driven turns intentionally
    /// never touch that field, but DID get a watchdog before this fold — this
    /// preserves that).
    watched_turn: Arc<std::sync::Mutex<Option<(String, u64)>>>,
    /// Origin of submitted vendor turns, keyed by the vendor's turn id. Human
    /// IM/web submit paths record `User`; A2A/delegation/pending-drain paths
    /// record `Internal`. The event pump consumes the entry at the vendor turn
    /// boundary; an untracked harness wake-up therefore defaults to internal.
    turn_origins: Arc<std::sync::Mutex<BTreeMap<String, TurnOrigin>>>,
    /// v0.9.0 W2 (F2) — delegation parent sid (the spawner's principal). `None`
    /// for a human-created (root) session. Mirrors `meta.parent_sid`, kept
    /// in-memory so the guardrail child/delegated counts + the stop-descendant
    /// walk are pure live-map scans (no per-session meta read).
    parent_sid: Option<String>,
    /// v0.9.0 W2 (F2/F5) — delegation depth (root = 0; child = parent + 1).
    /// Mirrors `meta.delegation_depth`; the source for a child's depth on its
    /// next spawn and the `delegation.max_depth` guardrail.
    delegation_depth: u32,
}

/// Snapshot used by the pure live-capacity eviction selector.
///
/// `last_active` is the PERSISTED `meta.json.last_active` (RFC3339,
/// lexically sortable), refreshed on every completed turn by
/// `refresh_session_activity_meta` — deliberately NOT the in-process
/// `GatewaySession::last_event_at` (`Instant`), which resets to `None` every
/// time a session is (re)constructed in memory (daemon restart, cold `/use`
/// resume). Under the old `Instant`-based ranking, `None` was treated as
/// "newest" (a reasonable default for a session that TRULY just spawned), but
/// a rebuilt/restored session also starts at `None` despite having a real,
/// possibly ancient, history — making a long-dormant-but-still-live session
/// permanently eviction-immune (it always looked "freshest") while genuinely
/// recently-used sessions got evicted around it. `last_active` fixes this: it
/// is accurate across restarts/rebuilds because it lives on disk, not in the
/// process. Empty (unreadable/never-written meta) sorts FIRST — a fail-safe
/// toward evictable, not toward immunity.
#[derive(Debug, Clone)]
struct LiveCapacityCandidate {
    sid: String,
    idle: bool,
    last_active: String,
    waiting_approval: bool,
}

/// Select the least-recently-active eligible live session. Idle sessions are
/// always preferred; busy sessions are considered only when no idle candidate
/// remains. Among ties, an older (lexically smaller) `last_active` wins; a sid
/// compare is the final deterministic tiebreak. Sids and HITL waiters are
/// excluded before ordering.
fn select_live_capacity_eviction(
    candidates: &[LiveCapacityCandidate],
    excluded: &HashSet<String>,
) -> Option<String> {
    candidates
        .iter()
        .filter(|candidate| !excluded.contains(&candidate.sid) && !candidate.waiting_approval)
        .min_by(|a, b| {
            b.idle
                .cmp(&a.idle)
                .then_with(|| a.last_active.cmp(&b.last_active))
                .then_with(|| a.sid.cmp(&b.sid))
        })
        .map(|candidate| candidate.sid.clone())
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

/// How a platform's operator roster was configured. `Named` lists the owner's
/// chats; `Wildcard` (`"*"`) is an explicit "anyone may talk", which names
/// nobody and therefore grants nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OperatorBinding {
    Named(BTreeSet<String>),
    Wildcard,
}

/// What [`Gateway::bind_operator_allowlist`] made of an allowlist — the daemon
/// turns this into a startup warning when a reachable bot names no owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorBindingKind {
    /// Concrete chat ids: those chats are the operator.
    Named,
    /// `"*"` — the bot is open, so nobody is the operator through it.
    Wildcard,
    /// No allowlist at all (pre-configuration / open mode): the legacy
    /// single-operator assumption still applies.
    Unconfigured,
}

/// Who a frontend chat speaks for. Resolved once, in [`Gateway::principal`],
/// and consumed by every ACL decision — the IM twin of the web
/// [`crate::auth::Identity`]. `Guest` is the fail-closed default: an
/// unrecognised chat is never the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Principal {
    /// The box owner: the admin web console, or a chat NAMED in a global bot's
    /// operator roster.
    Operator,
    /// A registered per-user tenant (`user:<id>`), via its own bot or web token.
    Tenant(String),
    /// Reached a bot but is named nowhere. Owns only what it creates.
    Guest(String),
}

/// In-memory v8.1 route table for one daemon process.
pub struct Gateway {
    adapter_factory: Arc<
        dyn Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync,
    >,
    default_project: String,
    /// v0.8.21 Wave-2 — persisted gateway ROUTING snapshot
    /// (`~/.ccteam/state/gateway/routing.json`): per-chat focus + the set of
    /// sids that were live at last persist. Session CONTENT lives in each
    /// session's `meta.json` (the SoT), never here. `None` ⇒ in-memory only.
    routing_path: Option<PathBuf>,
    /// v0.8.21 Wave-2 — persisted monotonic session-id counter
    /// (`~/.ccteam/state/sessions/next-sid`). Its own file so "sid never
    /// reused" survives a wiped routing table / purged meta.json set.
    next_sid_path: Option<PathBuf>,
    /// Daemon-wide monotonic scheduled-message counter (`d{n}`).
    next_scheduled_path: Option<PathBuf>,
    /// v0.8.21 Wave-2 — sids that were live at last persist, stashed by
    /// `load_state` (sync) for the async `resume_restored_sessions` step to
    /// cold-start rebuild from their `meta.json`. Drained once on startup.
    restore_pending: Vec<String>,
    projects: BTreeMap<String, PathBuf>,
    /// The chats that speak for the box OWNER, per platform (`"telegram"` →
    /// `{"339498819"}`, `"lark"` → `{"ou_…"}`). Fed from each global bot's
    /// credential allowlist by the daemon (see [`Self::bind_operator_chats`]).
    ///
    /// The operator is a NAMED principal: reaching a bot never confers it.
    /// Before this, `project_acl_identity` returned admin for every chat that
    /// was not a tenant bot, so the transport allowlist silently doubled as the
    /// privilege grant — and an OPEN bot (empty list, or Lark's `"*"` wildcard)
    /// handed operator rights to anyone who found it. An unnamed chat is a
    /// [`Principal::Guest`] instead: it owns only what it creates and sees no
    /// project, so it cannot reach anything of the owner's or a tenant's.
    operator_chats: BTreeMap<String, OperatorBinding>,
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
    next_scheduled: u64,
    /// Durable scheduled rows indexed by globally unique short id.
    scheduled_items: BTreeMap<String, ScheduledEntry>,
    /// Wakes the lightweight next-fire timer after create/cancel/GC.
    scheduled_notify: Arc<tokio::sync::Notify>,
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
    /// Signal to the daemon's IM-reload task that `credentials.json` changed
    /// and the credential-driven channel listeners should be rebuilt in place
    /// (no daemon restart, no agent-session restart). Wired by the daemon via
    /// [`Gateway::set_im_reload_trigger`]; `None` on the standalone / test path
    /// (where there is no reload task), so [`Gateway::request_im_reload`] is a
    /// safe no-op that returns `false`.
    im_reload_tx: Option<tokio::sync::mpsc::Sender<()>>,
    /// v0.8.x (concurrency review §4.1 P1) — per-chat spawn single-flight for
    /// the inbound hot path (see [`SpawnClaims`]); deliberately its own lock,
    /// never held under the gateway's own mutex for longer than a map lookup.
    spawn_claims: Arc<SpawnClaims>,
    /// v0.8.24 Track D — optional satellite agent proxy for remote-host
    /// stdio spawn. `None` ⇒ production [`crate::remote_host::HttpRemoteHostProxy`].
    remote_host_proxy: Option<std::sync::Arc<dyn crate::remote_host::RemoteHostProxy>>,
    /// Optional programmatic snapshot for the local vendor-availability
    /// preflight. Production leaves this `None` and runs the cached live probe;
    /// deterministic lib tests inject the stub adapters' declared availability
    /// without mutating process environment or depending on `PATH`.
    local_vendor_availability_override: Option<Vec<ccteam_core::VendorAvailability>>,
    /// v0.9.0 W2 (F2/F7) — in-memory mirror of the durable delegation watches
    /// (`child_sid → mirror`). The SoT is each child's
    /// `<project>/.ccteam/chat/<child_sid>/delegation.json`; this mirror keeps
    /// the completion-notification hot path (checked on every watched child
    /// turn) off the filesystem. Rebuilt from disk by the startup reconcile.
    delegations: std::collections::HashMap<String, DelegationMirror>,
    /// v0.9.0 W2 (F7) — idempotency cache for `session_spawn` (per-project
    /// `key → response body`). In-memory only (honest: a daemon restart forgets
    /// keys); within one lifetime a replay returns the original body + zero
    /// side effects.
    spawn_idem: crate::delegation::IdemCache,
    /// v0.9.0 W2 (F7) — idempotency cache for `session_dispatch` (per-child
    /// `key → response body`). Same honest in-memory scope as `spawn_idem`.
    dispatch_idem: crate::delegation::IdemCache,
    /// v0.9.0 W2 (F2) — sender the detached event pumps use to signal a
    /// completed child turn to the delegation notifier task (which owns a
    /// gateway handle and delivers the completion notification off the pump).
    /// `None` until [`Gateway::set_delegation_notifier_tx`] wires it.
    delegation_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::delegation::DelegationSignal>>,
    /// v0.9.0 W2 (F5) — optional programmatic override of the delegation
    /// guardrail posture. `None` (production) → read `config.yaml` (else
    /// defaults). Set by [`Gateway::set_delegation_config`] (tests use it to
    /// exercise the guardrails without spawning up-to-the-limit sessions).
    delegation_config_override: Option<ccteam_core::DelegationConfig>,
    /// v0.9.2 — optional programmatic capacity override. Production reads the
    /// hot config; deterministic gateway tests use this to exercise a tiny cap.
    sessions_config_override: Option<ccteam_core::SessionsConfig>,
}

#[derive(Debug, Clone)]
struct ScheduledEntry {
    project_dir: PathBuf,
    item: crate::scheduled::ScheduledItem,
}

/// v0.9.0 W2 (F2/F7) — in-memory mirror of one child's durable delegation
/// watch, for the hot completion-notification path (avoids a filesystem read
/// per completed child turn). The durable SoT is the child's
/// `delegation.json`; this is rebuilt from it on startup and updated in
/// lockstep with every durable write.
#[derive(Debug, Clone)]
struct DelegationMirror {
    /// sid of the session to notify on a watched turn (the dispatcher).
    parent_sid: String,
    /// When completion delivers a notification turn — `final` (turn boundary
    /// only, default) / `all` (every mirrored message) / `off` (ledger-only).
    notify: ccteam_harness::NotifyMode,
    /// Optional dispatch label (ledger/notification only — never a prompt).
    title: Option<String>,
    /// Project slug hosting the child's `delegation.json` + `turns.jsonl`.
    slug: String,
    /// Project dir hosting the child's `delegation.json` + `turns.jsonl`.
    project_dir: PathBuf,
    /// Child turns already notified — the at-least-once dedup set (mirrors the
    /// durable `DelegationWatch.notified_turns`).
    notified_turns: Vec<String>,
}

/// One structured step of session activity (v0.8.19). Carried by
/// [`GatewayEventKind::Activity`] alongside the folded `Progress` event: IM
/// ignores it (the folded status string still drives the status message),
/// the web chat renders it as activity cards. The `summary` is computed by
/// the SAME [`crate::progress`] helpers the IM fold uses, so the two
/// surfaces can never drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionActivity {
    /// What kind of step this is (tool call / thinking / file change …).
    pub kind: ActivityKind,
    /// Tool / category name; empty for [`ActivityKind::Thinking`].
    pub name: String,
    /// One-line human summary (the same preview `progress.rs` computes).
    pub summary: String,
    /// Lifecycle phase (started / completed / update) of this step.
    pub status: ActivityStatus,
    /// Adapter item id — lets the web dedup/merge a start↔complete pair.
    pub item_id: String,
}

/// The category of one [`SessionActivity`] step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// A tool invocation (`ToolCall` item).
    ToolCall,
    /// A tool's result (reserved; not emitted on stream-json yet).
    ToolResult,
    /// Model reasoning / thinking (`Reasoning` item).
    Thinking,
    /// A file edit/write (`FileChange` item).
    FileChange,
    /// A web search (`WebSearch` item).
    WebSearch,
    /// A shell command execution (`CommandExecution` item).
    CommandExec,
}

/// Lifecycle of one [`SessionActivity`] step, mapped from the source
/// [`ThreadEvent`] variant (`ItemStarted`→`Started`, `ItemCompleted`→
/// `Completed`, `ItemUpdated`→`Update`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    /// The step has just started (`ItemStarted`).
    Started,
    /// The step has completed (`ItemCompleted`).
    Completed,
    /// The step was updated in place (`ItemUpdated`, e.g. streamed reasoning).
    Update,
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
    /// Structured per-step activity (v0.8.19). Reuses the turn's Progress
    /// `status_key`. IM ignores it (the folded Progress event still drives
    /// the status message); web renders it as activity cards.
    Activity {
        /// The current progress epoch's `status_key` (`{sid}-{epoch}`), so
        /// the web can correlate an activity with its turn.
        status_key: String,
        /// The structured per-step activity payload the web renders.
        activity: SessionActivity,
    },
    /// The transient 👀 "received, processing" reaction on the inbound IM
    /// message that drove a turn. `on: true` adds it the moment the turn is
    /// dispatched (filling the silent time-to-first-token gap); `on: false`
    /// removes it the moment the turn's first event appears. **IM-only**: the
    /// daemon egress maps it to the channel's `add_reaction`/`remove_reaction`
    /// (default no-op for web/discord/slack), and the web SSE drops it (a
    /// reaction has no web representation — web has its own UI). The
    /// `GatewayEvent` already carries `channel`/`chat_id`/`sid`; this only adds
    /// the inbound `message_id` to react to.
    Reaction {
        /// The inbound IM message id to react to (Telegram `message_id`, Lark
        /// `om_…`). Channel-local; the daemon egress passes it verbatim to the
        /// provider.
        message_id: String,
        /// `true` = add the ack reaction; `false` = remove it.
        on: bool,
    },
    /// v0.9.0 W4 (F4) — a delegation lifecycle transition, broadcast for the
    /// web team view's global SSE (`GET /api/v1/agents/events`). **IM-only
    /// delivery ignores this** (no channel representation — mirrors
    /// `Activity`); `progress.jsonl`'s `delegation_*` events (schema owned by
    /// `progress_bridge`) remain the durable SoT, this is the live-fan-out
    /// twin emitted at the SAME call sites (see
    /// [`Gateway::emit_delegation_progress`]).
    Delegation {
        /// One of `spawned|dispatched|completed|notified|collected|stopped|
        /// denied` (the `delegation_*` progress-event suffix).
        relation: String,
        /// The dispatching/spawning session's sid.
        parent_sid: String,
        /// The delegated session's sid (empty for a pre-spawn `denied`, which
        /// has no child yet).
        child_sid: String,
        /// Optional dispatch/spawn label (ledger/display only — never a
        /// prompt).
        title: Option<String>,
        /// Present only for `denied` (`depth|children|delegated|cycle|budget`).
        reason: Option<String>,
    },
    /// v0.9.2 — a session lifecycle transition for live web surfaces. These
    /// frames are broadcast-only (no IM delivery); the durable state twin is
    /// whatever already persisted the change — a progress event
    /// (`session_evicted`) or `meta.json` itself (`renamed`).
    SessionLifecycle {
        /// Lifecycle state: `evicted` | `renamed`.
        state: String,
        /// Machine-readable cause: `capacity` (evicted) | `user` (renamed).
        reason: String,
    },
    /// A scheduled queue changed for this sid. Broadcast-only; web re-fetches
    /// the authoritative list instead of receiving queue contents over SSE.
    ScheduledChanged,
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
    /// v0.9.0 W4 (F4) — project slug owning this event, when known. The ACL
    /// key for the team view's global SSE (`GET /api/v1/agents/events`): a
    /// tenant only sees frames whose `slug` is one of their visible
    /// projects; a frame with no `slug` is dropped for a tenant (fail-closed)
    /// and shown only to an admin. Populated for delegation events (the
    /// project the delegation lives in) and the ordinary per-session events
    /// where the emitting session's project is cheaply known; `None`
    /// elsewhere (additive field, never blocks IM delivery which ignores it).
    pub slug: Option<String>,
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

    /// Send a delivery-only event to the daemon egress without publishing it
    /// to either web SSE ring. Async web-answer IM mirrors use this path: the
    /// original sid-keyed Answer already feeds SSE, while the duplicate is a
    /// phone notification only.
    fn send_delivery_only(&self, event: GatewayEvent) -> bool {
        self.mpsc.send(event).is_ok()
    }
}

/// Everything a HITL approval prompt needs to render + route for one
/// session, resolved in one shot by [`Gateway::hitl_prompt_context_for`]
/// (v0.8.22 P0-2). Shared by BOTH Claude HITL surfaces — the terminal
/// protocol's `permission/ask` (over mcp.sock) and the stream-json
/// protocol's in-process `can_use_tool` resolver — so they render the exact
/// same prompt shape from the exact same session state.
#[derive(Debug, Clone)]
pub struct HitlPromptContext {
    /// IM/web channel to render the approve/deny prompt on.
    pub channel: String,
    /// Platform chat/recipient id within `channel`.
    pub chat_id: String,
    /// The session's role (persona), for the "session sX (role) wants to
    /// run …" label.
    pub role: String,
    /// The owning project's `progress.jsonl` path, for the best-effort
    /// `chat_permission_prompt_outstanding` operator-visibility line.
    /// `None` when the gateway was never given project paths (unit tests).
    pub progress_path: Option<PathBuf>,
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
// `Eq` dropped (v0.8.22 P1): `cost_usd: Option<f64>` is PartialEq-only (`f64`
// has no total order → no `Eq`). Every existing comparison site uses
// `assert_eq!`/`==`, which only need `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// v0.8.22 P0-3 — RFC3339 spawn time, read from `meta.json`. Empty when
    /// the meta can't be resolved (never blocks the listing). `#[serde(default)]`
    /// keeps older clients tolerant.
    #[serde(default)]
    pub created_at: String,
    /// v0.8.22 P0-3 — RFC3339 last-turn-completion time, read from
    /// `meta.json`. Drives the SPA/IM "recency" sort and relative-time
    /// display; empty when the meta can't be resolved.
    #[serde(default)]
    pub last_active: String,
    /// v0.8.22 P1 — user-facing session title (session-title system), read
    /// from `meta.json`. `None` until the first user message is auto-titled
    /// or a vendor/explicit title is set — callers fall back to `role`/`sid`
    /// display. `#[serde(default)]` keeps older clients tolerant.
    #[serde(default)]
    pub title: Option<String>,
    /// v0.8.22 P1 — turns.jsonl line count, read from `meta.json`.
    #[serde(default)]
    pub turn_count: u64,
    /// v0.8.22 P1 — accrued priced cost (USD), read from `meta.json`. `None`
    /// when no turn has priced yet (never a faked `0.0`).
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// v0.9.5 feedback fix — accrued raw token count from `meta.json`
    /// (`tokens_total`): the honest ledger number for vendors with no USD
    /// price table. `None` when no turn reported usage. `#[serde(default)]`
    /// keeps older clients tolerant.
    #[serde(default)]
    pub tokens_total: Option<u64>,
    /// Opaque model requested at spawn; omitted when the vendor default was
    /// requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// v0.8.23 review §1.3-D item 9 — true when a HITL approval is currently
    /// outstanding for this sid ([`crate::pending::PendingInteractions::pending_for_sid`]).
    /// Drives the "等待批准" attention badge (web rail/history rows) and the
    /// IM `/sessions` top-pin. Cheap best-effort: [`Gateway::session_views`]
    /// reads this via `try_lock` (never blocks on the async pending registry
    /// — a momentary contention just omits the flag for that one call, never
    /// panics/blocks). `#[serde(default)]` keeps older clients tolerant.
    #[serde(default)]
    pub waiting_approval: bool,
    /// v0.9.0 W2 (F2) — delegation parent sid (the spawner's principal), or
    /// `None` for a human/root session. Drives the `/sessions` + team-view
    /// tree. `#[serde(default)]` keeps older clients tolerant.
    #[serde(default)]
    pub parent_sid: Option<String>,
    /// v0.9.0 W2 (F2/F5) — delegation depth (root = 0). `#[serde(default)]`.
    #[serde(default)]
    pub delegation_depth: u32,
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
}

/// The one carriage for an explicit model / reasoning-effort choice made at
/// spawn time. `None` fields keep the existing defaults (role frontmatter
/// `model:` for the model, vendor default for effort). Passed by value
/// through `create_session_api_on_host` → `start_session` →
/// `plan_new_session`, and filled by every entry point: REST
/// (`spawn_tuning_from_form`), MCP `session_spawn`, IM `/new model= effort=`.
///
/// **No entry point may second-guess it.** Both facets ride to the vendor
/// verbatim for every vendor; ccteam does not filter them against any cached
/// capability table, because a dropped facet reaches the caller as a
/// successful spawn that quietly ran at the default, while an unsupported
/// token reaches them as the vendor's own error — the second is the honest
/// one. Discovery affordances (`GET /api/v1/models`, the MCP `status` panel)
/// exist so a caller can pick well; they are never a gate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnTuning {
    /// Explicit model id; overrides the role's `model:` frontmatter.
    pub model: Option<String>,
    /// Explicit reasoning-effort token (vendor-specific value set — the
    /// vendor validates it; see `SpawnCtx::effort`).
    pub effort: Option<String>,
}

impl SpawnTuning {
    /// Normalize: trim + drop empty strings so `Some("")` never leaks.
    fn normalized(self) -> Self {
        let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Self {
            model: clean(self.model),
            effort: clean(self.effort),
        }
    }
}

/// Result returned by the web/resource session creation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSessionOutcome {
    /// The freshly minted gateway session id (`s{n}`).
    pub sid: String,
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

/// What [`Gateway::rename_session`] hands its callers — everything both
/// frontends need to render an honest receipt without re-deriving anything:
/// the title actually stored (rule-truncated), what it replaced, and what the
/// VENDOR did with it. Kept as one value so the IM receipt and the web PATCH
/// response describe the same rename in the same terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRename {
    /// The renamed session's sid.
    pub sid: String,
    /// The cleaned title actually persisted to `meta.json`.
    pub title: String,
    /// The title this replaced; `None` when the session had none yet.
    pub previous: Option<String>,
    /// Vendor, stringified — the subject of `vendor_sync`'s message.
    pub vendor: String,
    /// Whether the vendor's own title surface took the new title.
    pub vendor_sync: TitleSync,
}

/// v0.9.0 W1 (F1) — the authenticated identity of a `session_*` MCP caller,
/// resolved by [`Gateway::verify_session_principal`] from the `(sid, secret)`
/// PRINCIPAL the caller presents. Generalizes the retired cto-only
/// `(role, secret)` gate: authorization is now "any live session that holds
/// this secret", with `role` demoted to an audit/display label. The gate reads
/// `slug` from the resolved session (never the caller-supplied `_caller_slug`)
/// so a caller can only operate its OWN project.
///
/// HONEST SCOPE unchanged: under the single-OS-uid full-trust model this only
/// RAISES THE BAR (a same-uid process can read another's env / files and
/// recover the secret); it is NOT a hard boundary. Real isolation = per-agent
/// OS user / sandbox (deferred). See `ccteam_core::session_secret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerCtx {
    /// The caller session's own gateway sid (`s{n}`).
    pub sid: String,
    /// The caller session's project slug — the authoritative project scope.
    pub slug: String,
    /// The caller session's role — audit/display label only (not authorization).
    pub role: String,
    /// v0.9.0 W2 (F2/F5) — the caller session's delegation depth (root = 0),
    /// read from the live map. The child's depth = this + 1; the
    /// `delegation.max_depth` guardrail caps it.
    pub depth: u32,
}

/// v0.9.0 W2 (F2/F5) — the resolved delegation parent for a `session_spawn`.
/// `Some` for an Ambient (agent-initiated) spawn — the child links to it and
/// the F5 guardrails apply; `None` for an Admin/human spawn (root, unrestricted).
#[derive(Debug, Clone)]
pub struct DelegationParent {
    /// The spawning principal's sid (the child's `parent_sid`).
    pub sid: String,
    /// The spawning principal's delegation depth (child depth = this + 1).
    pub depth: u32,
    /// The spawning principal's role at delegation time (audit label).
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewayRoute {
    chat: ChatKey,
    value: String,
}

/// v0.8.21 Wave-2 — the gateway's persisted ROUTING snapshot
/// (`~/.ccteam/state/gateway/routing.json`). Holds only the transient per-chat
/// focus (`default_project` / `current_project` / `current_session`) plus
/// `live_sids` (the set of sessions resident in the live map at last persist).
/// Session CONTENT is NOT here — it lives in each session's `meta.json` (the
/// SoT); on restart `live_sids` is cold-start rebuilt from those meta files.
/// This struct replaces the retired `SavedGatewayState.sessions` vec.
#[derive(Debug, Serialize, Deserialize)]
struct RoutingState {
    default_project: String,
    current_project: Vec<SavedGatewayRoute>,
    current_session: Vec<SavedGatewayRoute>,
    /// sids live at last persist; rebuilt from meta.json on next start.
    live_sids: Vec<String>,
}

/// v0.8.21 Wave-2 — the sync-computed inputs needed to spawn + insert a
/// rebuilt session. Splitting "plan" (sync, cheap) from "spawn" (the slow
/// `start_thread` await) lets the batch-restore path run the await OUTSIDE the
/// gateway lock — so a concurrent web `POST /sessions` never waits behind a
/// stale session's `system:init` — while the on-demand resume path runs the
/// same core under its already-held lock. One construction site, no drift.
struct MetaRebuildPlan {
    sid: String,
    slug: String,
    role: String,
    vendor: AgentVendor,
    protocol: SessionProtocol,
    host: String,
    wire_slug: String,
    permission_mode: PermissionMode,
    parent_sid: Option<String>,
    delegation_depth: u32,
    /// Canonical owner (from meta, else the rebuild's reply target).
    owner: ChatKey,
    /// @mention handle = role, else sid for a roleless session.
    handle: String,
    model_id: Option<String>,
    /// Replayed from `meta.effort`, exactly like `model_id` above.
    effort: Option<String>,
    secret: String,
    cwd: PathBuf,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    /// v0.9.0 W3 (G10) — snapshot of `Gateway::project_paths`/`remote_host_proxy`
    /// (cheap clones, taken sync under the lock) so [`Gateway::spawn_for_plan`]
    /// — self-less, runs OUTSIDE the lock in the batch-restore path — can
    /// re-gate a non-local `host` right before the slow spawn: online +
    /// registered → `SpawnCtx::remote` is populated; offline/unregistered →
    /// readable `Err`, and the caller must NOT fall back to a local respawn.
    ccteam_root: Option<PathBuf>,
    remote_proxy: Option<Arc<dyn crate::remote_host::RemoteHostProxy>>,
}

/// v0.8.x (concurrency review §4.1 P1) — the sync-computed inputs needed to
/// spawn + insert a BRAND NEW session (fresh `/new`, the implicit
/// first-message spawn `ensure_current_session` drives, and a bot template).
/// Same split as [`MetaRebuildPlan`]: "plan" (sync, cheap — resolves the
/// project/role/model, mints the sid + secret) is separated from "spawn" (the
/// slow `start_thread` await) so a caller that holds the gateway across a
/// wider critical section (today: every existing caller) can still do so
/// unchanged, while the hot-path entry point
/// ([`Gateway::handle_message_shared`]) drops the lock between the two. One
/// construction site (`plan_new_session`), no drift between the inline and
/// lock-dropping callers.
struct NewSessionPlan {
    id: String,
    /// Canonical resource owner used by session ACL and persisted meta.
    owner: ChatKey,
    /// Concrete frontend route used for async answer delivery.
    reply_to: ChatKey,
    project: String,
    vendor: AgentVendor,
    role: String,
    handle: String,
    permission_mode: PermissionMode,
    protocol: SessionProtocol,
    /// v0.8.24 Track D — host axis (`local` or registered satellite id).
    host: String,
    /// Satellite-local slug used only for the remote gate / ExecSpec cwd.
    wire_slug: String,
    secret: String,
    cwd: PathBuf,
    model_id: Option<String>,
    /// Explicit reasoning effort. Persisted in `meta.effort` alongside
    /// `meta.model`, so a resume / role switch / rebuild replays the pick
    /// instead of quietly reverting one axis to the vendor default.
    effort: Option<String>,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    /// v0.9.0 W2 (F2) — delegation parent sid (the spawning principal). `None`
    /// for a human/root spawn. Threaded into `meta.parent_sid` + the live
    /// `GatewaySession.parent_sid`.
    parent_sid: Option<String>,
    /// v0.9.0 W2 (F2) — the spawning principal's role at delegation time (audit
    /// label; `None` for a root spawn).
    spawned_by_role: Option<String>,
    /// v0.9.0 W2 (F2/F5) — delegation depth (root = 0; child = parent + 1).
    delegation_depth: u32,
    /// v0.9.0 W2 (F2) — explicit session title from `session_spawn` (ledger /
    /// display). `None` → auto-titled from the first message. Never a prompt.
    title: Option<String>,
    /// v0.9.0 W3 (F3) — the exec-bridge target when `host != local`, already
    /// resolved (+ gated) by the caller's `prepare_host_for_spawn` await
    /// BEFORE `plan_new_session` mints the sid (a fresh spawn must never
    /// burn an sid on a host that turns out offline). `None` for every
    /// caller that hardcodes `host = "local"` (the implicit first-message
    /// spawn and bot templates). Threaded verbatim into `SpawnCtx::remote`.
    remote: Option<ccteam_harness::RemoteExecTarget>,
    ccteam_root: Option<PathBuf>,
    remote_proxy: Option<Arc<dyn crate::remote_host::RemoteHostProxy>>,
}

/// v0.9 T2 — sync-computed inputs needed to resume a dead child in place.
/// Same three-phase split as [`NewSessionPlan`]: plan (under lock) → spawn
/// (no lock) → apply (re-lock + generation check). `generation` is a race
/// marker from the OLD thread (`identity@started_at`); apply discards the
/// freshly spawned thread if the session vanished or was replaced meanwhile.
struct ResumeDeadPlan {
    session_id: String,
    project: String,
    role: String,
    vendor: AgentVendor,
    protocol: SessionProtocol,
    /// v0.9.0 W3 (G10) — the session's host (from the live `GatewaySession`,
    /// which mirrors `meta.host`); `"local"` for the overwhelming majority.
    host: String,
    wire_slug: String,
    permission_mode: PermissionMode,
    secret: String,
    cwd: PathBuf,
    model_id: Option<String>,
    /// Replayed from `meta.effort`, exactly like `model_id` above.
    effort: Option<String>,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    /// Race marker: `format!("{identity}@{started_at}")` of the thread that
    /// was live when we planned. Apply aborts if the map's thread no longer
    /// matches (someone else resumed/replaced) or the sid is gone (stop).
    generation: String,
    /// v0.9.0 W3 (G10) — see `MetaRebuildPlan::ccteam_root` / `remote_proxy`;
    /// same re-gate contract, performed by `spawn_for_resume_plan`.
    ccteam_root: Option<PathBuf>,
    remote_proxy: Option<Arc<dyn crate::remote_host::RemoteHostProxy>>,
}

/// What [`Gateway::plan_ensure_current_session`] decided: either the chat
/// already has a session (nothing to do), or a brand-new one must be spawned
/// (the caller runs [`Gateway::spawn_for_new_session_plan`] + applies it).
enum EnsureSessionOutcome {
    AlreadyHasSession,
    // Boxed — `NewSessionPlan` is ~264 bytes and `AlreadyHasSession` carries
    // none, so boxing keeps the enum itself small regardless of which arm.
    Spawn(Box<NewSessionPlan>),
}

/// v0.8.x (concurrency review §4.1 P1) — per-chat spawn single-flight for the
/// inbound hot path. DELIBERATELY a lock separate from the gateway's own
/// `Arc<tokio::sync::Mutex<Gateway>>` — the same independent-lock house
/// pattern [`crate::pending::PendingInteractions`] already uses (see
/// `Gateway::pending`). [`Gateway::handle_message_shared`] uses this to
/// serialize the rare case where two concurrent inbound messages for the SAME
/// chat both observe "no session yet": only the first actually spawns, the
/// second just waits for the first's per-chat guard to release, then
/// re-checks (finds the session the first one just created) instead of
/// spawning a duplicate. Built on `tokio::sync::Mutex`'s own correct, FIFO
/// wait queue rather than a hand-rolled `Notify` (which has a well-known
/// registration-timing footgun if the "check, then wait" isn't done
/// carefully) — a second caller for the same chat simply blocks on
/// `.lock_owned().await` until the first's guard drops. Entries are never
/// removed: one tiny `tokio::sync::Mutex<()>` per DISTINCT chat that has ever
/// hit the implicit-spawn path, bounded exactly like the gateway's own
/// `current_project`/`current_session` maps (a chat is permanent once first
/// seen).
///
/// v0.9 T2 — also per-sid single-flight for dead-child resume
/// ([`Gateway::resume_dead_session_shared`] / [`Gateway::resume_dead_session`]):
/// concurrent resumes of the SAME sid serialize so the second waiter re-plans
/// after the first finishes (and may find the session already live or gone).
#[derive(Default)]
struct SpawnClaims {
    per_chat: std::sync::Mutex<BTreeMap<ChatKey, Arc<tokio::sync::Mutex<()>>>>,
    per_sid: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SpawnClaims {
    fn new() -> Self {
        Self::default()
    }

    /// Acquire (waiting if necessary) the single-flight claim for `chat`.
    async fn lock_for(&self, chat: &ChatKey) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = {
            let mut map = self.per_chat.lock().unwrap();
            Arc::clone(
                map.entry(chat.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        entry.lock_owned().await
    }

    /// Acquire (waiting if necessary) the single-flight claim for `sid`.
    /// Same FIFO house pattern as [`Self::lock_for`], keyed by session id so
    /// two concurrent dead-child resumes of the same sid never double-spawn.
    ///
    /// LOCK ORDER INVARIANT (v0.9 T2 review fix): acquire this claim strictly
    /// BEFORE the gateway lock; never await it while holding the gateway lock
    /// (ABBA deadlock — the claim holder needs the gateway lock for its
    /// plan/apply phases). Consequently only the shared lock-free resume
    /// flavor takes it; the `&mut self` flavor (already under the caller's
    /// gateway lock) relies on the apply-phase generation check instead.
    async fn lock_for_sid(&self, sid: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = {
            let mut map = self.per_sid.lock().unwrap();
            Arc::clone(
                map.entry(sid.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        entry.lock_owned().await
    }
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
    // v0.9.x (owner req) — high-frequency fleet views lead the Telegram command
    // menu (/status · /sessions · /projects), then session lifecycle, then the
    // rarer verbs. Dispatch is a `match` on the name (below), so this order only
    // drives the menu + /help; reordering is presentation-only.
    GatewayCommandSpec {
        name: "/status",
        arg_hint: None,
        help: "fleet health — per-session idle/working/stuck + model·ctx",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/sessions",
        arg_hint: Some("[all]"),
        help: "list this project's sessions (`all` = every project)",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/inbox",
        arg_hint: Some("[<time> <text>|cancel <dN>]"),
        help: "list, schedule, or cancel delayed user messages",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/projects",
        arg_hint: None,
        help: "list projects",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/new",
        arg_hint: Some("[vendor] [role] [hitl] [model=<id>] [effort=<level>]"),
        help: "start a new session (`hitl` = approve tools in IM; model=/effort= go to the vendor as typed)",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/use",
        arg_hint: Some("<id|@role>"),
        // v0.8.23 review §3.2-5 — `@role` is a shorthand for "the most
        // recently active session with that role" (silent recency tie-break;
        // an unmatched role lists what IS available).
        help: "switch to a session (`@role` = most-recent session with that role)",
        // v0.8.23 review §3.2-4 — the navigation verbs were menu-invisible
        // (only zero-arg commands were advertised), hiding most of the
        // multi-session workflow behind `/help`. Arg-bearing commands still
        // work fine from a menu tap: Telegram inserts the bare `/name ` and
        // the description (below, via `command_menu_description`) teaches
        // the argument.
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/cd",
        arg_hint: Some("<project>"),
        help: "switch project",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/role",
        arg_hint: Some("<role>"),
        help: "switch the current session to a fresh agent role",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/stop",
        arg_hint: Some("<id>"),
        help: "stop (destroy) a session by id",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/interrupt",
        arg_hint: Some("[id]"),
        help: "interrupt the running turn (keeps the session; bare = current)",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/rename",
        arg_hint: Some("[<id>] <title>"),
        help: "rename a session (bare = current; syncs the vendor's own title)",
        in_menu: true,
    },
    GatewayCommandSpec {
        name: "/newproject",
        arg_hint: Some("<slug> <path>"),
        help: "scaffold + register a project, then switch into it",
        in_menu: true,
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
///
/// v0.8.23 review §3.2-4 — navigation verbs that take an argument (`/use`,
/// `/cd`, `/role`, `/stop`, `/interrupt`, `/newproject`) are now `in_menu`
/// too (a menu tap still only inserts the bare command name — the channel
/// has no per-command "fill in the blank" affordance), so their
/// [`command_menu_description`] weaves the `arg_hint` into the description
/// the menu shows, teaching the user what to type next.
pub fn menu_command_specs() -> Vec<crate::transport::CommandSpec> {
    GATEWAY_COMMANDS
        .iter()
        .filter(|c| c.in_menu)
        .map(|c| crate::transport::CommandSpec {
            name: c.name.to_string(),
            description: command_menu_description(c),
        })
        .collect()
}

/// Menu-facing description for one [`GatewayCommandSpec`]: the arg hint
/// woven in front of the help text (e.g. `"<id|@role> — switch to a
/// session…"`) so an arg-bearing command's menu entry still teaches the
/// argument even though tapping it only inserts the bare command name.
/// Zero-arg commands are unaffected (just their `help`).
fn command_menu_description(c: &GatewayCommandSpec) -> String {
    match c.arg_hint {
        Some(hint) => format!("{hint} — {}", c.help),
        None => c.help.to_string(),
    }
}

/// v0.9.x (owner req) — the recommended next command appended as the last line
/// of a slash-command reply on IM, so each action teaches the natural next step
/// (e.g. `/use s62` → `using session s62` → `↓ 查看状态 → /status`). Format:
/// `↓ <描述> → /<cmd>`. Returns `None` for self-contained commands: `/status`
/// bakes its own `/projects` tail and the list commands (`/sessions`,
/// `/projects`) carry their own navigation, so appending here would double up;
/// vendor passthroughs (`/model`, `/compact`, …) are not gateway commands and
/// never reach this. Web is unaffected — its control face (`submit_web_sid`)
/// does not call this, and web navigates by GUI.
const NEXT_HINT_STATUS: &str = "↓ 查看状态 → /status";
const NEXT_HINT_SESSIONS: &str = "↓ 本项目会话 → /sessions";

fn command_next_hint(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "/new" | "/use" | "/role" | "/interrupt" => NEXT_HINT_STATUS,
        "/stop" | "/rename" | "/cd" | "/newproject" => NEXT_HINT_SESSIONS,
        _ => return None,
    })
}

fn append_next_hint(reply: &mut String, hint: &str) {
    if !reply.is_empty() {
        reply.push('\n');
    }
    reply.push_str(hint);
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
            routing_path: None,
            next_sid_path: None,
            next_scheduled_path: None,
            restore_pending: Vec::new(),
            projects,
            operator_chats: BTreeMap::new(),
            current_project: BTreeMap::new(),
            current_session: Arc::new(std::sync::RwLock::new(BTreeMap::new())),
            sessions: BTreeMap::new(),
            templates: Vec::new(),
            next_session: 0,
            next_scheduled: 0,
            scheduled_items: BTreeMap::new(),
            scheduled_notify: Arc::new(tokio::sync::Notify::new()),
            event_sink: None,
            events_broadcast,
            event_pumps: BTreeMap::new(),
            pending: Arc::new(tokio::sync::Mutex::new(
                crate::pending::PendingInteractions::new(),
            )),
            project_paths: None,
            config: None,
            im_reload_tx: None,
            spawn_claims: Arc::new(SpawnClaims::new()),
            remote_host_proxy: None,
            local_vendor_availability_override: None,
            delegations: std::collections::HashMap::new(),
            spawn_idem: crate::delegation::IdemCache::default(),
            dispatch_idem: crate::delegation::IdemCache::default(),
            delegation_tx: None,
            delegation_config_override: None,
            sessions_config_override: None,
        }
    }

    /// Wire the daemon's IM-reload trigger (the daemon owns the reload task +
    /// the channel listeners). After this, [`request_im_reload`](Self::request_im_reload)
    /// signals that task to rebuild the credential-driven channels from
    /// `credentials.json`. The standalone / test path never calls this, so
    /// `request_im_reload` stays a safe no-op there.
    pub fn set_im_reload_trigger(&mut self, tx: tokio::sync::mpsc::Sender<()>) {
        self.im_reload_tx = Some(tx);
    }

    /// v0.8.24 Track D — install a remote-host proxy (tests use
    /// [`crate::remote_host::FakeRemoteHostProxy`]; production leaves this
    /// `None` and falls through to the HTTP probe default).
    pub fn set_remote_host_proxy(
        &mut self,
        proxy: std::sync::Arc<dyn crate::remote_host::RemoteHostProxy>,
    ) {
        self.remote_host_proxy = Some(proxy);
    }

    /// Signal the daemon to reload IM channels from `credentials.json`. Returns
    /// `false` if no daemon reload task is wired (standalone / test). Non-blocking
    /// (the reload task coalesces; a full buffer just means a reload is already
    /// pending, so a dropped extra signal is harmless).
    pub fn request_im_reload(&self) -> bool {
        self.im_reload_tx
            .as_ref()
            .map(|tx| tx.try_send(()).is_ok())
            .unwrap_or(false)
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

    /// v0.9.0 W2 (F2) — wire the sender the detached event pumps use to signal a
    /// completed child turn to the delegation notifier task. MUST be called
    /// BEFORE [`set_event_sink`](Self::set_event_sink) (which spawns the pumps)
    /// so every pump captures the sender. The matching [`run_delegation_notifier`]
    /// owns the receiver + a gateway handle and delivers off the pump.
    pub fn set_delegation_notifier_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::delegation::DelegationSignal>,
    ) {
        self.delegation_tx = Some(tx);
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

    /// v0.8.22 P0-2 — the live pending-interaction registry `Arc`, whichever
    /// one is currently wired (an externally-injected one via
    /// [`Self::set_pending`], or the fresh default `Gateway::new_with_factory`
    /// created). Lets a caller wire a HITL resolver onto the SAME registry the
    /// gateway itself resolves IM/web clicks through, without having to know
    /// which of the two it is.
    pub fn pending_handle(&self) -> Arc<tokio::sync::Mutex<crate::pending::PendingInteractions>> {
        Arc::clone(&self.pending)
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

    /// Load and persist gateway routing + the session-id counter under
    /// `ccteam_root` (`<root>/state/gateway/routing.json` +
    /// `<root>/state/sessions/next-sid`). Session content is NOT persisted here
    /// — it lives in each session's `meta.json` (the v0.8.21 Wave-2 SoT); this
    /// only restores the per-chat focus + the live-set, then cold-start rebuilds
    /// those sessions from their meta.json (async, via
    /// [`resume_restored_sessions_shared`](Self::resume_restored_sessions_shared)).
    ///
    /// The daemon uses this for spawn-on-demand continuity across restarts.
    /// Unit tests keep the default in-memory mode (or pass a tempdir root).
    pub fn enable_persistence(&mut self, ccteam_root: impl Into<PathBuf>) -> Result<()> {
        let root = ccteam_root.into();
        self.routing_path = Some(crate::routing_state_path_in(&root));
        self.next_sid_path = Some(crate::next_sid_path_in(&root));
        self.next_scheduled_path = Some(
            root.join("state")
                .join("scheduled")
                .join("next-scheduled-id"),
        );
        self.load_state()?;
        self.load_scheduled_state();
        Ok(())
    }

    /// v0.8.21 Wave-2 — compute the (sync, no-`.await`) plan to rebuild a
    /// session from its `meta.json`: resolve the model (lenient — a rebuild must
    /// not fail because the role file was later deleted), mint a FRESH cto-gate
    /// secret (the old one died with the prior process; the re-spawned child's
    /// env gets this one, so pane-env and the gate map stay in lockstep at a new
    /// value), and select the adapter. The caller then `spawn_for_plan` (the
    /// slow await) + `apply_rebuilt_session` (insert + pump).
    fn plan_session_rebuild(
        &self,
        slug: &str,
        cwd: PathBuf,
        meta: &SessionMeta,
        reply_to: &ChatKey,
    ) -> Result<MetaRebuildPlan> {
        let (host, wire_slug) = self.ensure_session_host_binding(slug, &meta.host)?;
        let role_detail = ensure_role_exists(&cwd, &meta.role).ok().flatten();
        let model_id = meta
            .model
            .clone()
            .or_else(|| role_model_id(role_detail.as_ref()));
        let owner = self
            .tenant_project_owner(slug)
            .or_else(|| ChatKey::from_identity(&meta.owner))
            .unwrap_or_else(|| canonical_owner(reply_to));
        // Empty role ⇒ roleless: fall back to sid so @handle addressing stays
        // unique + non-empty (mirrors start_session / switch_current_role).
        let handle = if meta.role.is_empty() {
            meta.sid.clone()
        } else {
            meta.role.clone()
        };
        Ok(MetaRebuildPlan {
            sid: meta.sid.clone(),
            slug: slug.to_string(),
            role: meta.role.clone(),
            vendor: meta.vendor,
            protocol: meta.protocol,
            host,
            wire_slug,
            permission_mode: meta.permission_mode,
            parent_sid: meta.parent_sid.clone(),
            delegation_depth: meta.delegation_depth,
            owner,
            handle,
            model_id,
            effort: meta.effort.clone(),
            secret: ccteam_core::session_secret::mint(),
            cwd,
            adapter: (self.adapter_factory)(meta.vendor, meta.protocol),
            ccteam_root: self.project_paths.as_ref().map(|p| p.root.clone()),
            remote_proxy: self.remote_host_proxy.clone(),
        })
    }

    /// Cold-start the planned thread via the resume ladder (deterministic vendor
    /// uuid → `--resume`, so the Anthropic conversation resumes from transcript).
    /// The SLOW await — the batch-restore path runs this OUTSIDE the gateway lock.
    ///
    /// v0.9.0 W3 (G10, safety-critical) — a non-local `plan.host` is
    /// RE-GATED here, right before the spawn: online + registered on that
    /// satellite → `SpawnCtx::remote` carries the exec-bridge target;
    /// offline / unregistered / unknown → `Err`, and the caller (every
    /// rebuild path funnels through this one function) must leave the
    /// session stopped — NEVER silently respawn it locally.
    async fn spawn_for_plan(plan: &MetaRebuildPlan) -> Result<ThreadHandle, HarnessError> {
        let remote = crate::remote_host::regate_remote_host(
            plan.ccteam_root.as_deref(),
            &plan.host,
            &plan.wire_slug,
            plan.protocol,
            plan.remote_proxy.as_ref(),
        )
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("remote host re-gate: {e:#}")))?;
        // v0.9.0 W5 (real-machine smoke fix) — RE-WRITE the curated Claude
        // mcp.json with the FRESHLY-MINTED secret. Secrets are in-memory and
        // re-minted on every rebuild (`plan.secret`), but the file-based
        // mcp.json is only written on FRESH spawn — so without this a session
        // that cold-resumes after a daemon restart would keep the stale bearer
        // and LOSE all ccteam MCP tools (it could no longer delegate).
        // ACP/codex pass mcpServers inline with the current secret each resume,
        // so this file rewrite is the one gap. Best-effort (mirrors fresh path).
        if claude_session_mcp_config_applies(plan.vendor, plan.protocol) && !plan.secret.is_empty()
        {
            let input = ccteam_harness::execution::mcp_config::CuratedMcpInput {
                sid: &plan.sid,
                secret: &plan.secret,
                http_url: None,
            };
            if let Err(e) =
                ccteam_harness::execution::mcp_config::write_session_mcp_config(&plan.cwd, &input)
            {
                tracing::warn!(
                    sid = %plan.sid,
                    error = %e,
                    "curated mcp.json rewrite on resume failed; session continues without MCP"
                );
            }
        }
        plan.adapter
            .start_thread(
                &AgentSpecBrief {
                    role: plan.role.clone(),
                },
                &SpawnCtx {
                    slug: plan.slug.clone(),
                    sid: plan.sid.clone(),
                    cwd: plan.cwd.clone(),
                    project_dir: plan.cwd.clone(),
                    extra_args: vec![],
                    model_id: plan.model_id.clone(),
                    effort: plan.effort.clone(),
                    permission_mode: plan.permission_mode,
                    secret: plan.secret.clone(),
                    remote,
                },
            )
            .await
    }

    /// Admit the rebuilt session into the live map and start its event
    /// pump. `reply_to` is where this session's async turn answers route.
    async fn apply_rebuilt_session(
        &mut self,
        plan: MetaRebuildPlan,
        thread: ThreadHandle,
        reply_to: ChatKey,
    ) {
        let sid = plan.sid.clone();
        let excluded = self.live_capacity_exclusions(&sid, plan.parent_sid.as_deref());
        self.ensure_live_capacity(&excluded).await;
        self.sessions.insert(
            sid.clone(),
            GatewaySession {
                id: plan.sid,
                owner: plan.owner,
                project: plan.slug,
                role: plan.role,
                vendor: plan.vendor,
                protocol: plan.protocol,
                host: plan.host,
                permission_mode: plan.permission_mode,
                secret: plan.secret,
                handle: plan.handle,
                thread,
                adapter: plan.adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
                activity_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(reply_to)),
                pending_reaction: Arc::new(std::sync::Mutex::new(None)),
                turn_started_at: Arc::new(std::sync::Mutex::new(None)),
                steered_this_turn: Arc::new(AtomicBool::new(false)),
                last_event_at: Arc::new(std::sync::Mutex::new(None)),
                latest_activity: Arc::new(std::sync::Mutex::new(None)),
                watched_turn: Arc::new(std::sync::Mutex::new(None)),
                turn_origins: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                parent_sid: plan.parent_sid,
                delegation_depth: plan.delegation_depth,
            },
        );
        self.spawn_event_pump(&sid);
    }

    /// v0.8.21 Wave-2 — cold-start rebuild a live [`GatewaySession`] from its
    /// persisted `meta.json` (the session SoT), holding the gateway lock across
    /// the spawn (the scope on-demand `/use`/web resume + import already take).
    /// Does NOT touch `current_session` routing or persist — each caller owns
    /// that. The SINGLE rebuild core (`plan` + `spawn` + `apply`) shared by
    /// `/use`/web cold-resume ([`resume_stopped_session`](Self::resume_stopped_session)),
    /// external adopt ([`import_external_session`](Self::import_external_session)),
    /// and the batch restore (which instead spawns outside the lock).
    async fn rebuild_session_from_meta(
        &mut self,
        slug: &str,
        cwd: PathBuf,
        meta: &SessionMeta,
        reply_to: ChatKey,
    ) -> Result<()> {
        let plan = self.plan_session_rebuild(slug, cwd, meta, &reply_to)?;
        let thread = Self::spawn_for_plan(&plan).await?;
        self.apply_rebuilt_session(plan, thread, reply_to).await;
        Ok(())
    }

    /// Drop `current_session` routes pointing at a missing or foreign session.
    /// The ownership check also scrubs routes persisted by an older buggy
    /// session_spawn that bound a tenant project's sid to the admin web chat.
    fn drop_dead_session_routes(&self) {
        let mut memo = ProjectPrincipalMemo::new();
        self.current_session.write().unwrap().retain(|chat, sid| {
            self.sessions
                .get(sid)
                .is_some_and(|session| self.chat_can_access_with(chat, session, &mut memo))
        });
    }

    /// v0.8.21 Wave-2 — cold-start rebuild the sessions that were live at last
    /// persist (stashed in `restore_pending` by `load_state`) from their
    /// `meta.json`. A daemon restart kills every child (stream-json children are
    /// the daemon's own subprocesses; a terminal session also loses its tmux
    /// pane — authorized breaking), so there is nothing to reattach to: each
    /// session is re-spawned and resumes its conversation from the transcript
    /// (`--resume`). Routes to a session that fails to rebuild are dropped, then
    /// routing is re-persisted to reflect the rebuilt live-set. (The `&mut self`
    /// form holds the lock across spawns; the daemon uses the `_shared` form.)
    pub async fn resume_restored_sessions(&mut self) {
        let pending = std::mem::take(&mut self.restore_pending);
        for sid in pending {
            if self.sessions.contains_key(&sid) {
                continue;
            }
            let Ok((slug, cwd, meta)) = self.find_meta_for_sid(&sid) else {
                tracing::warn!(session = %sid, "ccteam-im: restore skipped; no meta.json found");
                continue;
            };
            let reply_to = self
                .tenant_project_owner_reply_target(&slug)
                .or_else(|| {
                    ChatKey::from_identity(&meta.owner).map(|owner| reply_target_for_owner(&owner))
                })
                .unwrap_or_else(web_api_chat);
            if let Err(err) = self
                .rebuild_session_from_meta(&slug, cwd, &meta, reply_to)
                .await
            {
                tracing::warn!(
                    session = %sid,
                    error = %err,
                    "ccteam-im: restored gateway session rebuild failed; left for on-demand resume"
                );
            }
        }
        self.drop_dead_session_routes();
        if let Err(err) = self.persist_routing() {
            tracing::warn!(error = %err, "ccteam-im: failed to persist restored routing");
        }
    }

    /// Startup-restore variant that does NOT hold the gateway lock across the
    /// slow `start_thread` await. Per session it: locks → builds the rebuild
    /// plan (sync, cheap) → UNLOCKS → spawns the thread (slow; stream-json may
    /// block on `system:init`) → re-locks → inserts. So a concurrent web `POST
    /// /sessions` never waits behind a stale session's startup.
    pub async fn resume_restored_sessions_shared(gateway: Arc<tokio::sync::Mutex<Self>>) {
        let pending = {
            let mut g = gateway.lock().await;
            std::mem::take(&mut g.restore_pending)
        };
        for sid in pending {
            // Build the plan under the lock, then drop it before the spawn await.
            let plan_and_reply = {
                let g = gateway.lock().await;
                if g.sessions.contains_key(&sid) {
                    None
                } else {
                    match g.find_meta_for_sid(&sid) {
                        Ok((slug, cwd, meta)) => {
                            let reply_to = g
                                .tenant_project_owner_reply_target(&slug)
                                .or_else(|| {
                                    ChatKey::from_identity(&meta.owner)
                                        .map(|owner| reply_target_for_owner(&owner))
                                })
                                .unwrap_or_else(web_api_chat);
                            match g.plan_session_rebuild(&slug, cwd, &meta, &reply_to) {
                                Ok(plan) => Some((plan, reply_to)),
                                Err(err) => {
                                    tracing::warn!(session = %sid, error = %err, "ccteam-im: restore skipped; project host binding changed");
                                    None
                                }
                            }
                        }
                        Err(_) => {
                            tracing::warn!(session = %sid, "ccteam-im: restore skipped; no meta.json found");
                            None
                        }
                    }
                }
            };
            let Some((plan, reply_to)) = plan_and_reply else {
                continue;
            };
            // Spawn OUTSIDE the lock — the slow part.
            match Self::spawn_for_plan(&plan).await {
                Ok(thread) => {
                    gateway
                        .lock()
                        .await
                        .apply_rebuilt_session(plan, thread, reply_to)
                        .await;
                }
                Err(err) => {
                    tracing::warn!(
                        session = %plan.sid,
                        error = %err,
                        "ccteam-im: restored gateway session rebuild failed; left for on-demand resume"
                    );
                }
            }
        }
        let g = gateway.lock().await;
        g.drop_dead_session_routes();
        if let Err(err) = g.persist_routing() {
            tracing::warn!(error = %err, "ccteam-im: failed to persist restored routing");
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

    /// Resolve the project-owned execution binding from the daemon catalog.
    /// Unregistered in-memory test/legacy projects remain local; serde-defaulted
    /// old entries also normalize to local. Only `wire_slug` crosses the host
    /// transport boundary — all daemon state stays keyed by `slug`.
    fn project_host_binding(&self, slug: &str) -> Result<(String, String)> {
        let entry = if let Some(config) = &self.config {
            let config = config
                .get()
                .with_context(|| format!("load project host binding for `{slug}`"))?;
            config
                .projects
                .iter()
                .find(|entry| entry.slug == slug)
                .cloned()
        } else if let Some(paths) = &self.project_paths {
            ccteam_core::config::lookup_project(&paths.root, slug)
                .with_context(|| format!("load project host binding for `{slug}`"))?
        } else {
            None
        };
        let Some(entry) = entry else {
            return Ok((ccteam_core::LOCAL_HOST.to_string(), slug.to_string()));
        };
        let host = if entry.host.trim().is_empty() {
            ccteam_core::LOCAL_HOST.to_string()
        } else {
            entry.host
        };
        let wire_slug = if host == ccteam_core::LOCAL_HOST {
            slug.to_string()
        } else {
            entry.remote_slug.unwrap_or_else(|| slug.to_string())
        };
        Ok((host, wire_slug))
    }

    /// v0.10 T1 — the execution host bound to a project (sync, no shellout),
    /// for the `session_spawn` availability pre-check. Any resolve miss → the
    /// local host (the overwhelming common case). Read-only, holds no
    /// `.await`.
    pub(crate) fn project_bound_host(&self, slug: &str) -> String {
        self.project_host_binding(slug)
            .map(|(host, _wire)| host)
            .unwrap_or_else(|_| ccteam_core::LOCAL_HOST.to_string())
    }

    /// Return the injected local availability snapshot, when present. The
    /// caller probes live when this is `None`.
    pub(crate) fn local_vendor_availability_override(
        &self,
    ) -> Option<Vec<ccteam_core::VendorAvailability>> {
        self.local_vendor_availability_override.clone()
    }

    /// Test-only local capability injection. Lib tests cannot mutate the
    /// process environment safely because they share one test process.
    #[cfg(test)]
    pub(crate) fn set_local_vendor_availability_for_tests(
        &mut self,
        availability: Vec<ccteam_core::VendorAvailability>,
    ) {
        self.local_vendor_availability_override = Some(availability);
    }

    /// v0.10 T1 — a satellite host's last control-channel agent report for the
    /// `session_spawn` availability discovery: `(online, heartbeat_age_secs,
    /// agents)`. `None` when the daemon has no home / registry, or the host is
    /// not registered (the caller then does not block — the existing
    /// offline/unknown-host gate in `prepare_host_for_spawn` owns those). Reads
    /// the on-disk registry (a quick fs read, like `prepare_host_for_spawn`);
    /// holds no `.await`.
    pub(crate) fn satellite_agent_snapshot(
        &self,
        host: &str,
    ) -> Option<(bool, u64, Vec<ccteam_core::HostAgentReport>)> {
        let root = self.project_paths.as_ref().map(|p| p.root.as_path())?;
        let reg =
            ccteam_core::HostRegistry::load(&ccteam_core::host_registry::registry_path_in(root))
                .ok()?;
        let rec = reg.get(host)?;
        let online = rec.is_online(ccteam_core::DEFAULT_HEARTBEAT_TTL_SECS);
        let age = ccteam_core::now_unix().saturating_sub(rec.last_heartbeat_unix);
        Some((online, age, rec.agents.clone()))
    }

    fn ensure_session_host_binding(
        &self,
        slug: &str,
        session_host: &str,
    ) -> Result<(String, String)> {
        let (host, wire_slug) = self.project_host_binding(slug)?;
        let session_host = if session_host.trim().is_empty() {
            ccteam_core::LOCAL_HOST
        } else {
            session_host
        };
        if session_host != host {
            return Err(anyhow!(
                "project host binding changed from `{session_host}` to `{host}`; start a new session"
            ));
        }
        Ok((host, wire_slug))
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
            // A `nav:` callback is a self-describing project/session SWITCH
            // button (it carries the target directly — `nav:cd:<slug>` /
            // `nav:use:<sid>` — with no pending-registry token), so it is
            // resolved here, before the token-keyed choice path.
            if let Some(nav) = reply.data.strip_prefix("nav:") {
                return self.resolve_nav_selection(&chat, nav).await;
            }
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
        if text.split_whitespace().next() == Some("/inbox") && !attachments.is_empty() {
            return Err(anyhow!(
                "/inbox scheduled messages do not support files or skills"
            ));
        }
        if let Some(mut reply) = self.handle_command(&chat, text).await? {
            // Owner req — teach the next step: append a recommended-command
            // footer as the reply's last line (see `command_next_hint`). IM only
            // — the web console navigates by GUI and reaches commands via
            // `submit_web_sid` in production, so it never gets the text footer.
            if chat.channel != "web" {
                if let Some(hint) = command_next_hint(text.split_whitespace().next().unwrap_or(""))
                {
                    append_next_hint(&mut reply, hint);
                }
            }
            return Ok(vec![reply]);
        }
        // A gateway command may handle itself ENTIRELY via the event sink and
        // return no inline reply — a project / session picker delivered as
        // text + inline buttons (`/projects`, `/sessions`). It must NOT fall
        // through to `submit_to_current` below, where its `/command` text
        // would be shipped to the agent verbatim.
        if Self::is_gateway_command(text) {
            return Ok(Vec::new());
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
                return self.submit_to_current(&chat, message_id, turn).await;
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
                return self.submit_to_current(&chat, message_id, turn).await;
            }
        }
        let templates = self.templates_for_chat(&chat);
        if templates.len() > 1 {
            let mut handles: Vec<String> = templates.iter().map(|t| t.handle.clone()).collect();
            handles.sort();
            handles.dedup();
            return Ok(vec![format_ambiguous_dm_reply(&handles)]);
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
        let mut replies = self.submit_to_current(&chat, message_id, turn).await?;
        if chat.channel != "web" && text.split_whitespace().next() == Some("/model") {
            if let Some(last) = replies.last_mut() {
                append_next_hint(last, NEXT_HINT_STATUS);
            }
        }
        Ok(replies)
    }

    /// v0.8.x (concurrency review §4.1 P1) — cheap, synchronous, read-only hot
    /// path hint for `spawn_inbound_consumer`: true when this inbound message
    /// MIGHT make [`Self::handle_message`] take a branch that spawns a
    /// brand-new session thread (`ensure_current_session`'s implicit
    /// first-message spawn — the common "new chat says hello" / "chat has no
    /// session yet" case). Deliberately conservative in BOTH directions
    /// because either wrong answer is safe, never a correctness bug: a
    /// `false` that turns out to need a spawn just runs the ordinary inline
    /// `handle_message` unchanged (today's behavior, holding the gateway lock
    /// across the spawn); a `true` that turns out NOT to need one just falls
    /// through inside [`Self::handle_message_shared`] to the same
    /// `handle_message` call. So this never has to track `handle_message`'s
    /// full branch tree exactly — it only needs to be right often enough to
    /// unblock the common case (see the daemon's `spawn_inbound_consumer` for
    /// how the two outcomes are used).
    ///
    /// A selection click, a recognized gateway command (`/new` `/role` `/use`
    /// `/clear` …), and an `@mention` are all left on the slow/legacy inline
    /// path in this pass — they have their OWN (rarer) spawning branches
    /// (`/new`, `/use` cold-resume, `/clear` codex-recycle, an
    /// `@mention`-to-a-template) that still hold the gateway lock across
    /// their spawn, same as before this change (a documented, scoped
    /// limitation — see the locking-protocol comment on
    /// `spawn_inbound_consumer`).
    pub fn inbound_may_spawn(
        &self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        text: &str,
        has_selection: bool,
    ) -> bool {
        if has_selection {
            return false;
        }
        if self.has_current_session(channel, chat_id, user_id) {
            return false;
        }
        if Self::is_gateway_command(text) {
            return false;
        }
        if crate::router::parse_first_mention(text).is_some() {
            return false;
        }
        true
    }

    /// v0.8.x (concurrency review §4.1 P1) — production entry point for the
    /// daemon's inbound hot path (`spawn_inbound_consumer`) for a message
    /// [`Self::inbound_may_spawn`] flagged as a candidate for the implicit
    /// first-message spawn. LOCKING PROTOCOL: everything that reads/mutates
    /// gateway state runs under a freshly (re-)acquired `gateway.lock().await`
    /// guard held only across synchronous work; the slow
    /// `adapter.start_thread` await (tmux/subprocess spawn, stream-json
    /// `system:init`) runs with NO gateway lock held at all — the same shape
    /// [`Self::resume_restored_sessions_shared`] already established. Two
    /// concurrent inbound messages for the SAME chat that both observe "no
    /// session yet" are serialized through [`SpawnClaims`] (a lock separate
    /// from the gateway's own — the `PendingInteractions` house pattern) so
    /// only ONE of them actually spawns; the other waits for the claim to
    /// clear and then re-checks, finding the session the first one created.
    // Same inbound per-field shape as `handle_message` plus the `Arc<Mutex<Gateway>>`
    // handle this "shared" variant needs to manage the lock itself — allow the
    // arg count for the same reason `handle_message` does.
    #[allow(clippy::too_many_arguments)]
    pub async fn handle_message_shared(
        gateway: Arc<tokio::sync::Mutex<Gateway>>,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        message_id: &str,
        text: &str,
        attachments: &[ChannelAttachment],
        selection: Option<&ChoiceReply>,
    ) -> Result<Vec<String>> {
        let chat = ChatKey::new(channel, chat_id, user_id);
        // Re-derive the "implicit spawn candidate" decision authoritatively
        // (the daemon's `inbound_may_spawn` call is only a hint to decide
        // inline-vs-background — this is the one that actually acts). Any
        // other shape (selection / command / mention / already-has-a-session)
        // falls straight through to the ordinary `handle_message` call below,
        // unchanged.
        let candidate = selection.is_none()
            && !Self::is_gateway_command(text)
            && crate::router::parse_first_mention(text).is_none()
            && !gateway
                .lock()
                .await
                .has_current_session(channel, chat_id, user_id);
        if candidate {
            let claims = Arc::clone(&gateway.lock().await.spawn_claims);
            // Hold the per-chat claim across plan+spawn+apply so a second
            // concurrent "no session yet" message for this SAME chat waits
            // here instead of racing to spawn a duplicate session.
            let _claim = claims.lock_for(&chat).await;
            let outcome = {
                let mut g = gateway.lock().await;
                g.plan_ensure_current_session(&chat)?
            };
            if let EnsureSessionOutcome::Spawn(plan) = outcome {
                // The slow part — deliberately NO gateway lock held here.
                let thread = Self::spawn_for_new_session_plan(&plan).await?;
                let sid = {
                    let mut g = gateway.lock().await;
                    let outcome = g.apply_new_session(*plan, thread, None).await?;
                    let sid = outcome.id.clone();
                    g.drain_and_dispatch_pending_turns(&sid).await;
                    sid
                };
                let _ = sid;
            }
            // `_claim` (and thus the per-chat single-flight) releases here,
            // waking any waiter for this same chat.
        }
        gateway
            .lock()
            .await
            .handle_message(
                channel,
                chat_id,
                user_id,
                message_id,
                text,
                attachments,
                selection,
            )
            .await
    }

    async fn handle_command(&mut self, chat: &ChatKey, text: &str) -> Result<Option<String>> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return Ok(None);
        }
        let mut parts = trimmed.split_whitespace();
        let cmd = parts.next().unwrap_or_default();
        match cmd {
            "/inbox" => Ok(Some(self.handle_inbox_command(chat, trimmed)?)),
            "/new" => {
                let args: Vec<&str> = parts.collect();
                let NewSessionArgs {
                    vendor,
                    role,
                    permission_mode,
                    protocol,
                    tuning,
                } = parse_new_command_args(&args)?;
                let project = self.require_current_project(chat)?;
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
                        tuning,
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
            "/rename" => {
                // Raw remainder (like `/newproject`'s path arg) — a title may
                // contain spaces, so this must NOT be whitespace-split.
                let mut it = trimmed.splitn(2, char::is_whitespace);
                let _cmd = it.next();
                let rest = it.next().unwrap_or("").trim();
                if rest.is_empty() {
                    return Err(anyhow!(
                        "用法: /rename [<sid>] <新标题>(省略 sid = 重命名当前会话)"
                    ));
                }
                // `[<sid>] <title>` — same leading-id convention as
                // /stop /use /interrupt. Splitting only when a REMAINDER
                // follows keeps `/rename s3` meaning "title the current
                // session s3", not "rename s3 to nothing".
                let (sid, raw_title) = match split_leading_sid(rest) {
                    Some((sid, title)) => (sid.to_string(), title),
                    None => {
                        let sid = self
                            .current_session
                            .read()
                            .unwrap()
                            .get(chat)
                            .cloned()
                            .ok_or_else(|| {
                                anyhow!(
                                    "/rename 需要一个活动会话:先 /new 或发条消息,\
                                     也可以 /rename <sid> <新标题> 指名重命名。"
                                )
                            })?;
                        (sid, rest)
                    }
                };
                // Own-only, live or stopped (a history session is renameable
                // too) — same visibility rule every other sid-addressed
                // command applies.
                if !self.chat_can_access_sid(chat, &sid) {
                    return Ok(Some(format!("unknown session for this chat: {sid}")));
                }
                let renamed = self.rename_session(&sid, raw_title).await?;
                Ok(Some(render_rename_receipt(&renamed)))
            }
            "/use" => {
                let arg = parts
                    .next()
                    .ok_or_else(|| anyhow!("/use requires a session id or @role"))?;
                // v0.8.23 review §3.2-5 — `/use @<role>` shorthand: resolve to
                // the chat-visible session with that role, most recently
                // active wins (silent recency tie-break). Only matches LIVE
                // sessions (a cold-resume-by-role would be ambiguous with a
                // stopped session's history); an unmatched role lists what IS
                // available.
                let owned_id;
                let id: &str = if let Some(role) = arg.strip_prefix('@') {
                    owned_id = self.resolve_use_role_shorthand(chat, role)?;
                    &owned_id
                } else {
                    arg
                };
                // The switch itself lives in `use_session` (shared with the
                // clickable session picker's `nav:use:<sid>` button tap).
                self.use_session(chat, id).await.map(Some)
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
                // v0.8.18 柱2 档0 — own-only: a chat can /stop only its own session.
                let accessible = self
                    .sessions
                    .get(&sid)
                    .map(|s| self.chat_can_access(chat, s))
                    .unwrap_or(false);
                if !accessible {
                    return Ok(Some(format!("unknown session for this chat: {sid}")));
                }
                self.stop_session(&sid).await?;
                Ok(Some(format!("stopped session {sid}")))
            }
            "/interrupt" => {
                // Interrupt the session's CURRENTLY-RUNNING turn WITHOUT
                // destroying it — the context survives, so the user can then
                // `/model` switch / send a follow-up on the same session. This
                // is the missing middle between a plain turn and `/stop`
                // (destroy). It is a GATEWAY command (handled here, before
                // `submit_to_current`), so it reaches the adapter OUT-OF-BAND
                // via `interrupt_turn` (stream-json `interrupt` control_request
                // / TUI ESC / codex `turn/interrupt`) and never queues behind
                // the running turn. Unlike `/stop` (explicit sid for safety), a
                // bare `/interrupt` targets the CURRENT session — non-destructive,
                // so fat-fingering it just stops the live turn.
                let sid = match parts.next() {
                    Some(id) => id.to_string(),
                    None => self
                        .current_session
                        .read()
                        .unwrap()
                        .get(chat)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!("/interrupt 需要一个活动会话(或 /interrupt <sid>)")
                        })?,
                };
                // Own-only ACL — identical to /stop (a chat can interrupt only
                // its own / the shared web-pool session).
                let accessible = self
                    .sessions
                    .get(&sid)
                    .map(|s| self.chat_can_access(chat, s))
                    .unwrap_or(false);
                if !accessible {
                    return Ok(Some(format!("unknown session for this chat: {sid}")));
                }
                self.interrupt_session(&sid).await?;
                Ok(Some(format!(
                    "已中断 session {sid} 当前 turn(会话保留,可继续 /model 等)"
                )))
            }
            "/cd" => {
                let project = parts
                    .next()
                    .ok_or_else(|| anyhow!("/cd requires a project"))?;
                // The switch itself lives in `change_project` (shared with the
                // clickable project picker's `nav:cd:<slug>` button tap).
                self.change_project(chat, project).map(Some)
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
                // v0.8.20 convergence — own the project by the CANONICAL identity
                // (`user:<tenant>` for a tenant bot), so the tenant's web console
                // sees the IM-bot-created project too (web ACL is project-owned).
                let owner_id = canonical_owner(chat).identity();
                // Creating a project in a chat means "I want to work here now":
                // `create_project` switches the chat into the new project (like a
                // `/cd`), so the next message spawns a session there instead of
                // landing back in the previous project.
                self.create_project(chat, slug, path, Some(&owner_id))
                    .map(Some)
            }
            "/sessions" => {
                // Default = the current project only (so switching between
                // projects never shows a confusing cross-project pile). `all`
                // (or `*`) opts into the full fleet across every project.
                let all = parts
                    .next()
                    .is_some_and(|a| a.eq_ignore_ascii_case("all") || a == "*");
                let text = self.render_sessions(chat, all).await;
                // On a button-capable channel (Telegram) the SAME list is
                // delivered via the event sink as text + one inline "switch"
                // button per live session (`nav:use:<sid>` tap → `/use`); the
                // command then returns no inline reply. Every other channel
                // (web's structured session frame, Lark's text-only send, the
                // test mock) keeps the plain-text reply unchanged.
                if Self::channel_supports_buttons(&chat.channel) {
                    let options = self.session_switch_options(chat, all);
                    self.emit_list_options(chat, text, options);
                    Ok(None)
                } else {
                    Ok(Some(text))
                }
            }
            "/status" => Ok(Some(self.render_status(chat).await)),
            "/projects" => {
                // Button-capable channel (Telegram) → a text header + one inline
                // "switch" button per project (`nav:cd:<slug>` tap → `/cd`),
                // delivered via the event sink. Others keep the bare
                // newline-separated slug list as an inline reply.
                if Self::channel_supports_buttons(&chat.channel) {
                    let options = self.project_switch_options(chat);
                    let cur = self.current_project_label(chat);
                    self.emit_list_options(
                        chat,
                        format!("📁 项目(点击切换,✓ = 当前 {cur}):"),
                        options,
                    );
                    Ok(None)
                } else {
                    Ok(Some(self.render_projects(chat)))
                }
            }
            "/help" => Ok(Some(format!(
                "📁 当前项目: {}\n\n{}",
                self.current_project_label(chat),
                render_help()
            ))),
            _ => Ok(None),
        }
    }

    fn handle_inbox_command(&mut self, chat: &ChatKey, trimmed: &str) -> Result<String> {
        self.gc_failed_scheduled(chrono::Utc::now());
        let rest = trimmed.strip_prefix("/inbox").unwrap_or("").trim();
        if rest.is_empty() {
            let mut items = self
                .scheduled_items
                .values()
                .filter(|entry| self.chat_can_access_scheduled_entry(chat, entry))
                .map(|entry| entry.item.clone())
                .collect::<Vec<_>>();
            items.sort_by(crate::scheduled::scheduled_order);
            if items.is_empty() {
                return Ok(format!(
                    "📥 /inbox is empty (daemon timezone: {})",
                    crate::scheduled::daemon_timezone_label()
                ));
            }
            let mut lines = vec![format!(
                "📥 scheduled messages (daemon timezone: {})",
                crate::scheduled::daemon_timezone_label()
            )];
            for item in items {
                let when = item
                    .send_at
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M");
                let state = match item.status {
                    crate::scheduled::ScheduledStatus::Pending => String::new(),
                    crate::scheduled::ScheduledStatus::Failed => format!(
                        " [failed: {}]",
                        item.fail_reason.as_deref().unwrap_or("unknown error")
                    ),
                };
                lines.push(format!(
                    "{} · {} · {} · {}{}",
                    item.id,
                    item.sid,
                    when,
                    crate::scheduled::preview(&item.text),
                    state
                ));
            }
            return Ok(lines.join("\n"));
        }

        if rest == "cancel" || rest.starts_with("cancel ") {
            let mut parts = rest["cancel".len()..].split_whitespace();
            let id = parts
                .next()
                .ok_or_else(|| anyhow!("usage: /inbox cancel <dN>"))?;
            if parts.next().is_some() {
                return Err(anyhow!("usage: /inbox cancel <dN>"));
            }
            let entry = self
                .scheduled_items
                .get(id)
                .cloned()
                .filter(|entry| self.chat_can_access_scheduled_entry(chat, entry))
                .ok_or_else(|| anyhow!("unknown scheduled message for this chat: {id}"))?;
            self.cancel_scheduled_message(&entry.item.sid, id)?;
            return Ok(format!("cancelled {id}"));
        }

        let (when, text) = parse_inbox_create_args(rest)?;
        let send_at = crate::scheduled::parse_send_time(&when)?;
        let sid = self
            .current_session
            .read()
            .unwrap()
            .get(chat)
            .cloned()
            .ok_or_else(|| {
                anyhow!("/inbox needs a current session; use /sessions then /use <sid>")
            })?;
        let visible_pending = self
            .scheduled_items
            .values()
            .filter(|entry| {
                entry.item.status == crate::scheduled::ScheduledStatus::Pending
                    && self.chat_can_access_scheduled_entry(chat, entry)
            })
            .count();
        let item = self.create_scheduled_message_inner(
            &sid,
            text,
            send_at,
            canonical_owner(chat).identity(),
            Some(chat.channel.clone()),
            Some(chat.chat_id.clone()),
            visible_pending,
        )?;
        Ok(format!(
            "scheduled {} → {} at {} ({})",
            item.id,
            item.sid,
            item.send_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M"),
            crate::scheduled::daemon_timezone_label()
        ))
    }

    fn chat_can_access_scheduled_entry(&self, chat: &ChatKey, entry: &ScheduledEntry) -> bool {
        if let Some(session) = self.sessions.get(&entry.item.sid) {
            return self.chat_can_access(chat, session);
        }
        read_session_meta(&entry.project_dir, &entry.item.sid)
            .map(|meta| self.project_session_owner_visible(chat, &meta.slug, &meta.owner))
            .unwrap_or(false)
    }

    /// Switch the chat's current project to `project` — the `/cd` core, shared
    /// with the clickable project picker (`nav:cd:<slug>` button tap).
    ///
    /// Picks up a project registered in `config.yaml` after the daemon started
    /// (the in-memory map is a cache; config.yaml is the source of truth), then
    /// moves the active session to follow the switch: adopt an existing session
    /// this chat owns in the target project (deterministic smallest id), else
    /// clear the active session so the next message spawns one there.
    fn change_project(&mut self, chat: &ChatKey, project: &str) -> Result<String> {
        self.ensure_project_loaded(project);
        // Existence AND ownership in one gate (same policy as the picker /
        // `/projects` / web REST): a chat may `/cd` only into a project it can
        // SEE, so a tenant can't switch into — and then spawn sessions in — the
        // admin's project by typing its slug. A hidden or nonexistent slug reads
        // identically ("unknown project"), disclosing nothing.
        if !self.can_see_project(chat, project) {
            return Err(anyhow!("unknown project: {project}"));
        }
        self.current_project
            .insert(chat.clone(), project.to_string());
        let adopted = self.adopt_session_in_project(chat, project);
        self.persist_routing()?;
        Ok(match adopted {
            Some(sid) => format!("project set to {project} (switched to {sid})"),
            None => format!("project set to {project} (next message starts a session there)"),
        })
    }

    /// Switch the chat's focus to session `id` — the `/use <sid>` core, shared
    /// with the clickable session picker (`nav:use:<sid>` button tap).
    ///
    /// ACL is own-only (`chat_can_access` / `owner_identity_visible`): a chat
    /// reaches only the sessions it owns plus the shared web pool. A session
    /// that isn't live is cold-resumed from its `meta.json` after the same
    /// owner check, so a button on a since-stopped session still works.
    async fn use_session(&mut self, chat: &ChatKey, id: &str) -> Result<String> {
        // v0.8.18 柱2 档0 — own-only: a chat can `/use` only its own (or a
        // shared web-pool) session; another chat's reads as unknown. Same-user
        // web↔IM reach returns via 档1; replies follow the per-turn submitter.
        let live_session = self
            .sessions
            .get(id)
            .filter(|s| self.chat_can_access(chat, s));
        if live_session.is_none() {
            // v0.8.21 — try to cold-resume a stopped session from meta.json.
            // ACL pre-check: peek at the stored owner BEFORE spawning so we
            // don't leak session existence to an unauthorised chat.
            let acl_ok = self
                .find_meta_for_sid(id)
                .ok()
                .map(|(slug, _, meta)| self.project_session_owner_visible(chat, &slug, &meta.owner))
                .unwrap_or(false);
            if !acl_ok {
                return Ok(format!("unknown session for this chat: {id}"));
            }
            let caller_identity = canonical_owner(chat).identity();
            // IM already owner-checked above, so no project-slug binding is
            // needed here — pass None.
            match self
                .resume_stopped_session(id, &caller_identity, None)
                .await
            {
                Ok(resumed_sid) => {
                    // Move current project to this session's project.
                    if let Some(s) = self.sessions.get(&resumed_sid) {
                        let proj = s.project.clone();
                        self.current_project.insert(chat.clone(), proj);
                    }
                    return Ok(format!("resumed session {resumed_sid}"));
                }
                Err(_) => return Ok(format!("unknown session for this chat: {id}")),
            }
        }
        let session = live_session.expect("checked above");
        let sid = session.id.clone();
        let project = session.project.clone();
        if let Ok(mut target) = session.reply_to.lock() {
            *target = chat.clone();
        }
        // Switching INTO a session moves the chat's "current project" to that
        // session's project, so a following /new (and /cd's default) lands in
        // the same project you just switched into — not the stale prior one.
        self.current_project.insert(chat.clone(), project);
        self.current_session
            .write()
            .unwrap()
            .insert(chat.clone(), sid.clone());
        self.persist_routing()?;
        Ok(format!("using session {sid}"))
    }

    /// Resolve a `nav:` switch-button tap (`cd:<slug>` / `use:<sid>`) by
    /// delegating to the SAME switch logic `/cd` / `/use` use (ACL + cold
    /// resume included). The payload is self-describing, so no pending-registry
    /// entry is consulted; a stale button just re-runs the (idempotent) switch
    /// or reads as an unknown target.
    async fn resolve_nav_selection(&mut self, chat: &ChatKey, nav: &str) -> Result<Vec<String>> {
        if let Some(slug) = nav.strip_prefix("cd:") {
            let mut reply = self.change_project(chat, slug)?;
            if chat.channel != "web" {
                append_next_hint(&mut reply, NEXT_HINT_SESSIONS);
            }
            return Ok(vec![reply]);
        }
        if let Some(sid) = nav.strip_prefix("use:") {
            let mut reply = self.use_session(chat, sid).await?;
            if chat.channel != "web" {
                append_next_hint(&mut reply, NEXT_HINT_STATUS);
            }
            return Ok(vec![reply]);
        }
        Ok(vec!["invalid selection".to_string()])
    }

    /// Whether a channel renders message `options` as tappable inline-keyboard
    /// buttons. Only Telegram does today: Lark's `send` ignores options, web
    /// turns an options-bearing message into a choice-chip frame (which would
    /// REPLACE its structured session list), and the test mock reads the plain
    /// text reply. Keyed by platform so per-tenant bots (`"telegram@<tenant>"`)
    /// are covered too. Extend as other providers gain native buttons.
    fn channel_supports_buttons(channel: &str) -> bool {
        crate::transport::platform_of(channel) == "telegram"
    }

    /// Project slugs `chat` may SEE — backs `/projects`, the `/cd` picker, and
    /// the `/status` project count. Same source web and `ccteam status` use
    /// (`collect_projects`: config.yaml filtered to on-disk `state.json`),
    /// filtered through the SAME ownership policy the web REST project list
    /// applies (`build_projects` → `Identity::can_see_owner`): a tenant sees only
    /// its own `user:<id>` projects, the operator/admin sees every non-tenant
    /// project. This is the IM twin of `build_projects`, so IM and web never
    /// diverge on who sees which project (the multi-user isolation contract).
    ///
    /// The candidate set is the UNION of the on-disk catalog and the in-memory
    /// routing registry, so this agrees with [`Self::can_see_project`] (which
    /// authorizes off `self.projects`) by construction — the list and the door
    /// used to disagree, and now that the list also drives project RESOLUTION
    /// (`current_project_for`), a project you may `/cd` into but that the
    /// catalog hasn't materialized would have read as "you have no project".
    /// Ownership comes from the persisted `ProjectState.owner`; with no
    /// `project_paths` wired (unit tests) every project reads as unowned, i.e.
    /// operator-visible and tenant-hidden — the same fail-safe as the web ACL.
    fn visible_project_slugs(&self, chat: &ChatKey) -> Vec<String> {
        let mut slugs: BTreeSet<String> = self.projects.keys().cloned().collect();
        if let Some(paths) = &self.project_paths {
            if let Ok(summaries) = ccteam_core::collect_projects(paths) {
                slugs.extend(summaries.into_iter().map(|s| s.state.slug));
            }
        }
        slugs
            .into_iter()
            .filter(|slug| {
                self.chat_can_see_project_owner(chat, self.project_owner(slug).as_deref())
            })
            .collect()
    }

    /// Whether `chat` may see/address the project `slug` — existence AND
    /// ownership, the gate behind `/cd <slug>`. Existence via the in-memory
    /// registry (as before); ownership via the SAME core policy as the picker /
    /// web REST, so a tenant can't `/cd` into the admin's project by typing its
    /// slug (visibility alone isn't enough — addressing must be gated too).
    fn can_see_project(&self, chat: &ChatKey, slug: &str) -> bool {
        self.projects.contains_key(slug)
            && self.chat_can_see_project_owner(chat, self.project_owner(slug).as_deref())
    }

    /// The persisted `ProjectState.owner` of `slug` (`None` when unset or the
    /// state can't be read — treated as unowned: operator-visible, tenant-hidden,
    /// a fail-safe that matches the web ACL).
    fn project_owner(&self, slug: &str) -> Option<String> {
        self.project_paths
            .as_ref()
            .and_then(|paths| ccteam_core::ProjectState::load(&paths.project_state(slug)).ok())
            .and_then(|state| state.owner)
    }

    /// Canonical tenant principal inherited by every session in a tenant-owned
    /// project. Operator-owned projects deliberately return `None`: separate
    /// admin IM chats keep their own session identities.
    fn tenant_project_owner(&self, slug: &str) -> Option<ChatKey> {
        let owner = self.project_owner(slug)?;
        ccteam_core::identity::is_tenant_owned(Some(&owner))
            .then(|| ChatKey::from_identity(&owner))
            .flatten()
    }

    /// Concrete web delivery route for a tenant project's principal.
    fn tenant_project_owner_reply_target(&self, slug: &str) -> Option<ChatKey> {
        self.tenant_project_owner(slug)
            .map(|owner| reply_target_for_owner(&owner))
    }

    /// One "switch project" button per project (`nav:cd:<slug>`), the current
    /// one marked `✓`. Payloads over Telegram's 64-byte `callback_data` cap are
    /// dropped (a pathologically long slug still shows in the text list).
    fn project_switch_options(&self, chat: &ChatKey) -> Vec<MessageOption> {
        let cur = self.current_project_for(chat).unwrap_or_default();
        let mut options: Vec<MessageOption> = self
            .visible_project_slugs(chat)
            .into_iter()
            .map(|slug| {
                // A consistent leading glyph (✓ current / ▸ others) lines the
                // labels up on the left, so the picker reads as a tidy list
                // rather than centre-floating text (owner req).
                let label = if slug == cur {
                    format!("✓ {slug}")
                } else {
                    format!("▸ {slug}")
                };
                MessageOption {
                    data: format!("nav:cd:{slug}"),
                    label,
                    id: slug,
                }
            })
            .filter(|o| o.data.len() <= TELEGRAM_CALLBACK_MAX)
            .collect();
        left_align_option_labels(&mut options);
        options
    }

    /// One "switch session" button per live, chat-visible session
    /// (`nav:use:<sid>`), the current one marked `✓`. Scoped + ordered like
    /// [`Self::render_sessions`] (current project unless `all`; recency then
    /// numeric-sid descending) so the buttons track the text list. Ended
    /// (history) sessions switch only via their text `→ /use <sid>` hint.
    fn session_switch_options(&self, chat: &ChatKey, all: bool) -> Vec<MessageOption> {
        let cur = self.current_project_for(chat).unwrap_or_default();
        let mut memo = ProjectPrincipalMemo::new();
        let mut visible: Vec<&GatewaySession> = self
            .sessions
            .values()
            .filter(|s| self.chat_can_access_with(chat, s, &mut memo))
            .filter(|s| all || s.project == cur)
            .collect();
        visible.sort_by(|a, b| {
            let la = self.session_last_active(a);
            let lb = self.session_last_active(b);
            lb.cmp(&la)
                .then_with(|| session_index(&b.id).cmp(&session_index(&a.id)))
        });
        let current_sid = self.current_session.read().unwrap().get(chat).cloned();
        let mut options: Vec<MessageOption> = visible
            .into_iter()
            .map(|s| {
                // Label = `sid vendor (title)` (✓ prefixes the current
                // session), arranged sid → vendor → title — same `sid vendor`
                // opening as the text rows. The button carries the human name;
                // a long title is clipped to `SESSION_BUTTON_TITLE_MAX_COLS`
                // display cols so one verbose title can't widen every button
                // (the left-align padding below pads all labels to the widest).
                // Callback `data` is unchanged.
                let marker = if Some(&s.id) == current_sid.as_ref() {
                    "✓ "
                } else {
                    ""
                };
                let mut label = format!("{marker}{} {}", s.id, vendor_str(s.vendor));
                if let Some(title) = self.session_title(s) {
                    label.push_str(&format!(
                        " ({})",
                        truncate_cols(&title, SESSION_BUTTON_TITLE_MAX_COLS)
                    ));
                }
                MessageOption {
                    data: format!("nav:use:{}", s.id),
                    label,
                    id: s.id.clone(),
                }
            })
            .filter(|o| o.data.len() <= TELEGRAM_CALLBACK_MAX)
            .collect();
        left_align_option_labels(&mut options);
        options
    }

    /// Emit a picker message (a project/session list) carrying inline `options`
    /// to a button-capable channel, via the user-signal sink.
    /// Delivers text + buttons as ONE message
    /// (`spawn_gateway_event_consumer` calls `.with_options`), so the caller
    /// returns no separate inline reply.
    fn emit_list_options(&self, chat: &ChatKey, content: String, options: Vec<MessageOption>) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        self.emit_user_signal(GatewayEvent {
            id: format!("gateway-picker-{}-{nanos}", chat.chat_id),
            channel: chat.channel.clone(),
            chat_id: chat.chat_id.clone(),
            thread_ts: None,
            content,
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options,
            sid: None,
            slug: None,
        });
    }

    /// Scaffold a ccteam project at `raw_path`, register it in
    /// `config.yaml`, and make it addressable by `/cd <slug>` in this
    /// running daemon. `raw_path` may be `~`-relative; it must resolve to
    /// an absolute directory (existing repos are adopted in place, empty
    /// dirs are created — `bootstrap_project_at_dir` leaves user files
    /// alone). Requires [`Gateway::enable_project_creation`].
    fn create_project(
        &mut self,
        chat: &ChatKey,
        slug: &str,
        raw_path: &str,
        owner: Option<&str>,
    ) -> Result<String> {
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
        // v0.8.18 柱2 — record the creating chat as owner (explicit field; NOT
        // path-derived). Use the known project dir: the config upsert happens
        // below, so `paths.project_state(slug)` would still resolve the fallback
        // projects-root path and miss an arbitrary-path project's real state.
        if let Some(owner) = owner {
            let state_path = ccteam_core::CcteamPaths::project_state_in(&abs);
            match ccteam_core::ProjectState::load(&state_path) {
                Ok(mut state) => {
                    state.owner = Some(owner.to_string());
                    if let Err(err) = state.save(&state_path) {
                        tracing::warn!(%slug, error = %err, "set project owner failed");
                    }
                }
                Err(err) => {
                    tracing::warn!(%slug, error = %err, "load state to set owner failed")
                }
            }
        }
        upsert_project(
            &paths.root,
            ProjectEntry {
                slug: slug.clone(),
                path: abs.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .with_context(|| format!("register project {slug} in config.yaml"))?;
        self.register_project(slug.clone(), abs.clone());
        // Switch the creating chat INTO the new project (mirror `/cd`): point its
        // `current_project` at the fresh slug and clear the active session — a
        // brand-new project owns none, so `adopt_session_in_project` returns
        // `None` and removes the stale pointer, making the next message spawn a
        // `cto` session HERE rather than in the chat's previous project.
        self.current_project.insert(chat.clone(), slug.clone());
        self.adopt_session_in_project(chat, &slug);
        if let Err(err) = self.persist_routing() {
            tracing::warn!(error = %err, "ccteam-im: persist after /newproject failed");
        }
        Ok(format!(
            "✅ 已创建并切换到 {slug}\n   📁 {}\n   发条消息即在此开一个会话(或 /new)",
            abs.display()
        ))
    }

    async fn ensure_current_session(&mut self, chat: &ChatKey) -> Result<()> {
        match self.plan_ensure_current_session(chat)? {
            EnsureSessionOutcome::AlreadyHasSession => Ok(()),
            EnsureSessionOutcome::Spawn(plan) => {
                let thread = Self::spawn_for_new_session_plan(&plan).await?;
                let outcome = self.apply_new_session(*plan, thread, None).await?;
                self.drain_and_dispatch_pending_turns(&outcome.id).await;
                Ok(())
            }
        }
    }

    /// v0.8.x (concurrency review §4.1 P1) — the sync half of
    /// `ensure_current_session`: decide whether this chat needs a brand-new
    /// implicit session and, if so, build its [`NewSessionPlan`] WITHOUT
    /// awaiting the spawn. Same decision tree as the pre-fold
    /// `ensure_current_session` (single-template auto-spawn / ambiguous
    /// multi-template error / default `cto`), just stopping short of the
    /// slow await so [`Gateway::handle_message_shared`] can drop the gateway
    /// lock before running it.
    fn plan_ensure_current_session(&mut self, chat: &ChatKey) -> Result<EnsureSessionOutcome> {
        if self.current_session.read().unwrap().contains_key(chat) {
            return Ok(EnsureSessionOutcome::AlreadyHasSession);
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
            let template = templates[0].clone();
            let cd_elsewhere = self
                .current_project
                .get(chat)
                .is_some_and(|p| *p != template.project);
            if !cd_elsewhere {
                // Mirrors `start_template_session`'s pre-spawn mutation: switching
                // INTO the bot's project happens regardless of whether the spawn
                // itself later runs inline or with the lock dropped.
                self.current_project
                    .insert(chat.clone(), template.project.clone());
                let plan = self.plan_new_session(
                    chat.clone(),
                    template.project,
                    template.vendor,
                    template.role,
                    template.handle,
                    // Template-spawned sessions are skip (the route template has
                    // no mode field; HITL is opt-in per session, not per route).
                    PermissionMode::Skip,
                    // Template sessions default to the stream-json protocol.
                    SessionProtocol::StreamJson,
                    SpawnTuning::default(),
                )?;
                return Ok(EnsureSessionOutcome::Spawn(Box::new(plan)));
            }
        }
        if templates.len() > 1 {
            let mut handles: Vec<String> = templates.iter().map(|t| t.handle.clone()).collect();
            handles.sort();
            handles.dedup();
            return Err(anyhow!(format_ambiguous_dm_reply(&handles)));
        }
        let project = self.require_current_project(chat)?;
        let plan = self.plan_new_session(
            chat.clone(),
            project,
            AgentVendor::Claude,
            // v0.9.0 W2 (F6.1) — engine neutralization: the implicit
            // first-message spawn is ROLELESS (empty role omits `--agent`; the
            // bare vendor reads the project CLAUDE.md/AGENTS.md as its brain).
            // ccteam seeds no persona; orchestration lives in user space / hub.
            String::new(),
            String::new(),
            // Implicit first-message spawn stays skip — HITL is opt-in via
            // `/new … hitl` / API / session_spawn.
            PermissionMode::Skip,
            // v0.8.11 E2 — defaults to the stream-json protocol (a pure chat
            // session with no terminal needs).
            SessionProtocol::StreamJson,
            SpawnTuning::default(),
        )?;
        Ok(EnsureSessionOutcome::Spawn(Box::new(plan)))
    }

    /// Build the `/new` receipt. v0.8.8 F1 — every `/new` mints a fresh sid
    /// (no more `(project, role)` reuse), so the posture is always exactly the
    /// requested one; the receipt just names the new session + flags hitl.
    ///
    /// v0.8.22 P0-2 (review §3.1-1) — this claim is Claude-only-honest as of
    /// this fix: BOTH Claude protocols now route non-allowlist tool calls to
    /// an IM/web approval prompt (terminal via the `PermissionRequest` hook's
    /// `permission/ask`; stream-json — the default — via the in-process
    /// `can_use_tool` resolver `daemon::default_adapter_factory_with_stream_json_handle`
    /// wires). Before this fix a `hitl` stream-json session silently denied
    /// every non-allowlist tool with NO prompt ever rendered, making this
    /// exact wording false for the default protocol. Codex `hitl` sessions
    /// are NOT covered (`codex_app_server.rs` documents: no codex→IM
    /// approval routing exists yet, so they stay locked-down rather than
    /// prompting) — this receipt is still emitted for a codex hitl spawn and
    /// is honest ONLY in the "you get less access, not a bypass" sense, not
    /// the "you'll be asked" sense a Claude user would read into it.
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
                SpawnTuning::default(),
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
            SpawnTuning::default(),
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
        tuning: SpawnTuning,
    ) -> Result<StartOutcome> {
        let (host, wire_slug) = self.project_host_binding(&project)?;
        // v0.8.24 Track D — remote-host gate BEFORE minting a sid: offline /
        // terminal-on-remote / unknown host all fail without creating a
        // session (red line: never kill / never half-create). v0.9.0 W3
        // (G9) additionally gates on the satellite having `project`
        // registered, and returns the exec-bridge target for an online host.
        let host_target = crate::remote_host::prepare_host_for_spawn(
            self.project_paths.as_ref().map(|p| p.root.as_path()),
            &host,
            &wire_slug,
            protocol,
            self.remote_host_proxy.as_ref(),
        )
        .await?;
        // v0.8.x (concurrency review §4.1 P1) — split into plan (sync) / spawn
        // (the slow `start_thread` await) / apply (sync), same shape as the
        // meta-rebuild trio (`plan_session_rebuild` / `spawn_for_plan` /
        // `apply_rebuilt_session`). This caller composes all three inline
        // (unchanged behavior/lock-scope: every existing `start_session`
        // caller still awaits the spawn under whatever lock IT holds);
        // `Gateway::handle_message_shared` is the one caller that instead
        // drops the gateway lock between `plan_new_session` and
        // `spawn_for_new_session_plan`.
        let mut plan = self.plan_new_session(
            owner,
            project,
            vendor,
            role,
            handle,
            permission_mode,
            protocol,
            tuning,
        )?;
        plan.remote = host_target.remote;
        let thread = Self::spawn_for_new_session_plan(&plan).await?;
        let outcome = self.apply_new_session(plan, thread, None).await?;
        self.drain_and_dispatch_pending_turns(&outcome.id).await;
        Ok(outcome)
    }

    /// v0.8.x (concurrency review §4.1 P1) — the sync half of `start_session`:
    /// resolve the project/role/model, mint the fresh monotonic sid + secret
    /// (durable BEFORE use — red line: sid never reused), and select the
    /// adapter. No `.await` at all, so a caller can run this under the
    /// gateway lock and then drop it before the slow spawn.
    #[allow(clippy::too_many_arguments)]
    fn plan_new_session(
        &mut self,
        reply_to: ChatKey,
        project: String,
        vendor: AgentVendor,
        role: String,
        handle: String,
        permission_mode: PermissionMode,
        protocol: SessionProtocol,
        tuning: SpawnTuning,
    ) -> Result<NewSessionPlan> {
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
        // v0.8.24 A-U3 — an explicit composer choice beats the role's
        // `model:` frontmatter; effort has no role-level default.
        let tuning = tuning.normalized();
        let model_id = tuning.model.or_else(|| role_model_id(role_detail.as_ref()));
        let effort = tuning.effort;
        self.next_session += 1;
        // Make the counter durable BEFORE the sid is used (a later spawn failure
        // then leaves a harmless gap, never a reused sid — red line: monotonic).
        self.persist_next_sid()?;
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
        let (host, wire_slug) = self.project_host_binding(&project)?;
        let secret = ccteam_core::session_secret::mint();
        let adapter = (self.adapter_factory)(vendor, protocol);
        // Ownership is decided HERE, the one sync core every fresh spawn funnels
        // through (IM `/new` → `start_session`, REST `POST …/sessions` →
        // `create_session_api_tuned`, MCP `session_spawn` →
        // `create_delegated_session`), so a future entry inherits it for free
        // instead of needing its own patch. project 是归属单元, session 继承: a
        // tenant-owned project stamps its own principal even when a fleet-wide
        // admin or an ambient agent does the spawning; operator-owned and
        // unowned projects have no principal, so the caller's canonical identity
        // still owns (keeping separate admin IM chats apart). `reply_to` is NOT
        // touched — owner is the resource identity, reply_to the concrete
        // delivery frontend, and a tenant's own bot must keep its IM route.
        let owner = self
            .tenant_project_owner(&project)
            .unwrap_or_else(|| canonical_owner(&reply_to));
        Ok(NewSessionPlan {
            id,
            owner,
            reply_to,
            project,
            vendor,
            role,
            handle,
            permission_mode,
            protocol,
            host,
            wire_slug,
            secret,
            cwd,
            model_id,
            effort,
            adapter,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
            title: None,
            remote: None,
            ccteam_root: self.project_paths.as_ref().map(|paths| paths.root.clone()),
            remote_proxy: self.remote_host_proxy.clone(),
        })
    }

    /// v0.8.x (concurrency review §4.1 P1) — the SLOW await for a
    /// [`NewSessionPlan`]. Self-less (no `&self`/`&mut self`) so a caller can
    /// run it with NO gateway lock held at all — mirrors [`Self::spawn_for_plan`].
    async fn spawn_for_new_session_plan(
        plan: &NewSessionPlan,
    ) -> Result<ThreadHandle, HarnessError> {
        let remote = if plan.remote.is_some() || plan.host == ccteam_core::LOCAL_HOST {
            plan.remote.clone()
        } else {
            crate::remote_host::regate_remote_host(
                plan.ccteam_root.as_deref(),
                &plan.host,
                &plan.wire_slug,
                plan.protocol,
                plan.remote_proxy.as_ref(),
            )
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("remote host gate: {e:#}")))?
        };
        // v0.8.24 C1 — curated per-session MCP for Claude sessions. File path is
        // well-known (chat/<sid>/mcp.json); the adapter attaches --mcp-config
        // when the file exists. Best-effort: spawn still proceeds if write fails
        // (session runs without in-agent ccteam tools).
        if claude_session_mcp_config_applies(plan.vendor, plan.protocol) && !plan.secret.is_empty()
        {
            let input = ccteam_harness::execution::mcp_config::CuratedMcpInput {
                sid: &plan.id,
                secret: &plan.secret,
                http_url: None,
            };
            if let Err(e) =
                ccteam_harness::execution::mcp_config::write_session_mcp_config(&plan.cwd, &input)
            {
                tracing::warn!(
                    sid = %plan.id,
                    error = %e,
                    "curated mcp.json write failed; session continues without MCP"
                );
            }
        }
        plan.adapter
            .start_thread(
                &AgentSpecBrief {
                    role: plan.role.clone(),
                },
                &SpawnCtx {
                    slug: plan.project.clone(),
                    sid: plan.id.clone(),
                    cwd: plan.cwd.clone(),
                    project_dir: plan.cwd.clone(),
                    extra_args: vec![],
                    model_id: plan.model_id.clone(),
                    effort: plan.effort.clone(),
                    permission_mode: plan.permission_mode,
                    secret: plan.secret.clone(),
                    remote,
                },
            )
            .await
    }

    /// v0.8.x (concurrency review §4.1 P1) — the apply half after the spawn:
    /// insert the live [`GatewaySession`], persist routing + `meta.json`, and
    /// start its event pump. Mirrors `apply_rebuilt_session`'s shape.
    async fn apply_new_session(
        &mut self,
        plan: NewSessionPlan,
        thread: ThreadHandle,
        trigger: Option<&str>,
    ) -> Result<StartOutcome> {
        let NewSessionPlan {
            id,
            owner,
            reply_to,
            project,
            vendor,
            role,
            handle,
            permission_mode,
            protocol,
            host,
            wire_slug: _,
            secret,
            cwd: _,
            model_id,
            effort: effort_meta,
            adapter,
            parent_sid,
            spawned_by_role,
            delegation_depth,
            title: spawn_title,
            remote: _,
            ccteam_root: _,
            remote_proxy: _,
        } = plan;
        // Capture before the session insert moves these.
        let meta_vendor_uuid = thread
            .raw_extras
            .get("vendor_uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let meta_project = project.clone();
        let meta_role = role.clone();
        let owner_channel = reply_to.channel.clone();
        let excluded = self.live_capacity_exclusions(&id, parent_sid.as_deref());
        self.ensure_live_capacity(&excluded).await;
        self.sessions.insert(
            id.clone(),
            GatewaySession {
                id: id.clone(),
                // v0.8.20 web↔IM convergence — owner is the canonical resource
                // identity while reply_to remains the concrete frontend route.
                owner,
                project,
                role,
                vendor,
                protocol,
                host: host.clone(),
                permission_mode,
                secret,
                handle,
                thread,
                adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
                activity_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(reply_to.clone())),
                pending_reaction: Arc::new(std::sync::Mutex::new(None)),
                turn_started_at: Arc::new(std::sync::Mutex::new(None)),
                steered_this_turn: Arc::new(AtomicBool::new(false)),
                last_event_at: Arc::new(std::sync::Mutex::new(None)),
                latest_activity: Arc::new(std::sync::Mutex::new(None)),
                watched_turn: Arc::new(std::sync::Mutex::new(None)),
                turn_origins: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                parent_sid: parent_sid.clone(),
                delegation_depth,
            },
        );
        self.current_session
            .write()
            .unwrap()
            .insert(reply_to, id.clone());
        self.persist_routing()?;
        // Write per-session meta.json for history list + resume after stop.
        {
            let now = chrono::Utc::now().to_rfc3339();
            let owner_tag = self
                .sessions
                .get(&id)
                .map(|s| s.owner.identity())
                .unwrap_or_default();
            // v0.9 T5 — snapshot role/skill fingerprints at spawn (not rehashed
            // mid-session).
            let (role_sha, skills_sha) = self
                .projects
                .get(&meta_project)
                .map(|cwd| {
                    (
                        ccteam_harness::execution::experience::role_fingerprint(cwd, &meta_role),
                        ccteam_harness::execution::experience::skills_fingerprint(cwd),
                    )
                })
                .unwrap_or((None, None));
            // v0.8.24 F5 — surface attribution from owner channel. MCP
            // `session_spawn` supplies its explicit trigger at the apply seam.
            let trigger = trigger.map(str::to_string).unwrap_or_else(|| {
                let ch = owner_channel.as_str();
                if ch == "web" || ch == "user" {
                    "web".to_string()
                } else if ch == "mcp" || ch == "session" {
                    "mcp".to_string()
                } else {
                    "im".to_string()
                }
            });
            let meta = SessionMeta {
                sid: id.clone(),
                slug: meta_project.clone(),
                vendor,
                protocol,
                role: meta_role,
                permission_mode,
                owner: owner_tag,
                vendor_uuid: meta_vendor_uuid,
                model: model_id,
                effort: effort_meta,
                host: host.clone(),
                created_at: now.clone(),
                last_active: now,
                origin: SessionOrigin::Ccteam,
                // A fresh spawn has no title yet — the first mirrored user
                // message auto-titles it (see `mirror_user_turn`).
                title: None,
                title_source: None,
                turn_count: 0,
                cost_usd: None,
                tokens_total: None,
                role_sha,
                skills_sha,
                trigger: Some(trigger),
                parent_sid: parent_sid.clone(),
                spawned_by_role: spawned_by_role.clone(),
                delegation_depth,
            };
            // v0.9.0 W2 (F2) — an explicit `session_spawn` title is a
            // user-authored label (highest precedence: it survives the
            // first-message auto-title). Ledger/display only — never a prompt.
            let mut meta = meta;
            if let Some(t) = spawn_title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                apply_title(&mut meta, t.to_string(), TitleSource::User);
            }
            if let Some(cwd) = self.projects.get(&meta_project) {
                if let Err(e) = write_session_meta(cwd, &meta) {
                    tracing::warn!(sid = %id, err = %e, "failed to write session meta.json");
                }
            }
        }
        self.spawn_event_pump(&id);
        // Pending-turn drain is async (re-enters submit_resolved); the
        // async caller of apply_new_session must invoke
        // `drain_and_dispatch_pending_turns` after this returns.
        Ok(StartOutcome {
            id,
            // Fresh spawn ran with exactly the requested posture.
            permission_mode,
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
        // v0.9.0 W2 (F2) — a `/role` switch re-spawns the SAME sid, so it keeps
        // its delegation lineage (parent + depth are a property of the session,
        // not its persona). meta.json already carries them; mirror them here.
        let parent_sid = old.parent_sid.clone();
        let delegation_depth = old.delegation_depth;
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

        // v0.8.7 review-fix (R-M1) — a `/role` switch closes the old pane and
        // spawns a brand-new one, so mint a FRESH secret: the new pane's env
        // gets it, and the in-place record below stores the same value, keeping
        // pane-env and gate-map in lockstep.
        let secret = ccteam_core::session_secret::mint();
        // Capture before `cwd`/`role` are moved into the spawn + insert below —
        // needed to sync meta.json (the session SoT) with the new role (v0.8.21
        // Wave-2: a restart rebuilds from meta, so the role change must persist).
        let meta_dir = cwd.clone();
        let meta_role = role.clone();
        // `/role` re-derives the MODEL from the new role's frontmatter, but the
        // effort belongs to the session, not the role — replay it so a switch
        // doesn't silently drop the level the session was spawned with.
        let effort = read_session_meta(&meta_dir, &sid)
            .ok()
            .and_then(|meta| meta.effort);
        let (adapter, thread) = self
            .spawn_session_thread(
                vendor,
                protocol,
                &role,
                &project,
                &sid,
                cwd,
                model_id.clone(),
                effort.clone(),
                permission_mode,
                secret.clone(),
                &host,
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
                activity_events: Arc::new(AtomicU64::new(0)),
                reply_to: Arc::new(std::sync::Mutex::new(owner)),
                pending_reaction: Arc::new(std::sync::Mutex::new(None)),
                turn_started_at: Arc::new(std::sync::Mutex::new(None)),
                steered_this_turn: Arc::new(AtomicBool::new(false)),
                last_event_at: Arc::new(std::sync::Mutex::new(None)),
                latest_activity: Arc::new(std::sync::Mutex::new(None)),
                watched_turn: Arc::new(std::sync::Mutex::new(None)),
                turn_origins: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                parent_sid,
                delegation_depth,
            },
        );
        self.current_session
            .write()
            .unwrap()
            .insert(chat.clone(), sid.clone());
        self.persist_routing()?;
        // v0.8.21 Wave-2 — keep meta.json (the session SoT) in sync: `/role`
        // changed the role, so a daemon restart must rebuild at the NEW role.
        // The rest of the descriptor (vendor/uuid/owner/origin) is unchanged
        // (same sid ⇒ same deterministic vendor uuid). Best-effort.
        // v0.9 T5 — re-snapshot role_sha for the new role (spawn-time semantics).
        if let Ok(mut meta) = read_session_meta(&meta_dir, &sid) {
            meta.role = meta_role.clone();
            meta.model = model_id;
            meta.effort = effort;
            meta.role_sha =
                ccteam_harness::execution::experience::role_fingerprint(&meta_dir, &meta_role);
            meta.skills_sha = ccteam_harness::execution::experience::skills_fingerprint(&meta_dir);
            meta.last_active = chrono::Utc::now().to_rfc3339();
            let _ = write_session_meta(&meta_dir, &meta);
        }
        self.spawn_event_pump(&sid);
        Ok(sid)
    }

    fn emit_user_signal(&self, event: GatewayEvent) {
        if let Some(tx) = self.event_sink.clone() {
            let _ = tx.send(event);
        } else {
            let _ = self.events_broadcast.send(event);
        }
    }

    /// v0.8.24 F5 — after a session becomes live, drain any cold-start
    /// pending turns (FIFO) and re-submit them through the normal path.
    /// Returns the turn ids of successfully submitted drained turns (order
    /// preserved) so a not-live submit can surface a real id to callers.
    async fn drain_and_dispatch_pending_turns(&mut self, session_id: &str) -> Vec<String> {
        let (project, chat, owner) = {
            let Some(session) = self.sessions.get(session_id) else {
                return Vec::new();
            };
            let chat = session
                .reply_to
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_else(|| session.owner.clone());
            (session.project.clone(), chat, session.owner.clone())
        };
        let _ = owner;
        let Some(cwd) = self.projects.get(&project).cloned() else {
            return Vec::new();
        };
        let pending = match crate::pending_turns::drain_pending_turns(&cwd, session_id) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(sid = %session_id, error = %e, "drain pending_turns failed");
                return Vec::new();
            }
        };
        if pending.is_empty() {
            return Vec::new();
        }
        let mut ids = Vec::new();
        for turn in pending {
            // Box::pin: drain ↔ submit_resolved are mutually recursive when
            // a not-live submit enqueues then drains (async recursion needs
            // indirection for a finite future type).
            match Box::pin(self.submit_resolved(
                &chat,
                session_id,
                "",
                turn.text,
                TurnOrigin::Internal,
                turn.literal,
            ))
            .await
            {
                Ok(SubmitResult::Turn { id, .. }) => ids.push(id),
                Ok(SubmitResult::Directive(_)) => {}
                Err(e) => {
                    tracing::warn!(
                        sid = %session_id,
                        error = %e,
                        "dispatch pending turn failed"
                    );
                }
            }
        }
        ids
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
        let mirror_paths = self.project_paths.clone();
        // v0.9 T5 — spawn-time fingerprints for experience.jsonl (do NOT re-read
        // meta.json per turn). Missing meta → None digests.
        let (pump_role_sha, pump_skills_sha) = project_dir
            .as_ref()
            .and_then(|dir| read_session_meta(dir, &session.id).ok())
            .map(|m| (m.role_sha, m.skills_sha))
            .unwrap_or((None, None));
        let session_id = session.id.clone();
        let pump_key = session_id.clone();
        // v0.8.10 routing-isolation — read handle to the chat→focus map so the
        // detached pump can label out-of-band answers/errors (events from a
        // session that is no longer the chat's current focus).
        let current_session = Arc::clone(&self.current_session);
        // v0.9.0 W2 (F2) — signal completed child turns to the delegation
        // notifier (the pump is detached + holds no gateway lock, so it can't
        // deliver the notification itself). `None` when no notifier is wired.
        let delegation_tx = self.delegation_tx.clone();
        let pump_vendor = session.vendor;
        let pump_host = session.host.clone();
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
            // v0.9 T5 — baseline activity_events at the start of each turn
            // (approximation for signals.tool_calls).
            let mut activity_at_turn_start: Option<u64> = None;
            // v0.9.5 feedback fix — delegation notifications fire on the VENDOR
            // TURN boundary, not per mirrored assistant message (codex narrates
            // checkpoints as separate messages inside one turn; per-message
            // notification floods the parent's context). Track, within the
            // current vendor turn: the latest mirrored (turn_id, text) — the
            // boundary's final answer — plus every mirrored turn id (batch
            // dedup bookkeeping) and the interim count.
            let mut turn_last_answer: Option<(String, String)> = None;
            let mut turn_covered: Vec<String> = Vec::new();
            let mut turn_notes: usize = 0;
            // The final text + the reply target used by its ordinary web
            // Answer. Held independently from delegation bookkeeping so an
            // IM mirror never depends on the turns.jsonl append succeeding.
            let mut mirror_last_answer: Option<(String, ChatKey)> = None;
            // v0.8.x (concurrency review §4.1 P2) — the turn-timeout watchdog,
            // folded from a detached per-turn `tokio::spawn` into THIS
            // per-session pump's own `tokio::select!` loop (one fewer task per
            // turn; the pump already lives for the session's whole lifetime).
            // `watch_timeout == 0` disables it entirely (matches the pre-fold
            // early return). `stream_ended` tracks whether the adapter's own
            // event stream has terminated (child exited): once it has, the
            // `events.next()` branch is permanently disabled (a `select!`
            // guard, not a `break`) so this loop doesn't hot-spin re-polling
            // an exhausted stream, but the watchdog keeps ticking (mirrors the
            // pre-fold design, where the watchdog task was fully independent
            // of the pump's own stream lifecycle — a dead/hung child still
            // gets the heads-up).
            let watch_timeout = gateway_turn_timeout_duration();
            let watch_poll = watch_timeout.min(std::time::Duration::from_secs(10));
            let mut stream_ended = false;
            let mut watch_tracked_turn: Option<String> = None;
            let mut watch_idle = std::time::Duration::ZERO;
            let mut watch_last_activity: u64 = 0;
            let mut watch_warned_turn: Option<String> = None;
            // Item ids of in-flight tool/command work. A long silent tool
            // (build, test suite, MCP call) legitimately emits no further
            // events while it runs — that is NOT a stall. Cleared on item
            // complete and at turn boundary so a lost complete cannot
            // suppress the watchdog forever.
            let mut open_work_items: std::collections::HashSet<String> =
                std::collections::HashSet::new();

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
                // Watchdog tick, armed only while the window is enabled — a
                // fresh timer each loop iteration (same shape as `flush`), so
                // it naturally restarts on every event this select! observes
                // (`biased` events.next()/flush always win a simultaneous
                // wakeup), giving idle timing that starts from the true last
                // activity rather than a fixed independent cadence.
                let watchdog_tick = async {
                    if watch_timeout.is_zero() {
                        std::future::pending::<()>().await;
                    } else {
                        tokio::time::sleep(watch_poll).await;
                    }
                };

                tokio::select! {
                    biased;
                    maybe = events.next(), if !stream_ended => {
                        let Some(evt) = maybe else {
                            stream_ended = true;
                            if watch_timeout.is_zero() {
                                break;
                            }
                            continue;
                        };
                        // Liveness tick: ANY event (assistant delta, tool-use,
                        // progress, turn-completed) means the turn is doing work,
                        // so the turn-timeout watchdog resets its idle clock. Only
                        // TRUE silence (no event for the whole idle window) is a
                        // stall. Counts every event, before any branch/filter.
                        session.activity_events.fetch_add(1, Ordering::SeqCst);
                        // Track open tool/command work so a long silent tool
                        // run does not look like a hung turn.
                        track_open_work_items(&mut open_work_items, &evt);
                        // v0.8.19 `/status` — record the wall-clock of this event
                        // right beside the liveness counter. `/status` derives the
                        // 🔴 stuck state from it the SAME way the watchdog does (a
                        // turn in flight whose last event is older than the idle
                        // window = silent = STUCK).
                        if let Ok(mut last) = session.last_event_at.lock() {
                            *last = Some(Instant::now());
                        }
                        // A queued adapter submission starts only after its
                        // predecessor completes. The submit call cannot stamp
                        // that future boundary, so canonical TurnStarted is the
                        // authority that opens a fresh working window when the
                        // previous TurnCompleted already cleared it. Native
                        // same-turn Inject emits no second TurnStarted and thus
                        // preserves the original elapsed time.
                        if let ThreadEvent::TurnStarted { turn_id } = &evt {
                            if let Ok(mut started) = session.turn_started_at.lock() {
                                if started.is_none() {
                                    *started = Some(Instant::now());
                                }
                            }
                            // …and the silence watchdog follows the same
                            // authority. The submit path arms only the turns it
                            // knows are running (`Started` / `Injected`), so a
                            // turn that was QUEUED behind a predecessor is armed
                            // here, the moment it actually begins — with a fresh
                            // `visible_events` baseline. Re-arming on a
                            // different id also drops a stale arm left by a turn
                            // that already answered but whose tick has not run
                            // yet; without that, every queued turn would go
                            // unwatched.
                            if !watch_timeout.is_zero() {
                                if let Ok(mut watch) = session.watched_turn.lock() {
                                    let stale = watch
                                        .as_ref()
                                        .map(|(armed, _)| armed != turn_id)
                                        .unwrap_or(true);
                                    if stale {
                                        *watch = Some((
                                            turn_id.clone(),
                                            session.visible_events.load(Ordering::SeqCst),
                                        ));
                                    }
                                }
                            }
                        }
                        // 👀 ack clear: the FIRST event of a turn is the moment
                        // the silent gap ends (💭 thinking / first progress), so
                        // remove the ack reaction added at dispatch. TAKE the
                        // pending msg_id (→ None) so this fires exactly once per
                        // turn; route the clear to the session's CURRENT reply
                        // target (the same reply_to→owner the answer below uses),
                        // so a turn driven from Telegram clears on Telegram.
                        // Fire-and-forget: the daemon egress swallows failures.
                        let pending_ack = session
                            .pending_reaction
                            .lock()
                            .ok()
                            .and_then(|mut p| p.take());
                        if let Some(ack_msg_id) = pending_ack {
                            let ack_target = session
                                .reply_to
                                .lock()
                                .map(|k| k.clone())
                                .unwrap_or_else(|_| session.owner.clone());
                            if !tx.send(GatewayEvent {
                                id: format!("gateway-reaction-clear-{session_id}-{ack_msg_id}"),
                                channel: ack_target.channel.clone(),
                                chat_id: ack_target.chat_id.clone(),
                                thread_ts: None,
                                content: String::new(),
                                kind: GatewayEventKind::Reaction {
                                    message_id: ack_msg_id,
                                    on: false,
                                },
                                attachments: Vec::new(),
                                options: Vec::new(),
                                sid: Some(session_id.clone()),
                                slug: Some(session.project.clone()),
                            }) {
                                break;
                            }
                        }
                        // v0.9 T5 — track activity baseline while a turn is in
                        // flight (for experience signals.tool_calls). Snapshot
                        // on the first event after turn_started_at becomes Some.
                        {
                            let in_flight = session
                                .turn_started_at
                                .lock()
                                .map(|g| g.is_some())
                                .unwrap_or(false);
                            if in_flight && activity_at_turn_start.is_none() {
                                // activity_events already includes this event.
                                activity_at_turn_start = Some(
                                    session
                                        .activity_events
                                        .load(Ordering::SeqCst)
                                        .saturating_sub(1),
                                );
                            }
                        }
                        // A paneless adapter's structured turn start is the
                        // first authoritative proof that work is underway.
                        // Persist it immediately: capacity eviction ranks by
                        // meta.last_active, so waiting until a long turn ends
                        // can misclassify an actively working child as oldest.
                        // Terminal sessions have hook-owned activity updates.
                        if !session.protocol.is_terminal()
                            && matches!(&evt, ThreadEvent::TurnStarted { .. })
                        {
                            if let Some(dir) = project_dir.as_ref() {
                                refresh_session_activity_meta(
                                    dir,
                                    &session_id,
                                    session.vendor,
                                    progress_path.as_deref(),
                                );
                            }
                        }
                        // v0.8.19 `/status` — a completed turn ends the in-flight
                        // window: clear `turn_started_at` (→ 🟢 idle) and the
                        // activity summary. Protocol-INDEPENDENT (both tmux and
                        // stream-json adapters emit `TurnCompleted`), unlike the
                        // stream-json-only progress mirror just below. Mirrors the
                        // submit path's set, on the same shared cell.
                        //
                        // v0.9 T5 — BEFORE clearing, capture duration + signals
                        // and append one kind:turn experience record (derived
                        // index; failure never breaks the pump).
                        if let ThreadEvent::TurnCompleted {
                            turn_id,
                            usage,
                            model,
                        } = &evt
                        {
                            let completed_origin = take_turn_origin(&session, Some(turn_id));
                            let mirror_answer = mirror_last_answer.take();
                            let duration_ms = session
                                .turn_started_at
                                .lock()
                                .ok()
                                .and_then(|g| *g)
                                .map(|start| start.elapsed().as_millis() as u64);
                            let steered = session
                                .steered_this_turn
                                .swap(false, Ordering::SeqCst);
                            let activity_now =
                                session.activity_events.load(Ordering::SeqCst);
                            let tool_calls = activity_now.saturating_sub(
                                activity_at_turn_start.unwrap_or(activity_now),
                            );
                            activity_at_turn_start = None;

                            if let Ok(mut started) = session.turn_started_at.lock() {
                                *started = None;
                            }
                            if let Ok(mut act) = session.latest_activity.lock() {
                                *act = None;
                            }

                            if let Some(dir) = project_dir.as_ref() {
                                let model_owned = model.clone().filter(|m| !m.is_empty());
                                let cost_usd = {
                                    let m = model_owned.as_deref().unwrap_or("");
                                    ccteam_cost::estimate_cost(
                                        usage,
                                        session.vendor.cost_vendor(),
                                        m,
                                    )
                                };
                                // Prefer the adapter turn_id (joins chat_turn_completed);
                                // synthesize `{sid}-{seq+1}` when empty (matches the
                                // upcoming ANSWER-side turns writer id shape).
                                let exp_turn_id = if turn_id.is_empty() {
                                    format!("{session_id}-{}", seq.saturating_add(1))
                                } else {
                                    turn_id.clone()
                                };
                                let usage_opt = if usage.total() == 0 && model_owned.is_none() {
                                    None
                                } else {
                                    Some(*usage)
                                };
                                let record = ccteam_harness::execution::experience::ExperienceRecord::Turn(
                                    ccteam_harness::execution::experience::TurnExperience {
                                        sid: session_id.clone(),
                                        turn_id: exp_turn_id,
                                        ts: chrono::Utc::now(),
                                        vendor: vendor_str(session.vendor).to_string(),
                                        model: model_owned,
                                        role: session.role.clone(),
                                        usage: usage_opt,
                                        cost_usd,
                                        duration_ms,
                                        role_sha: pump_role_sha.clone(),
                                        skills_sha: pump_skills_sha.clone(),
                                        signals: ccteam_harness::execution::experience::TurnSignals {
                                            tool_calls,
                                            steered,
                                            error_recovered: None,
                                        },
                                    },
                                );
                                if let Err(err) = ccteam_harness::execution::experience::append_experience(
                                    dir, &record,
                                ) {
                                    tracing::warn!(
                                        session = %session_id,
                                        error = %err,
                                        "ccteam-im: failed to append experience.jsonl"
                                    );
                                }
                            }

                            // v0.9.5 feedback fix — the vendor turn is DONE and
                            // the child is idle: flush ONE boundary signal
                            // carrying the turn's final answer (the last
                            // mirrored message) + the whole turn's mirrored ids
                            // for batch dedup. Interim messages already flowed
                            // as non-boundary signals (an `all` watch consumes
                            // those); the default `final` watch notifies here
                            // and ONLY here.
                            let finished = turn_last_answer.take();
                            let covered = std::mem::take(&mut turn_covered);
                            let notes = turn_notes;
                            turn_notes = 0;
                            if let (Some(dtx), Some((final_turn, final_text))) =
                                (delegation_tx.as_ref(), finished.as_ref())
                            {
                                let _ = dtx.send(crate::delegation::DelegationSignal {
                                    child_sid: session_id.clone(),
                                    turn_id: final_turn.clone(),
                                    tail: final_text.clone(),
                                    vendor: pump_vendor,
                                    host: pump_host.clone(),
                                    boundary: true,
                                    vendor_error: false,
                                    interim_notes: notes.saturating_sub(1),
                                    covered_turns: covered,
                                });
                            }
                            if let Some((final_text, reply_to)) = mirror_answer {
                                mirror_internal_web_answer(
                                    &tx,
                                    mirror_paths.as_ref(),
                                    &session,
                                    &reply_to,
                                    completed_origin,
                                    &session_id,
                                    seq,
                                    &final_text,
                                );
                            }
                        }
                        // A canonical failure is a terminal turn boundary too.
                        // Leaving the submit-side marker set makes inline waits
                        // time out and reports the failed session as still
                        // working even though every adapter has ended the turn.
                        if matches!(
                            &evt,
                            ThreadEvent::TurnFailed { .. } | ThreadEvent::Error(_)
                        ) {
                            activity_at_turn_start = None;
                            session.steered_this_turn.store(false, Ordering::SeqCst);
                            if let Ok(mut started) = session.turn_started_at.lock() {
                                *started = None;
                            }
                            if let Ok(mut act) = session.latest_activity.lock() {
                                *act = None;
                            }
                        }
                        // v0.8.11 E4 — for a paneless session (no hooks:
                        // stream-json AND acp), the pump mirrors each completed
                        // turn to progress.jsonl with the sid, so the
                        // session-list activity classifier (which keys off the
                        // latest sid-tagged event) sees it as active and
                        // `last_activity_seconds` tracks — and the per-session
                        // cost/token accounting has its `usage` row for EVERY
                        // vendor (v0.9.5 feedback: codex/grok/opencode/kimi
                        // sessions previously never accrued a ledger row on the
                        // acp path). Tmux sessions get this from their Stop
                        // hook → gate on protocol to avoid a double-write.
                        if !session.protocol.is_terminal() {
                            if let (
                                ThreadEvent::TurnCompleted {
                                    turn_id,
                                    usage,
                                    model,
                                },
                                Some(ppath),
                            ) = (&evt, progress_path.as_ref())
                            {
                                let ev = ccteam_core::progress::build_chat_turn_completed_event(
                                    &session.role,
                                    &session_id,
                                    turn_id,
                                    usage,
                                    model.as_deref(),
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
                            let boundary_origin = match &evt {
                                ThreadEvent::TurnFailed { turn_id, .. } => {
                                    Some(take_turn_origin(&session, Some(turn_id)))
                                }
                                ThreadEvent::Error(_) => Some(take_turn_origin(&session, None)),
                                _ => None,
                            };
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
                                let failure = thread_event_failure(&evt);
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
                                    outcome: failure.map(|_| "failed".to_string()),
                                    error_kind: failure.map(|err| err.kind.clone()),
                                    error: failure.map(|err| err.message.clone()),
                                };
                                match ccteam_harness::execution::turns_mirror::append_turn(
                                    dir,
                                    &session_id,
                                    &record,
                                ) {
                                    Ok(_) => {
                                        // v0.9.0 W2 (F2/F7) — ORDERING CONTRACT:
                                        // the child turn is now DURABLY on disk, so
                                        // signal the notifier (which then submits the
                                        // completion turn to the parent). collect
                                        // after a notification is guaranteed to see
                                        // this turn. Fire-and-forget; the notifier
                                        // filters non-watched sids.
                                        //
                                        // v0.9.5 feedback fix — notification unit =
                                        // the TASK: a TurnFailed/Error text IS the
                                        // turn boundary (flush immediately, folding
                                        // any interim notes); an ordinary assistant
                                        // message flows as an interim signal (only
                                        // an `all` watch notifies) and is remembered
                                        // as the boundary candidate flushed on
                                        // `TurnCompleted` above.
                                        let is_boundary_evt = matches!(
                                            &evt,
                                            ThreadEvent::TurnFailed { .. }
                                                | ThreadEvent::Error(_)
                                        );
                                        turn_covered.push(record.turn_id.clone());
                                        if is_boundary_evt {
                                            let notes = turn_notes;
                                            turn_notes = 0;
                                            turn_last_answer = None;
                                            let covered = std::mem::take(&mut turn_covered);
                                            if let Some(dtx) = delegation_tx.as_ref() {
                                                let _ = dtx.send(crate::delegation::DelegationSignal {
                                                    child_sid: session_id.clone(),
                                                    turn_id: record.turn_id.clone(),
                                                    tail: text.clone(),
                                                    vendor: pump_vendor,
                                                    host: pump_host.clone(),
                                                    boundary: true,
                                                    vendor_error: true,
                                                    interim_notes: notes,
                                                    covered_turns: covered,
                                                });
                                            }
                                        } else {
                                            turn_notes = turn_notes.saturating_add(1);
                                            turn_last_answer = Some((
                                                record.turn_id.clone(),
                                                text.clone(),
                                            ));
                                            if let Some(dtx) = delegation_tx.as_ref() {
                                                let _ = dtx.send(crate::delegation::DelegationSignal {
                                                    child_sid: session_id.clone(),
                                                    turn_id: record.turn_id.clone(),
                                                    tail: text.clone(),
                                                    vendor: pump_vendor,
                                                    host: pump_host.clone(),
                                                    boundary: false,
                                                    vendor_error: false,
                                                    interim_notes: 0,
                                                    covered_turns: vec![record.turn_id.clone()],
                                                });
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            session = %session_id,
                                            error = %err,
                                            "ccteam-im: failed to mirror turn to turns.jsonl"
                                        );
                                    }
                                }
                                // Refresh last_active/turn_count/cost_usd in
                                // meta.json on turn completion (v0.8.22 P1).
                                refresh_session_activity_meta(
                                    dir,
                                    &session_id,
                                    session.vendor,
                                    progress_path.as_deref(),
                                );
                            }
                            // Resolve the live reply target ONCE (reply_to → owner
                            // fallback, same as pump_target) and reuse it for the
                            // focus check below.
                            let chat_key = session
                                .reply_to
                                .lock()
                                .map(|k| k.clone())
                                .unwrap_or_else(|_| session.owner.clone());
                            if boundary_origin.is_some() {
                                mirror_last_answer = None;
                            } else {
                                mirror_last_answer = Some((text.clone(), chat_key.clone()));
                            }
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
                            // v0.8.23 review §3.2-5 (item 2a) — a FOCUSED answer
                            // otherwise carries NO context at all, so the "which
                            // session just answered" question only had an answer for
                            // the out-of-focus case above. Append a compact one-line
                            // echo (`→ slug/sid (role)`) so a multi-session chat
                            // always knows. IM text surface only (`channel != "web"`)
                            // — the web console already shows the session in its own
                            // chrome (per-session tab, project picker). Best-effort
                            // title lookup from the same meta.json this turn just
                            // refreshed above.
                            let content = if is_focused {
                                if channel == "web" {
                                    text.clone()
                                } else {
                                    let title = project_dir
                                        .as_ref()
                                        .and_then(|dir| read_session_meta(dir, &session_id).ok())
                                        .and_then(|m| m.title);
                                    format!(
                                        "{text}\n\n{}",
                                        context_echo_line(
                                            &session.project,
                                            &session_id,
                                            &session.role,
                                            title.as_deref(),
                                        )
                                    )
                                }
                            } else {
                                contextual_answer(
                                    &session_id,
                                    &session.project,
                                    session.vendor,
                                    &session.role,
                                    &text,
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
                                slug: Some(session.project.clone()),
                            }) {
                                break;
                            }
                            if let Some(origin) = boundary_origin {
                                mirror_internal_web_answer(
                                    &tx,
                                    mirror_paths.as_ref(),
                                    &session,
                                    &chat_key,
                                    origin,
                                    &session_id,
                                    seq,
                                    &text,
                                );
                            }
                        } else if progress_on {
                            // ----- PROGRESS (IM, unchanged) -----
                            // The fold drives the IM status string; its dirty
                            // signal gates the throttled status edit exactly as
                            // before — IM behavior is byte-identical.
                            if fold.apply(&evt) {
                                dirty = true;
                                // v0.8.19 `/status` — publish the compact
                                // `read×N·bash×M` counts (NOT the full progress
                                // text) for the working line. Same fold, so the
                                // IM status and `/status` can never disagree.
                                if let Ok(mut act) = session.latest_activity.lock() {
                                    *act = fold.compact_counts();
                                }
                                let ready =
                                    last_emit.map(|t| t.elapsed() >= throttle).unwrap_or(true);
                                if ready
                                    && !flush_progress(
                                        &tx, &session, &session_id, epoch, &fold,
                                        &mut last_sent, &mut last_emit, &mut dirty,
                                    )
                                {
                                    break;
                                }
                            }
                            // ----- ACTIVITY (web-only, v0.8.19) -----
                            // Emit the SAME event's structured form, computed by
                            // the shared `progress::activity_for` summarizer. It
                            // fires for EVERY renderable item event (start /
                            // update / complete), independent of whether the
                            // fold bucketed it — both fire. IM drops it (strict
                            // no-op arm); web renders activity cards.
                            if let Some(activity) = crate::progress::activity_for(&evt) {
                                if !emit_activity(&tx, &session, &session_id, epoch, activity) {
                                    break;
                                }
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
                    _ = watchdog_tick => {
                        // Snapshot + release immediately (never hold this
                        // std::sync::Mutex across an .await).
                        let armed = session.watched_turn.lock().ok().and_then(|g| g.clone());
                        match armed {
                            None => {
                                watch_tracked_turn = None;
                                watch_idle = std::time::Duration::ZERO;
                            }
                            Some((turn_id, start_visible)) => {
                                if session.visible_events.load(Ordering::SeqCst) != start_visible {
                                    // The turn already produced a visible answer
                                    // (finished) — clear the arm so a stale one
                                    // is never re-warned, mirroring the pre-fold
                                    // watchdog's own "already answered" early
                                    // return.
                                    if let Ok(mut w) = session.watched_turn.lock() {
                                        *w = None;
                                    }
                                    watch_tracked_turn = None;
                                    watch_idle = std::time::Duration::ZERO;
                                } else if watch_warned_turn.as_deref() != Some(turn_id.as_str()) {
                                    if watch_tracked_turn.as_deref() != Some(turn_id.as_str()) {
                                        // Newly-noticed turn (or the pump only
                                        // just started tracking it) — start its
                                        // idle clock fresh.
                                        watch_tracked_turn = Some(turn_id.clone());
                                        watch_idle = std::time::Duration::ZERO;
                                        watch_last_activity =
                                            session.activity_events.load(Ordering::SeqCst);
                                    } else {
                                        let cur = session.activity_events.load(Ordering::SeqCst);
                                        // Still working if either the event
                                        // counter moved OR a tool/command is
                                        // open (long silent tools are real work).
                                        if cur != watch_last_activity || !open_work_items.is_empty()
                                        {
                                            watch_last_activity = cur;
                                            watch_idle = std::time::Duration::ZERO;
                                        } else {
                                            watch_idle += watch_poll;
                                        }
                                    }
                                    if watch_idle >= watch_timeout {
                                        // A full idle window of total silence —
                                        // ONE-SHOT, warn-only (red line: never
                                        // kill). `emit_turn_stall_warning` never
                                        // interrupts the turn on any protocol.
                                        emit_turn_stall_warning(
                                            &tx, &session, &turn_id, watch_timeout,
                                            progress_path.as_deref(),
                                        );
                                        watch_warned_turn = Some(turn_id);
                                    }
                                }
                            }
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

    /// v0.8.21 Wave-2 — restore ROUTING + the sid counter (sync). The live map
    /// is left EMPTY; the sids that were live at last persist are stashed in
    /// `restore_pending` for the async
    /// [`resume_restored_sessions_shared`](Self::resume_restored_sessions_shared)
    /// step to cold-start rebuild from each session's `meta.json` (the SoT).
    /// No session content is read here (spawning is async; load_state is not).
    fn load_state(&mut self) -> Result<()> {
        // Monotonic sid counter — its own file, read independently of routing so
        // a wiped routing table never resets it (red line: sid never reused).
        if let Some(path) = self.next_sid_path.as_ref() {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(n) = raw.trim().parse::<u64>() {
                    self.next_session = n;
                }
            }
        }
        let Some(path) = self.routing_path.as_ref() else {
            return Ok(());
        };
        if !path.exists() {
            self.recover_routing_from_meta();
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)?;
        let saved: RoutingState = serde_json::from_str(&raw)?;
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
        // Live map stays empty; rebuild happens async from meta.json. Dead-route
        // cleanup is deferred to AFTER rebuild (a route to a sid that fails to
        // rebuild is dropped there), since nothing is live yet at this point.
        self.sessions.clear();
        self.restore_pending = saved.live_sids;
        Ok(())
    }

    /// `routing.json` doesn't exist yet — either this is the daemon's very
    /// first boot, or the file was lost some other way. There is no persisted
    /// live-set to trust, but every session's `meta.json` is real, independent,
    /// on-disk history (written at spawn, per project) — rebuild the live-set
    /// from it rather than starting blank, which would otherwise read as
    /// "every in-flight chat session vanished" the moment routing.json is
    /// absent. A chat's `current_project`/`current_session` FOCUS still starts
    /// blank: `meta.json`'s `owner` tag is deliberately `"channel:chat_id"`
    /// only (project ownership is per-CHAT, see [`ChatKey::identity`]), so it
    /// can't be turned back into the exact `(channel, chat_id, user_id)` key
    /// the routing maps use — guessing would risk steering one user's message
    /// into another user's recovered session. The owner just needs one
    /// `/use <sid>` (surfaced by `/sessions`, which reads the live map this
    /// populates) to reattach.
    ///
    /// Best-effort and one-time: `meta.json` carries no "explicitly stopped"
    /// marker (`/stop` never touches it), so a session the owner stopped long
    /// ago could get resurrected here too. That's an acceptable one-off cost
    /// (a stray `/stop` afterwards) against the alternative of silently
    /// dropping genuinely-live conversations.
    fn recover_routing_from_meta(&mut self) {
        self.restore_pending = self
            .projects
            .values()
            .flat_map(|dir| list_session_metas(dir))
            .map(|meta| meta.sid)
            .collect();
    }

    /// v0.8.21 Wave-2 — persist the ROUTING snapshot (per-chat focus + the
    /// current live-set). Idempotent: always serializes the full current state,
    /// so every call site need only call this after ANY routing / live-map
    /// change without tracking exactly what moved. Session content is NOT
    /// written (it lives in `meta.json`).
    ///
    /// Durability review P0-5: `atomic_write_durable` fsyncs the tmp file
    /// before rename (+ best-effort parent-dir fsync) so a power loss can't
    /// roll `live_sids` back to a stale snapshot.
    fn persist_routing(&self) -> Result<()> {
        let Some(path) = self.routing_path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let saved = RoutingState {
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
            live_sids: self.sessions.keys().cloned().collect(),
        };
        atomic_write_durable(path, &serde_json::to_vec_pretty(&saved)?)
    }

    /// v0.8.21 Wave-2 — persist the monotonic session-id counter to its own
    /// file. Called right after each `next_session` bump so the counter is
    /// durable BEFORE the sid is used (a spawn failure then leaves a harmless
    /// gap, never a reused sid).
    ///
    /// Durability review P0-5: this is the file whose power-loss rollback
    /// risk is sharpest — a reverted counter means a REUSED sid, breaking
    /// the "sid never reused" invariant. `atomic_write_durable` fsyncs the
    /// tmp file before rename (+ best-effort parent-dir fsync).
    fn persist_next_sid(&self) -> Result<()> {
        let Some(path) = self.next_sid_path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write_durable(path, self.next_session.to_string().as_bytes())
    }

    fn load_scheduled_state(&mut self) {
        if let Some(path) = self.next_scheduled_path.as_ref() {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(next) = raw.trim().parse::<u64>() {
                    self.next_scheduled = next;
                }
            }
        }
        self.scheduled_items.clear();
        for (project_dir, item) in crate::scheduled::scan_scheduled(&self.projects) {
            if let Some(number) = item
                .id
                .strip_prefix('d')
                .and_then(|n| n.parse::<u64>().ok())
            {
                self.next_scheduled = self.next_scheduled.max(number);
            }
            self.scheduled_items
                .insert(item.id.clone(), ScheduledEntry { project_dir, item });
        }
        // If a surviving queue proves the counter file was stale/missing, repair
        // it before accepting another create. Fired ids still rely on the
        // durable counter, which is written before their queue row is created.
        if let Err(err) = self.persist_next_scheduled() {
            tracing::warn!(error = %err, "failed to repair scheduled-message counter");
        }
        self.gc_failed_scheduled(chrono::Utc::now());
    }

    fn persist_next_scheduled(&self) -> Result<()> {
        let Some(path) = self.next_scheduled_path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write_durable(path, self.next_scheduled.to_string().as_bytes())
    }

    fn scheduled_target(&self, sid: &str) -> Result<(String, PathBuf)> {
        if let Some(session) = self.sessions.get(sid) {
            let dir = self
                .projects
                .get(&session.project)
                .cloned()
                .ok_or_else(|| anyhow!("unknown project for session {sid}"))?;
            return Ok((session.project.clone(), dir));
        }
        self.find_meta_for_sid(sid)
            .map(|(slug, dir, _)| (slug, dir))
            .map_err(|_| anyhow!("unknown session: {sid}"))
    }

    fn persist_scheduled_sid(&self, project_dir: &Path, sid: &str) -> Result<()> {
        let mut items = self
            .scheduled_items
            .values()
            .filter(|entry| entry.item.sid == sid)
            .map(|entry| entry.item.clone())
            .collect::<Vec<_>>();
        items.sort_by(crate::scheduled::scheduled_order);
        crate::scheduled::write_scheduled(project_dir, sid, &items)
    }

    /// Pending rows visible in a set of projects. `None` means every project
    /// (admin); tenants pass their project ACL set from the REST layer.
    pub fn scheduled_pending_count_in_projects(
        &self,
        visible_projects: Option<&HashSet<String>>,
    ) -> usize {
        self.scheduled_items
            .values()
            .filter(|entry| {
                entry.item.status == crate::scheduled::ScheduledStatus::Pending
                    && visible_projects
                        .map(|projects| projects.contains(&entry.item.project))
                        .unwrap_or(true)
            })
            .count()
    }

    /// List pending and short-lived failed rows for one sid.
    pub fn scheduled_items_for_sid(
        &mut self,
        sid: &str,
    ) -> Result<Vec<crate::scheduled::ScheduledItem>> {
        self.scheduled_target(sid)?;
        self.gc_failed_scheduled(chrono::Utc::now());
        let mut items = self
            .scheduled_items
            .values()
            .filter(|entry| entry.item.sid == sid)
            .map(|entry| entry.item.clone())
            .collect::<Vec<_>>();
        items.sort_by(crate::scheduled::scheduled_order);
        Ok(items)
    }

    /// REST/web create. The route supplies its already-resolved visible project
    /// set so the 100-pending human limit is checked atomically under the same
    /// gateway lock as insertion.
    pub fn create_scheduled_message(
        &mut self,
        sid: &str,
        text: String,
        send_at: chrono::DateTime<chrono::Utc>,
        created_by: String,
        visible_projects: Option<&HashSet<String>>,
    ) -> Result<crate::scheduled::ScheduledItem> {
        let visible_count = self.scheduled_pending_count_in_projects(visible_projects);
        self.create_scheduled_message_inner(
            sid,
            text,
            send_at,
            created_by,
            None,
            None,
            visible_count,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_scheduled_message_inner(
        &mut self,
        sid: &str,
        text: String,
        send_at: chrono::DateTime<chrono::Utc>,
        created_by: String,
        reply_channel: Option<String>,
        reply_chat_id: Option<String>,
        visible_pending: usize,
    ) -> Result<crate::scheduled::ScheduledItem> {
        if text.trim().is_empty() {
            return Err(anyhow!("scheduled message text cannot be empty"));
        }
        let now = chrono::Utc::now();
        if send_at <= now {
            return Err(anyhow!("scheduled time must be in the future"));
        }
        if send_at.signed_duration_since(now) > crate::scheduled::MAX_HORIZON {
            return Err(anyhow!("scheduled time must be within 7 days"));
        }
        let (project, project_dir) = self.scheduled_target(sid)?;
        let sid_pending = self
            .scheduled_items
            .values()
            .filter(|entry| {
                entry.item.sid == sid
                    && entry.item.status == crate::scheduled::ScheduledStatus::Pending
            })
            .count();
        if sid_pending >= crate::scheduled::MAX_PENDING_PER_SID {
            return Err(anyhow!(
                "session {sid} already has {} pending scheduled messages (limit {})",
                sid_pending,
                crate::scheduled::MAX_PENDING_PER_SID
            ));
        }
        if visible_pending >= crate::scheduled::MAX_PENDING_VISIBLE {
            return Err(anyhow!(
                "this chat already sees {} pending scheduled messages (limit {})",
                visible_pending,
                crate::scheduled::MAX_PENDING_VISIBLE
            ));
        }

        self.next_scheduled += 1;
        // Counter durability comes before queue insertion: a later write failure
        // burns a harmless gap but can never reuse the id.
        self.persist_next_scheduled()?;
        let item = crate::scheduled::ScheduledItem {
            id: format!("d{}", self.next_scheduled),
            sid: sid.to_string(),
            project,
            text,
            send_at,
            created_at: now,
            created_by,
            status: crate::scheduled::ScheduledStatus::Pending,
            fail_reason: None,
            failed_at: None,
            reply_channel,
            reply_chat_id,
        };
        self.scheduled_items.insert(
            item.id.clone(),
            ScheduledEntry {
                project_dir: project_dir.clone(),
                item: item.clone(),
            },
        );
        if let Err(err) = self.persist_scheduled_sid(&project_dir, sid) {
            self.scheduled_items.remove(&item.id);
            return Err(err);
        }
        self.emit_scheduled_progress(
            &item,
            ccteam_harness::execution::progress_bridge::SCHEDULED_ENQUEUED,
            None,
        );
        self.emit_scheduled_changed(&item);
        self.scheduled_notify.notify_one();
        Ok(item)
    }

    /// Cancel/dismiss one pending or failed row. The sid is part of the REST
    /// resource address and prevents a globally-valid id being cancelled under
    /// another session path.
    pub fn cancel_scheduled_message(
        &mut self,
        sid: &str,
        id: &str,
    ) -> Result<crate::scheduled::ScheduledItem> {
        let entry = self
            .scheduled_items
            .get(id)
            .filter(|entry| entry.item.sid == sid)
            .cloned()
            .ok_or_else(|| anyhow!("unknown scheduled message: {id}"))?;
        self.scheduled_items.remove(id);
        if let Err(err) = self.persist_scheduled_sid(&entry.project_dir, sid) {
            self.scheduled_items.insert(id.to_string(), entry.clone());
            return Err(err);
        }
        self.emit_scheduled_progress(
            &entry.item,
            ccteam_harness::execution::progress_bridge::SCHEDULED_CANCELLED,
            None,
        );
        self.emit_scheduled_changed(&entry.item);
        self.scheduled_notify.notify_one();
        Ok(entry.item)
    }

    fn emit_scheduled_progress(
        &self,
        item: &crate::scheduled::ScheduledItem,
        event: &str,
        reason: Option<&str>,
    ) {
        let Some(paths) = self.project_paths.as_ref() else {
            return;
        };
        let preview = crate::scheduled::preview(&item.text);
        let row = ccteam_harness::execution::progress_bridge::build_scheduled_event(
            event,
            &item.id,
            &item.sid,
            &item.send_at.to_rfc3339(),
            (!preview.is_empty()).then_some(preview.as_str()),
            reason,
        );
        if let Err(err) =
            ccteam_core::progress::append_event(&paths.progress_jsonl(&item.project), &row)
        {
            tracing::warn!(id = %item.id, sid = %item.sid, error = %err, "append scheduled progress failed");
        }
    }

    fn emit_scheduled_changed(&self, item: &crate::scheduled::ScheduledItem) {
        let _ = self.events_broadcast.send(GatewayEvent {
            id: format!("scheduled-changed-{}-{}", item.sid, item.id),
            channel: "web".to_string(),
            chat_id: "web-api".to_string(),
            thread_ts: None,
            content: String::new(),
            kind: GatewayEventKind::ScheduledChanged,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(item.sid.clone()),
            slug: Some(item.project.clone()),
        });
    }

    fn gc_failed_scheduled(&mut self, now: chrono::DateTime<chrono::Utc>) {
        let expired = self
            .scheduled_items
            .iter()
            .filter(|(_, entry)| entry.item.failed_expired(now))
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect::<Vec<_>>();
        for (id, entry) in expired {
            self.scheduled_items.remove(&id);
            if let Err(err) = self.persist_scheduled_sid(&entry.project_dir, &entry.item.sid) {
                tracing::warn!(id = %id, error = %err, "persist scheduled failed-item GC failed");
                self.scheduled_items.insert(id, entry);
                continue;
            }
            self.emit_scheduled_changed(&entry.item);
        }
    }

    fn next_scheduled_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.scheduled_items
            .values()
            .filter_map(|entry| match entry.item.status {
                crate::scheduled::ScheduledStatus::Pending => Some(entry.item.send_at),
                crate::scheduled::ScheduledStatus::Failed => entry
                    .item
                    .failed_at
                    .map(|at| at + crate::scheduled::FAILED_RETENTION),
            })
            .min()
    }

    async fn fire_due_scheduled(&mut self, now: chrono::DateTime<chrono::Utc>) {
        let mut due = self
            .scheduled_items
            .values()
            .filter(|entry| {
                entry.item.status == crate::scheduled::ScheduledStatus::Pending
                    && entry.item.send_at <= now
            })
            .cloned()
            .collect::<Vec<_>>();
        due.sort_by(|a, b| crate::scheduled::scheduled_order(&a.item, &b.item));
        for entry in due {
            if now.signed_duration_since(entry.item.send_at) > crate::scheduled::MAX_CATCH_UP_AGE {
                self.fail_scheduled(
                    &entry.item.id,
                    "catch-up is older than 24 hours".to_string(),
                );
                continue;
            }
            let result = self.submit_scheduled_user_turn(&entry.item).await;
            match result {
                Ok(()) => self.complete_scheduled_fire(&entry),
                Err(err) => self.fail_scheduled(&entry.item.id, format!("{err:#}")),
            }
        }
    }

    async fn submit_scheduled_user_turn(
        &mut self,
        item: &crate::scheduled::ScheduledItem,
    ) -> Result<()> {
        let chat = match (&item.reply_channel, &item.reply_chat_id) {
            (Some(channel), Some(chat_id)) => ChatKey::new(channel, chat_id, chat_id),
            _ => web_api_chat(),
        };
        match self
            .submit_resolved(
                &chat,
                &item.sid,
                "",
                item.text.clone(),
                TurnOrigin::User,
                true,
            )
            .await?
        {
            SubmitResult::Turn { .. } => Ok(()),
            SubmitResult::Directive(_) => Err(anyhow!("scheduled body was parsed as a directive")),
        }
    }

    fn complete_scheduled_fire(&mut self, entry: &ScheduledEntry) {
        self.scheduled_items.remove(&entry.item.id);
        if let Err(err) = self.persist_scheduled_sid(&entry.project_dir, &entry.item.sid) {
            tracing::warn!(id = %entry.item.id, error = %err, "persist fired scheduled removal failed");
        }
        self.emit_scheduled_progress(
            &entry.item,
            ccteam_harness::execution::progress_bridge::SCHEDULED_FIRED,
            None,
        );
        self.emit_scheduled_changed(&entry.item);
    }

    fn fail_scheduled(&mut self, id: &str, reason: String) {
        let Some(mut entry) = self.scheduled_items.get(id).cloned() else {
            return;
        };
        entry.item.status = crate::scheduled::ScheduledStatus::Failed;
        entry.item.fail_reason = Some(reason.clone());
        entry.item.failed_at = Some(chrono::Utc::now());
        self.scheduled_items.insert(id.to_string(), entry.clone());
        if let Err(err) = self.persist_scheduled_sid(&entry.project_dir, &entry.item.sid) {
            tracing::warn!(id = %id, error = %err, "persist scheduled failure failed");
        }
        self.emit_scheduled_progress(
            &entry.item,
            ccteam_harness::execution::progress_bridge::SCHEDULED_FAILED,
            Some(&reason),
        );
        self.emit_scheduled_changed(&entry.item);
        if let (Some(channel), Some(chat_id)) =
            (&entry.item.reply_channel, &entry.item.reply_chat_id)
        {
            self.emit_user_signal(GatewayEvent {
                id: format!("scheduled-failed-{}", entry.item.id),
                channel: channel.clone(),
                chat_id: chat_id.clone(),
                thread_ts: None,
                content: format!(
                    "⏰ {} failed to send to {}: {} (kept in /inbox for 24h)",
                    entry.item.id, entry.item.sid, reason
                ),
                kind: GatewayEventKind::Answer,
                attachments: Vec::new(),
                options: Vec::new(),
                sid: Some(entry.item.sid.clone()),
                slug: Some(entry.item.project.clone()),
            });
        }
    }

    /// Lightweight wakeable next-fire task. It sleeps until the earliest UTC
    /// deadline and is notified on every queue mutation; there is no periodic
    /// orchestrator tick.
    pub async fn run_scheduled_scheduler(gateway: Arc<tokio::sync::Mutex<Self>>) {
        let notify = { Arc::clone(&gateway.lock().await.scheduled_notify) };
        loop {
            let next = {
                let mut guard = gateway.lock().await;
                let now = chrono::Utc::now();
                guard.gc_failed_scheduled(now);
                if guard.next_scheduled_at().is_some_and(|at| at <= now) {
                    guard.fire_due_scheduled(now).await;
                }
                guard.next_scheduled_at()
            };
            match next {
                Some(at) => {
                    let delay = at
                        .signed_duration_since(chrono::Utc::now())
                        .to_std()
                        .unwrap_or_default();
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = notify.notified() => {}
                    }
                }
                None => notify.notified().await,
            }
        }
    }

    /// Process restart catch-up before the general live-session restore. A due
    /// target cold-resumes itself through `submit_resolved`; the subsequent
    /// restore sees it in the live map and skips a duplicate spawn.
    pub async fn catch_up_scheduled(gateway: Arc<tokio::sync::Mutex<Self>>) {
        let mut guard = gateway.lock().await;
        let now = chrono::Utc::now();
        guard.gc_failed_scheduled(now);
        guard.fire_due_scheduled(now).await;
    }

    /// v0.8.8 bug-fix — persist the USER side of a turn to
    /// `.ccteam/chat/<sid>/turns.jsonl`. The event pump only observes ANSWER
    /// events, so it writes assistant-only records; without this the user's
    /// prompts never land in the mirror and a session reopened from history
    /// (`GET /sessions/{sid}`) shows only the agent's replies (the user's
    /// messages "disappear" on session switch). Appended at submit time as a
    /// user-only record keyed by the submission's unique input id; the pump
    /// later appends the assistant-only vendor-turn record. `historyToRows`
    /// renders both in append order without duplicate row keys when several
    /// inputs join one vendor turn. Best-effort: warns on failure, never blocks
    /// the turn; holds no gateway lock (O_APPEND atomic write).
    fn mirror_user_turn(&self, session: &GatewaySession, user_text: &str, turn_id: &str) {
        if user_text.is_empty() {
            return;
        }
        let Some(project_dir) = self.projects.get(&session.project).cloned() else {
            return;
        };
        // v0.8.22 P1 — the session's default title = a rule-based truncation
        // of its FIRST user message (never an LLM call — pure string logic).
        // Gated on `title.is_none()` so this only ever fires once: a vendor
        // title adopted at import, or an earlier `/rename`, already occupies
        // the slot and is left alone (`apply_title`'s precedence would reject
        // an Auto write over either anyway — this check just skips the
        // redundant meta.json read/write on every later turn).
        if let Ok(mut meta) = read_session_meta(&project_dir, &session.id) {
            if meta.title.is_none() {
                if let Some(candidate) = truncate_title(user_text) {
                    if apply_title(&mut meta, candidate, TitleSource::Auto) {
                        let _ = write_session_meta(&project_dir, &meta);
                    }
                }
            }
        }
        let record = ccteam_harness::execution::turns_mirror::TurnRecord {
            turn_id: turn_id.to_string(),
            ts: chrono::Utc::now(),
            vendor: vendor_str(session.vendor).to_string(),
            role: session.role.clone(),
            user: user_text.to_string(),
            assistant: String::new(),
            usage: serde_json::Value::Null,
            tool_calls: Vec::new(),
            outcome: None,
            error_kind: None,
            error: None,
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

    async fn submit_to_current(
        &mut self,
        chat: &ChatKey,
        message_id: &str,
        payload: String,
    ) -> Result<Vec<String>> {
        let session_id = self
            .current_session
            .read()
            .unwrap()
            .get(chat)
            .ok_or_else(|| anyhow!("no current session for chat"))?
            .clone();
        // Both user-entry legs (this IM path and the web `submit_to_sid`) drive
        // a session through the SAME `submit_resolved` core, so directive
        // handling can never drift between them — the bug this unification
        // closed: the web leg used to skip the directive parse and ship
        // `/model …` to the agent as literal text. The IM handler returns the
        // synchronous receipts up the inbound stack to send over the channel;
        // an async turn's answer streams over the event sink (empty Vec here).
        // `message_id` (the inbound IM message) seeds the 👀 ack reaction when a
        // real turn is submitted (empty for the web leg → no reaction).
        match self
            .submit_resolved(
                chat,
                &session_id,
                message_id,
                payload,
                TurnOrigin::User,
                false,
            )
            .await?
        {
            SubmitResult::Directive(replies) => Ok(replies),
            SubmitResult::Turn { drained, .. } => Ok(drained),
        }
    }

    /// Transparently re-establish a session whose underlying child has exited
    /// (crash / OOM / a long idle window) — the "会话 = resume-by-session-id"
    /// red line for the dead-child case. Re-spawns via the resume-aware
    /// `start_thread` with the SAME sid (→ same deterministic vendor uuid →
    /// `--resume`), REUSING the session's project / role / vendor / protocol /
    /// permission posture / cto-gate secret, so the Anthropic conversation
    /// resumes from its transcript (context preserved). The fresh handle/adapter
    /// are swapped into the existing [`GatewaySession`] IN PLACE (every
    /// Arc-shared cell — `reply_to` / counters — survives); the stale event pump
    /// (already ending on the closed transport) is dropped and a fresh one is
    /// bound to the resumed transport. NOT a `/new`: same sid, same identity, no
    /// fresh context.
    ///
    /// LOCK SCOPE (v0.9 T2): three-phase plan → spawn → apply, same shape as
    /// [`Self::start_session`] / [`Self::handle_message_shared`]. The slow
    /// `start_thread` await does **not** need the gateway lock structurally —
    /// [`Self::resume_dead_session_shared`] is the lock-free entry (claim sid →
    /// brief plan lock → spawn with no gateway lock → brief apply lock). This
    /// `&mut self` form still composes the three phases under whatever outer
    /// caller lock is held (submit / directive paths today).
    ///
    /// LOCK ORDER (v0.9 T2 review fix): the per-sid [`SpawnClaims`] claim is
    /// acquired strictly BEFORE the gateway lock (see the shared flavor) and
    /// must NEVER be awaited while holding it. This `&mut self` form runs
    /// under the caller's gateway lock, so it takes NO claim — awaiting one
    /// here ABBA-deadlocks against a shared-flavor resume that holds the claim
    /// and is waiting for the gateway lock to plan/apply. Races with such a
    /// concurrent resume are instead resolved by the generation check in
    /// [`Self::apply_resume_dead_session`]: worst case one freshly spawned
    /// thread loses and is discarded via `close_thread` (never a zombie).
    async fn resume_dead_session(&mut self, session_id: &str) -> Result<()> {
        let Some(plan) = self.plan_resume_dead_session(session_id)? else {
            // Concurrent resume finished first; child is already live.
            return Ok(());
        };
        let thread = Self::spawn_for_resume_plan(&plan).await?;
        self.apply_resume_dead_session(plan, thread).await
    }

    /// v0.9 T2 — shared-handle flavor of dead-child resume: holds the gateway
    /// lock only across plan + apply; the slow `start_thread` runs with **no**
    /// gateway lock. Per-sid single-flight via [`SpawnClaims::lock_for_sid`].
    pub async fn resume_dead_session_shared(
        gateway: Arc<tokio::sync::Mutex<Gateway>>,
        session_id: &str,
    ) -> Result<()> {
        let claims = Arc::clone(&gateway.lock().await.spawn_claims);
        let _claim = claims.lock_for_sid(session_id).await;
        let plan = {
            let mut g = gateway.lock().await;
            g.plan_resume_dead_session(session_id)?
        };
        let Some(plan) = plan else {
            return Ok(());
        };
        // Slow part — deliberately NO gateway lock held here.
        let thread = Self::spawn_for_resume_plan(&plan).await?;
        gateway
            .lock()
            .await
            .apply_resume_dead_session(plan, thread)
            .await
    }

    /// v0.9 T2 — sync plan half of dead-child resume. Snapshots spawn inputs +
    /// a generation marker from the current thread. Returns `Ok(None)` when the
    /// child is already live (a concurrent resume already finished). Does **not**
    /// claim the sid (claim is async; the shared/`&mut self` wrappers own it).
    fn plan_resume_dead_session(&mut self, session_id: &str) -> Result<Option<ResumeDeadPlan>> {
        let s = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session vanished: {session_id}"))?;
        // Second waiter after a concurrent resume: re-plan finds the child live.
        if s.adapter.thread_is_live(&s.thread) {
            return Ok(None);
        }
        let project = s.project.clone();
        let role = s.role.clone();
        let vendor = s.vendor;
        let protocol = s.protocol;
        let host = s.host.clone();
        let permission_mode = s.permission_mode;
        let secret = s.secret.clone();
        let generation = format!("{}@{}", s.thread.identity, s.thread.started_at);
        // Sync a possibly-registered-after-start project from the config.yaml
        // SoT before the lookup (mirrors start_session / `/role`).
        self.ensure_project_loaded(&project);
        let cwd = self
            .projects
            .get(&project)
            .cloned()
            .ok_or_else(|| anyhow!("unknown project: {project}"))?;
        let role_detail = ensure_role_exists(&cwd, &role)?;
        // One read for both axes: `model` falls back to the role's frontmatter
        // (a role may pin a model), `effort` has no such fallback — the vendor
        // default is the honest answer when the session never named one.
        let meta = read_session_meta(&cwd, session_id).ok();
        let model_id = meta
            .as_ref()
            .and_then(|m| m.model.clone())
            .or_else(|| role_model_id(role_detail.as_ref()));
        let effort = meta.and_then(|m| m.effort);
        let (host, wire_slug) = self.ensure_session_host_binding(&project, &host)?;
        // Reuse the existing secret: the resumed child's env is re-stamped with
        // it, so pane-env and the cto-gate map stay in lockstep (no fresh mint →
        // no stored-secret update).
        let adapter = (self.adapter_factory)(vendor, protocol);
        Ok(Some(ResumeDeadPlan {
            session_id: session_id.to_string(),
            project,
            role,
            vendor,
            protocol,
            host,
            wire_slug,
            permission_mode,
            secret,
            cwd,
            model_id,
            effort,
            adapter,
            generation,
            ccteam_root: self.project_paths.as_ref().map(|p| p.root.clone()),
            remote_proxy: self.remote_host_proxy.clone(),
        }))
    }

    /// v0.9 T2 — the SLOW await for a [`ResumeDeadPlan`]. Self-less so a caller
    /// can run it with NO gateway lock held — same SpawnCtx assembly as the
    /// resume path of [`Self::spawn_session_thread`].
    async fn spawn_for_resume_plan(plan: &ResumeDeadPlan) -> Result<ThreadHandle, HarnessError> {
        // v0.9.0 W3 (G10) — re-gate a non-local host right before the spawn;
        // see `spawn_for_plan`'s doc for the full contract.
        let remote = crate::remote_host::regate_remote_host(
            plan.ccteam_root.as_deref(),
            &plan.host,
            &plan.wire_slug,
            plan.protocol,
            plan.remote_proxy.as_ref(),
        )
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("remote host re-gate: {e:#}")))?;
        plan.adapter
            .start_thread(
                &AgentSpecBrief {
                    role: plan.role.clone(),
                },
                &SpawnCtx {
                    slug: plan.project.clone(),
                    sid: plan.session_id.clone(),
                    cwd: plan.cwd.clone(),
                    project_dir: plan.cwd.clone(),
                    extra_args: vec![],
                    model_id: plan.model_id.clone(),
                    effort: plan.effort.clone(),
                    permission_mode: plan.permission_mode,
                    secret: plan.secret.clone(),
                    remote,
                },
            )
            .await
    }

    /// v0.9 T2 — re-lock apply after a dead-child spawn. Verifies the session
    /// still exists and the generation marker still matches; if not (stopped /
    /// replaced meanwhile) closes the freshly spawned thread and returns Err
    /// without inserting a zombie. On match: swap thread/adapter in place and
    /// abort + respawn the event pump.
    async fn apply_resume_dead_session(
        &mut self,
        plan: ResumeDeadPlan,
        thread: ThreadHandle,
    ) -> Result<()> {
        let ResumeDeadPlan {
            session_id,
            project,
            role,
            vendor,
            protocol,
            host: _,
            wire_slug: _,
            permission_mode: _,
            secret: _,
            cwd: _,
            model_id: _,
            effort: _,
            adapter,
            generation,
            ccteam_root: _,
            remote_proxy: _,
        } = plan;
        let still_matches = self.sessions.get(&session_id).is_some_and(|s| {
            format!("{}@{}", s.thread.identity, s.thread.started_at) == generation
        });
        if !still_matches {
            // Session was stopped or its thread was replaced while we spawned —
            // do NOT insert a zombie; close the fresh thread gracefully.
            let _ = adapter.close_thread(&thread).await;
            tracing::warn!(
                session_id = %session_id,
                vendor = ?vendor,
                protocol = ?protocol,
                "ccteam-im: discarded resumed thread (session gone or replaced during spawn)"
            );
            return Err(anyhow!(
                "session vanished or replaced during resume: {session_id}"
            ));
        }
        // Swap the fresh handle/adapter into the existing GatewaySession in
        // place — every Arc-shared cell (owner / reply_to / counters) survives.
        // Turn lifecycle (turn_started_at / latest_activity) is owned by the
        // submit flow + the pump, so resume leaves it ALONE: the turn path
        // re-stamps turn_started_at itself, and clearing it here would race the
        // in-flight reactive retry (a turn IS running when that path resumes).
        if let Some(s) = self.sessions.get_mut(&session_id) {
            s.thread = thread;
            s.adapter = adapter;
        }
        // Drop the stale pump (its task is already ending — it was bound to the
        // now-closed transport) so spawn_event_pump installs a fresh one on the
        // resumed transport (it early-returns while a handle is still keyed).
        if let Some(old) = self.event_pumps.remove(&session_id) {
            old.abort();
        }
        self.spawn_event_pump(&session_id);
        // Note: do NOT drain pending turns here — apply_resume is often
        // called from inside submit_resolved (ThreadDied retry). Draining
        // would recurse. Fresh-start path drains after spawn_event_pump.
        tracing::info!(
            session_id = %session_id,
            project = %project,
            role = %role,
            vendor = ?vendor,
            protocol = ?protocol,
            "ccteam-im: resumed dead session in place (child had exited)"
        );
        Ok(())
    }

    /// Build the [`SpawnCtx`] for a session thread and start it via the
    /// (vendor, protocol) adapter — the SINGLE assembly point every spawn site
    /// shares (fresh `/new` `start_session`, `/role` re-spawn
    /// `switch_current_role`, dead-child [`resume_dead_session`]) so the ctx
    /// fields can't drift between them. Returns the bound adapter + handle; the
    /// caller records them (insert a fresh [`GatewaySession`], or swap in place).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    async fn spawn_session_thread(
        &self,
        vendor: AgentVendor,
        protocol: SessionProtocol,
        role: &str,
        slug: &str,
        sid: &str,
        cwd: PathBuf,
        model_id: Option<String>,
        // Replayed from `meta.effort` — a re-spawn must not reset the axis the
        // caller picked (`/role` swaps the ROLE, not the reasoning level).
        effort: Option<String>,
        permission_mode: PermissionMode,
        secret: String,
        host: &str,
    ) -> Result<(Arc<dyn HarnessAdapter + Send + Sync>, ThreadHandle), HarnessError> {
        let (bound_host, wire_slug) = self
            .ensure_session_host_binding(slug, host)
            .map_err(|e| HarnessError::SpawnFailed(e.to_string()))?;
        // v0.9.0 W3 (G10) — a `/role` switch re-spawns the SAME sid on the
        // SAME host; re-gate a non-local host here too (same contract as
        // `spawn_for_plan`/`spawn_for_resume_plan` — this is, in effect, a
        // third rebuild path).
        let remote = crate::remote_host::regate_remote_host(
            self.project_paths.as_ref().map(|p| p.root.as_path()),
            &bound_host,
            &wire_slug,
            protocol,
            self.remote_host_proxy.as_ref(),
        )
        .await
        .map_err(|e| HarnessError::SpawnFailed(format!("remote host re-gate: {e:#}")))?;
        let adapter = (self.adapter_factory)(vendor, protocol);
        let thread = adapter
            .start_thread(
                &AgentSpecBrief {
                    role: role.to_string(),
                },
                &SpawnCtx {
                    slug: slug.to_string(),
                    sid: sid.to_string(),
                    cwd: cwd.clone(),
                    project_dir: cwd,
                    extra_args: vec![],
                    model_id,
                    effort,
                    permission_mode,
                    secret,
                    remote,
                },
            )
            .await?;
        Ok((adapter, thread))
    }

    /// Probe the session's liveness and transparently [`resume_dead_session`] it
    /// if its child has died. A no-op for a live session (the hot path) and for
    /// adapters that never silently die (default `thread_is_live` → `true`).
    ///
    /// This PRE-CHECK guards the DIRECTIVE path (`/compact`, `/clear`, …): a
    /// directive can have side effects, so it must NOT be blindly retried on a
    /// failure — probe-then-dispatch is the safe shape. The TURN path takes the
    /// opposite, race-free shape (submit → on [`HarnessError::ThreadDied`]
    /// resume + retry once): a turn is idempotent on a dead-before-delivery
    /// signal, so reacting to the failure closes the probe→send TOCTOU window a
    /// pre-check inherently leaves open.
    async fn ensure_session_live(&mut self, session_id: &str, chat: &ChatKey) -> Result<()> {
        // Deepest rung: a session absent from the live map entirely (evicted /
        // restart-rebuild-failed / stopped) is cold-resumed from meta.json, so a
        // directive (`/model`, …) revives a "disappeared" session exactly like a
        // turn does. Err for a genuinely unknown sid.
        self.cold_resume_absent_sid(session_id, chat).await?;
        let alive = match self.sessions.get(session_id) {
            Some(s) => s.adapter.thread_is_live(&s.thread),
            // Vanished post-resume (race) → let the caller surface its error.
            None => return Ok(()),
        };
        if !alive {
            self.resume_dead_session(session_id).await?;
        }
        Ok(())
    }

    /// Resume-by-sid, deepest rung: if `sid` is ABSENT from the live map —
    /// evicted for capacity, dropped by a daemon restart whose rebuild failed,
    /// or explicitly stopped — cold-resume it from its on-disk `meta.json` (a
    /// fresh live thread), binding replies to `reply_to`. A no-op when the sid
    /// is already in the map (a live / in-map-dead session is handled by the
    /// caller's own live/dead ladder). `Err` only for a genuinely unknown sid
    /// (no meta). This is the deeper twin of [`resume_dead_session`]: keeping
    /// BOTH rungs in the shared submit core ([`submit_resolved`] + the directive
    /// path's [`ensure_session_live`]) means every submit-by-sid path — IM
    /// current-session, `@handle`, MCP `session_dispatch`, the web turn — revives
    /// a session that left the live map identically, rather than each frontend
    /// special-casing it. ACL is the caller's concern (each frontend gates who
    /// may address a sid before reaching the submit core).
    async fn cold_resume_absent_sid(&mut self, sid: &str, reply_to: &ChatKey) -> Result<()> {
        if self.sessions.contains_key(sid) {
            return Ok(());
        }
        let (slug, cwd, meta) = self
            .find_meta_for_sid(sid)
            .map_err(|_| anyhow!("unknown session: {sid}"))?;
        self.rebuild_session_from_meta(&slug, cwd, &meta, reply_to.clone())
            .await
    }

    /// Shared submit core (sid already resolved). A single-line `/command` is a
    /// session directive interpreted by the owning adapter — the only thing
    /// that knows its vendor's command surface (`/model` → `set_model`, etc.);
    /// anything else (incl. multi-line text starting with `/`, e.g. a pasted
    /// path / code block) is ordinary user text submitted as a turn. `chat` is
    /// the reply target written to the session's `reply_to`. Returns either the
    /// directive's synchronous receipts or the new turn's id + any sink-less
    /// drained answer; each caller takes the leg it needs (IM returns the
    /// replies; the web/MCP leg returns the turn id and SSE-emits the receipts).
    async fn submit_resolved(
        &mut self,
        chat: &ChatKey,
        session_id: &str,
        message_id: &str,
        payload: String,
        origin: TurnOrigin,
        literal_user_text: bool,
    ) -> Result<SubmitResult> {
        if !literal_user_text {
            if let Some(directive) = parse_session_directive(&payload) {
                // Directive path: PROBE-and-resume a dead child before dispatching
                // (a directive may have side effects → never blindly retried). The
                // turn path below uses the race-free reactive shape instead.
                self.ensure_session_live(session_id, chat).await?;
                let replies = self
                    .dispatch_directive(chat, session_id, directive, origin)
                    .await?;
                return Ok(SubmitResult::Directive(replies));
            }
        }

        // Resume-by-sid (deepest rung): a session ABSENT from the live map —
        // evicted for capacity, dropped by a daemon restart whose rebuild failed,
        // or stopped — is cold-resumed from meta.json here, so the turn revives
        // it exactly as the in-map dead-child case below does (and symmetric with
        // the web turn / MCP dispatch, which funnel through this same core). Err
        // (→ caller) for a genuinely unknown sid; a no-op when already in the map.
        self.cold_resume_absent_sid(session_id, chat).await?;
        // v0.8.24 F5 — if the child is not live (starting/resuming/dead),
        // enqueue the user text (FIFO, file-backed) and revive; drain after
        // live so turns are not lost. Gateway remains the sole turns writer
        // (drain re-enters submit_resolved once live). Callers get the real
        // drained turn id when resume+submit succeeds.
        let not_live = match self.sessions.get(session_id) {
            Some(s) => !s.adapter.thread_is_live(&s.thread),
            None => {
                return Err(anyhow!("current session missing: {session_id}"));
            }
        };
        if not_live {
            let (project, origin) = {
                let s = self
                    .sessions
                    .get(session_id)
                    .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
                if let Ok(mut target) = s.reply_to.lock() {
                    *target = chat.clone();
                }
                (s.project.clone(), chat.channel.clone())
            };
            let cwd = self
                .projects
                .get(&project)
                .cloned()
                .ok_or_else(|| anyhow!("unknown project for pending turn: {project}"))?;
            crate::pending_turns::enqueue_pending_turn(
                &cwd,
                session_id,
                payload.clone(),
                Some(origin),
                literal_user_text,
            )?;
            self.resume_dead_session(session_id).await?;
            let drained_ids = self.drain_and_dispatch_pending_turns(session_id).await;
            let id = drained_ids
                .into_iter()
                .last()
                .unwrap_or_else(|| format!("pending-drain:{session_id}"));
            return Ok(SubmitResult::Turn {
                id,
                // Answers stream via the pump after drain re-submits.
                drained: Vec::new(),
            });
        }

        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
        // Replies for this turn go back to whoever sent it.
        if let Ok(mut target) = session.reply_to.lock() {
            *target = chat.clone();
        }
        // v0.8.19 `/status` — a real Turn is now in flight: stamp its start (the
        // pump clears it on `TurnCompleted`). A same-turn Inject preserves the
        // original timestamp; resetting it here made any long-running turn look
        // newly started whenever another message arrived. Directives never
        // reach here, so a `/model` switch is not counted as a working turn.
        // v0.9 T5 — if a prior turn was still in flight this is a mid-turn
        // steer; otherwise clear the steered flag for a fresh turn.
        let was_in_flight = session
            .turn_started_at
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false);
        let prior_steered = session.steered_this_turn.load(Ordering::SeqCst);
        let requested_routing = TurnRouting::Inject;
        let provisional_inject = was_in_flight && requested_routing == TurnRouting::Inject;
        if !was_in_flight {
            session.steered_this_turn.store(false, Ordering::SeqCst);
            if let Ok(mut started) = session.turn_started_at.lock() {
                *started = Some(Instant::now());
            }
        } else if provisional_inject {
            // Mark before awaiting the vendor acknowledgement: the active turn
            // may complete immediately after accepting an interjection. An
            // adapter that reports an actual Queue below rolls this provisional
            // bit back while the original turn is still active.
            session.steered_this_turn.store(true, Ordering::SeqCst);
        }
        // 👀 ack: add the transient "received, processing" reaction on the
        // inbound IM message the moment this turn is dispatched, filling the
        // silent time-to-first-token gap. IM-only (`web` has its own UI and no
        // inbound message_id → skip); needs a non-empty message_id. Record the
        // pending msg_id on the session so the event pump can clear it (emit
        // `Reaction{on:false}`) the instant the turn's first event appears.
        // Fire-and-forget: a reaction never affects turn submission.
        if chat.channel != "web" && !message_id.is_empty() {
            if let Ok(mut pending) = session.pending_reaction.lock() {
                *pending = Some(message_id.to_string());
            }
            self.emit_user_signal(GatewayEvent {
                id: format!("gateway-reaction-add-{session_id}-{message_id}"),
                channel: chat.channel.clone(),
                chat_id: chat.chat_id.clone(),
                thread_ts: None,
                content: String::new(),
                kind: GatewayEventKind::Reaction {
                    message_id: message_id.to_string(),
                    on: true,
                },
                attachments: Vec::new(),
                options: Vec::new(),
                sid: Some(session_id.to_string()),
                slug: Some(session.project.clone()),
            });
        }
        let start_visible_events = session.visible_events.load(Ordering::SeqCst);
        // v0.8.8 bug-fix — keep the user's prompt so it can be mirrored into
        // turns.jsonl after a successful submit (the pump records only the
        // assistant side, so without this the user's message is lost on a
        // history reseed / session switch).
        let user_text = payload.clone();
        // Submit with reactive resume-and-retry: a turn is idempotent on a
        // `ThreadDied` (the child exited before the line was delivered), so on
        // that signal resume-by-sid and retry EXACTLY once — closing the
        // probe→send race a pre-check can't. Any other error (a real rejection /
        // timeout) is surfaced as-is, never retried. Each attempt re-borrows the
        // session so the retry sees the resumed thread/adapter.
        let submit_wait = gateway_submit_timeout_duration();
        let mut attempt: u8 = 0;
        let submitted = loop {
            attempt += 1;
            let outcome = {
                let session = self
                    .sessions
                    .get(session_id)
                    .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
                tokio::time::timeout(
                    submit_wait,
                    session.adapter.submit_turn_routed(
                        &session.thread,
                        TurnInput::UserText(payload.clone()),
                        requested_routing,
                    ),
                )
                .await
            };
            match outcome {
                Err(_) => {
                    break Err(anyhow!(
                        "submit timed out after {submit_wait:?} for {session_id}"
                    ));
                }
                Ok(Ok(submitted)) => break Ok(submitted),
                Ok(Err(HarnessError::ThreadDied(_))) if attempt == 1 => {
                    if let Err(error) = self.resume_dead_session(session_id).await {
                        break Err(error);
                    }
                    continue;
                }
                Ok(Err(error)) => break Err(error.into()),
            }
        };
        let mut submitted = match submitted {
            Ok(submitted) => submitted,
            Err(error) => {
                if provisional_inject {
                    if let Some(session) = self.sessions.get(session_id) {
                        let still_active = session
                            .turn_started_at
                            .lock()
                            .map(|started| started.is_some())
                            .unwrap_or(false);
                        if still_active {
                            session
                                .steered_this_turn
                                .store(prior_steered, Ordering::SeqCst);
                        }
                    }
                }
                return Err(error);
            }
        };
        // Re-fetch the session (a retry may have swapped its thread/adapter) for
        // the post-submit mirror + sink-less drain.
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
        if submitted.disposition == TurnDisposition::Started {
            // Adapter truth wins over the pre-submit Gateway marker. A dead
            // mid-turn session can retain a stale Some until reactive resume;
            // once the retried adapter starts a NEW vendor turn, reset both its
            // elapsed clock and turn-scoped steer state unconditionally.
            if let Ok(mut started) = session.turn_started_at.lock() {
                *started = Some(Instant::now());
            }
            session.steered_this_turn.store(false, Ordering::SeqCst);
        } else if provisional_inject && submitted.disposition == TurnDisposition::Queued {
            let still_active = session
                .turn_started_at
                .lock()
                .map(|started| started.is_some())
                .unwrap_or(false);
            if still_active {
                session
                    .steered_this_turn
                    .store(prior_steered, Ordering::SeqCst);
            }
        }
        let turn_id = submitted.turn_id.clone();
        if let Ok(mut origins) = session.turn_origins.lock() {
            origins.insert(turn_id.0.clone(), origin);
        }
        self.mirror_user_turn(session, &user_text, &submitted.input_id);
        // Grok's native interjection path may hold its TurnCompleted boundary
        // until the accepted input is registered above. Release only after
        // origin and transcript bookkeeping are visible.
        submitted.release_completion();
        let drained = self
            .after_turn_submitted(
                session,
                start_visible_events,
                &turn_id.0,
                submitted.disposition,
            )
            .await?;
        Ok(SubmitResult::Turn {
            id: turn_id.0,
            drained,
        })
    }

    /// Interpret a session directive through the owning adapter, then render
    /// the [`DirectiveOutcome`] back into outbound replies (v0.8.5 D1).
    async fn dispatch_directive(
        &self,
        chat: &ChatKey,
        session_id: &str,
        directive: Directive,
        origin: TurnOrigin,
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
                if let Ok(mut origins) = session.turn_origins.lock() {
                    origins.insert(turn_id.0.clone(), origin);
                }
                // A directive that resolves to a turn was submitted by the
                // adapter itself (`/model` re-prompt, grok steer): it is
                // running, not queued.
                self.after_turn_submitted(
                    session,
                    start_visible_events,
                    &turn_id.0,
                    TurnDisposition::Started,
                )
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
        disposition: TurnDisposition,
    ) -> Result<Vec<String>> {
        if self.event_sink.is_some() {
            // v0.8.x (concurrency review §4.1 P2) — the turn-timeout watchdog
            // used to be a detached `tokio::spawn`ed task PER TURN
            // (`spawn_turn_timeout_watchdog`). It is now folded into the
            // session's own (already-running, one-per-session) event pump: arm
            // the pump's watchdog by recording this turn's id + the
            // `visible_events` snapshot at submission time, and the pump's own
            // `tokio::select!` loop does the idle-polling + one-shot warn
            // (`emit_turn_stall_warning`) that `spawn_turn_timeout_watchdog`
            // used to do standalone. Skip arming entirely when the window is
            // disabled (`0` = off) — matches the old early return.
            //
            // A QUEUED submission is NOT in flight: an adapter without a native
            // interjection channel (kimi / opencode ACP) parks it behind the
            // active vendor turn, and it may legitimately sit there for as long
            // as that turn runs. Arming here would blame the queued turn for
            // its predecessor's silence and tell the user a turn "went silent"
            // when it has not started — the same reason `turn_started_at` is
            // only stamped for `Started`. The pump arms it instead when its
            // canonical `TurnStarted` proves it really began.
            if !gateway_turn_timeout_duration().is_zero() && disposition != TurnDisposition::Queued
            {
                if let Ok(mut watch) = session.watched_turn.lock() {
                    *watch = Some((turn_id.to_string(), start_visible_events));
                }
            }
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
                slug: self.sessions.get(session_id).map(|s| s.project.clone()),
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
                self.dispatch_directive(chat, &session_id, directive, TurnOrigin::User)
                    .await
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

    /// The chat's working project — **always one this principal may see**, or
    /// `None` when it may see none.
    ///
    /// The fallback used to be the daemon's `default_project`, unconditionally.
    /// That was the widest cross-user hole in the IM path: a tenant's bot (or
    /// any unnamed chat) that had never `/cd`'d anywhere silently inherited the
    /// OWNER's default project on its very first message — and the implicit
    /// first-message spawn then started an agent, `--dangerously-skip-permissions`,
    /// inside the owner's working tree. `/cd` was gated all along; the implicit
    /// fallback was not, so the gate simply never ran for the common path.
    ///
    /// Resolution stays inside the principal's own view: the `/cd`-selected
    /// project when still visible, else the daemon default when visible (the
    /// single-user case, unchanged), else its first visible project.
    fn current_project_for(&self, chat: &ChatKey) -> Option<String> {
        let visible = self.visible_project_slugs(chat);
        let visible_has = |slug: &str| visible.iter().any(|v| v == slug);
        if let Some(cur) = self.current_project.get(chat) {
            if visible_has(cur) {
                return Some(cur.clone());
            }
        }
        if visible_has(&self.default_project) {
            return Some(self.default_project.clone());
        }
        let mut rest = visible;
        rest.sort();
        rest.into_iter().next()
    }

    /// Display form of [`Self::current_project_for`] — a principal with no
    /// visible project reads as `(无项目)` rather than borrowing someone else's
    /// slug for the banner.
    fn current_project_label(&self, chat: &ChatKey) -> String {
        self.current_project_for(chat)
            .unwrap_or_else(|| "(无项目)".to_string())
    }

    /// The project a spawn must land in, or a directed error. Guests and
    /// tenants with nothing of their own get an actionable refusal instead of
    /// the owner's default project.
    fn require_current_project(&self, chat: &ChatKey) -> Result<String> {
        self.current_project_for(chat).ok_or_else(|| {
            if matches!(self.principal(chat), Principal::Guest(_)) {
                anyhow!(
                    "这个 chat 还没有绑定到 ccteam —— 把它加入 bot 的允许列表(web 设置 → 接入 → 捕获 chat id),或让 owner 建一个用户给你"
                )
            } else {
                anyhow!("你还没有可用的项目 —— 用 /newproject <slug> <path> 建一个")
            }
        })
    }

    /// Session access scope — visibility (`/sessions`) AND addressing
    /// (`/use` / `/stop`).
    ///
    /// v0.8.18 柱2 (multi-user soft-partition 档0) — **OWN + one's OWN web
    /// console pool**: a chat reaches the sessions it OWNS, plus the sessions
    /// created by the web console OF THE SAME IDENTITY (the admin's
    /// `user:web-api`; a tenant's `user:<tenant>`) — the common single-user flow
    /// is "create it on web, drive it from your phone". IM-created sessions stay
    /// PRIVATE to their chat: two distinct telegram `chat_id`s never see each
    /// other's IM-created sessions.
    ///
    /// The pre-0.8.18 leaks are still gone: a `web` QUERIER never sees IM
    /// sessions (they are not in the `user:` pool), and the same-current-project
    /// leak (the v0.8.13 cross-frontend-by-project sharing) is removed. Reply
    /// routing is unaffected (per-turn submitter via `reply_to`). HONEST SCOPE:
    /// soft (UX) isolation under one OS uid, NOT a security boundary.
    ///
    /// v0.8.20 — a PER-TENANT IM bot (channel `"<platform>@<tenant>"`) and that
    /// tenant's web console are ONE identity (`user:<tenant>`, via
    /// `canonical_owner`): the bot sees + drives the tenant's WEB-created sessions
    /// AND its own, and the tenant's web sees the bot's — CONVERGED. It inherits
    /// no other pool (other tenants + the admin pool stay hidden).
    ///
    /// Cross-user fix (2026-07-28) — the pool leg is no longer a blanket "any `user:*` owner": with
    /// per-user web tokens that handed the admin's global IM bot every tenant's
    /// sessions (and every tenant's console every other tenant's). It now routes
    /// through [`ccteam_core::identity::can_see_session_owner`], the session twin
    /// of the project ACL — one policy, both frontends.
    ///
    /// Project inheritance (2026-07-30) — the LIVE gate reads the same rule as
    /// its stopped twin ([`Self::chat_can_access_sid`]) via
    /// [`Self::project_owner_visible_with`]: project 是归属单元, session 继承,
    /// so a tenant-owned project's principal decides regardless of the owner
    /// stamped on the session. It used to read `session.owner` raw, so a tenant
    /// whose children carried the caller's admin pool saw them when they were
    /// STOPPED and not when they were LIVE — while its web team page (gated at
    /// project level) showed them either way. Two frontends, two answers.
    fn chat_can_access(&self, chat: &ChatKey, session: &GatewaySession) -> bool {
        self.chat_can_access_with(chat, session, &mut ProjectPrincipalMemo::new())
    }

    /// [`Self::chat_can_access`] with a caller-supplied per-pass memo, for the
    /// filters that run it over EVERY live session (`/status`, `/sessions`, the
    /// switch pickers, the route scrub). Resolving the project principal costs
    /// a `state.json` read, so a 30-session fleet would otherwise pay 30
    /// identical reads per render; the memo makes it one per DISTINCT slug.
    fn chat_can_access_with(
        &self,
        chat: &ChatKey,
        session: &GatewaySession,
        memo: &mut ProjectPrincipalMemo,
    ) -> bool {
        self.project_owner_visible_with(chat, &session.project, &session.owner.identity(), memo)
    }

    /// [`Self::chat_can_access`] for a BARE sid, live or stopped: the live map
    /// first (O(1)), else the on-disk `meta.json`'s owner — the same two-rung
    /// ladder [`Self::project_slug_for_sid`] and the scheduled-entry gate walk.
    /// One gate for every sid-addressed command that must also reach history
    /// sessions (`/rename` today), so "stopped" never becomes an ACL hole:
    /// an unknown sid and a foreign one are indistinguishable (`false`).
    fn chat_can_access_sid(&self, chat: &ChatKey, sid: &str) -> bool {
        if let Some(session) = self.sessions.get(sid) {
            return self.chat_can_access(chat, session);
        }
        self.find_meta_for_sid(sid)
            .map(|(slug, _dir, meta)| self.project_session_owner_visible(chat, &slug, &meta.owner))
            .unwrap_or(false)
    }

    /// v0.8.20 F2 — the pure visibility rule, expressed on the canonical owner
    /// IDENTITY string so it is unit-testable without a full
    /// [`GatewaySession`], and identical for a live session and a persisted
    /// `meta.owner`. A chat sees a session owned by `owner_identity` iff (a) it
    /// owns it, or (b) the session sits in the WEB-CONSOLE pool of the SAME
    /// identity (the admin's `user:web-api`, or a tenant's `user:<tenant>`).
    /// Another identity's console — and any other IM chat — stays hidden.
    ///
    /// We MUST compare on the identity string, not by reconstructing a `ChatKey`
    /// via `from_identity` and using `==`: `ChatKey` equality includes `user_id`,
    /// but `identity()` drops it and `from_identity` forces `user_id = chat_id`,
    /// so a round-trip loses the real `user_id`. For a non-tenant IM bot (whose
    /// `canonical_owner` keeps the sender's `user_id`), that round-trip would
    /// wrongly deny the legitimate owner. Comparing the user_id-free identity
    /// strings sidesteps the lossy round-trip entirely — which is also why a
    /// session rebuilt from `meta.json` after a restart still reaches its owner.
    /// Ownership is chat-level by design ("owned by the CHAT, not a member"),
    /// and chat_id isolation is preserved (different chat_id ⇒ different
    /// identity ⇒ not visible).
    fn owner_identity_visible(&self, chat: &ChatKey, owner_identity: &str) -> bool {
        // ONE policy for both frontends: own ⊕ the web-console pool this
        // identity may see (`ccteam_core::identity::can_see_session_owner` —
        // the session twin of the project ACL's `can_see_owner`, keyed on the
        // same owner tags). The pool leg used to be a blanket "any `user:*`
        // owner", which predates per-user web tokens: it leaked EVERY tenant's
        // sessions into the admin's global IM bot (and into every other
        // tenant's console), so a `/sessions` list — or a `/use` on a listed
        // sid, which re-points `reply_to` — crossed users.
        let (user_id, is_admin) = self.project_acl_identity(chat);
        ccteam_core::identity::can_see_session_owner(
            &canonical_owner(chat).identity(),
            &user_id,
            is_admin,
            owner_identity,
        )
    }

    /// Session ownership inherits the project principal. The stored owner is
    /// only a fallback for unowned legacy projects; this keeps stopped-session
    /// ACL checks safe even when old metadata was persisted with the caller's
    /// admin identity instead of the tenant project's identity.
    fn project_session_owner_visible(
        &self,
        chat: &ChatKey,
        slug: &str,
        stored_owner: &str,
    ) -> bool {
        self.project_owner_visible_with(chat, slug, stored_owner, &mut ProjectPrincipalMemo::new())
    }

    /// The ONE session-ownership rule, shared by the live gate
    /// ([`Self::chat_can_access_with`]) and the stopped/meta gate
    /// ([`Self::project_session_owner_visible`]): a tenant-owned project's
    /// principal decides; operator-owned and unowned projects have none, so the
    /// owner stamped on the session decides instead (that fallback is what
    /// keeps two admin IM chats isolated from each other).
    fn project_owner_visible_with(
        &self,
        chat: &ChatKey,
        slug: &str,
        stored_owner: &str,
        memo: &mut ProjectPrincipalMemo,
    ) -> bool {
        match self.memoized_tenant_project_owner(memo, slug) {
            Some(project_owner) => self.owner_identity_visible(chat, project_owner),
            None => self.owner_identity_visible(chat, stored_owner),
        }
    }

    /// [`Self::tenant_project_owner`] behind a per-pass memo. Deliberately a
    /// LOCAL value threaded through one pass rather than a field on `Gateway`:
    /// an ACL cache that outlives the pass can serve stale visibility after an
    /// ownership change, and a stale ACL is worse than a `state.json` read.
    fn memoized_tenant_project_owner<'a>(
        &self,
        memo: &'a mut ProjectPrincipalMemo,
        slug: &str,
    ) -> Option<&'a str> {
        if !memo.contains_key(slug) {
            memo.insert(
                slug.to_string(),
                self.tenant_project_owner(slug).map(|o| o.identity()),
            );
        }
        memo.get(slug).and_then(Option::as_deref)
    }

    /// Resolve a frontend chat to the principal it speaks for — the ONE place
    /// an identity is decided, for both frontends.
    ///
    /// - **Operator** — the admin web console (holder of the web token) and the
    ///   chats explicitly NAMED in a global bot's credential allowlist.
    /// - **Tenant** — a per-tenant IM bot (`<platform>@<tenant>`) or a per-user
    ///   web token; both converge on `user:<tenant>`.
    /// - **Guest** — anyone else who got through a bot's transport allowlist.
    ///   Owns only what it creates; sees no project; cannot create one.
    ///
    /// The Guest arm is the fix for a fail-OPEN default: this used to return
    /// operator for every chat that was not a tenant bot, which made the
    /// transport allowlist double as the privilege grant. An OPEN bot (empty
    /// `allowed_chat_ids`, or Lark's `"*"` wildcard) therefore handed full
    /// operator rights — every project, every session, spawn anywhere — to
    /// whoever found it. Privilege is now a NAME, never a side effect of
    /// reaching the door.
    fn principal(&self, chat: &ChatKey) -> Principal {
        if let Some(tid) = crate::transport::tenant_of_bot_channel(&chat.channel) {
            return Principal::Tenant(tid.to_string());
        }
        if chat.channel == "web" {
            return if chat.chat_id == ccteam_core::identity::ADMIN_WEB_ID {
                Principal::Operator
            } else {
                Principal::Tenant(chat.chat_id.clone())
            };
        }
        if self.is_operator_chat(chat) {
            return Principal::Operator;
        }
        Principal::Guest(canonical_owner(chat).identity())
    }

    /// Whether this IM chat speaks for the box owner.
    ///
    /// - platform bound `Named` → only the listed chats. A platform lists
    ///   whichever key it authorizes by (Telegram a numeric chat id →
    ///   `ChatKey::chat_id`; Lark a sender `open_id` → `ChatKey::user_id`), so
    ///   either field may carry the name.
    /// - platform bound `Wildcard` → **nobody**. `"*"` is an explicit "I did not
    ///   name anyone", and an unnamed door must not grant the box.
    /// - platform not bound (no credentials, or the pre-configuration empty
    ///   allowlist, or an embedded/test gateway) → the legacy single-operator
    ///   assumption holds. Locking the half-configured owner out of their own
    ///   bot would be a worse failure than the exposure the daemon warns about.
    fn is_operator_chat(&self, chat: &ChatKey) -> bool {
        match self.operator_chats.get(&chat.channel) {
            Some(OperatorBinding::Named(named)) => {
                named.contains(&chat.chat_id) || named.contains(&chat.user_id)
            }
            Some(OperatorBinding::Wildcard) => false,
            None => true,
        }
    }

    /// Bind `platform`'s operator roster from that global bot's credential
    /// allowlist, and report what the binding means so the daemon can warn.
    pub fn bind_operator_allowlist<I: IntoIterator<Item = String>>(
        &mut self,
        platform: &str,
        allowlist: I,
    ) -> OperatorBindingKind {
        let entries: Vec<String> = allowlist
            .into_iter()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect();
        if entries.is_empty() {
            return OperatorBindingKind::Unconfigured;
        }
        if entries.iter().any(|id| id == "*") {
            self.operator_chats
                .insert(platform.to_string(), OperatorBinding::Wildcard);
            return OperatorBindingKind::Wildcard;
        }
        match self
            .operator_chats
            .entry(platform.to_string())
            .or_insert_with(|| OperatorBinding::Named(BTreeSet::new()))
        {
            OperatorBinding::Named(named) => named.extend(entries),
            slot @ OperatorBinding::Wildcard => {
                *slot = OperatorBinding::Named(entries.into_iter().collect())
            }
        }
        OperatorBindingKind::Named
    }

    /// Map a chat to the shared core ownership identity `(user_id, is_admin)`
    /// that `ccteam_core::identity::can_see_owner` expects — the IM-side twin of
    /// the web [`crate::auth::Identity`], so BOTH frontends resolve visibility
    /// through ONE policy (web/IM symmetry). HONEST SCOPE: like the session ACL
    /// this is soft (UX) isolation under one OS uid, not a security boundary.
    fn project_acl_identity(&self, chat: &ChatKey) -> (String, bool) {
        match self.principal(chat) {
            Principal::Operator => (ccteam_core::identity::ADMIN_WEB_ID.to_string(), true),
            Principal::Tenant(id) | Principal::Guest(id) => (id, false),
        }
    }

    /// Whether `chat` may see/address a project whose persisted
    /// `ProjectState.owner` is `owner`. Delegates to the SAME core policy the web
    /// REST project ACL uses (`build_projects` / `can_see_project` →
    /// `ccteam_core::identity::can_see_owner`), so a tenant's IM bot sees exactly
    /// the projects its web console does: it never sees the admin's projects, and
    /// the admin never peeks into a tenant's. Fixes the IM project-visibility
    /// leak where `/projects` / the `/cd` picker showed every owner's projects.
    fn chat_can_see_project_owner(&self, chat: &ChatKey, owner: Option<&str>) -> bool {
        let (user_id, is_admin) = self.project_acl_identity(chat);
        ccteam_core::identity::can_see_owner(&user_id, is_admin, owner)
    }

    /// Point the chat's active session at an existing session owned by this
    /// chat in `project` (deterministic: smallest session index), returning its
    /// id. When none exists, clear the active session so the next message spawns
    /// one on demand in `project`. Backs `/cd` so the project switch is real.
    fn adopt_session_in_project(&mut self, chat: &ChatKey, project: &str) -> Option<String> {
        // Own = owned by the chat's CANONICAL identity (`user:<id>` for web /
        // tenant bots, the chat itself for the admin/global IM bot). Owner is the
        // synthetic `user:` channel, never the querier's delivery channel, so we
        // must canonicalize before comparing — a plain `s.owner == *chat` would
        // miss a web chat's own (`user:`-owned) sessions. See `canonical_owner`.
        let canon = canonical_owner(chat);
        let adopted = self
            .sessions
            .values()
            .filter(|s| s.owner == canon && s.project == project)
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
        // Own-scoped @handle resolution: match the chat's CANONICAL identity
        // (`user:<id>` for web / tenant bots), not the raw querier chat — owner
        // lives in the synthetic `user:` channel. See `canonical_owner`.
        let canon = canonical_owner(chat);
        self.sessions
            .values()
            .find(|s| s.owner == canon && s.handle == handle)
            .map(|s| s.id.clone())
    }

    /// `/use @<role>` shorthand (v0.8.23 review §3.2-5, item 2b): resolve
    /// `role` to the sid of the chat-VISIBLE session (same rule as
    /// `/sessions`/`/status` — [`Self::chat_can_access`], i.e. own + the
    /// shared `user:` pool for the admin/web querier) with that role whose
    /// `last_active` is most recent. Ambiguity (two+ sessions share the role)
    /// is resolved SILENTLY by recency — documented in `/help`, not an error.
    /// An unmatched role returns a usage error listing the roles that ARE
    /// available to this chat, so the user can immediately retry.
    fn resolve_use_role_shorthand(&self, chat: &ChatKey, role: &str) -> Result<String> {
        let mut memo = ProjectPrincipalMemo::new();
        let mut candidates: Vec<&GatewaySession> = self
            .sessions
            .values()
            .filter(|s| self.chat_can_access_with(chat, s, &mut memo) && s.role == role)
            .collect();
        if candidates.is_empty() {
            let mut available: Vec<&str> = self
                .sessions
                .values()
                .filter(|s| self.chat_can_access_with(chat, s, &mut memo) && !s.role.is_empty())
                .map(|s| s.role.as_str())
                .collect();
            available.sort_unstable();
            available.dedup();
            return Err(if available.is_empty() {
                anyhow!("没有可按 role 匹配的会话 —— 用 /use <sid>,或先 /new 一个")
            } else {
                anyhow!(
                    "未找到 role `{role}` 的会话 —— 可用: {}",
                    available.join(", ")
                )
            });
        }
        candidates.sort_by(|a, b| {
            self.session_last_active(b)
                .cmp(&self.session_last_active(a))
                .then_with(|| session_index(&b.id).cmp(&session_index(&a.id)))
        });
        Ok(candidates[0].id.clone())
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

    async fn render_sessions(&self, chat: &ChatKey, all: bool) -> String {
        // v0.8.18 柱2 档0 — own-only: a chat lists only the sessions it owns
        // (the web-global + same-project leaks are gone). Soft per-chat isolation.
        let mut memo = ProjectPrincipalMemo::new();
        let accessible: Vec<&GatewaySession> = self
            .sessions
            .values()
            .filter(|s| self.chat_can_access_with(chat, s, &mut memo))
            .collect();
        // Web has its own GUI chrome (project picker, session list, Status page)
        // AND the chat bridge parses this reply into a structured frame, so the
        // web reply MUST stay the bare `id:project:vendor:role` rows (no banner /
        // scoping / footer — those would break `parse_sessions_reply`). The IM
        // text surface, which has no chrome, instead gets a current-project
        // banner + project scoping + an `/sessions all` footer, and compact
        // `sid vendor[.model][.effort][.window(pct%)]` rows (leading
        // `sid vendor` mirrors the switch buttons; no project slug per row —
        // the banner names it).
        let is_web = chat.channel == "web";
        // Default scope = the chat's CURRENT project; `all` (and web) lists the
        // full fleet. `elsewhere` counts accessible sessions in OTHER projects so
        // the IM footer can point at `/sessions all`.
        let cur = self.current_project_label(chat);
        let (mut visible, elsewhere): (Vec<&GatewaySession>, usize) = if all || is_web {
            (accessible, 0)
        } else {
            let elsewhere = accessible.iter().filter(|s| s.project != cur).count();
            let scoped = accessible
                .into_iter()
                .filter(|s| s.project == cur)
                .collect();
            (scoped, elsewhere)
        };
        // v0.8.22 P0-3 — order by recency (last_active desc), not the
        // `BTreeMap<sid, _>` iteration order (`s1,s10,s2…` string sort).
        // `last_active` is read best-effort from each session's `meta.json`;
        // a missing/unreadable meta sorts as "oldest" (empty string). Numeric
        // sid desc breaks ties (equal or both-missing last_active) so the
        // order stays fully deterministic.
        visible.sort_by(|a, b| {
            let la = self.session_last_active(a);
            let lb = self.session_last_active(b);
            lb.cmp(&la)
                .then_with(|| session_index(&b.id).cmp(&session_index(&a.id)))
        });
        // v0.8.23 review §1.3-D item 9 — the IM `/sessions` view (NOT the web
        // bare-row feed `parse_sessions_reply` depends on) pins a session
        // waiting on a HITL approval to the top: a stable sort on top of the
        // recency order above, so ties within each group (waiting / not)
        // keep their recency order. Cheap — reuses the existing pending
        // registry (already shared with the daemon's `permission/ask`
        // handler), no new progress.jsonl read.
        let waiting_sids: HashSet<String> = if is_web {
            HashSet::new()
        } else {
            let pend = self.pending.lock().await;
            visible
                .iter()
                .filter(|s| pend.pending_for_sid(&s.id).is_some())
                .map(|s| s.id.clone())
                .collect()
        };
        if !waiting_sids.is_empty() {
            visible.sort_by_key(|s| !waiting_sids.contains(&s.id));
        }
        if visible.is_empty() {
            if is_web {
                return "no sessions".to_string();
            }
            if elsewhere > 0 {
                return format!(
                    "📁 当前项目: {cur}\n本项目暂无会话 —— ↓ 其他项目还有 {elsewhere} 个 → /sessions all"
                );
            }
            return format!("📁 当前项目: {cur}\n暂无会话 —— /new 开一个");
        }
        // Render each visible session's row (async `thread_status`) once,
        // keyed by sid for the IM tree; web keeps the flat bare-row feed.
        let mut web_rows: Vec<String> = Vec::with_capacity(visible.len());
        let mut rendered: std::collections::HashMap<String, String> =
            std::collections::HashMap::with_capacity(visible.len());
        for s in &visible {
            // P3 — model + ctx from the owning adapter's `thread_status`.
            // Statusless adapters (bg / default) report `ThreadStatus::default()`
            // (no model / no context). Per-session failures degrade to the bare
            // row (never break the listing).
            let status = (s.adapter.thread_status(&s.thread).await).ok();
            // Web rows stay the machine-parsed `sid:project:vendor:role` feed +
            // the full, shared `status_suffix()` (matches Codex /status;
            // `parse_sessions_reply` splits on exactly 4 colon fields).
            if is_web {
                let base = format!("{}:{}:{}:{}", s.id, s.project, vendor_str(s.vendor), s.role);
                web_rows.push(match status.as_ref().and_then(|st| st.status_suffix()) {
                    Some(sfx) => format!("{base} — {sfx}"),
                    None => base,
                });
                continue;
            }
            // IM row: COMPACT, single-line, `.`-joined with no padding. Leads
            // with `sid vendor` (the SAME opening as the switch button —
            // `session_switch_options`: `sid vendor (title)`), then — when the
            // adapter reports them — `.model`, `.effort`, and `.window(pct%)`
            // context: the TOTAL window (absolute, via the same
            // `format_tokens` humanizer `ContextUsage::render` uses) + the
            // used percentage, but NOT the absolute used count. NO project
            // slug (the `📁 当前项目:` header already names it; `/sessions all`
            // spans projects without a per-row slug), NO role, NO `ctx` label
            // (all on /status); the title lives on the button; the vendor tag
            // + activity dot are gone.
            let mut row = format!("{} {}", s.id, vendor_str(s.vendor));
            if let Some(st) = &status {
                if let Some(m) = st.model.as_deref().filter(|m| !m.is_empty()) {
                    row.push_str(&format!(
                        ".{}",
                        strip_vendor_prefix(vendor_str(s.vendor), m)
                    ));
                }
                if let Some(e) = st.effort.as_deref().filter(|e| !e.is_empty()) {
                    row.push_str(&format!(".{e}"));
                }
                if let Some(ctx) = st.context.as_ref().filter(|c| c.window_tokens > 0) {
                    // Window known but occupancy not (a just-resumed ACP
                    // session, a vendor with no usage channel) renders `—`,
                    // never `0%` — the row must not claim an empty context.
                    let pct = match ctx.pct() {
                        Some(p) => format!("{p:.0}%"),
                        None => "—".to_string(),
                    };
                    row.push_str(&format!(".{}({pct})", format_tokens(ctx.window_tokens)));
                }
            }
            // v0.9.0 W2 (F2) — annotate the IM row with a non-local host.
            if !s.host.is_empty() && s.host != "local" {
                row.push_str(&format!(" @{}", s.host));
            }
            // v0.8.23 review item 9 — ⏳ marks a session pinned to the top for
            // an outstanding HITL approval. Prefixed last so it stays the
            // leftmost glance cue.
            if waiting_sids.contains(&s.id) {
                row = format!("⏳ {row}");
            }
            rendered.insert(s.id.clone(), row);
        }
        if is_web {
            return web_rows.join("\n");
        }
        // v0.9.0 W2 (F2) — IM tree: roots (a session with no VISIBLE parent —
        // a true root, or a parent in another project) sorted by sid, each
        // followed by its children indented `└─ ` (recursively). A parent-chain
        // cycle can never orphan a session: any unvisited row is appended flat.
        let visible_sids: std::collections::HashSet<&str> =
            visible.iter().map(|s| s.id.as_str()).collect();
        let mut children_of: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for s in &visible {
            if let Some(p) = s.parent_sid.as_deref() {
                if visible_sids.contains(p) {
                    children_of.entry(p).or_default().push(s.id.as_str());
                }
            }
        }
        // Roots + children keep the ALREADY-computed `visible` order (recency +
        // waiting-approval pin); the tree only adds indentation, never reorders
        // (so the ⏳ pin + recency sort above are preserved).
        let roots: Vec<&str> = visible
            .iter()
            .filter(|s| {
                s.parent_sid
                    .as_deref()
                    .map(|p| !visible_sids.contains(p))
                    .unwrap_or(true)
            })
            .map(|s| s.id.as_str())
            .collect();
        let mut tree_rows: Vec<String> = Vec::with_capacity(visible.len());
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut stack: Vec<(&str, usize)> = roots.iter().rev().map(|r| (*r, 0usize)).collect();
        while let Some((sid, depth)) = stack.pop() {
            if !visited.insert(sid) {
                continue;
            }
            if let Some(row) = rendered.get(sid) {
                let prefix = if depth == 0 {
                    String::new()
                } else {
                    format!("{}└─ ", "   ".repeat(depth - 1))
                };
                tree_rows.push(format!("{prefix}{row}"));
            }
            if let Some(kids) = children_of.get(sid) {
                for k in kids.iter().rev() {
                    stack.push((k, depth + 1));
                }
            }
        }
        // Cycle-orphaned leftovers → flat, sorted by sid (never drop a row).
        let mut leftovers: Vec<&str> = visible
            .iter()
            .map(|s| s.id.as_str())
            .filter(|sid| !visited.contains(sid))
            .collect();
        leftovers.sort_by_key(|sid| session_index(sid));
        for sid in leftovers {
            if let Some(row) = rendered.get(sid) {
                tree_rows.push(row.clone());
            }
        }
        let mut out = format!("📁 当前项目: {cur}\n{}", tree_rows.join("\n"));
        if elsewhere > 0 {
            out.push_str(&format!(
                "\n↓ 其他项目还有 {elsewhere} 个会话 → /sessions all"
            ));
        }
        out
    }

    /// Best-effort `last_active` (RFC3339) for a LIVE session, read from its
    /// `meta.json`. Empty when the project/meta can't be resolved — sorts as
    /// "oldest" in [`render_sessions`]'s ordering, never panics/blocks the list.
    fn session_last_active(&self, s: &GatewaySession) -> String {
        self.projects
            .get(&s.project)
            .and_then(|dir| read_session_meta(dir, &s.id).ok())
            .map(|m| m.last_active)
            .unwrap_or_default()
    }

    /// Best-effort user-facing title (v0.8.22 P1) for a LIVE session, read
    /// from its `meta.json`. `None` when untitled or the meta can't be
    /// resolved — [`render_sessions`] then falls back to the existing bare
    /// `id:project:vendor:role` row (no behavior change for an untitled
    /// session).
    fn session_title(&self, s: &GatewaySession) -> Option<String> {
        self.projects
            .get(&s.project)
            .and_then(|dir| read_session_meta(dir, &s.id).ok())
            .and_then(|m| m.title)
    }

    /// Classify live sessions from the same file-backed progress truth and
    /// with the same `working|idle|stale|stuck` semantics as MCP
    /// `session_list`. Reads each distinct project's progress stream once.
    fn session_activity_snapshot(
        &self,
        sessions: &[&GatewaySession],
    ) -> std::collections::HashMap<String, String> {
        let fallback_by_project: std::collections::HashMap<String, u64> = self
            .project_paths
            .as_ref()
            .and_then(|paths| ccteam_core::collect_projects(paths).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|project| (project.state.slug, project.stall_silent_seconds))
            .collect();
        let mut project_events: std::collections::HashMap<String, (Vec<serde_json::Value>, u64)> =
            std::collections::HashMap::new();
        for session in sessions {
            project_events
                .entry(session.project.clone())
                .or_insert_with(|| {
                    let events = self
                        .project_paths
                        .as_ref()
                        .map(|paths| {
                            ccteam_core::progress::read_all_events(
                                &paths.progress_jsonl(&session.project),
                            )
                            .unwrap_or_default()
                        })
                        .unwrap_or_default();
                    let silent = fallback_by_project
                        .get(&session.project)
                        .copied()
                        .unwrap_or(0);
                    (events, silent)
                });
        }
        let now = chrono::Utc::now();
        sessions
            .iter()
            .map(|session| {
                let (events, silent) = project_events
                    .get(&session.project)
                    .expect("every requested project was classified");
                let activity = ccteam_core::stall::classify_progress_activity_for_sid(
                    events,
                    &session.id,
                    *silent,
                    now,
                )
                .status
                .activity;
                (session.id.clone(), activity.to_string())
            })
            .collect()
    }

    /// v0.8.19 — PULL-only fleet-health view for `/status`. One line per
    /// accessible session: state (🟢 idle / 🔵 working / 🔴 stuck) · sid ·
    /// session-name (`ccteam-chat-<slug>-<sid>`) · the real vendor `--resume`
    /// id (`resume <uuid>`, or `resume —` when none) · project · role ·
    /// state-detail · model · effort · ctx, plus the live activity counts
    /// (`read×N·bash×M`) while working. Same ACL + iteration as
    /// [`render_sessions`]; pure rendering (no side effects, no push, no
    /// mutation) — it only renders when the user types `/status`.
    ///
    /// State derivation (mirrors the turn-timeout watchdog's own "silent for a
    /// full idle window = stalled" definition, so 🔴 here means exactly what the
    /// watchdog would flag):
    /// - `turn_started_at == None` ⇒ 🟢 **idle**.
    /// - in flight, last event recent (< idle window) ⇒ 🔵 **working** (show
    ///   `now - start`).
    /// - in flight, last event stale (≥ idle window) ⇒ 🔴 **STUCK** (show the
    ///   silent duration).
    ///
    /// When the watchdog window is disabled (`CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS
    /// == 0`), fall back to a fixed 300s stuck threshold so 🔴 still works.
    async fn render_status(&self, chat: &ChatKey) -> String {
        let mut memo = ProjectPrincipalMemo::new();
        let visible: Vec<&GatewaySession> = self
            .sessions
            .values()
            .filter(|s| self.chat_can_access_with(chat, s, &mut memo))
            .collect();
        if visible.is_empty() {
            return "no sessions — start one with /new".to_string();
        }
        // /status = the chat's CURRENT (focused) session in DEPTH; /sessions is
        // the full fleet list. Resolve the focused sid; if none is set (or it has
        // gone), point at /use rather than guess which session is "current".
        let cur_sid = self
            .current_session
            .read()
            .ok()
            .and_then(|m| m.get(chat).cloned());
        let Some(s) = cur_sid
            .as_ref()
            .and_then(|sid| visible.iter().copied().find(|s| &s.id == sid))
        else {
            // No focused session — keep the user oriented by leading with the
            // current project, then point at the right next step: a fresh project
            // wants a first message; a project with sessions wants `/use`.
            let cur = self.current_project_label(chat);
            let in_proj = visible.iter().filter(|s| s.project == cur).count();
            return if in_proj > 0 {
                format!(
                    "📁 当前项目: {cur}\n无当前会话 —— /use <id> 选一个驱动(本项目 {in_proj} 个;/sessions 看全部)"
                )
            } else {
                format!("📁 当前项目: {cur}\n本项目暂无会话 —— 发条消息开一个(或 /new)")
            };
        };

        // Pull live facts FROM the harness — never folded by ccteam: the
        // model/effort/ctx/goal status, the running subagent/workflow list
        // (claude's own `system:task_*` lifecycle), and account usage.
        let status = s.adapter.thread_status(&s.thread).await.ok();
        let running = s.adapter.running_tasks(&s.thread).await;
        // Account usage is account-scoped but PER VENDOR: a Codex session must
        // never display a Claude account's windows (and vice-versa). Prefer the
        // current session; else borrow from another visible session OF THE SAME
        // VENDOR whose adapter answers (so usage still shows when the current
        // session is idle/released). No same-vendor answer ⇒ omit the row.
        let mut account = s.adapter.account_usage(&s.thread).await;
        if account.is_none() {
            let vendor = s.adapter.vendor();
            for o in &visible {
                if o.adapter.vendor() != vendor {
                    continue;
                }
                if let Some(u) = o.adapter.account_usage(&o.thread).await {
                    account = Some(u);
                    break;
                }
            }
        }

        // State: a turn in flight ⇒ 🔵 working; silent past the idle window ⇒
        // 🔴 stuck — EXCEPT running subagents are an AUTHORITATIVE "still working"
        // signal (straight from claude) that overrides the silence heuristic, so
        // a main session quietly awaiting subagents never mis-reads idle/stuck.
        // Turn-OUTLIVING tasks (background workflows / shells / monitors) do NOT
        // override: they survive the spawning turn by design, so a leftover run
        // must not mask a genuinely stuck later turn. Same vocabulary as the
        // harness turn-end eviction (`RunningTask::outlives_turn`).
        let turn_scoped_running = running.iter().any(|t| !t.outlives_turn());
        let mut stuck_after = gateway_turn_timeout_duration();
        if stuck_after.is_zero() {
            stuck_after = std::time::Duration::from_secs(300);
        }
        let now = Instant::now();
        let started = s.turn_started_at.lock().ok().and_then(|g| *g);
        let last_event = s.last_event_at.lock().ok().and_then(|g| *g);
        let silent_for = started.map(|t| match last_event {
            Some(ev) => now.saturating_duration_since(ev),
            None => now.saturating_duration_since(t),
        });
        let (state, detail) = match (started, silent_for) {
            (None, _) => ("🟢", "idle".to_string()),
            // Running subagents ⇒ definitively working (overrides silence).
            (Some(t), _) if turn_scoped_running => (
                "🔵",
                format!("working {}", humanize_dur(now.saturating_duration_since(t))),
            ),
            (Some(_), Some(silent)) if silent >= stuck_after => {
                ("🔴", format!("STUCK {} silent", humanize_dur(silent)))
            }
            (Some(t), _) => (
                "🔵",
                format!("working {}", humanize_dur(now.saturating_duration_since(t))),
            ),
        };

        let role = if s.role.is_empty() { "—" } else { &s.role };
        // v0.8.23 review §3.2-5 (item 2c) — "你在哪": a standalone header line
        // giving the project slug + current session (sid/role/title) ahead of
        // the existing deep-view body, so the two-pointer (project × session)
        // mental model has one line that answers both at a glance. Same
        // format as the turn-answer context echo (`context_echo_line`), so
        // the two surfaces read identically.
        let title = self.session_title(s);
        let mut out = format!(
            "🧭 {}\n📍 当前会话 {} · {} · {} · {role} · {state} {detail}",
            context_echo_line(&s.project, &s.id, &s.role, title.as_deref()),
            s.id,
            s.project,
            vendor_str(s.vendor)
        );

        // Project working-tree PATH — disambiguates an auto-appended slug
        // (demo2 vs demo): the real dir is unambiguous. Resolved from the loaded
        // project map (slug → dir); omitted if the project isn't mapped.
        if let Some(dir) = self.projects.get(&s.project) {
            out.push_str(&format!("\n   📁 {}", dir.display()));
        }

        // Line 2: model · effort · ctx · resume (same fields /sessions shows, on
        // their own line for the deep view). Statusless/failed → `—` placeholder.
        let model = status
            .as_ref()
            .and_then(|st| st.model.as_deref())
            .filter(|m| !m.is_empty())
            .unwrap_or("—");
        let effort = status
            .as_ref()
            .and_then(|st| st.effort.as_deref())
            .filter(|e| !e.is_empty())
            .unwrap_or("—");
        let ctx = match status
            .as_ref()
            .and_then(|st| st.context.as_ref())
            .and_then(|c| c.pct())
        {
            Some(pct) => format!("ctx {pct:.0}%"),
            None => "ctx —".to_string(),
        };
        // The REAL `--resume` id (Anthropic session uuid), shown in full so it
        // can be matched against `tmux ls` / `claude --resume`; `—` for a
        // tmux/codex session that carries no stream-json uuid (never fabricated).
        let resume = thread_vendor_uuid(&s.thread)
            .map(|u| format!("resume {u}"))
            .unwrap_or_else(|| "resume —".to_string());
        out.push_str(&format!("\n   {model} · {effort} · {ctx} · {resume}"));

        // Running subagents / background workflows — straight from claude's task
        // lifecycle (NOT a fold). Subagents only exist while a turn is working;
        // background workflows outlive the turn, so an idle session can still
        // show its running workflows here.
        out.push_str(&format_running_tasks(&running));

        // Goal (🎯 open / ✅ met) — from the same thread_status the statusline uses.
        if let Some(g) = status.as_ref().and_then(|st| st.goal.as_ref()) {
            let cond = g.condition.trim();
            if !cond.is_empty() {
                let marker = if g.met { "✅" } else { "🎯" };
                let shown: String = if cond.chars().count() > 60 {
                    format!("{}…", cond.chars().take(59).collect::<String>())
                } else {
                    cond.to_string()
                };
                out.push_str(&format!("\n   {marker} {shown}"));
            }
        }

        // Account usage (5h / weekly / credits) — the vendor rate-limit windows.
        if let Some(u) = &account {
            let usage = format_account_usage(u);
            if !usage.is_empty() {
                out.push_str("\n   ");
                out.push_str(&usage);
            }
        }

        // Direct delegated children belong on the root's deep status card:
        // their work explains why an otherwise-idle parent is still waiting.
        // Only live, chat-visible children participate. Deeper descendants
        // are intentionally collapsed to a count to keep the phone card small.
        let mut direct_children: Vec<&GatewaySession> = visible
            .iter()
            .copied()
            .filter(|child| child.parent_sid.as_deref() == Some(s.id.as_str()))
            .collect();
        if !direct_children.is_empty() {
            direct_children.sort_by_key(|child| session_index(&child.id));
            let child_activity = self.session_activity_snapshot(&direct_children);
            out.push_str("\n   👥 直接子会话:");
            for child in &direct_children {
                let activity = child_activity
                    .get(&child.id)
                    .map(String::as_str)
                    .unwrap_or("idle");
                let title = self
                    .session_title(child)
                    .and_then(|title| truncate_title(&title))
                    .unwrap_or_else(|| "—".to_string());
                out.push_str(&format!(
                    "\n      · {} · {} · {} · {title}",
                    child.id,
                    vendor_str(child.vendor),
                    activity_marker(activity)
                ));
            }

            let mut descendants: HashSet<String> = direct_children
                .iter()
                .map(|child| child.id.clone())
                .collect();
            let mut frontier: Vec<String> = descendants.iter().cloned().collect();
            while let Some(parent) = frontier.pop() {
                for descendant in &visible {
                    if descendant.id != s.id
                        && descendant.parent_sid.as_deref() == Some(parent.as_str())
                        && descendants.insert(descendant.id.clone())
                    {
                        frontier.push(descendant.id.clone());
                    }
                }
            }
            let deeper = descendants.len().saturating_sub(direct_children.len());
            if deeper > 0 {
                out.push_str(&format!("\n      … 另有 {deeper} 个更深后代"));
            }
        }

        // Footer: the rest of the fleet lives in /sessions. Split by project so
        // the counts line up with the project-scoped `/sessions` (same project)
        // vs the full-fleet `/sessions all` (other projects).
        let same = visible
            .iter()
            .filter(|o| o.project == s.project && o.id != s.id)
            .count();
        if same > 0 {
            out.push_str(&format!("\n   ↓ 本项目其他 {same} 个会话 → /sessions"));
        }
        // Owner req — the last line points at the full project list (with a live
        // count), replacing the old cross-project `/sessions all` fleet pointer.
        // Count only the projects THIS chat may see (same ACL as `/projects`), so
        // the pointer never advertises another owner's projects.
        let nproj = self.visible_project_slugs(chat).len();
        out.push_str(&format!("\n   ↓ 所有 {nproj} 个项目 → /projects"));
        out
    }

    fn render_projects(&self, chat: &ChatKey) -> String {
        // `visible_project_slugs` reads the SAME source web / `ccteam status` use
        // — `collect_projects` (config.yaml filtered to projects that have an
        // on-disk `state.json`) — filtered by the SAME per-owner ACL web applies,
        // so IM `/projects` never diverges from the other surfaces: a
        // half-registered project (in config, no state.json) shows in NEITHER, a
        // removed project disappears from BOTH, and each owner sees only its own.
        self.visible_project_slugs(chat).join("\n")
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
            .map(|s| format!("{}:{}:{}:{}", s.id, s.project, vendor_str(s.vendor), s.role))
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
    /// gateway only long enough to clone scalar fields plus a best-effort
    /// `meta.json` read per session (sync fs, no `.await` runs under any
    /// lock) — so an SSE/list handler can call this cheaply. A session is
    /// `current` when it is the active session for at least one routed chat.
    /// v0.8.22 P0-3 — ordered by `last_active` desc (numeric sid desc
    /// tiebreak for equal/missing `last_active`), replacing the old
    /// creation-order (`session_index` ascending) sort so the REST session
    /// list reads recency-first like the IM `/sessions` view.
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
            .map(|s| {
                // Best-effort `meta.json` read for created_at/last_active — a
                // missing/unreadable meta degrades to empty strings (never
                // panics, never drops the row).
                let meta = self
                    .projects
                    .get(&s.project)
                    .and_then(|dir| read_session_meta(dir, &s.id).ok());
                let created_at = meta
                    .as_ref()
                    .map(|m| m.created_at.clone())
                    .unwrap_or_default();
                let last_active = meta
                    .as_ref()
                    .map(|m| m.last_active.clone())
                    .unwrap_or_default();
                let title = meta.as_ref().and_then(|m| m.title.clone());
                let turn_count = meta.as_ref().map(|m| m.turn_count).unwrap_or(0);
                let cost_usd = meta.as_ref().and_then(|m| m.cost_usd);
                let tokens_total = meta.as_ref().and_then(|m| m.tokens_total);
                let model = meta.as_ref().and_then(|m| m.model.clone());
                // v0.8.23 review item 9 — best-effort (never blocks): a
                // `try_lock` failure (rare, momentary registry contention)
                // just reports "not waiting" for this one snapshot rather
                // than making the whole sync `session_views()` async.
                let waiting_approval = self
                    .pending
                    .try_lock()
                    .map(|guard| guard.pending_for_sid(&s.id).is_some())
                    .unwrap_or(false);
                SessionView {
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
                    created_at,
                    last_active,
                    title,
                    turn_count,
                    cost_usd,
                    tokens_total,
                    model,
                    waiting_approval,
                    parent_sid: s.parent_sid.clone(),
                    delegation_depth: s.delegation_depth,
                }
            })
            .collect();
        views.sort_by(|a, b| {
            b.last_active
                .cmp(&a.last_active)
                .then_with(|| session_index(&b.sid).cmp(&session_index(&a.sid)))
        });
        views
    }

    // ── v0.8.21 history / resume / external-import ────────────────────────────

    /// List *stopped* ccteam sessions for a project — sessions with a
    /// `meta.json` on disk that are NOT currently in the live map.
    /// Returns them sorted by `last_active` descending (scan-on-demand,
    /// called lazily from the web UI "expand history" affordance).
    pub fn list_history_sessions(&self, slug: &str) -> Vec<SessionMeta> {
        let Some(cwd) = self.projects.get(slug) else {
            return vec![];
        };
        let live_sids: HashSet<&str> = self.sessions.keys().map(|s| s.as_str()).collect();
        list_session_metas(cwd)
            .into_iter()
            .filter(|m| !live_sids.contains(m.sid.as_str()))
            .collect()
    }

    /// Discover external Claude sessions under `~/.claude/projects/` whose
    /// recorded `cwd` matches this project, excluding any already adopted
    /// (i.e. uuid already tracked in a `meta.json` for this project).
    pub fn list_external_claude_sessions(&self, slug: &str) -> Vec<ExternalClaudeSession> {
        let Some(cwd) = self.projects.get(slug) else {
            return vec![];
        };
        let known_uuids: HashSet<String> = list_session_metas(cwd)
            .into_iter()
            .filter(|m| !m.vendor_uuid.is_empty())
            .map(|m| m.vendor_uuid)
            .collect();
        discover_external_claude_sessions(cwd, &known_uuids)
    }

    /// Resume a *stopped* ccteam session by its sid: read `meta.json`, re-insert
    /// into the live map, spawn the child via the fidelity ladder, persist state.
    /// The `caller` chat becomes the `reply_to` target for this session.
    /// Returns the sid on success so the caller can navigate to it.
    pub async fn resume_stopped_session(
        &mut self,
        sid: &str,
        caller_identity: &str,
        expected_slug: Option<&str>,
    ) -> Result<String> {
        let caller = ChatKey::from_identity(caller_identity)
            .unwrap_or_else(|| ChatKey::new("web", "web-api", "web-api"));
        // Guard: already live.
        if self.sessions.contains_key(sid) {
            return Ok(sid.to_string());
        }
        // Find which project this session belongs to.
        let (slug, cwd, meta) = self.find_meta_for_sid(sid)?;
        // ACL: the web caller is authorised for a SPECIFIC project (the URL
        // slug). `find_meta_for_sid` resolves the sid across ALL projects, so
        // without this guard a tenant authorised for project A could resume a
        // session belonging to project B by passing B's sid under A's slug.
        // Bind the resolved project to the caller's authorised slug. (The IM
        // path passes `None` — it owner-checks via `project_session_owner_visible`
        // before calling, which is the IM equivalent of this project gate.)
        if let Some(exp) = expected_slug {
            if exp != slug {
                anyhow::bail!("session {sid} does not belong to project {exp}");
            }
        }
        // Cold-start rebuild from meta.json — the SINGLE rebuild path (shared
        // with import + daemon-restart restore). Spawns via the resume ladder,
        // mints a fresh secret, inserts into the live map, starts the pump.
        self.rebuild_session_from_meta(&slug, cwd, &meta, caller.clone())
            .await?;
        self.current_session
            .write()
            .unwrap()
            .insert(caller, sid.to_string());
        self.persist_routing()?;
        Ok(sid.to_string())
    }

    /// Adopt an external Claude session: mint a new sid, write `meta.json`,
    /// insert into live map, and resume via fidelity ladder.
    /// Returns the new ccteam `sid`.
    pub async fn import_external_session(
        &mut self,
        slug: &str,
        vendor_uuid: &str,
        caller_identity: &str,
    ) -> Result<String> {
        let caller = ChatKey::from_identity(caller_identity)
            .unwrap_or_else(|| ChatKey::new("web", "web-api", "web-api"));
        self.ensure_project_loaded(slug);
        let cwd = self
            .projects
            .get(slug)
            .cloned()
            .ok_or_else(|| anyhow!("unknown project: {slug}"))?;
        // ACL: the caller-supplied `vendor_uuid` MUST be a session whose recorded
        // cwd matches THIS project — otherwise a tenant authorised for one
        // project could adopt (and read) any Claude transcript on the host by
        // uuid. Re-run the same cwd-filtered discovery the list endpoint uses and
        // require the uuid to appear in it (this also rejects already-adopted
        // uuids, which discovery excludes). Done BEFORE bumping `next_session` so
        // a rejected import does not burn an `s{n}`.
        let known_uuids: HashSet<String> = list_session_metas(&cwd)
            .into_iter()
            .filter(|m| !m.vendor_uuid.is_empty())
            .map(|m| m.vendor_uuid)
            .collect();
        // v0.8.22 P1 — keep the MATCHED row (not just a bool) so its
        // best-effort vendor title (extracted from the jsonl tail's
        // `ai-title`/`custom-title`) can seed `meta.title` below instead of
        // being discarded after the import dialog shows it once.
        let matched = discover_external_claude_sessions(&cwd, &known_uuids)
            .into_iter()
            .find(|s| s.vendor_uuid == vendor_uuid);
        let Some(matched) = matched else {
            anyhow::bail!(
                "vendor_uuid {vendor_uuid} is not an adoptable session for project {slug}"
            );
        };
        self.next_session += 1;
        // Make the counter durable BEFORE the sid is used (a later failure then
        // leaves a harmless gap, never a reused sid).
        self.persist_next_sid()?;
        let sid = format!("s{}", self.next_session);
        let now = chrono::Utc::now().to_rfc3339();
        let owner_tag = canonical_owner(&caller).identity();
        let mut meta = SessionMeta {
            sid: sid.clone(),
            slug: slug.to_string(),
            vendor: AgentVendor::Claude,
            protocol: SessionProtocol::StreamJson,
            role: String::new(),
            permission_mode: PermissionMode::Skip,
            owner: owner_tag,
            vendor_uuid: vendor_uuid.to_string(),
            model: None,
            effort: None,
            host: "local".to_string(),
            created_at: now.clone(),
            last_active: now,
            origin: SessionOrigin::Adopted,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            // Roleless adoption → no role file; still snapshot project skills.
            role_sha: None,
            skills_sha: ccteam_harness::execution::experience::skills_fingerprint(&cwd),
            trigger: None,
            // Adopting an external vendor session = a human/root session.
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        // v0.8.22 P1 — adopt the vendor's own title (if any) as the session's
        // starting title. `TitleSource::Vendor` still yields to a later
        // explicit `/rename` (precedence in `apply_title`), but wins over the
        // first-message auto-title (the session's first LIVE turn after
        // import is not really its "first message" — it already has history).
        if !matched.title.trim().is_empty() {
            apply_title(&mut meta, matched.title.clone(), TitleSource::Vendor);
        }
        // Cold-start rebuild from the freshly-built meta (shared path: spawn via
        // the resume ladder, insert, pump). Persist `meta.json` only AFTER a
        // successful spawn so a spawn failure leaves no orphan meta (a phantom
        // "stopped" session in history).
        self.rebuild_session_from_meta(slug, cwd.clone(), &meta, caller.clone())
            .await?;
        write_session_meta(&cwd, &meta)?;
        self.current_session
            .write()
            .unwrap()
            .insert(caller, sid.clone());
        self.persist_routing()?;
        Ok(sid)
    }

    /// Find a `meta.json` for `sid` by scanning all registered project dirs.
    fn find_meta_for_sid(&self, sid: &str) -> Result<(String, PathBuf, SessionMeta)> {
        for (slug, cwd) in &self.projects {
            if let Ok(meta) = read_session_meta(cwd, sid) {
                return Ok((slug.clone(), cwd.clone(), meta));
            }
        }
        anyhow::bail!("no meta.json found for session {sid}")
    }

    /// True when `sid` is currently in the live session map (spawned + tracked).
    /// A `false` result means the session was evicted for capacity, dropped by a
    /// daemon restart whose rebuild failed, or explicitly stopped — its
    /// `meta.json` may still exist on disk (see [`Self::project_slug_for_sid`]),
    /// making it resumable by sid.
    pub fn is_session_live(&self, sid: &str) -> bool {
        self.sessions.contains_key(sid)
    }

    /// Resolve the project slug that owns `sid`, whether it is currently live or
    /// a *stopped* session with an on-disk `meta.json`. Checks the live map
    /// first (O(1)); only a non-live sid pays the `meta.json` scan. Returns
    /// `None` only when the sid has neither a live session nor any `meta.json` —
    /// a genuinely unknown id. Unlike the capped history list the web rail
    /// loads, this always finds a stopped session's project.
    pub fn project_slug_for_sid(&self, sid: &str) -> Option<String> {
        if let Some(session) = self.sessions.get(sid) {
            return Some(session.project.clone());
        }
        self.find_meta_for_sid(sid).ok().map(|(slug, _, _)| slug)
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

    /// v0.9.0 W1 (F1) — authenticate a `session_*` caller by its `(sid, secret)`
    /// PRINCIPAL and return the resolved [`CallerCtx`] (sid + project slug +
    /// role). Generalizes the retired cto-only `(role, secret)` gate: a caller
    /// is authorized iff the live session named `sid` holds a secret equal
    /// (constant-time) to `presented_secret` — role plays no part in
    /// authorization (audit label only). The returned `slug` is the SERVER's
    /// view of the caller's project, so the gate overwrites any caller-supplied
    /// `_caller_slug` with it (a caller can only operate its OWN project). An
    /// empty secret always returns `None` (fail-closed): a pre-secret restored
    /// session or a forger with no secret can never authenticate. Read-only,
    /// holds no `.await`.
    ///
    /// HONEST SCOPE: this only RAISES THE BAR. Under the single-OS-uid
    /// full-trust model any agent can read another's `/proc/<pid>/environ`,
    /// files, or ptrace it and recover the secret, so this is best-effort
    /// defense-in-depth, NOT a hard boundary. Real isolation = per-agent OS
    /// user / sandbox (deferred). See `ccteam_core::session_secret`.
    pub fn verify_session_principal(&self, sid: &str, presented_secret: &str) -> Option<CallerCtx> {
        if presented_secret.is_empty() {
            return None;
        }
        let s = self.sessions.get(sid)?;
        if s.secret.is_empty() || !ccteam_core::session_secret::ct_eq(&s.secret, presented_secret) {
            return None;
        }
        Some(CallerCtx {
            sid: s.id.clone(),
            slug: s.project.clone(),
            role: s.role.clone(),
            depth: s.delegation_depth,
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

    /// v0.8.22 P0-2 — everything a HITL approval prompt needs for `sid`, in
    /// one lock acquisition: the live reply target ([`Self::reply_target_for`]),
    /// the role (for the "session sX (role) wants to run …" label), and the
    /// project's `progress.jsonl` path (best-effort operator visibility —
    /// `None` when `project_paths` was never wired, e.g. unit tests). `None`
    /// when `sid` is not tracked, so a firing resolver fails safe to deny
    /// (no chat to ask) instead of panicking. Read-only, holds no `.await`.
    pub fn hitl_prompt_context_for(&self, sid: &str) -> Option<HitlPromptContext> {
        let session = self.sessions.get(sid)?;
        let (channel, chat_id) = pump_target(session);
        let progress_path = self
            .project_paths
            .as_ref()
            .map(|paths| paths.progress_jsonl(&session.project));
        Some(HitlPromptContext {
            channel,
            chat_id,
            role: session.role.clone(),
            progress_path,
        })
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
            // The non-proto wrapper is the admin/owner pool (tests + legacy
            // callers); the REST path uses `_proto` with the caller's identity.
            "web-api".to_string(),
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
        owner_id: String,
    ) -> Result<CreateSessionOutcome> {
        self.create_session_api_tuned(
            project,
            role,
            vendor,
            permission_mode,
            protocol,
            owner_id,
            SpawnTuning::default(),
        )
        .await
    }

    /// Like [`Self::create_session_api_proto`] with explicit model/effort.
    /// The execution host is resolved exclusively from the project catalog.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_api_tuned(
        &mut self,
        project: String,
        role: String,
        vendor: AgentVendor,
        permission_mode: PermissionMode,
        protocol: SessionProtocol,
        owner_id: String,
        tuning: SpawnTuning,
    ) -> Result<CreateSessionOutcome> {
        // v0.8.20 web↔IM convergence — a web session is OWNED by the caller's
        // identity (`user:<tenant>`, or `user:web-api` for the admin/owner): we
        // pass the web frontend chat (channel "web") here as `reply_to`, and
        // `start_session` derives the owner via `canonical_owner` (→ `user:<id>`),
        // so the same tenant's own IM bot sees it too. Delivery stays a web SSE
        // subscriber (channel "web", filtered by `sid` — the chat_id is
        // irrelevant to web delivery), so the reply routing is unchanged.
        let owner = ChatKey::new("web", &owner_id, &owner_id);
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
            tuning,
        )
        .await
        .map(|o| CreateSessionOutcome { sid: o.id })
    }

    // ── live-session capacity (v0.9.2) ──────────────────────────────────────

    fn sessions_config(&self) -> ccteam_core::SessionsConfig {
        if let Some(cfg) = &self.sessions_config_override {
            return cfg.clone();
        }
        self.config
            .as_ref()
            .and_then(|c| c.get().ok())
            .map(|cfg| cfg.sessions.clone())
            .unwrap_or_default()
    }

    /// Override the daemon-wide live-session cap. Production reads the hot
    /// config; deterministic tests use this to make eviction cheap to exercise.
    pub fn set_sessions_config(&mut self, cfg: ccteam_core::SessionsConfig) {
        self.sessions_config_override = Some(cfg);
    }

    /// Build the exclusion set for a session admission: the admitted sid plus
    /// its live parent chain. The bounded/cycle-safe walk is defensive against
    /// corrupt in-memory lineage and deliberately independent of max_depth.
    fn live_capacity_exclusions(&self, sid: &str, parent_sid: Option<&str>) -> Vec<String> {
        let mut out = vec![sid.to_string()];
        let mut seen = HashSet::from([sid.to_string()]);
        let mut current = parent_sid.map(str::to_string);
        for _ in 0..64 {
            let Some(current_sid) = current else {
                break;
            };
            if !seen.insert(current_sid.clone()) {
                break;
            }
            out.push(current_sid.clone());
            current = self
                .sessions
                .get(&current_sid)
                .and_then(|session| session.parent_sid.clone());
        }
        out
    }

    /// Append the durable progress event and broadcast its live lifecycle twin
    /// from the same call site so state and web surfaces cannot drift.
    fn emit_session_evicted(&self, sid: &str, slug: &str) {
        if let Some(paths) = self.project_paths.as_ref() {
            let event = ccteam_harness::execution::progress_bridge::build_session_evicted_event(
                sid, "capacity",
            );
            let path = paths.progress_jsonl(slug);
            if let Err(err) = ccteam_core::progress::append_event(&path, &event) {
                tracing::warn!(%sid, %slug, error = %err,
                    "ccteam-im: failed to append session eviction progress event");
            }
        }
        self.emit_user_signal(GatewayEvent {
            id: format!("session-evicted-{sid}"),
            channel: String::new(),
            chat_id: String::new(),
            thread_ts: None,
            content: format!("session evicted: {sid}"),
            kind: GatewayEventKind::SessionLifecycle {
                state: "evicted".to_string(),
                reason: "capacity".to_string(),
            },
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(sid.to_string()),
            slug: Some(slug.to_string()),
        });
    }

    /// Ensure one more live session can be admitted. Capacity never rejects a
    /// creation/revival: eligible sessions are gracefully stopped LRU-first;
    /// if every live session is protected, admission proceeds over cap.
    async fn ensure_live_capacity(&mut self, exclude: &[String]) {
        let configured = self.sessions_config().max_live;
        let max_live = configured.max(1) as usize;
        if configured == 0 {
            tracing::warn!(
                "ccteam-im: sessions.max_live=0 cannot admit a session; treating it as 1"
            );
        }
        let excluded: HashSet<String> = exclude.iter().cloned().collect();

        while self.sessions.len() >= max_live {
            let candidates = {
                let pending = self.pending.lock().await;
                self.sessions
                    .values()
                    .map(|session| LiveCapacityCandidate {
                        sid: session.id.clone(),
                        idle: session
                            .turn_started_at
                            .lock()
                            .map(|started| started.is_none())
                            .unwrap_or(false),
                        last_active: self.session_last_active(session),
                        waiting_approval: pending.pending_for_sid(&session.id).is_some(),
                    })
                    .collect::<Vec<_>>()
            };
            let Some(candidate_sid) = select_live_capacity_eviction(&candidates, &excluded) else {
                tracing::warn!(
                    live = self.sessions.len(),
                    max_live,
                    excluded = ?excluded,
                    "ccteam-im: live-session capacity has no eligible eviction candidate; admitting over cap"
                );
                break;
            };
            let slug = self
                .sessions
                .get(&candidate_sid)
                .map(|session| session.project.clone())
                .unwrap_or_default();
            let stop_result = self.stop_session(&candidate_sid).await;
            if !self.sessions.contains_key(&candidate_sid) {
                self.emit_session_evicted(&candidate_sid, &slug);
                tracing::info!(
                    sid = %candidate_sid,
                    %slug,
                    reason = "capacity",
                    max_live,
                    "ccteam-im: gracefully evicted live session"
                );
            }
            if let Err(err) = stop_result {
                tracing::warn!(sid = %candidate_sid, error = %err,
                    "ccteam-im: capacity eviction stop reported an error");
            }
            if self.sessions.contains_key(&candidate_sid) {
                tracing::warn!(sid = %candidate_sid,
                    "ccteam-im: capacity eviction left candidate live; admitting over cap");
                break;
            }
        }
    }

    // ── delegation (v0.9.0 W2 — F2/F5/F7) ──────────────────────────────────

    /// The active delegation guardrail posture (hot-reloaded config, else the
    /// documented defaults). Zero-config runs safely.
    fn delegation_config(&self) -> ccteam_core::DelegationConfig {
        if let Some(cfg) = &self.delegation_config_override {
            return cfg.clone();
        }
        self.config
            .as_ref()
            .and_then(|c| c.get().ok())
            .map(|cfg| cfg.delegation.clone())
            .unwrap_or_default()
    }

    /// v0.9.0 W2 (F5) — set the delegation guardrail posture programmatically
    /// (overrides `config.yaml`). Prod leaves this unset; tests use it to
    /// exercise the guardrails with tiny limits.
    pub fn set_delegation_config(&mut self, cfg: ccteam_core::DelegationConfig) {
        self.delegation_config_override = Some(cfg);
    }

    /// v0.9.0 W2 (F5) — true when the vendor's trailing-24h project cost has
    /// reached its configured budget cap (the Ambient delegation budget gate,
    /// applied on both spawn + dispatch). No `project_paths` / no cap configured
    /// / a vendor with no price table (grok/opencode/kimi) → `false` (inert),
    /// so the count guardrails are those vendors' only ceiling.
    pub(crate) fn delegation_budget_exceeded(&self, slug: &str, vendor: AgentVendor) -> bool {
        self.project_paths
            .as_ref()
            .map(|p| crate::delegation::budget_exceeded(p, slug, vendor))
            .unwrap_or(false)
    }

    /// v0.9.0 W2 (F5) — count a parent's ACTIVE (live-map) direct children.
    /// Pure live-map scan (no meta read): a stopped child is already out of the
    /// map, so this is exactly the "active direct children" the `max_children`
    /// guardrail caps. Honest scope: an idle-released child is also out of the
    /// map, so this can under-count — an anti-runaway ceiling, not an exact
    /// census.
    fn count_active_children(&self, parent_sid: &str) -> u32 {
        self.sessions
            .values()
            .filter(|s| s.parent_sid.as_deref() == Some(parent_sid))
            .count() as u32
    }

    /// v0.9.0 W2 (F5) — count ALL active (live-map) delegated sessions in one
    /// project (any non-`None` `parent_sid`) — the `max_delegated` runaway
    /// ceiling. Same honest live-map scope as [`Self::count_active_children`].
    fn count_active_delegated(&self, project: &str) -> u32 {
        self.sessions
            .values()
            .filter(|s| s.project == project && s.parent_sid.is_some())
            .count() as u32
    }

    /// v0.9.0 W2 (F2) — walk `sid`'s ancestor chain (via the live map's
    /// `parent_sid`) up to `delegation.max_depth + 1` steps, returning the set
    /// of ancestor sids (INCLUDING `sid` itself). Used by the dispatch cycle
    /// guard (target ∈ ancestors → reject) and the stop-descendant check.
    pub(crate) fn ancestor_chain(&self, sid: &str) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let mut cur = Some(sid.to_string());
        // Bound the walk so a corrupt cycle in the map can never spin forever.
        let cap = (self.delegation_config().max_depth as usize)
            .saturating_add(2)
            .min(64);
        for _ in 0..=cap {
            let Some(c) = cur else { break };
            if !out.insert(c.clone()) {
                break;
            }
            cur = self.sessions.get(&c).and_then(|s| s.parent_sid.clone());
        }
        out
    }

    /// v0.9.0 W2 (F2) — append one `delegation_*` event to the project's
    /// `progress.jsonl` (the state SoT; schema owned by `progress_bridge`).
    /// Best-effort: a write failure only warns, never blocks the delegation.
    ///
    /// v0.9.0 W4 (F4) — ALSO broadcasts a [`GatewayEventKind::Delegation`]
    /// (the live-fan-out twin for the team view's global SSE,
    /// `GET /api/v1/agents/events`) at this SAME call site, so every
    /// `delegation_*` progress point and every `Delegation` broadcast stay in
    /// lockstep by construction (one helper, not one per call site). The
    /// broadcast fires independent of `self.project_paths` (unlike the
    /// progress-file write below) since it has no filesystem dependency —
    /// tests that never wire project paths still observe it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_delegation_progress(
        &self,
        slug: &str,
        event: &str,
        parent_sid: &str,
        child_sid: &str,
        vendor: AgentVendor,
        host: &str,
        turn: Option<&str>,
        title: Option<&str>,
        reason: Option<&str>,
    ) {
        let relation = event.strip_prefix("delegation_").unwrap_or(event);
        let title_owned = title.filter(|t| !t.is_empty()).map(str::to_string);
        let reason_owned = reason.filter(|r| !r.is_empty()).map(str::to_string);
        self.emit_user_signal(GatewayEvent {
            id: format!(
                "delegation-{relation}-{parent_sid}-{child_sid}-{}",
                turn.unwrap_or("")
            ),
            channel: String::new(),
            chat_id: String::new(),
            thread_ts: None,
            content: format!("delegation {relation}: {parent_sid} -> {child_sid}"),
            kind: GatewayEventKind::Delegation {
                relation: relation.to_string(),
                parent_sid: parent_sid.to_string(),
                child_sid: child_sid.to_string(),
                title: title_owned,
                reason: reason_owned,
            },
            attachments: Vec::new(),
            options: Vec::new(),
            // Not a per-session event (it names TWO sessions) — never routed
            // through the per-sid SSE ring/filter; the global agents SSE
            // subscribes the broadcast directly (see `crate::ring`).
            sid: None,
            slug: Some(slug.to_string()),
        });

        let Some(paths) = self.project_paths.as_ref() else {
            return;
        };
        let ev = ccteam_harness::execution::progress_bridge::build_delegation_event(
            event,
            parent_sid,
            child_sid,
            crate::delegation::vendor_key(vendor),
            host,
            turn,
            title,
            reason,
        );
        let path = paths.progress_jsonl(slug);
        if let Err(e) = ccteam_core::progress::append_event(&path, &ev) {
            tracing::warn!(slug = %slug, event = %event, err = %e,
                "ccteam-im: failed to append delegation progress event");
        }
    }

    /// v0.9.0 W2 (F2/F5) — the delegation-aware spawn `session_spawn` routes
    /// through. Mirrors [`Self::create_session_api_on_host`] but (a) links the
    /// child to its `parent` (parent_sid/depth/spawned_by_role/title +
    /// `trigger="session_spawn"`), (b) enforces the F5 guardrails on an Ambient
    /// (agent-initiated) spawn BEFORE any side effect — emitting
    /// `delegation_denied{reason}` + a readable error on rejection, and (c)
    /// emits `delegation_spawned` on success. `parent = None` (Admin/human) is
    /// unrestricted (still tagged `trigger="session_spawn"`, still a root:
    /// depth 0, no parent link). Called UNDER the gateway lock (like every
    /// `create_session_*`) so the guardrail counts + insert are consistent.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_delegated_session(
        &mut self,
        project: String,
        role: String,
        vendor: AgentVendor,
        permission_mode: PermissionMode,
        protocol: SessionProtocol,
        owner_id: String,
        tuning: SpawnTuning,
        parent: Option<DelegationParent>,
        title: Option<String>,
    ) -> Result<CreateSessionOutcome> {
        let (host, wire_slug) = self.project_host_binding(&project)?;
        // ---- F5 guardrails (Ambient spawn with a real parent only) ----
        let (parent_sid, spawned_by_role, child_depth) = if let Some(p) = &parent {
            let cfg = self.delegation_config();
            let child_depth = p.depth.saturating_add(1);
            let deny = |me: &Self, reason: crate::delegation::DenyReason, msg: String| {
                me.emit_delegation_progress(
                    &project,
                    ccteam_harness::execution::progress_bridge::DELEGATION_DENIED,
                    &p.sid,
                    "",
                    vendor,
                    &host,
                    None,
                    title.as_deref(),
                    Some(reason.tag()),
                );
                anyhow!("{msg}")
            };
            if child_depth > cfg.max_depth {
                return Err(deny(
                    self,
                    crate::delegation::DenyReason::Depth,
                    format!(
                        "delegation denied: depth limit reached (child would be depth {child_depth} > delegation.max_depth {})",
                        cfg.max_depth
                    ),
                ));
            }
            let children = self.count_active_children(&p.sid);
            if children >= cfg.max_children {
                return Err(deny(
                    self,
                    crate::delegation::DenyReason::Children,
                    format!(
                        "delegation denied: fan-out limit reached (parent {} already has {children} active children ≥ delegation.max_children {})",
                        p.sid, cfg.max_children
                    ),
                ));
            }
            let delegated = self.count_active_delegated(&project);
            if delegated >= cfg.max_delegated {
                return Err(deny(
                    self,
                    crate::delegation::DenyReason::Delegated,
                    format!(
                        "delegation denied: project delegation ceiling reached ({delegated} active delegated sessions ≥ delegation.max_delegated {})",
                        cfg.max_delegated
                    ),
                ));
            }
            {
                if self.delegation_budget_exceeded(&project, vendor) {
                    return Err(deny(
                        self,
                        crate::delegation::DenyReason::Budget,
                        format!(
                            "delegation denied: vendor `{}` has reached its 24h budget for project `{project}` (adjust budgets or wait for the window to slide / choose another vendor)",
                            crate::delegation::vendor_key(vendor)
                        ),
                    ));
                }
            }
            (Some(p.sid.clone()), Some(p.role.clone()), child_depth)
        } else {
            (None, None, 0)
        };

        // ---- spawn (mirrors start_session: gate host → plan → spawn → apply) ----
        let host_target = crate::remote_host::prepare_host_for_spawn(
            self.project_paths.as_ref().map(|p| p.root.as_path()),
            &host,
            &wire_slug,
            protocol,
            self.remote_host_proxy.as_ref(),
        )
        .await?;
        let host = host_target.host.clone();
        // DELIVERY only — ownership comes from `plan_new_session` (the project
        // principal). A delegated spawn has no frontend of its own (its caller
        // is an agent, not a human chat), so a tenant project's answers must
        // land in THAT tenant's web console rather than the caller's.
        let reply_to = self
            .tenant_project_owner_reply_target(&project)
            .unwrap_or_else(|| ChatKey::new("web", &owner_id, &owner_id));
        let handle = role.clone();
        let mut plan = self.plan_new_session(
            reply_to,
            project.clone(),
            vendor,
            role,
            handle,
            permission_mode,
            protocol,
            tuning,
        )?;
        plan.remote = host_target.remote;
        plan.parent_sid = parent_sid.clone();
        plan.spawned_by_role = spawned_by_role;
        plan.delegation_depth = child_depth;
        plan.title = title.clone();
        let child_sid = plan.id.clone();
        let thread = Self::spawn_for_new_session_plan(&plan).await?;
        let outcome = self
            .apply_new_session(plan, thread, Some("session_spawn"))
            .await?;
        self.drain_and_dispatch_pending_turns(&outcome.id).await;

        // ---- delegation_spawned (only when it IS a delegation) ----
        if let Some(psid) = &parent_sid {
            self.emit_delegation_progress(
                &project,
                ccteam_harness::execution::progress_bridge::DELEGATION_SPAWNED,
                psid,
                &child_sid,
                vendor,
                &host,
                None,
                title.as_deref(),
                None,
            );
        }
        Ok(CreateSessionOutcome { sid: outcome.id })
    }

    /// v0.9.0 W2 (F7) — idempotent-spawn replay: return the recorded response
    /// body for `(project, key)` if this key already spawned within the TTL
    /// (zero side effects), else `None`.
    pub fn spawn_idem_replay(&mut self, project: &str, key: &str) -> Option<String> {
        self.spawn_idem
            .get(&crate::delegation::IdemCache::scoped(project, key))
    }

    /// v0.9.0 W2 (F7) — record a successful spawn's response body under
    /// `(project, key)` so a client retry replays it instead of double-spawning.
    pub fn spawn_idem_record(&mut self, project: &str, key: &str, body: &str) {
        self.spawn_idem.put(
            crate::delegation::IdemCache::scoped(project, key),
            body.to_string(),
        );
    }

    /// v0.9.0 W2 (F7) — idempotent-dispatch replay for `(child_sid, key)`.
    pub fn dispatch_idem_replay(&mut self, child_sid: &str, key: &str) -> Option<String> {
        self.dispatch_idem
            .get(&crate::delegation::IdemCache::scoped(child_sid, key))
    }

    /// v0.9.0 W2 (F7) — record a successful dispatch's response body under
    /// `(child_sid, key)` so a client retry replays it instead of double-dispatching.
    pub fn dispatch_idem_record(&mut self, child_sid: &str, key: &str, body: &str) {
        self.dispatch_idem.put(
            crate::delegation::IdemCache::scoped(child_sid, key),
            body.to_string(),
        );
    }

    /// v0.9.0 W2 (F2) — `(vendor, host, slug)` for a live sid (for the
    /// `delegation_*` event fields). `None` when the sid isn't tracked.
    pub fn session_vendor_host_slug(&self, sid: &str) -> Option<(AgentVendor, String, String)> {
        self.sessions
            .get(sid)
            .map(|s| (s.vendor, s.host.clone(), s.project.clone()))
    }

    /// v0.9.5 feedback fix — is `sid`'s vendor turn still in flight? Reads the
    /// same `turn_started_at` cell the submit path sets and the pump clears on
    /// every terminal boundary (protocol-independent). `false` for an unknown sid. A
    /// `wait_seconds` dispatch uses this to keep waiting past interim assistant
    /// messages (codex narrates mid-turn) until the TASK actually finishes.
    pub fn session_turn_in_flight(&self, sid: &str) -> bool {
        self.sessions
            .get(sid)
            .map(|s| {
                s.turn_started_at
                    .lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// v0.9.0 W2 (F2/F7) — arm/refresh the durable completion watch for a child
    /// after a dispatch: writes `<project>/.ccteam/chat/<child>/delegation.json`
    /// (atomic-durable) + the in-memory mirror. A re-dispatch UPDATES
    /// parent/notify/title/dispatched_turn but PRESERVES `notified_turns` (never
    /// re-notifies an already-delivered turn). Returns false (no watch) when the
    /// child's project can't be resolved. `parent_sid` is the DISPATCHER's
    /// principal (usually the spawner, but not necessarily).
    pub fn arm_delegation_watch(
        &mut self,
        child_sid: &str,
        parent_sid: &str,
        notify: ccteam_harness::NotifyMode,
        title: Option<String>,
        dispatched_turn: Option<String>,
    ) -> bool {
        let Some(resolved) = self.session_resolve(child_sid) else {
            return false;
        };
        let project_dir = resolved.project_dir;
        let slug = resolved.project;
        let notified_turns = ccteam_harness::read_delegation_watch(&project_dir, child_sid)
            .map(|w| w.notified_turns)
            .unwrap_or_default();
        let mut watch = ccteam_harness::DelegationWatch::armed(
            parent_sid,
            notify,
            title.clone(),
            dispatched_turn,
        );
        watch.notified_turns = notified_turns.clone();
        if let Err(e) = ccteam_harness::write_delegation_watch(&project_dir, child_sid, &watch) {
            tracing::warn!(child = %child_sid, err = %e, "ccteam-im: failed to write delegation.json");
            return false;
        }
        self.delegations.insert(
            child_sid.to_string(),
            DelegationMirror {
                parent_sid: parent_sid.to_string(),
                notify,
                title,
                slug,
                project_dir,
                notified_turns,
            },
        );
        true
    }

    /// v0.9.0 W2 (F2) — drop a child's completion watch (mirror + durable
    /// `delegation.json`). Used on an inline `wait` completion (suppress the
    /// redundant notification) and by the reconcile when the parent is gone.
    pub fn disarm_delegation_watch(&mut self, child_sid: &str) {
        if let Some(m) = self.delegations.remove(child_sid) {
            ccteam_harness::execution::delegation::remove_delegation_watch(
                &m.project_dir,
                child_sid,
            );
        } else if let Some(resolved) = self.session_resolve(child_sid) {
            ccteam_harness::execution::delegation::remove_delegation_watch(
                &resolved.project_dir,
                child_sid,
            );
        }
    }

    /// v0.9.0 W4 (F4) — child sids with an ARMED completion watch, for the
    /// team view graph's best-effort `edges[].active` seed (a dispatch that
    /// hasn't yet been disarmed — see [`Self::disarm_delegation_watch`] for
    /// when that happens). Honest scope: a watch also stays armed briefly
    /// after a delivered (notified) completion until the next inline-wait
    /// disarm or a parent-gone reconcile, so this is a best-effort snapshot —
    /// the client corrects it live from `dispatched`/`completed` SSE frames.
    pub fn armed_delegation_watch_sids(&self) -> std::collections::HashSet<String> {
        self.delegations.keys().cloned().collect()
    }

    /// v0.9.0 W2 (F2/F7) — deliver one completed child turn to its watching
    /// parent. The in-memory `delegations` mirror is the HOT-PATH GATE: a turn
    /// from a session with no mirror entry (the vast majority) is a cheap
    /// no-op (no fs read). For a watched, not-yet-notified turn it (a) emits
    /// `delegation_completed`, (b) — when `notify` — builds the English
    /// notification and submits it to the parent via the ordinary submit path
    /// (live=steer / dead=pending-turns FIFO), emitting `delegation_notified`,
    /// and (c) records the turn in `notified_turns` (mirror + durable
    /// `delegation.json`) so it is delivered AT-MOST-once per turn. A parent
    /// that no longer exists drops the watch (+ a warn). Called under the
    /// gateway lock by the notifier task / reconcile (submit is a gateway
    /// method — the same lock scope every submit uses).
    pub(crate) async fn deliver_delegation_signal(
        &mut self,
        signal: crate::delegation::DelegationSignal,
    ) {
        use ccteam_harness::NotifyMode;
        let child = signal.child_sid.clone();
        // Hot-path gate: no mirror entry ⇒ not a watched child ⇒ nothing to do.
        let Some(mirror) = self.delegations.get(&child).cloned() else {
            return;
        };
        // v0.9.5 feedback fix — interim messages (a still-running vendor turn)
        // only concern an `all` watch; the default `final` mode drops them here
        // (they stay in the ledger) so a chatty child can NEVER flood its
        // parent's context or hold the notifier busy.
        if !signal.boundary && mirror.notify != NotifyMode::All {
            return;
        }
        // Dedup: this exact delivery was already handled (crash-safe
        // at-most-once). A boundary uses a distinct `{turn_id}#final` key so an
        // `all` watch still gets its idle wake-up after the same turn's interim
        // notification.
        let dedup_key = if signal.boundary {
            crate::delegation::final_dedup_key(&signal.turn_id)
        } else {
            signal.turn_id.clone()
        };
        if mirror.notified_turns.iter().any(|t| t == &dedup_key) {
            return;
        }
        // The watched TASK completed (turn boundary; regardless of notify).
        if signal.boundary {
            self.emit_delegation_progress(
                &mirror.slug,
                ccteam_harness::execution::progress_bridge::DELEGATION_COMPLETED,
                &mirror.parent_sid,
                &child,
                signal.vendor,
                &signal.host,
                Some(&signal.turn_id),
                mirror.title.as_deref(),
                None,
            );
        }
        let should_notify = match mirror.notify {
            NotifyMode::Final => signal.boundary,
            NotifyMode::All => true,
            NotifyMode::Off => false,
        };
        if should_notify {
            let text = if signal.boundary {
                crate::delegation::build_notification_text_with_outcome(
                    &child,
                    signal.vendor,
                    mirror.title.as_deref(),
                    &signal.turn_id,
                    &signal.tail,
                    signal.interim_notes,
                    signal.vendor_error,
                )
            } else {
                crate::delegation::build_interim_notification_text(
                    &child,
                    signal.vendor,
                    mirror.title.as_deref(),
                    &signal.turn_id,
                    &signal.tail,
                )
            };
            match self.submit_to_sid(&mirror.parent_sid, text).await {
                Ok(_) => {
                    self.emit_delegation_progress(
                        &mirror.slug,
                        ccteam_harness::execution::progress_bridge::DELEGATION_NOTIFIED,
                        &mirror.parent_sid,
                        &child,
                        signal.vendor,
                        &signal.host,
                        Some(&signal.turn_id),
                        mirror.title.as_deref(),
                        None,
                    );
                }
                Err(e) => {
                    tracing::warn!(parent = %mirror.parent_sid, child = %child, err = %e,
                        "ccteam-im: delegation notify failed (parent gone); dropping watch");
                    self.disarm_delegation_watch(&child);
                    return;
                }
            }
        }
        // Record the delivery as handled — mirror (hot path) + durable
        // delegation.json. A boundary batch-records every turn id it covered
        // (the folded interim messages) so a daemon-restart reconcile never
        // re-delivers them.
        let mut to_record: Vec<String> = signal.covered_turns.clone();
        to_record.push(dedup_key);
        if let Some(m) = self.delegations.get_mut(&child) {
            for id in &to_record {
                if !m.notified_turns.iter().any(|t| t == id) {
                    m.notified_turns.push(id.clone());
                }
            }
        }
        let mut watch = ccteam_harness::read_delegation_watch(&mirror.project_dir, &child)
            .unwrap_or_else(|| {
                ccteam_harness::DelegationWatch::armed(
                    &mirror.parent_sid,
                    mirror.notify,
                    mirror.title.clone(),
                    None,
                )
            });
        for id in &to_record {
            if !watch.notified_turns.iter().any(|t| t == id) {
                watch.notified_turns.push(id.clone());
            }
        }
        if let Err(e) = ccteam_harness::write_delegation_watch(&mirror.project_dir, &child, &watch)
        {
            tracing::warn!(child = %child, err = %e,
                "ccteam-im: failed to persist delegation.json notified_turns");
        }
    }

    /// v0.9.0 W2 (F2) — the delegation notifier task: run once on the passed
    /// gateway handle. Startup reconcile delivers notifications missed while the
    /// daemon was down, then it drains the pump signal channel for the daemon's
    /// lifetime, delivering each completed watched child turn off the pump.
    pub async fn run_delegation_notifier(
        gateway: Arc<tokio::sync::Mutex<Self>>,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<crate::delegation::DelegationSignal>,
    ) {
        Self::reconcile_delegations(Arc::clone(&gateway)).await;
        while let Some(signal) = rx.recv().await {
            let mut gw = gateway.lock().await;
            gw.deliver_delegation_signal(signal).await;
        }
    }

    /// v0.9.0 W2 (F7) — startup (or on-demand) reconcile of the durable
    /// delegation watches: deliver every completed child turn that was NOT yet
    /// notified while the daemon was down, exactly once (deduped by
    /// `notified_turns`). LOCK DISCIPLINE: snapshot the project set under the
    /// lock, do all fs IO (scan `delegation.json` + read `turns.jsonl`) OFF the
    /// lock, then seed the mirror + deliver UNDER the lock. A second reconcile
    /// over the same state delivers nothing (the turns are now recorded).
    pub async fn reconcile_delegations(gateway: Arc<tokio::sync::Mutex<Self>>) {
        // snapshot-under-lock: the project (slug → dir) set.
        let projects: Vec<(String, PathBuf)> = {
            let gw = gateway.lock().await;
            gw.projects
                .iter()
                .map(|(s, p)| (s.clone(), p.clone()))
                .collect()
        };
        // IO-off-lock: scan every project's watches + transcripts.
        let mut seeds: Vec<(String, DelegationMirror)> = Vec::new();
        let mut pending: Vec<crate::delegation::DelegationSignal> = Vec::new();
        for (slug, dir) in &projects {
            for (child_sid, watch) in ccteam_harness::scan_delegation_watches(dir) {
                let (vendor, host) =
                    ccteam_harness::execution::session_meta::read_session_meta(dir, &child_sid)
                        .map(|m| (m.vendor, m.host))
                        .unwrap_or((AgentVendor::Claude, "local".to_string()));
                seeds.push((
                    child_sid.clone(),
                    DelegationMirror {
                        parent_sid: watch.parent_sid.clone(),
                        notify: watch.notify,
                        title: watch.title.clone(),
                        slug: slug.clone(),
                        project_dir: dir.clone(),
                        notified_turns: watch.notified_turns.clone(),
                    },
                ));
                // v0.9.5 feedback fix — a restart reconcile folds ALL missed
                // turns into ONE boundary signal (latest text wins, earlier
                // ones counted as interim notes): after a daemon restart the
                // child is idle by construction (its process died with the
                // daemon), so the "task finished / child idle" shape is the
                // honest one — and a chatty child can't flood the parent with
                // a backlog replay. Covered ids batch-record so a second
                // reconcile delivers nothing.
                let missed: Vec<_> =
                    ccteam_harness::execution::turns_mirror::read_all_turns(dir, &child_sid)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|t| {
                            !t.assistant.is_empty()
                                && !watch.notified_turns.iter().any(|n| n == &t.turn_id)
                        })
                        .collect();
                if let Some(last) = missed.last() {
                    pending.push(crate::delegation::DelegationSignal {
                        child_sid: child_sid.clone(),
                        turn_id: last.turn_id.clone(),
                        tail: last.assistant.clone(),
                        vendor,
                        host: host.clone(),
                        boundary: true,
                        vendor_error: false,
                        interim_notes: missed.len().saturating_sub(1),
                        covered_turns: missed.iter().map(|t| t.turn_id.clone()).collect(),
                    });
                }
            }
        }
        // apply-under-lock: seed the mirror (never clobber a fresher live entry),
        // then deliver each missed turn (deliver re-checks notified_turns).
        {
            let mut gw = gateway.lock().await;
            for (child, mirror) in seeds {
                gw.delegations.entry(child).or_insert(mirror);
            }
        }
        for signal in pending {
            let mut gw = gateway.lock().await;
            gw.deliver_delegation_signal(signal).await;
        }
    }

    /// Submit a user-text turn to a session addressed by `sid` (W5b).
    /// Looks the session up by id (not by current-chat routing), resolves its
    /// project owner's concrete frontend as `reply_to`, then submits via the
    /// owning adapter. The lock is held only across the (fast) `submit_turn`
    /// send-keys / RPC; the long turn streams asynchronously through the event
    /// pump. Returns the submitted [`TurnId`]'s inner string.
    pub async fn submit_to_sid(&mut self, sid: &str, text: String) -> Result<String> {
        self.submit_to_sid_with_origin(sid, text, TurnOrigin::Internal)
            .await
    }

    async fn submit_to_sid_with_origin(
        &mut self,
        sid: &str,
        text: String,
        origin: TurnOrigin,
    ) -> Result<String> {
        // Same core as the IM `submit_to_current` path (parity by construction):
        // a single-line `/command` is a session directive, everything else a
        // turn. Project ownership selects the concrete reply frontend; an empty
        // `message_id` and a web target suppress the IM-only 👀 ack reaction.
        let reply_to = if let Some(session) = self.sessions.get(sid) {
            self.tenant_project_owner_reply_target(&session.project)
                .unwrap_or_else(|| reply_target_for_owner(&session.owner))
        } else if let Ok((slug, _, meta)) = self.find_meta_for_sid(sid) {
            self.tenant_project_owner_reply_target(&slug)
                .or_else(|| {
                    ChatKey::from_identity(&meta.owner).map(|owner| reply_target_for_owner(&owner))
                })
                .unwrap_or_else(web_api_chat)
        } else {
            web_api_chat()
        };
        match self
            .submit_resolved(&reply_to, sid, "", text, origin, false)
            .await?
        {
            // A turn's answer streams over the pump → SSE; hand back the turn id
            // so a `session_dispatch` caller can `session_collect{since: id}`.
            SubmitResult::Turn { id, .. } => Ok(id),
            // A directive's synchronous receipt (e.g. "已切换 model → opus") has
            // no turn id, and the POST already returned 202 — so deliver it over
            // the session's SSE stream as an Answer keyed on `sid`, the web peer
            // of the IM handler sending submit_to_current's returned Vec back.
            SubmitResult::Directive(replies) => {
                for (i, reply) in replies.into_iter().enumerate() {
                    self.emit_sid_answer(sid, i, reply);
                }
                Ok(format!("directive:{sid}"))
            }
        }
    }

    /// Web interactive submit for a per-`sid` chat. Mirrors the IM
    /// [`handle_message`](Self::handle_message) control-command face so the web
    /// input box behaves exactly like IM (three-surface parity): a **gateway**
    /// control command (`/status`, `/sessions`, `/help`, `/projects`, `/new`,
    /// `/role`, …) is handled here and its reply delivered over the session's
    /// SSE stream (like a directive receipt); a **vendor** directive (`/model`,
    /// `/compact`, `/clear`, …) and plain text fall through to
    /// [`submit_to_sid`](Self::submit_to_sid) unchanged.
    ///
    /// Regression fix: `/status` from the web console used to reach the vendor
    /// verbatim → "/status isn't available in this environment".
    ///
    /// `is_admin` gates the control face: the fleet renders (`/status`,
    /// `/sessions`) use the IM chat-level ACL (`chat_can_access` over the shared
    /// `user:` pool), which is coarser than the web's per-tenant REST ACL
    /// (`can_see_owner`) — so exposing it to a tenant could over-share another
    /// tenant's sessions. It is therefore admin-only, matching the web Status /
    /// 主机 / Settings nav (already `useMe().isAdmin`-gated). A non-admin caller
    /// falls straight through to a turn/vendor-directive (today's behaviour).
    /// A2A `session_dispatch` keeps calling `submit_to_sid` directly, so
    /// agent→agent routing is deliberately never given the human control face.
    pub async fn submit_web_sid(
        &mut self,
        sid: &str,
        text: String,
        is_admin: bool,
    ) -> Result<String> {
        if is_admin && text.trim_start().starts_with('/') {
            let chat = web_api_chat();
            // Web navigates by URL, so the POST's `sid` is the authoritative
            // focus: point the shared web chat at this session (+ its project)
            // so `/status`/`/sessions` render THIS session in depth, matching
            // the IM current-session semantics.
            if let Some(project) = self.sessions.get(sid).map(|s| s.project.clone()) {
                self.current_project.insert(chat.clone(), project);
                if let Ok(mut cur) = self.current_session.write() {
                    cur.insert(chat.clone(), sid.to_string());
                }
            }
            if let Some(reply) = self.handle_command(&chat, &text).await? {
                self.emit_sid_answer(sid, 0, reply);
                return Ok(format!("command:{sid}"));
            }
        }
        self.submit_to_sid_with_origin(sid, text, TurnOrigin::User)
            .await
    }

    /// Deliver one synchronous directive receipt to a web session's SSE stream
    /// as an `Answer` event keyed on `sid` (the per-`sid` SSE filter). The web
    /// peer of the IM handler sending a `submit_to_current` reply back over the
    /// channel; `nanos`+`i` keep the outbound id unique.
    fn emit_sid_answer(&self, sid: &str, i: usize, content: String) {
        let chat = web_api_chat();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        self.emit_user_signal(GatewayEvent {
            id: format!("gateway-directive-{sid}-{nanos}-{i}"),
            channel: chat.channel.clone(),
            chat_id: chat.chat_id.clone(),
            thread_ts: None,
            content,
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(sid.to_string()),
            slug: self.sessions.get(sid).map(|s| s.project.clone()),
        });
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
        self.persist_routing()?;
        Ok(())
    }

    /// Interrupt a session's CURRENTLY-RUNNING turn without destroying it (the
    /// non-destructive twin of [`Self::stop_session`]). Looks up the session,
    /// calls the owning adapter's `interrupt_turn` (stream-json `interrupt`
    /// control_request / TUI ESC / codex `turn/interrupt`) — an OUT-OF-BAND
    /// control that reaches the vendor mid-turn — and returns. The session is
    /// left fully live + idle: NO pump abort, NO `close_thread`, NO map
    /// removal, NO `current_session` change, NO persist (nothing changed in the
    /// registry), so a following `/model` etc. drives the SAME session on the
    /// SAME context.
    ///
    /// This is the ACL-less core shared by the IM `/interrupt` command (which
    /// applies `chat_can_access` first) and the web `POST
    /// /sessions/{sid}/interrupt` route (which applies the project ACL via
    /// `gate_sid` first). Unknown sid → `Err` so the web edge can 404.
    pub async fn interrupt_session(&mut self, sid: &str) -> Result<()> {
        let session = self
            .sessions
            .get(sid)
            .ok_or_else(|| anyhow!("unknown session: {sid}"))?;
        let thread = session.thread.clone();
        let adapter = Arc::clone(&session.adapter);
        adapter
            .interrupt_turn(&thread)
            .await
            .map_err(|e| anyhow!("interrupt failed for {sid}: {e}"))?;
        Ok(())
    }

    /// Rename a session's user-facing title — the ACL-less core every frontend
    /// shares (IM `/rename`, web `PATCH /api/v1/sessions/{sid}`), so the two
    /// can never diverge in what a rename means.
    ///
    /// Three things happen, in this order:
    ///
    /// 1. **ccteam SoT** — the cleaned title (the same rule-based
    ///    [`truncate_title`] the auto-title path uses; no LLM, ever) lands in
    ///    `meta.json` as `TitleSource::User`, which `apply_title` treats as
    ///    STICKY (never overwritten later by the first-message auto-title or a
    ///    vendor `ai-title`).
    /// 2. **vendor mirror** — the same title is pushed to the vendor's OWN
    ///    title surface via [`HarnessAdapter::set_session_title`], so the
    ///    session reads identically in `claude --resume`'s picker / codex's
    ///    thread list. Best-effort by design: the vendor answer is REPORTED
    ///    ([`TitleSync`]), never fatal — meta.json is the SoT and the user's
    ///    rename must not fail because a vendor has no title API.
    /// 3. **live surfaces** — a `SessionLifecycle{state:"renamed"}` broadcast,
    ///    the same frame capacity eviction uses, so every open web console
    ///    refreshes its rail the moment an IM `/rename` lands (and vice versa).
    ///
    /// Works on a LIVE **or a STOPPED** session: `meta.json` outlives the live
    /// map, so the sid is resolved from the live map first and from the
    /// on-disk metas otherwise (the same ladder [`Self::project_slug_for_sid`]
    /// walks). A stopped session simply has no live thread to hand the vendor
    /// push, which the adapter reports as [`TitleSync::Deferred`].
    pub async fn rename_session(&self, sid: &str, raw_title: &str) -> Result<SessionRename> {
        let cleaned =
            truncate_title(raw_title).ok_or_else(|| anyhow!("title must not be blank"))?;
        // Resolve (project dir, vendor, adapter, live thread) for a live OR a
        // stopped session. A stopped one has no adapter instance in the live
        // map, so mint one from the same factory every spawn path uses — it is
        // a plain value (no child process) until something spawns through it.
        let (dir, vendor, adapter, thread) = match self.sessions.get(sid) {
            Some(session) => {
                let dir = self
                    .projects
                    .get(&session.project)
                    .cloned()
                    .ok_or_else(|| anyhow!("unknown project: {}", session.project))?;
                (
                    dir,
                    session.vendor,
                    Arc::clone(&session.adapter),
                    Some(session.thread.clone()),
                )
            }
            None => {
                let (_slug, dir, meta) = self.find_meta_for_sid(sid)?;
                let adapter = (self.adapter_factory)(meta.vendor, meta.protocol);
                (dir, meta.vendor, adapter, None)
            }
        };

        let mut meta = read_session_meta(&dir, sid)
            .map_err(|e| anyhow!("meta.json missing for session {sid}: {e}"))?;
        let previous = meta.title.clone();
        apply_title(&mut meta, cleaned.clone(), TitleSource::User);
        write_session_meta(&dir, &meta)?;

        let target = SessionTitleTarget {
            sid: sid.to_string(),
            vendor_uuid: meta.vendor_uuid.clone(),
            project_dir: dir,
            thread,
        };
        let vendor_sync = match adapter.set_session_title(&target, &cleaned).await {
            Ok(sync) => sync,
            Err(err) => {
                // The ccteam-side rename already succeeded; say what the vendor
                // did rather than failing a rename the user can see took.
                tracing::warn!(%sid, %err, "ccteam-im: vendor title push failed");
                TitleSync::Deferred(format!("vendor push failed: {err}"))
            }
        };

        self.emit_session_renamed(sid, &meta.slug);
        Ok(SessionRename {
            sid: sid.to_string(),
            title: cleaned,
            previous,
            vendor: vendor_str(vendor).to_string(),
            vendor_sync,
        })
    }

    /// Broadcast-only twin of a rename for live web surfaces (the durable state
    /// twin is `meta.json` itself, already written by the caller). Same frame
    /// family as [`Self::emit_session_evicted`], so the SPA's existing
    /// `session_lifecycle` listener refreshes the rail with no client change.
    fn emit_session_renamed(&self, sid: &str, slug: &str) {
        self.emit_user_signal(GatewayEvent {
            id: format!("session-renamed-{sid}"),
            channel: String::new(),
            chat_id: String::new(),
            thread_ts: None,
            content: format!("session renamed: {sid}"),
            kind: GatewayEventKind::SessionLifecycle {
                state: "renamed".to_string(),
                reason: "user".to_string(),
            },
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(sid.to_string()),
            slug: Some(slug.to_string()),
        });
    }
}

/// Stringify a vendor for the [`SessionView`] wire shape. Kept local so
/// the web layer never depends on the harness enum's serde rename.
fn vendor_str(v: AgentVendor) -> &'static str {
    match v {
        AgentVendor::Claude => "claude",
        AgentVendor::Codex => "codex",
        AgentVendor::Grok => "grok",
        AgentVendor::Opencode => "opencode",
        AgentVendor::Kimi => "kimi",
    }
}

/// Whether a session gets the curated per-session `mcp.json` (HTTP + its own
/// `ccteam-sid:<sid>:<secret>` bearer) that both Claude spawn paths attach with
/// `--mcp-config`.
///
/// Claude only: codex and the ACP vendors pass their MCP server inline on every
/// `thread/start` / `session/new`, so they need no file. BOTH Claude protocols
/// qualify — the global `~/.claude.json` entry is HTTP with the *admin* bearer,
/// so without this file a managed session would authenticate as admin and lose
/// its own principal (no delegation parent edge). The single fresh-spawn and
/// single rebuild call site share this predicate so the two can't drift.
fn claude_session_mcp_config_applies(vendor: AgentVendor, protocol: SessionProtocol) -> bool {
    vendor == AgentVendor::Claude
        && matches!(
            protocol,
            SessionProtocol::StreamJson | SessionProtocol::Terminal
        )
}

/// Model ids commonly repeat the vendor name as their own prefix
/// (`claude-opus-4-8`, `grok-4.5`) — since the compact `/sessions` row
/// ALREADY leads with the vendor (`sid vendor.model…`), showing the model
/// verbatim reads redundant: `claude.claude-opus-4-8[1m]`. This strips a
/// leading `{vendor}` + separator (`-`/`_`/`.`/`/`) for DISPLAY ONLY —
/// `claude.opus-4-8[1m]` — never touching the real model id used for
/// `--model` respawns / cost accounting / persistence. A false-positive
/// partial-word match (no separator boundary) or a match that would leave
/// nothing after stripping returns `model` unchanged.
fn strip_vendor_prefix<'a>(vendor: &str, model: &'a str) -> &'a str {
    let lower = model.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix(vendor) else {
        return model;
    };
    let sep_len = match rest.as_bytes().first() {
        None => 0,
        Some(b'-' | b'_' | b'.' | b'/') => 1,
        Some(_) => return model,
    };
    let stripped = &model[vendor.len() + sep_len..];
    if stripped.is_empty() {
        model
    } else {
        stripped
    }
}

fn activity_marker(activity: &str) -> &'static str {
    match activity {
        "working" => "🟡 working",
        "idle" => "🟢 idle",
        "stale" => "🟠 stale",
        "stuck" => "🔴 stuck",
        _ => "⚪ unknown",
    }
}

/// Split a `/rename` argument into an explicit `[<sid>] <title>` pair. A
/// leading `s<N>` token counts as a TARGET only when a title follows it, so
/// `/rename s3` still titles the current session `s3` rather than trying to
/// rename `s3` to nothing. Pure — unit-tested directly.
fn split_leading_sid(rest: &str) -> Option<(&str, &str)> {
    let (head, tail) = rest.split_once(char::is_whitespace)?;
    let looks_like_sid = head
        .strip_prefix('s')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    let title = tail.trim();
    (looks_like_sid && !title.is_empty()).then_some((head, title))
}

/// The `/rename` receipt: which session changed, from what to what, and how
/// far the new title travelled. The vendor clause is NEVER omitted — a title
/// that only exists ccteam-side must not read as though the vendor adopted it.
fn render_rename_receipt(r: &SessionRename) -> String {
    let mut out = match r.previous.as_deref().filter(|p| !p.is_empty()) {
        Some(prev) => format!("已重命名 {} 「{prev}」→「{}」", r.sid, r.title),
        None => format!("已重命名 {} →「{}」", r.sid, r.title),
    };
    out.push('\n');
    out.push_str(&match &r.vendor_sync {
        TitleSync::Pushed => format!("· 已同步到 {} 自己的会话标题", r.vendor),
        TitleSync::Deferred(reason) => format!("· 仅 ccteam 侧({reason})"),
        TitleSync::Unsupported => format!("· 仅 ccteam 侧({} 无会话标题接口)", r.vendor),
    });
    out
}

/// v0.8.23 review §3.2-5 (item 2a) — the compact "which session just spoke"
/// context line (`→ <slug>/<sid> (<role>)`), shared by the turn-answer echo
/// (the detached event pump, IM only) and the `/status` "you are here"
/// header, so the two surfaces read identically. `role` is omitted for a
/// roleless session; `title` (when cheaply available from `meta.json`) is
/// appended as a trailing `「tag」`.
fn context_echo_line(slug: &str, sid: &str, role: &str, title: Option<&str>) -> String {
    let mut line = format!("→ {slug}/{sid}");
    if !role.is_empty() {
        line.push_str(&format!(" ({role})"));
    }
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        line.push_str(&format!(" 「{t}」"));
    }
    line
}

/// v0.8.22 P1 — one read-modify-write refreshing meta.json's activity trio
/// when paneless work starts or an assistant/error row lands: `last_active`
/// (as `touch_last_active` always did), `turn_count` (the turns.jsonl line
/// count), and `cost_usd` (this sid's priced `chat_turn_completed` events in
/// progress.jsonl — the same
/// deterministic per-turn accounting `GET /api/v1/status`'s
/// `build_session_cost_rows` uses, scoped to one sid). Best-effort, like the
/// `touch_last_active` it replaces: a missing/unreadable meta is silently
/// skipped, never blocking the reply. Called from the detached event pump
/// (free function, not a `&self` method — the pump has already moved its
/// captures out of the gateway by the time this runs).
fn refresh_session_activity_meta(
    project_dir: &Path,
    sid: &str,
    vendor: AgentVendor,
    progress_path: Option<&Path>,
) {
    let Ok(mut meta) = read_session_meta(project_dir, sid) else {
        return;
    };
    meta.last_active = chrono::Utc::now().to_rfc3339();
    meta.turn_count = ccteam_harness::execution::turns_mirror::read_all_turns(project_dir, sid)
        .map(|turns| turns.len() as u64)
        .unwrap_or(meta.turn_count);
    if let Some(path) = progress_path {
        let (cost_usd, tokens_total) = session_cost_and_tokens(path, sid, vendor);
        meta.cost_usd = cost_usd;
        meta.tokens_total = tokens_total;
    }
    let _ = write_session_meta(project_dir, &meta);
}

/// Sum the deterministic per-turn cost AND raw token count of every
/// `chat_turn_completed` event in `progress_path` tagged with `sid`, pricing
/// each turn's `usage` against its own canonical `model` via
/// [`ccteam_cost::estimate_cost`] — mirrors `ccteam-web`'s
/// `status::build_session_cost_rows`, scoped to one sid so it can run from the
/// harness-side pump (which has no access to that web-layer helper). `None`
/// when nothing priced/counted yet (never a faked `0.0`); a turn whose model
/// is absent/not in the pricing table is silently skipped from the COST sum
/// (no fallback to a wrong rate — same honesty contract as the status route)
/// but still counted into `tokens_total` — the v0.9.5 feedback fix: a vendor
/// with no price table (codex/grok/opencode/kimi) at least accrues an honest
/// token ledger instead of a permanently-null row.
fn session_cost_and_tokens(
    progress_path: &Path,
    sid: &str,
    vendor: AgentVendor,
) -> (Option<f64>, Option<u64>) {
    let Ok(events) = ccteam_core::progress::read_all_events(progress_path) else {
        return (None, None);
    };
    let cost_vendor = vendor.cost_vendor();
    let mut total = 0.0_f64;
    let mut priced = 0usize;
    let mut tokens = 0u64;
    let mut counted = 0usize;
    for ev in &events {
        if ev.get("event").and_then(|v| v.as_str())
            != Some(ccteam_core::progress::CHAT_TURN_COMPLETED)
        {
            continue;
        }
        if ev.get("sid").and_then(|v| v.as_str()) != Some(sid) {
            continue;
        }
        let Some(usage) = ev
            .get("usage")
            .and_then(|u| serde_json::from_value::<ccteam_cost::UnifiedTokenUsage>(u.clone()).ok())
        else {
            continue;
        };
        if usage.total() > 0 {
            tokens = tokens.saturating_add(usage.total());
            counted += 1;
        }
        let model = ev.get("model").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(cost) = ccteam_cost::estimate_cost(&usage, cost_vendor, model) {
            total += cost;
            priced += 1;
        }
    }
    (
        (priced > 0).then_some(total),
        (counted > 0).then_some(tokens),
    )
}

/// Admin-web fallback for an unowned legacy resource or an unknown sid.
fn web_api_chat() -> ChatKey {
    ChatKey::new("web", "web-api", "web-api")
}

/// v0.8.20 — the canonical OWNER identity of a frontend chat (web↔IM
/// convergence). A per-tenant IM bot (`"<platform>@<tenant>"`) and that tenant's
/// web console are ONE identity (`user:<tenant>`), so both frontends OWN + SEE
/// the same sessions. Everything else (the admin/global bot) owns by the chat
/// itself. The `user:` namespace is a SYNTHETIC identity channel — it is NEVER a
/// delivery channel; reply routing uses the per-turn `reply_to` (the actual
/// frontend chat, e.g. the web console's channel `"web"`), NOT the owner (see
/// `pump_target`). So the owner tag stays clear (`user:<id>`) while web SSE / IM
/// delivery is untouched.
fn canonical_owner(chat: &ChatKey) -> ChatKey {
    if let Some(tid) = crate::transport::tenant_of_bot_channel(&chat.channel) {
        // A per-tenant IM bot → its tenant identity.
        ChatKey::new("user", tid, tid)
    } else if chat.channel == "web" {
        // The web console (admin `web-api` or a tenant) → the user identity. The
        // frontend chat itself (channel "web") remains the delivery `reply_to`.
        ChatKey::new("user", &chat.chat_id, &chat.chat_id)
    } else {
        // The admin/global IM bot, etc. — owns by the chat itself.
        chat.clone()
    }
}

/// Map a canonical resource owner back to a concrete delivery frontend.
/// Synthetic `user:<id>` identities use that user's web channel; real IM
/// owners already name their delivery channel directly.
fn reply_target_for_owner(owner: &ChatKey) -> ChatKey {
    if owner.channel == "user" {
        ChatKey::new("web", &owner.chat_id, &owner.chat_id)
    } else {
        owner.clone()
    }
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

/// v0.8.21 Wave-2 — the session `meta.json`s that were LIVE at last persist,
/// resolved from `routing.json.live_sids` (the live-set) ⋈ each session's
/// `meta.json` (the SoT). The daemon-independent source the `ccteam session ls`
/// / `status` views reconcile against — replaces parsing the retired
/// `gateway-state.json` `sessions` vec. STOPPED sessions (whose meta.json
/// lingers as history) are excluded because they are not in `live_sids`.
///
/// Returns empty when routing.json is absent / unreadable / lists no live sids,
/// or when config.yaml can't be loaded — every case means "nothing to reconcile
/// against", never an error (these are glance views, not liveness gates).
fn live_session_metas(ccteam_root: &Path) -> Vec<SessionMeta> {
    let routing_path = crate::routing_state_path_in(ccteam_root);
    let Ok(raw) = std::fs::read_to_string(&routing_path) else {
        return vec![];
    };
    let Ok(routing) = serde_json::from_str::<RoutingState>(&raw) else {
        return vec![];
    };
    let live: HashSet<String> = routing.live_sids.into_iter().collect();
    if live.is_empty() {
        return vec![];
    }
    let Ok(cfg) = ccteam_core::config::load(ccteam_root) else {
        return vec![];
    };
    let mut out = vec![];
    for project in cfg.projects {
        for meta in list_session_metas(&project.path) {
            if live.contains(&meta.sid) {
                out.push(meta);
            }
        }
    }
    out
}

/// Load the set of canonical chat-session names (`ccteam-chat-<slug>-<sid>`)
/// the gateway has live, from `routing.json` + the live sessions' `meta.json`
/// (v0.8.21 Wave-2; see [`live_session_metas`]).
///
/// Returns an empty set when nothing is persisted yet — so every live OS pane
/// is by definition an orphan. The daemon-independent registry source the
/// `ccteam sessions` CLI view reconciles against; strictly read-only.
pub fn tracked_chat_session_names(
    ccteam_root: &Path,
) -> Result<std::collections::BTreeSet<String>> {
    Ok(live_session_metas(ccteam_root)
        .into_iter()
        // v0.8.8 F1 — canonical name keys on the sid (`s<N>`), matching the
        // pane name the adapter spawns; computing from role here would make
        // every live pane reconcile as an orphan.
        .map(|m| chat_session_name(&m.slug, &m.sid))
        .collect())
}

/// v0.8.8 B4/F3 — one tracked gateway session, flattened for out-of-process
/// readers (the `ccteam session ls` / `ccteam status` CLI). The gateway's
/// in-memory [`SessionView`] lives inside the daemon process; the CLI is a
/// separate process and reads the persisted `routing.json` + `meta.json` (the
/// v0.8.21 Wave-2 SoT). This projection exposes exactly the columns those views
/// render (sid · project · role · vendor · permission_mode).
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
    /// RFC3339 `last_active` from `meta.json` (v0.8.22 P0-3), used to sort
    /// `ccteam session ls` by recency instead of sid string order.
    pub last_active: String,
    /// v0.8.22 P1 — user-facing session title from `meta.json`. `None` when
    /// not yet titled — `ccteam session ls` falls back to role/sid display.
    pub title: Option<String>,
}

/// Load the gateway's live sessions as flat [`TrackedSessionRow`]s from
/// `routing.json` + each live session's `meta.json` (v0.8.21 Wave-2; see
/// [`live_session_metas`]).
///
/// Shares the exact read path of [`tracked_chat_session_names`] so the two
/// daemon-independent CLI views (`session ls` reconcile + `status` nesting)
/// never drift. **Nothing persisted ⇒ empty `Vec`**, never an error. The
/// sub-second drift between the in-memory gateway map and this on-disk snapshot
/// is accepted for the status / ls views (a glance, not a liveness gate).
pub fn tracked_chat_sessions(ccteam_root: &Path) -> Result<Vec<TrackedSessionRow>> {
    Ok(live_session_metas(ccteam_root)
        .into_iter()
        .map(|m| TrackedSessionRow {
            sid: m.sid,
            project: m.slug,
            role: m.role,
            vendor: vendor_str(m.vendor).to_string(),
            permission_mode: m.permission_mode.as_str().to_string(),
            last_active: m.last_active,
            title: m.title,
        })
        .collect())
}

impl Gateway {
    /// Abort every per-session event pump.
    ///
    /// [`Drop`] calls this too, but relying on Drop alone is fragile: the
    /// gateway lives behind an `Arc<Mutex<Gateway>>` whose clones (the restore
    /// / notifier tasks, the web `AppState`, the MCP socket server) can outlive
    /// the daemon future, so the Drop may never run at shutdown. The pumps are
    /// then merely *detached* — a dropped `JoinHandle` does NOT cancel its task
    /// — and keep polling their adapters forever. Harmless when the process
    /// exits, but a real leak in-process: a second daemon started in the same
    /// process races the stale pumps for the same session's events, and
    /// whichever pops first wins. So the daemon shutdown path calls this
    /// explicitly, alongside the listener/consumer aborts.
    pub fn abort_event_pumps(&mut self) {
        for (_, handle) in std::mem::take(&mut self.event_pumps) {
            handle.abort();
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.abort_event_pumps();
    }
}

/// v0.8.x (concurrency review §4.1 P2) — the ONE-SHOT "turn went silent"
/// warn-only heads-up, folded from the old detached per-turn
/// `spawn_turn_timeout_watchdog` task into the session's own event pump (see
/// `spawn_event_pump`'s watchdog-tick branch, which calls this once idle time
/// reaches the configured window). Content/shape is byte-identical to the
/// pre-fold task. **NEVER interrupts** the turn, on ANY protocol (tmux / rmux
/// / stream-json) — red line: 永不主动 kill long sessions/turns. A long
/// SILENT command (a benchmark, a big build) is real work; mis-killing it
/// would be worse than a stray heads-up. The user `/stop`s if it is genuinely
/// stuck.
fn emit_turn_stall_warning(
    tx: &GatewayEventSink,
    session: &GatewaySession,
    turn_id: &str,
    timeout: std::time::Duration,
    progress_path: Option<&Path>,
) {
    let session_id = session.id.as_str();
    if let Some(progress_path) = progress_path {
        let ev = ccteam_core::progress::build_chat_turn_timeout_event(
            &session.role,
            session_id,
            &session.project,
            turn_id,
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
    // v0.8.18 (owner request) — WARN-ONLY: never interrupt. A turn that
    // produced NO events for the whole window is FLAGGED, not killed — a
    // long silent command (a benchmark, a big build) is real work, and an
    // `esc` here mis-kills it. The user `/stop`s if it is genuinely stuck.
    let (channel, chat_id) = match session.reply_to.lock() {
        Ok(target) => (target.channel.clone(), target.chat_id.clone()),
        Err(_) => (session.owner.channel.clone(), session.owner.chat_id.clone()),
    };
    // Carry the two facts ccteam actually owns about the silence: how long the
    // turn has been running and what the last thing it did was. Without them
    // the warning cannot be told apart from a legitimately quiet build — and a
    // vendor hung in its own internal retry loop (kimi's 429 backoff reports
    // nothing on the ACP wire) reads as "last seen: a tool call 16m ago", which
    // is diagnosable.
    let elapsed = session
        .turn_started_at
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|start| format!(" (running {})", humanize_dur(start.elapsed())))
        .unwrap_or_default();
    let last_seen = session
        .latest_activity
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .filter(|a| !a.trim().is_empty())
        .map(|a| format!(" Last observed activity: {a}."))
        .unwrap_or_else(|| " No activity was ever observed for this turn.".into());
    let content = format!(
        "⏱️ turn {turn_id} went silent for {timeout:?} for {session_id}{elapsed} — no tokens, \
         tool calls or progress.{last_seen} Heads-up only — the watchdog does NOT interrupt it \
         (a long command like a benchmark legitimately emits no events, and a vendor stuck in \
         its own retry loop reports nothing either). If it is truly stuck, `/stop` the session; \
         tune the window via CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS (0 = off)."
    );
    let _ = tx.send(GatewayEvent {
        id: format!("gateway-timeout-{session_id}-{turn_id}"),
        channel,
        chat_id,
        thread_ts: None,
        content,
        kind: GatewayEventKind::Answer,
        attachments: Vec::new(),
        options: Vec::new(),
        sid: Some(session_id.to_string()),
        slug: Some(session.project.clone()),
    });
}

/// Reply for a DM that could bind to more than one registered-bot
/// template: ask the user to pick a handle instead of guessing.
fn format_ambiguous_dm_reply(available: &[String]) -> String {
    if available.is_empty() {
        return "No bots available in this chat.".to_string();
    }
    let mentions: Vec<String> = available.iter().map(|h| format!("@{h}")).collect();
    format!(
        "Multiple bots in this chat. Specify one: {}",
        mentions.join(" ")
    )
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
        if i == 0 {
            let key = crate::transport::attachment_path_key(att.kind);
            attrs.push_str(&format!(" {key}=\"{}\"", att.local_path));
        } else {
            extra_lines.push(crate::transport::attachment_line(att));
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

/// Resolve a session pump's live reply target `(channel, chat_id)`,
/// honoring a `/cd`-updated `reply_to` and falling back to the owner.
fn pump_target(session: &GatewaySession) -> (String, String) {
    match session.reply_to.lock() {
        Ok(target) => (target.channel.clone(), target.chat_id.clone()),
        Err(_) => (session.owner.channel.clone(), session.owner.chat_id.clone()),
    }
}

fn take_turn_origin(session: &GatewaySession, turn_id: Option<&str>) -> TurnOrigin {
    let Ok(mut origins) = session.turn_origins.lock() else {
        return TurnOrigin::Internal;
    };
    match turn_id {
        Some(id) => origins.remove(id).unwrap_or(TurnOrigin::Internal),
        None if origins.len() == 1 => {
            let id = origins.keys().next().cloned();
            id.and_then(|id| origins.remove(&id))
                .unwrap_or(TurnOrigin::Internal)
        }
        None => TurnOrigin::Internal,
    }
}

fn contextual_answer(
    sid: &str,
    project: &str,
    vendor: AgentVendor,
    role: &str,
    text: &str,
) -> String {
    format!("[{sid} {project} {} {role}] {text}", vendor_str(vendor))
}

fn web_owner_im_target(paths: &CcteamPaths, owner: &ChatKey) -> Option<(String, String)> {
    if owner.channel != "user" {
        return None;
    }
    if owner.chat_id == "web-api" {
        let credentials = crate::credentials::load(Some(&paths.im_credentials_path())).ok()?;
        let telegram = credentials.telegram?;
        if telegram.bot_token.trim().is_empty() {
            return None;
        }
        let chat_id = telegram
            .allowed_chat_ids
            .first()
            .filter(|chat_id| !chat_id.trim().is_empty())?;
        return Some(("telegram".to_string(), chat_id.clone()));
    }
    crate::mcp::dispatch::user_delivery_target(paths, &owner.chat_id).ok()
}

#[allow(clippy::too_many_arguments)]
fn mirror_internal_web_answer(
    tx: &GatewayEventSink,
    paths: Option<&CcteamPaths>,
    session: &GatewaySession,
    reply_to: &ChatKey,
    origin: TurnOrigin,
    session_id: &str,
    seq: u64,
    text: &str,
) {
    if origin != TurnOrigin::Internal || reply_to.channel != "web" || session.parent_sid.is_some() {
        return;
    }
    // `session.owner` is authoritative: ownership is settled once at spawn
    // (`plan_new_session` inherits the project principal) and repaired at
    // rebuild (`plan_session_rebuild` re-reads it from the project SoT), so
    // there is no live session left whose owner could point at the wrong
    // mailbox. Re-deriving it from `ProjectState` here would be a compat shim
    // for metadata this repo explicitly does not migrate (§五.3/§五.4) — and a
    // second home for the ownership rule, on the delivery hot path.
    let Some((channel, chat_id)) = paths.and_then(|p| web_owner_im_target(p, &session.owner))
    else {
        return;
    };
    let _ = tx.send_delivery_only(GatewayEvent {
        id: format!("gateway-mirror-{session_id}-{seq}"),
        channel,
        chat_id,
        thread_ts: None,
        content: contextual_answer(
            session_id,
            &session.project,
            session.vendor,
            &session.role,
            text,
        ),
        kind: GatewayEventKind::Answer,
        attachments: Vec::new(),
        options: Vec::new(),
        sid: Some(session_id.to_string()),
        slug: Some(session.project.clone()),
    });
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
        slug: Some(session.project.clone()),
        kind: GatewayEventKind::Progress { status_key, done },
        attachments: Vec::new(),
        options: Vec::new(),
    })
}

/// Emit one structured `Activity` gateway event (v0.8.19) for the given
/// step, keyed to the same progress `status_key` as the turn's folded
/// status. IM drops it (a strict no-op arm); web renders it. The `content`
/// mirrors the summary so a generic consumer still has a human line.
/// Returns `false` only if the sink is closed (pump should stop).
fn emit_activity(
    tx: &GatewayEventSink,
    session: &GatewaySession,
    session_id: &str,
    epoch: u64,
    activity: SessionActivity,
) -> bool {
    let (channel, chat_id) = pump_target(session);
    let status_key = format!("{session_id}-{epoch}");
    let content = activity.summary.clone();
    tx.send(GatewayEvent {
        id: format!("gateway-activity-{status_key}-{}", activity.item_id),
        channel,
        chat_id,
        thread_ts: None,
        content,
        sid: Some(session_id.to_string()),
        kind: GatewayEventKind::Activity {
            status_key,
            activity,
        },
        attachments: Vec::new(),
        options: Vec::new(),
        slug: Some(session.project.clone()),
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

/// Minimum interval between status-message edits (default 800ms — lowered
/// from 1500ms for snappier activity updates; the live daemon showed ZERO
/// Telegram 429 backoff at the old rate, and each edit still pays a ~0.5s
/// platform round-trip so this stays comfortably under the edit rate-limit).
/// Override with `CCTEAM_IM_PROGRESS_THROTTLE_MS`; `=0` makes every step emit,
/// for deterministic tests that don't rely on sleeps.
fn progress_throttle() -> std::time::Duration {
    let ms = std::env::var("CCTEAM_IM_PROGRESS_THROTTLE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(800);
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

/// Canonical terminal failure carried by an event, independent of vendor.
fn thread_event_failure(evt: &ThreadEvent) -> Option<&ccteam_harness::ThreadErrorEvent> {
    match evt {
        ThreadEvent::TurnFailed { err, .. } | ThreadEvent::Error(err) => Some(err),
        _ => None,
    }
}

/// Whether this item is long-running work that can legitimately go silent
/// between start and complete (tools, shell, search — not the final answer).
fn is_openable_work_item(details: &ThreadItemDetails) -> bool {
    matches!(
        details,
        ThreadItemDetails::ToolCall { .. }
            | ThreadItemDetails::CommandExecution { .. }
            | ThreadItemDetails::WebSearch { .. }
            | ThreadItemDetails::FileChange { .. }
    )
}

/// Maintain the set of in-flight tool/command item ids for the silence
/// watchdog. Start opens, complete closes, turn boundary clears all.
fn track_open_work_items(open: &mut std::collections::HashSet<String>, evt: &ThreadEvent) {
    match evt {
        ThreadEvent::ItemStarted { item } if is_openable_work_item(&item.details) => {
            open.insert(item.id.clone());
        }
        ThreadEvent::ItemCompleted { item } if is_openable_work_item(&item.details) => {
            open.remove(&item.id);
        }
        ThreadEvent::TurnCompleted { .. }
        | ThreadEvent::TurnFailed { .. }
        | ThreadEvent::Error(_) => {
            open.clear();
        }
        _ => {}
    }
}

#[cfg(test)]
mod open_work_items_tests {
    use super::*;
    use ccteam_harness::{ThreadItem, ThreadItemDetails, UnifiedTokenUsage};

    #[test]
    fn open_work_tracks_tool_start_complete_and_clears_on_turn_end() {
        let mut open = std::collections::HashSet::new();
        track_open_work_items(
            &mut open,
            &ThreadEvent::ItemStarted {
                item: ThreadItem {
                    id: "tc1".into(),
                    details: ThreadItemDetails::ToolCall {
                        name: "Bash".into(),
                        args: serde_json::json!({}),
                    },
                },
            },
        );
        assert!(open.contains("tc1"));
        // Answer deltas are not open work.
        track_open_work_items(
            &mut open,
            &ThreadEvent::ItemUpdated {
                item: ThreadItem {
                    id: "msg".into(),
                    details: ThreadItemDetails::AgentMessage("draft".into()),
                },
            },
        );
        assert_eq!(open.len(), 1);
        track_open_work_items(
            &mut open,
            &ThreadEvent::ItemCompleted {
                item: ThreadItem {
                    id: "tc1".into(),
                    details: ThreadItemDetails::ToolCall {
                        name: "Bash".into(),
                        args: serde_json::json!({}),
                    },
                },
            },
        );
        assert!(open.is_empty());
        track_open_work_items(
            &mut open,
            &ThreadEvent::ItemStarted {
                item: ThreadItem {
                    id: "tc2".into(),
                    details: ThreadItemDetails::CommandExecution {
                        cmd: "sleep 600".into(),
                        status: "running".into(),
                    },
                },
            },
        );
        assert!(open.contains("tc2"));
        track_open_work_items(
            &mut open,
            &ThreadEvent::TurnCompleted {
                turn_id: "t1".into(),
                usage: UnifiedTokenUsage::default(),
                model: None,
            },
        );
        assert!(
            open.is_empty(),
            "turn boundary must clear so a lost ItemCompleted cannot mute forever"
        );
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
    // Idle window (v0.8.15): how long a turn may emit NO events at all before
    // the watchdog treats it as stalled. Generous by default — real work runs
    // long single tool calls (builds / test suites) that are silent to the
    // gateway while they execute. A streaming turn resets this on every event,
    // so this only bites genuine hangs. `0` disables the watchdog.
    const DEFAULT_MS: u64 = 300_000;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

/// Humanize a [`Duration`](std::time::Duration) for the `/status` fleet line:
/// `45s`, `1m12s`, `6m`, `2h3m`. Compact (no leading-zero noise); seconds are
/// dropped once the span is ≥ 1h to keep the line tidy. Sub-second rounds to
/// `0s`.
/// Render the `/status` running-task block — claude's own task lifecycle
/// mirrored verbatim (NOT a fold), oldest first (longest-running on top).
/// Empty string when nothing runs. Three buckets by `task_type`: subagents
/// (`local_agent`, turn-scoped), workflows (`local_workflow`) and background
/// shells (`local_bash` = Bash run_in_background + Monitor watches) — the
/// latter two outlive the spawning turn, so an idle session still shows its
/// in-flight `make test` here instead of a bare `🟢 idle`.
fn format_running_tasks(running: &[RunningTask]) -> String {
    if running.is_empty() {
        return String::new();
    }
    let workflows = running
        .iter()
        .filter(|t| t.task_type == "local_workflow")
        .count();
    let bg_shells = running
        .iter()
        .filter(|t| t.task_type == "local_bash")
        .count();
    let subagents = running.len() - workflows - bg_shells;
    let mut kinds: Vec<String> = Vec::new();
    if subagents > 0 {
        kinds.push(format!("subagent ({subagents})"));
    }
    if workflows > 0 {
        kinds.push(format!("workflow ({workflows})"));
    }
    if bg_shells > 0 {
        kinds.push(format!("后台任务 ({bg_shells})"));
    }
    let mut out = format!("\n   🤖 在跑 {}:", kinds.join(" + "));
    let mut tasks: Vec<&RunningTask> = running.iter().collect();
    tasks.sort_by_key(|t| t.started);
    for t in tasks {
        let kind = match t.task_type.as_str() {
            "local_workflow" => "workflow",
            "local_bash" => "后台",
            _ if t.kind.is_empty() => "subagent",
            _ => t.kind.as_str(),
        };
        let elapsed = humanize_dur(t.started.elapsed());
        let desc = t.description.trim();
        if desc.is_empty() {
            out.push_str(&format!("\n      · {kind} · {elapsed}"));
        } else {
            let shown: String = if desc.chars().count() > 40 {
                format!("{}…", desc.chars().take(39).collect::<String>())
            } else {
                desc.to_string()
            };
            out.push_str(&format!("\n      · {kind}「{shown}」· {elapsed}"));
        }
    }
    out
}

/// Render [`AccountUsage`] as the `/status` dashboard usage line:
/// `⚡ 用量: 5h 17% (→19:00) · 周 78%⚠ (→06/29) · 额度 46% · max`. Each field is
/// omitted when the vendor didn't report it; an empty result = nothing to show.
fn format_account_usage(u: &AccountUsage) -> String {
    // Short reset hint from an ISO-8601 `resets_at`: HH:MM for the 5-hour window,
    // MM/DD for the weekly. Empty when unparseable.
    fn reset_hm(iso: &Option<String>) -> String {
        iso.as_deref()
            .and_then(|s| s.split('T').nth(1))
            .map(|t| format!(" (→{})", &t[..t.len().min(5)]))
            .unwrap_or_default()
    }
    fn reset_md(iso: &Option<String>) -> String {
        iso.as_deref()
            .and_then(|s| s.split('T').next())
            .and_then(|d| d.get(5..)) // "06-29" from "2026-06-29"
            .map(|md| format!(" (→{})", md.replace('-', "/")))
            .unwrap_or_default()
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = u.five_hour_pct {
        parts.push(format!("5h {p}%{}", reset_hm(&u.five_hour_resets_at)));
    }
    if let Some(p) = u.weekly_pct {
        let warn = if u.weekly_severity.as_deref() == Some("warning") {
            "⚠"
        } else {
            ""
        };
        parts.push(format!("周 {p}%{warn}{}", reset_md(&u.weekly_resets_at)));
    }
    if let Some(p) = u.credits_pct {
        parts.push(format!("额度 {p}%"));
    }
    if parts.is_empty() {
        return String::new();
    }
    if let Some(sub) = u.subscription.as_deref() {
        parts.push(sub.to_string());
    }
    format!("⚡ 用量: {}", parts.join(" · "))
}

fn humanize_dur(d: std::time::Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else if m > 0 {
        if s > 0 {
            format!("{m}m{s}s")
        } else {
            format!("{m}m")
        }
    } else {
        format!("{s}s")
    }
}

/// The vendor `--resume` id (Anthropic session UUID) carried by a stream-json
/// [`ThreadHandle`], or `None` for a tmux / Codex handle (which has no
/// stream-json uuid). Read from `raw_extras["vendor_uuid"]` — the field both
/// the spawn and resume paths populate, persisted across daemon restarts — so
/// `/status` shows the actual id that `--resume` would use. Filters out an
/// empty string so a blank uuid degrades to `None` (→ `resume —`).
fn thread_vendor_uuid(thread: &ThreadHandle) -> Option<String> {
    thread
        .raw_extras
        .get("vendor_uuid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Outcome of [`Gateway::submit_resolved`] — the one core both user-entry
/// legs (IM `submit_to_current`, web `submit_to_sid`) funnel through. A
/// `/command` runs synchronously (carry its receipt lines); plain text becomes
/// a turn whose answer streams async (carry the turn id + any sink-less drain).
enum SubmitResult {
    /// A `/command` directive ran. Its synchronous reply lines
    /// (`Done`/`Rejected`/`Redirect` receipts; empty when the directive became
    /// a streaming turn or a sink-delivered choice).
    Directive(Vec<String>),
    /// Plain user text submitted as a new turn. `id` is the `TurnId` string
    /// (handed to `session_dispatch` for `session_collect{since}`); `drained`
    /// is the sink-less drained answer (empty in production — answers stream).
    Turn { id: String, drained: Vec<String> },
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

/// Telegram's hard cap on inline-button `callback_data` (bytes). The nav
/// picker (`project_switch_options` / `session_switch_options`) drops any
/// button whose payload would exceed it; slugs/sids are short, so this only
/// guards a pathological slug and never the common case.
const TELEGRAM_CALLBACK_MAX: usize = 64;

/// Max display columns a session title may occupy inside a `/sessions` switch
/// button label. A longer title is clipped with `…` so one verbose title can't
/// widen every button — the left-align padding aligns all labels to the widest.
const SESSION_BUTTON_TITLE_MAX_COLS: usize = 32;

/// Clip `s` to at most `max_cols` DISPLAY columns, appending `…` on overflow.
/// Column-based (not char count) so a CJK double-width title clips at a stable
/// visual width; the ellipsis reserves one column.
fn truncate_cols(s: &str, max_cols: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max_cols {
        return s.to_string();
    }
    let budget = max_cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Floor for [`left_align_option_labels`]'s pad target: Telegram renders every
/// option as its OWN full-width single-button row (one button per
/// `inline_keyboard` row — see `TelegramChannel::send`), so its text is
/// centered within the FULL row width, not a column sized to the option set.
/// Padding rows to only the OBSERVED max-in-set (e.g. all bare sids like
/// "s1"/"s2"/"s28") still centers a short block deep in a wide row — visually
/// indistinguishable from plain centering. Padding every label out to at
/// least this many columns pushes that block toward the row's left edge on a
/// typical phone-width chat. Approximate by nature: the bot API exposes no
/// client viewport, so this can undershoot (desktop) or overshoot slightly.
const PICKER_LABEL_MIN_PAD_COLS: usize = 30;

/// Right-pad every picker label to at least the widest row in the SET, or
/// [`PICKER_LABEL_MIN_PAD_COLS`], whichever is larger — so Telegram's
/// centre-aligned button text reads as a LEFT-aligned list (owner req,
/// tg-6955) even when every option in the set is short. Telegram strips
/// ordinary trailing whitespace from button labels, so the pad is U+2800
/// BRAILLE PATTERN BLANK — renders blank, survives the trim, ~1 cell wide.
fn left_align_option_labels(options: &mut [MessageOption]) {
    use unicode_width::UnicodeWidthStr;
    let observed_max = options
        .iter()
        .map(|o| o.label.as_str().width())
        .max()
        .unwrap_or(0);
    let target = observed_max.max(PICKER_LABEL_MIN_PAD_COLS);
    for o in options.iter_mut() {
        for _ in 0..target.saturating_sub(o.label.as_str().width()) {
            o.label.push('\u{2800}');
        }
    }
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

fn parse_inbox_create_args(rest: &str) -> Result<(String, String)> {
    fn take_word(value: &str) -> Option<(&str, &str)> {
        let value = value.trim_start();
        let split = value.find(char::is_whitespace).unwrap_or(value.len());
        let word = &value[..split];
        (!word.is_empty()).then(|| (word, value[split..].trim_start()))
    }

    let (first, after_first) =
        take_word(rest).ok_or_else(|| anyhow!("usage: /inbox <time> <text>"))?;
    let needs_second = first == "今天"
        || first == "明天"
        || (first.len() == 10
            && first.as_bytes().get(4) == Some(&b'-')
            && first.as_bytes().get(7) == Some(&b'-'));
    let (when, text) = if needs_second {
        let (second, body) =
            take_word(after_first).ok_or_else(|| anyhow!("usage: /inbox <time> <text>"))?;
        (format!("{first} {second}"), body)
    } else {
        (first.to_string(), after_first)
    };
    if text.trim().is_empty() {
        return Err(anyhow!("scheduled message text cannot be empty"));
    }
    Ok((when, text.to_string()))
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
        "grok" => Ok(AgentVendor::Grok),
        "opencode" => Ok(AgentVendor::Opencode),
        "kimi" => Ok(AgentVendor::Kimi),
        other => Err(anyhow!("unknown vendor: {other}")),
    }
}

/// Everything `/new` resolves before it touches the session spine.
#[derive(Debug, PartialEq, Eq)]
struct NewSessionArgs {
    vendor: AgentVendor,
    role: String,
    permission_mode: PermissionMode,
    protocol: SessionProtocol,
    tuning: SpawnTuning,
}

/// The one-line `/new` syntax, echoed by every parse error so a chat user
/// never has to leave the conversation to find the shape.
const NEW_COMMAND_SYNTAX: &str =
    "/new [vendor] [role] [hitl|skip] [terminal|acp] [model=<id>] [effort=<level>]";

/// Parse the tokens after `/new`.
///
/// Token grammar (all order-free after the leading vendor):
/// - **vendor** — the FIRST token when it names a harness; omitted ⇒ claude.
/// - **`key=value`** — the spawn-tuning facets `model=` / `effort=`
///   (`m=` / `e=` short forms). Matched BEFORE the bare-token arms so a
///   mistyped key (`modle=opus`) surfaces as an error instead of quietly
///   becoming the session's role name.
/// - **flags** — `hitl`/`skip`, `terminal`/`acp`/`stream-json`.
/// - **the first remaining bare token** — the role. v0.8.18 (owner): NO role
///   token ⇒ **roleless** (bare vendor self-reads the project `CLAUDE.md`);
///   no explicit `-` needed. So `/new claude` and `/new claude hitl` are both
///   roleless, while `/new claude reviewer hitl` is role `reviewer` + hitl.
///
/// `model` / `effort` are carried into [`SpawnTuning`] untouched: the vendor
/// owns the verdict on its own value set, and a bad token must come back as
/// the vendor's own spawn error rather than as a session that silently ran at
/// the default. Defaults = skip + stream-json; the ACP-only vendors override
/// the protocol axis last (a `terminal` flag there is a no-op, not an error).
fn parse_new_command_args(args: &[&str]) -> Result<NewSessionArgs> {
    let mut rest = args.iter().copied();
    let vendor = parse_vendor(rest.next().unwrap_or("claude"))?;
    let mut role = String::new();
    let mut role_set = false;
    let mut permission_mode = PermissionMode::Skip;
    let mut protocol = SessionProtocol::StreamJson;
    let mut tuning = SpawnTuning::default();
    for tok in rest {
        if let Some((key, value)) = tok.split_once('=') {
            let value = value.trim();
            if value.is_empty() {
                return Err(anyhow!(
                    "/new: `{tok}` has no value — write `{key}=<value>`\nsyntax: {NEW_COMMAND_SYNTAX}"
                ));
            }
            match key {
                "model" | "m" => tuning.model = Some(value.to_string()),
                "effort" | "e" => tuning.effort = Some(value.to_string()),
                other => {
                    return Err(anyhow!(
                        "/new: unknown option `{other}=` (accepts model=<id> / m=, effort=<level> / e=)\nsyntax: {NEW_COMMAND_SYNTAX}"
                    ));
                }
            }
            continue;
        }
        match tok {
            "hitl" | "skip" => {
                permission_mode = PermissionMode::parse_opt(Some(tok)).map_err(|e| anyhow!(e))?;
            }
            "terminal" | "tmux" | "stream-json" | "streamjson" | "stream_json" | "acp" => {
                protocol = SessionProtocol::parse_opt(Some(tok)).map_err(|e| anyhow!(e))?;
            }
            other if !role_set => {
                role = other.to_string();
                role_set = true;
            }
            other => {
                return Err(anyhow!(
                    "/new: unexpected token `{other}` (role `{role}` was already given)\nsyntax: {NEW_COMMAND_SYNTAX}"
                ));
            }
        }
    }
    // Grok/OpenCode/Kimi always speak ACP (v0.8.23) — settled after the loop
    // so token order never changes the outcome.
    if matches!(
        vendor,
        AgentVendor::Grok | AgentVendor::Opencode | AgentVendor::Kimi
    ) {
        protocol = SessionProtocol::Acp;
    }
    Ok(NewSessionArgs {
        vendor,
        role,
        permission_mode,
        protocol,
        tuning,
    })
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
        ChoiceOption, ContextSource, ContextUsage, ExecutionMode, HarnessError, ThreadItem,
        ThreadStatus, TurnId,
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

    fn capacity_candidate(sid: &str, idle: bool, last_active: &str) -> LiveCapacityCandidate {
        LiveCapacityCandidate {
            sid: sid.to_string(),
            idle,
            last_active: last_active.to_string(),
            waiting_approval: false,
        }
    }

    /// RFC3339 timestamp `secs_ago` seconds in the past — matches
    /// `meta.json.last_active`'s format, which sorts correctly as a plain
    /// string (the property `select_live_capacity_eviction` relies on).
    fn capacity_ts(secs_ago: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(secs_ago)).to_rfc3339()
    }

    fn parse_new(args: &str) -> Result<NewSessionArgs> {
        let tokens: Vec<&str> = args.split_whitespace().collect();
        parse_new_command_args(&tokens)
    }

    /// The historical `/new` shapes must keep parsing byte-identically after
    /// the `key=value` facets were added — the tuning tokens are additive, not
    /// a new grammar.
    #[test]
    fn new_command_keeps_its_vendor_role_flag_grammar() {
        let bare = parse_new("").unwrap();
        assert_eq!(bare.vendor, AgentVendor::Claude);
        assert_eq!(bare.role, "", "no role token ⇒ roleless");
        assert_eq!(bare.permission_mode, PermissionMode::Skip);
        assert_eq!(bare.protocol, SessionProtocol::StreamJson);
        assert_eq!(bare.tuning, SpawnTuning::default());

        let roled = parse_new("claude reviewer hitl").unwrap();
        assert_eq!(roled.role, "reviewer");
        assert_eq!(roled.permission_mode, PermissionMode::Hitl);

        // ACP vendors settle the protocol axis last, whatever was typed.
        assert_eq!(
            parse_new("grok terminal").unwrap().protocol,
            SessionProtocol::Acp
        );
    }

    /// `model=` / `effort=` are order-free and reach [`SpawnTuning`] verbatim
    /// — no vendor filtering, no normalization of the vendor's own token
    /// vocabulary. The short forms exist because these are typed on a phone.
    #[test]
    fn new_command_parses_order_free_model_and_effort_tokens() {
        for args in [
            "kimi effort=max model=kimi-code/k3 reviewer",
            "kimi reviewer model=kimi-code/k3 effort=max",
            "kimi model=kimi-code/k3 reviewer effort=max",
            "kimi e=max reviewer m=kimi-code/k3",
        ] {
            let parsed = parse_new(args).unwrap();
            assert_eq!(parsed.role, "reviewer", "{args}");
            assert_eq!(parsed.vendor, AgentVendor::Kimi, "{args}");
            assert_eq!(
                parsed.tuning,
                SpawnTuning {
                    model: Some("kimi-code/k3".to_string()),
                    effort: Some("max".to_string()),
                },
                "{args}"
            );
        }

        // A token ccteam has never heard of still rides through: the vendor
        // owns the verdict on its own value set.
        assert_eq!(
            parse_new("grok effort=ludicrous").unwrap().tuning.effort,
            Some("ludicrous".to_string())
        );
    }

    /// A mistyped key must NOT fall through to the role arm: `/new modle=opus`
    /// silently becoming role `modle=opus` is the same class of failure as a
    /// silently dropped effort — the user asked for something and got
    /// something else without being told.
    #[test]
    fn new_command_rejects_unknown_keys_with_an_honest_syntax_line() {
        let err = parse_new("claude modle=opus").unwrap_err().to_string();
        assert!(err.contains("unknown option `modle=`"), "{err}");
        assert!(err.contains("model=<id>"), "{err}");
        assert!(err.contains("effort=<level>"), "{err}");
        assert!(err.contains(NEW_COMMAND_SYNTAX), "{err}");

        let err = parse_new("claude model=").unwrap_err().to_string();
        assert!(err.contains("has no value"), "{err}");
        assert!(err.contains(NEW_COMMAND_SYNTAX), "{err}");

        // A second bare token is still the pre-existing "one role" error, now
        // carrying the same syntax line.
        let err = parse_new("claude reviewer auditor")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unexpected token `auditor`"), "{err}");
        assert!(err.contains(NEW_COMMAND_SYNTAX), "{err}");
    }

    /// The menu/help entry must teach the tuning tokens: a facet nobody can
    /// discover is a facet nobody uses.
    #[test]
    fn new_command_help_advertises_model_and_effort() {
        let spec = GATEWAY_COMMANDS
            .iter()
            .find(|c| c.name == "/new")
            .expect("/new is a gateway command");
        let hint = spec.arg_hint.expect("/new takes args");
        assert!(hint.contains("model=<id>"), "{hint}");
        assert!(hint.contains("effort=<level>"), "{hint}");
    }

    #[test]
    fn capacity_eviction_prefers_idle_over_busy_even_if_more_recently_active() {
        let candidates = vec![
            capacity_candidate("idle-recent", true, &capacity_ts(1)),
            capacity_candidate("busy-older", false, &capacity_ts(100)),
        ];
        assert_eq!(
            select_live_capacity_eviction(&candidates, &HashSet::new()).as_deref(),
            Some("idle-recent"),
            "idle is preferred for eviction even over a busier-but-older session"
        );
    }

    #[test]
    fn capacity_eviction_prefers_oldest_last_active_among_idle() {
        let candidates = vec![
            capacity_candidate("idle-new", true, &capacity_ts(5)),
            capacity_candidate("idle-old", true, &capacity_ts(20)),
        ];
        assert_eq!(
            select_live_capacity_eviction(&candidates, &HashSet::new()).as_deref(),
            Some("idle-old")
        );
    }

    /// THE BUG FIX — a candidate with no readable `last_active` (empty: no
    /// meta, or a meta that never completed a turn) must be treated as the
    /// OLDEST (most evictable), never the newest. The prior `Instant`-based
    /// design treated its "no signal yet" case (`None`) as newest — correct
    /// for a session that TRULY just spawned, but also hit by every
    /// REBUILT/RESTORED session (daemon restart, cold `/use` resume), whose
    /// in-process clock resets to `None` regardless of how stale its real
    /// history was. That made a long-dormant-but-still-live session
    /// PERMANENTLY eviction-immune while genuinely-recent sessions were
    /// evicted around it. Reading `last_active` from disk (refreshed on every
    /// completed turn, survives restarts) fixes the common case; this test
    /// locks the remaining fallback to fail toward evictable, not immunity.
    #[test]
    fn capacity_eviction_treats_missing_last_active_as_oldest_not_newest() {
        let candidates = vec![
            capacity_candidate("has-recent-real-activity", true, &capacity_ts(10)),
            capacity_candidate("no-meta-signal", true, ""),
        ];
        assert_eq!(
            select_live_capacity_eviction(&candidates, &HashSet::new()).as_deref(),
            Some("no-meta-signal"),
            "a blank/unreadable last_active must be evicted BEFORE a session with real recent activity"
        );
    }

    #[test]
    fn capacity_eviction_falls_back_to_busy_oldest() {
        let candidates = vec![
            capacity_candidate("busy-new", false, &capacity_ts(2)),
            capacity_candidate("busy-old", false, &capacity_ts(30)),
        ];
        assert_eq!(
            select_live_capacity_eviction(&candidates, &HashSet::new()).as_deref(),
            Some("busy-old")
        );
    }

    #[test]
    fn capacity_eviction_respects_sid_and_hitl_exclusions() {
        let mut waiting = capacity_candidate("waiting", true, &capacity_ts(60));
        waiting.waiting_approval = true;
        let candidates = vec![
            waiting,
            capacity_candidate("parent", true, &capacity_ts(40)),
            capacity_candidate("eligible", true, &capacity_ts(10)),
        ];
        let excluded = HashSet::from(["parent".to_string()]);
        assert_eq!(
            select_live_capacity_eviction(&candidates, &excluded).as_deref(),
            Some("eligible")
        );
    }

    #[test]
    fn capacity_eviction_returns_none_when_all_candidates_are_excluded() {
        let candidates = vec![
            capacity_candidate("s1", true, &capacity_ts(0)),
            capacity_candidate("s2", false, ""),
        ];
        let excluded = HashSet::from(["s1".to_string(), "s2".to_string()]);
        assert_eq!(select_live_capacity_eviction(&candidates, &excluded), None);
    }

    #[tokio::test]
    async fn spawn_at_capacity_evicts_lru_idle_and_admits_new_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        gateway.set_sessions_config(ccteam_core::SessionsConfig { max_live: 2 });

        let first = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let second = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        gateway.sessions.get_mut(&second).unwrap().parent_sid = Some(first.clone());
        assert_eq!(
            gateway.live_capacity_exclusions("s3", Some(&second)),
            vec!["s3".to_string(), second.clone(), first.clone()]
        );
        gateway.sessions.get_mut(&second).unwrap().parent_sid = None;
        // Eviction ranks on the PERSISTED `meta.json.last_active` (not an
        // in-process clock — see `LiveCapacityCandidate`), so seed it on disk:
        // `first` older, `second` more recent.
        for (sid, secs_ago) in [(&first, 30i64), (&second, 5)] {
            let mut meta = read_session_meta(tmp.path(), sid).unwrap();
            meta.last_active =
                (chrono::Utc::now() - chrono::Duration::seconds(secs_ago)).to_rfc3339();
            write_session_meta(tmp.path(), &meta).unwrap();
        }

        let mut events = gateway.subscribe_events();
        let third = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;

        assert!(!gateway.sessions.contains_key(&first));
        assert!(gateway.sessions.contains_key(&second));
        assert!(gateway.sessions.contains_key(&third));
        assert_eq!(gateway.sessions.len(), 2);
        let event = events.recv().await.unwrap();
        assert_eq!(event.sid.as_deref(), Some(first.as_str()));
        assert!(matches!(
            event.kind,
            GatewayEventKind::SessionLifecycle { ref state, ref reason }
                if state == "evicted" && reason == "capacity"
        ));
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

    /// Poll a [`Gateway::subscribe_events`] receiver for the next `Answer`
    /// event, skipping any `Reaction`/`Progress` noise in between. Bounded so
    /// a real regression (the pump never sends) fails the test instead of
    /// hanging.
    async fn recv_answer(
        events: &mut tokio::sync::broadcast::Receiver<GatewayEvent>,
    ) -> GatewayEvent {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let ev = events.recv().await.expect("broadcast tee still open");
                if matches!(ev.kind, GatewayEventKind::Answer) {
                    return ev;
                }
            }
        })
        .await
        .expect("an Answer event arrives")
    }

    async fn recv_sink_answers(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<GatewayEvent>,
        count: usize,
    ) -> Vec<GatewayEvent> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut answers = Vec::new();
            while answers.len() < count {
                let event = events.recv().await.expect("gateway sink remains open");
                if matches!(event.kind, GatewayEventKind::Answer) {
                    answers.push(event);
                }
            }
            answers
        })
        .await
        .expect("expected Answer events arrive")
    }

    async fn wait_for_turn_idle(gateway: &Gateway, sid: &str) {
        for _ in 0..100 {
            if !gateway.session_turn_in_flight(sid) {
                tokio::task::yield_now().await;
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("turn {sid} did not reach its boundary");
    }

    /// Register `slug` in the catalog with a persisted `ProjectState.owner`.
    /// Ownership is what a tenant's visibility is computed from, so any fixture
    /// that drives a NON-operator chat has to say which project it owns —
    /// otherwise the principal correctly resolves to "no project" and cannot
    /// spawn (`current_project_for`).
    fn seed_owned_project(paths: &ccteam_core::CcteamPaths, slug: &str, owner: Option<&str>) {
        let dir = paths.projects_root.join(slug);
        std::fs::create_dir_all(dir.join(".ccteam")).unwrap();
        let mut state = ccteam_core::ProjectState::initial(slug.to_string());
        state.owner = owner.map(str::to_string);
        state
            .save(&ccteam_core::CcteamPaths::project_state_in(&dir))
            .unwrap();
        ccteam_core::config::upsert_project(
            &paths.root,
            ccteam_core::ProjectEntry {
                slug: slug.to_string(),
                path: dir,
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();
    }

    fn mirror_test_paths(tmp: &tempfile::TempDir) -> (CcteamPaths, PathBuf) {
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let project_dir = paths.projects_root.join("alpha");
        std::fs::create_dir_all(&project_dir).unwrap();
        (paths, project_dir)
    }

    fn seed_global_telegram(paths: &CcteamPaths, allowed_chat_ids: Vec<String>) {
        crate::credentials::save(
            &paths.im_credentials_path(),
            &crate::credentials::Credentials {
                telegram: Some(crate::credentials::TelegramCreds {
                    bot_token: "123:test".into(),
                    allowed_chat_ids,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// Fetch a `/sessions` or `/projects` list as the user SEES it, regardless
    /// of whether it arrives as a plain-text inline reply (mock / web / Lark)
    /// or — on a button-capable channel (Telegram) — as an event carrying the
    /// list text + inline switch buttons. Returns the list text as a
    /// single-element Vec so ACL assertions read identically across channels.
    async fn list_text(
        gateway: &mut Gateway,
        events: &mut tokio::sync::broadcast::Receiver<GatewayEvent>,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        cmd: &str,
    ) -> Vec<String> {
        let replies = gateway
            .handle_text(channel, chat_id, user_id, cmd)
            .await
            .unwrap();
        if replies.is_empty() {
            // Button-capable channel → the list rode the event sink.
            vec![recv_answer(events).await.content]
        } else {
            replies
        }
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
            slug: None,
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

    /// v0.8.24 A-U3 — an explicit `SpawnTuning` (composer model + effort)
    /// reaches the adapter's `SpawnCtx`, and the explicit model beats the
    /// role's `model:` frontmatter; without tuning the role default holds
    /// and effort stays `None`.
    #[tokio::test]
    async fn create_session_api_threads_model_and_effort_into_spawn_ctx() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role_with_model(tmp.path(), "reviewer", Some("sonnet"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());

        // Explicit tuning wins over the role frontmatter.
        let created = gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning {
                    model: Some("future-model-from-vendor".into()),
                    effort: Some("max".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(created.sid, "s1");

        // No tuning → role frontmatter model, no effort.
        gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
            )
            .await
            .unwrap();

        // Whitespace-only tuning normalizes to None (role default holds).
        gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning {
                    model: Some("  ".into()),
                    effort: Some("".into()),
                },
            )
            .await
            .unwrap();

        let tunings = fake.spawn_tunings.lock().await.clone();
        assert_eq!(
            tunings,
            vec![
                (Some("future-model-from-vendor".into()), Some("max".into())),
                (Some("sonnet".into()), None),
                (Some("sonnet".into()), None),
            ]
        );
    }

    /// A spawn-time pick must survive the session, not just its first process.
    /// `meta.json` persisted `model` but not `effort`, so every re-spawn path
    /// (resume a dead session, `/role` switch, daemon-restart rebuild) restored
    /// the model and reset the effort to the vendor default — one axis of the
    /// same explicit choice silently evaporating, with a live sid to suggest
    /// nothing had happened.
    #[tokio::test]
    async fn an_explicit_effort_survives_a_respawn() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role_with_model(tmp.path(), "reviewer", None);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());

        gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning {
                    model: Some("opus".into()),
                    effort: Some("xhigh".into()),
                },
            )
            .await
            .unwrap();

        // Both axes are on disk — meta.json is the session SoT every re-spawn
        // path rebuilds from.
        let meta = read_session_meta(tmp.path(), "s1").expect("meta written");
        assert_eq!(meta.model.as_deref(), Some("opus"));
        assert_eq!(meta.effort.as_deref(), Some("xhigh"));

        // Stop it, then resume by sid — the rebuild-from-meta path a daemon
        // restart, a capacity eviction and a web resume all share. The re-spawn
        // must carry BOTH axes, not just the model.
        gateway.stop_session("s1").await.ok();
        gateway
            .resume_stopped_session("s1", "user:web-api", Some("alpha"))
            .await
            .expect("resume");
        let last = fake.spawn_tunings.lock().await.last().cloned();
        assert_eq!(
            last,
            Some((Some("opus".into()), Some("xhigh".into()))),
            "a resumed session re-spawns with the effort it was created with"
        );
    }

    // ----- P2a wrap_inbound (turn-text + attachment paths) ----------

    use crate::transport::AttachmentKind;

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

    /// `(model_id, effort)` captured from a `SpawnCtx` (clippy type_complexity).
    type CapturedTuning = (Option<String>, Option<String>);

    #[derive(Debug)]
    struct FakeAdapter {
        vendor: AgentVendor,
        starts: AtomicUsize,
        submissions: Arc<Mutex<Vec<(String, String)>>>,
        routings: Arc<Mutex<Vec<TurnRouting>>>,
        degrade_inject_to_queue: bool,
        events: Arc<Mutex<VecDeque<(String, ThreadEvent)>>>,
        /// Notified whenever an event is pushed so `events()` can wait rather
        /// than terminate when the queue is momentarily empty (fixes the
        /// multi-thread runtime race where the pump polls before `submit_turn`).
        events_notify: Arc<tokio::sync::Notify>,
        event_delay: std::time::Duration,
        resume_delay: std::time::Duration,
        /// v0.8.21 Wave-2 — delay inside `start_thread` so a test can assert the
        /// batch restore (which cold-starts via `start_thread`) does not hold the
        /// gateway lock across the slow spawn.
        start_delay: std::time::Duration,
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
        /// v0.8.24 A-U3 — `(model_id, effort)` captured per start_thread so a
        /// test can assert an explicit composer choice reached the SpawnCtx.
        spawn_tunings: Arc<Mutex<Vec<CapturedTuning>>>,
        /// v0.8.11 E4 — when set, `submit_turn` ALSO enqueues a `TurnCompleted`
        /// after the `AgentMessage` (mirrors a real adapter's turn boundary).
        /// Off by default so the sync-drain tests (which only take the first
        /// text-bearing event) don't leave a stale `TurnCompleted` queued.
        emit_turn_boundary: bool,
        /// Emit the structured paneless turn-start boundary before reply data.
        /// Opt-in so timing-sensitive tests can pause between start/completion.
        emit_turn_started: bool,
        /// Emit a structured terminal vendor failure instead of a normal
        /// assistant answer/boundary. Models Codex `error{willRetry:false}` and
        /// ACP prompt-RPC errors after their harness translation.
        turn_failure: Option<String>,
        /// v0.8.19 — thread identities passed to `interrupt_turn`, in call
        /// order, so the `/interrupt` test can assert the gateway invoked the
        /// adapter's interrupt (not destroy).
        interrupts: Arc<Mutex<Vec<String>>>,
        /// `set_session_title` pushes, in call order: `(sid, title, live)` —
        /// `live` records whether a thread handle came with it, which is what
        /// separates a rename on a live session from one on a stopped one.
        title_pushes: Arc<Mutex<Vec<(String, String, bool)>>>,
        /// When set, this fake claims a vendor title surface and answers
        /// `Pushed`; off by default so the honest "this vendor has no title
        /// API" wording stays the default under test.
        title_surface: bool,
        /// Liveness reported by `thread_is_live`. A test flips this to `false`
        /// to simulate the child exiting out from under a held handle (crash /
        /// OOM / long idle); `start_thread` flips it back `true` so a resume
        /// "revives" it — exactly the stream-json dead-child → resume case.
        live: Arc<std::sync::atomic::AtomicBool>,
        /// v0.9 T2 — `close_thread` call count (stop path + discarded zombie
        /// resume both close).
        closes: AtomicUsize,
    }

    impl Default for FakeAdapter {
        fn default() -> Self {
            Self::new(AgentVendor::Claude)
        }
    }

    /// A bare gateway whose operator roster names `telegram:339` — exactly what
    /// the daemon binds from `credentials.telegram.allowed_chat_ids`. The ACL
    /// predicates are instance methods because privilege is CONFIGURATION (a
    /// named chat), not a property of the chat's shape: an unnamed chat is a
    /// guest, so a test that means "the owner's phone" must say so.
    fn acl_gateway() -> Gateway {
        let mut gateway = Gateway::new(
            Arc::new(FakeAdapter::new(AgentVendor::Claude)),
            "alpha",
            "/tmp/alpha-acl",
        );
        gateway.bind_operator_allowlist("telegram", ["339".to_string()]);
        gateway
    }

    impl FakeAdapter {
        fn new(vendor: AgentVendor) -> Self {
            Self {
                vendor,
                starts: AtomicUsize::new(0),
                submissions: Arc::new(Mutex::new(Vec::new())),
                routings: Arc::new(Mutex::new(Vec::new())),
                degrade_inject_to_queue: false,
                events: Arc::new(Mutex::new(VecDeque::new())),
                events_notify: Arc::new(tokio::sync::Notify::new()),
                event_delay: std::time::Duration::ZERO,
                resume_delay: std::time::Duration::ZERO,
                start_delay: std::time::Duration::ZERO,
                resume_started: Arc::new(AtomicUsize::new(0)),
                directives: Arc::new(Mutex::new(Vec::new())),
                directive_script: Arc::new(Mutex::new(VecDeque::new())),
                status: Arc::new(Mutex::new(ThreadStatus::default())),
                spawn_modes: Arc::new(Mutex::new(Vec::new())),
                spawn_secrets: Arc::new(Mutex::new(Vec::new())),
                spawn_tunings: Arc::new(Mutex::new(Vec::new())),
                emit_turn_boundary: false,
                emit_turn_started: false,
                turn_failure: None,
                interrupts: Arc::new(Mutex::new(Vec::new())),
                title_pushes: Arc::new(Mutex::new(Vec::new())),
                title_surface: false,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                closes: AtomicUsize::new(0),
            }
        }

        /// Opt into emitting a `TurnCompleted` boundary after the answer
        /// (v0.8.11 E4 — drives the stream-json pump's progress.jsonl mirror).
        fn with_turn_boundary(mut self) -> Self {
            self.emit_turn_boundary = true;
            self
        }

        fn with_turn_started(mut self) -> Self {
            self.emit_turn_started = true;
            self
        }

        fn with_inject_degraded_to_queue(mut self) -> Self {
            self.degrade_inject_to_queue = true;
            self
        }

        fn with_turn_failure(mut self, message: impl Into<String>) -> Self {
            self.turn_failure = Some(message.into());
            self
        }

        /// Opt into a vendor that HAS a session-title surface (claude/codex);
        /// the default fake reports `Unsupported` like an ACP vendor.
        fn with_title_surface(mut self) -> Self {
            self.title_surface = true;
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

        fn with_start_delay(mut self, start_delay: std::time::Duration) -> Self {
            self.start_delay = start_delay;
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
            if !self.start_delay.is_zero() {
                tokio::time::sleep(self.start_delay).await;
            }
            self.spawn_modes.lock().await.push(ctx.permission_mode);
            self.spawn_secrets.lock().await.push(ctx.secret.clone());
            self.spawn_tunings
                .lock()
                .await
                .push((ctx.model_id.clone(), ctx.effort.clone()));
            // A fresh/resumed child is alive.
            self.live.store(true, Ordering::SeqCst);
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
            // Model a stream-json child that has exited: a dead session's submit
            // returns the recoverable ThreadDied WITHOUT delivering (so the
            // gateway resumes + retries, and no double-submit is recorded). A
            // resume revives `live` (start_thread sets it true).
            if !self.live.load(Ordering::SeqCst) {
                return Err(HarnessError::ThreadDied(format!(
                    "fake child exited: {}",
                    h.identity
                )));
            }
            let text = match input {
                TurnInput::UserText(text) => text,
                _ => String::new(),
            };
            self.submissions
                .lock()
                .await
                .push((h.identity.clone(), text.clone()));
            let turn_id = format!("turn-{}", h.identity);
            if self.emit_turn_started {
                self.events.lock().await.push_back((
                    h.identity.clone(),
                    ThreadEvent::TurnStarted {
                        turn_id: turn_id.clone(),
                    },
                ));
            }
            if let Some(message) = self.turn_failure.as_ref() {
                self.events.lock().await.push_back((
                    h.identity.clone(),
                    ThreadEvent::TurnFailed {
                        turn_id: turn_id.clone(),
                        err: ccteam_harness::ThreadErrorEvent {
                            kind: "turn_failed".to_string(),
                            message: message.clone(),
                        },
                    },
                ));
            } else {
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
                            turn_id: turn_id.clone(),
                            // Non-zero usage so experience cost_usd prices > 0 for
                            // known models (v0.9 T5 pump writer).
                            usage: ccteam_harness::UnifiedTokenUsage {
                                input_tokens: 1_000,
                                output_tokens: 500,
                                ..Default::default()
                            },
                            // A real claude turn carries its canonical model; seed
                            // one so the pump's chat_turn_completed mirror exercises
                            // the per-turn model path.
                            model: Some("claude-sonnet-4-6".to_string()),
                        },
                    ));
                }
            }
            // Wake any pump task that is waiting in `events()` for new work.
            self.events_notify.notify_one();
            Ok(TurnId::new(turn_id))
        }

        async fn submit_turn_routed(
            &self,
            h: &ThreadHandle,
            input: TurnInput,
            routing: TurnRouting,
        ) -> Result<ccteam_harness::TurnSubmission, HarnessError> {
            self.routings.lock().await.push(routing);
            let had_active = !self.submissions.lock().await.is_empty();
            let disposition =
                if self.degrade_inject_to_queue && had_active && routing == TurnRouting::Inject {
                    TurnDisposition::Queued
                } else if had_active && routing == TurnRouting::Inject {
                    TurnDisposition::Injected
                } else {
                    TurnDisposition::Started
                };
            let turn_id = self.submit_turn(h, input).await?;
            Ok(match disposition {
                TurnDisposition::Started => ccteam_harness::TurnSubmission::started(turn_id),
                TurnDisposition::Injected => ccteam_harness::TurnSubmission::injected(turn_id),
                TurnDisposition::Queued => ccteam_harness::TurnSubmission::queued(turn_id),
            })
        }

        fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
            let events = Arc::clone(&self.events);
            let notify = Arc::clone(&self.events_notify);
            let wanted = h.identity.clone();
            let delay = self.event_delay;
            Box::pin(futures::stream::unfold((), move |_| {
                let events = Arc::clone(&events);
                let notify = Arc::clone(&notify);
                let wanted = wanted.clone();
                async move {
                    loop {
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        let mut guard = events.lock().await;
                        let idx = guard.iter().position(|(thread, _)| thread == &wanted);
                        if let Some(idx) = idx {
                            let (_, evt) = guard.remove(idx).unwrap();
                            return Some((evt, ()));
                        }
                        drop(guard);
                        // Queue is empty; wait for new events rather than
                        // terminating the stream. This matches the behaviour of
                        // a real adapter (which blocks until the child writes)
                        // and prevents a multi-thread runtime race where the pump
                        // polls before `submit_turn` has pushed anything.
                        notify.notified().await;
                    }
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
            self.closes.fetch_add(1, Ordering::SeqCst);
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

        async fn interrupt_turn(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
            // Record the interrupt; the session is NOT destroyed (the gateway
            // keeps the record), so this just notes the turn-stop was driven.
            self.interrupts.lock().await.push(h.identity.clone());
            Ok(())
        }

        fn thread_is_live(&self, _h: &ThreadHandle) -> bool {
            self.live.load(Ordering::SeqCst)
        }

        async fn set_session_title(
            &self,
            target: &SessionTitleTarget,
            title: &str,
        ) -> Result<TitleSync, HarnessError> {
            self.title_pushes.lock().await.push((
                target.sid.clone(),
                title.to_string(),
                target.thread.is_some(),
            ));
            Ok(if self.title_surface {
                TitleSync::Pushed
            } else {
                TitleSync::Unsupported
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paneless_turn_start_refreshes_last_active_before_delayed_completion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(
            FakeAdapter::new_with_event_delay(
                AgentVendor::Claude,
                std::time::Duration::from_millis(250),
            )
            .with_turn_started()
            .with_turn_boundary(),
        );
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let mut meta = read_session_meta(tmp.path(), &sid).unwrap();
        let spawn_last_active = "2000-01-01T00:00:00Z".to_string();
        meta.last_active.clone_from(&spawn_last_active);
        write_session_meta(tmp.path(), &meta).unwrap();

        gateway
            .submit_to_sid(&sid, "long repository task".into())
            .await
            .unwrap();
        for _ in 0..100 {
            let started_seen = gateway
                .sessions
                .get(&sid)
                .is_some_and(|session| session.activity_events.load(Ordering::SeqCst) > 0);
            if started_seen {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            gateway
                .sessions
                .get(&sid)
                .is_some_and(|session| session.activity_events.load(Ordering::SeqCst) > 0),
            "fake vendor must emit TurnStarted before its delayed answer"
        );
        // Let the pump finish the TurnStarted branch, while the next fake event
        // remains delayed well beyond this read.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let during_turn = read_session_meta(tmp.path(), &sid).unwrap();
        assert_ne!(
            during_turn.last_active, spawn_last_active,
            "TurnStarted must refresh persisted last_active before completion"
        );
        assert!(
            gateway.session_turn_in_flight(&sid),
            "the delayed vendor turn must still be running at this checkpoint"
        );
    }

    #[tokio::test]
    async fn gateway_plain_message_submits_to_current_session_and_echoes() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let created = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(created, vec!["created session s1\n↓ 查看状态 → /status"]);

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

    #[tokio::test]
    async fn mid_turn_inject_preserves_original_working_start() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;

        // This sink-less fake emits an answer but no TurnCompleted boundary,
        // leaving the first vendor turn marked in flight for the steer below.
        gateway.submit_to_sid(&sid, "first".into()).await.unwrap();
        let first_started = gateway.sessions[&sid]
            .turn_started_at
            .lock()
            .unwrap()
            .expect("first turn start");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        gateway.submit_to_sid(&sid, "steer".into()).await.unwrap();
        let after_steer = gateway.sessions[&sid]
            .turn_started_at
            .lock()
            .unwrap()
            .expect("turn remains active");
        assert_eq!(after_steer, first_started, "Inject must not reset elapsed");
        assert!(
            gateway.sessions[&sid]
                .steered_this_turn
                .load(Ordering::SeqCst),
            "same-turn submit is recorded as steered"
        );
        assert_eq!(
            fake.routings.lock().await.as_slice(),
            &[TurnRouting::Inject, TurnRouting::Inject],
            "the application route is explicitly Inject for every message"
        );
    }

    #[tokio::test]
    async fn started_submission_refreshes_stale_working_start() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;

        let stale = Instant::now() - std::time::Duration::from_secs(90);
        *gateway.sessions[&sid].turn_started_at.lock().unwrap() = Some(stale);

        // The fake has no active adapter submission, so despite the stale
        // Gateway marker it honestly reports this accepted message as Started.
        gateway
            .submit_to_sid(&sid, "after resume".into())
            .await
            .unwrap();
        let refreshed = gateway.sessions[&sid]
            .turn_started_at
            .lock()
            .unwrap()
            .expect("new turn remains in flight");
        assert!(
            refreshed > stale + std::time::Duration::from_secs(60),
            "Started disposition must replace stale turn_started_at"
        );
    }

    #[tokio::test]
    async fn degraded_inject_is_reported_as_queue_not_steer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::default().with_inject_degraded_to_queue());
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Kimi,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;

        gateway.submit_to_sid(&sid, "first".into()).await.unwrap();
        gateway.submit_to_sid(&sid, "queued".into()).await.unwrap();
        assert!(
            !gateway.sessions[&sid]
                .steered_this_turn
                .load(Ordering::SeqCst),
            "adapter-reported Queue must roll back provisional steer state"
        );
    }

    #[tokio::test]
    async fn canonical_turn_started_reopens_working_window_for_queued_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Kimi,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let identity = gateway.sessions[&sid].thread.identity.clone();
        *gateway.sessions[&sid].turn_started_at.lock().unwrap() = None;
        fake.events.lock().await.push_back((
            identity,
            ThreadEvent::TurnStarted {
                turn_id: "queued-2".into(),
            },
        ));
        fake.events_notify.notify_one();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if gateway.session_turn_in_flight(&sid) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("event pump stamps queued TurnStarted");
    }

    /// A QUEUED submission is waiting, not silent. Arming the silence watchdog
    /// on it produces a "turn went silent — /stop the session" warning for a
    /// turn that never started, blaming it for its predecessor's silence (the
    /// s172 shape: kimi ACP parks a mid-turn message in FIFO, and the queued
    /// turn got a stall warning 5m later while the real turn was still running).
    /// The pump's canonical `TurnStarted` is the authority instead.
    #[tokio::test]
    async fn queued_submission_never_arms_the_watchdog_but_its_turn_start_does() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::default().with_inject_degraded_to_queue());
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Kimi,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let identity = gateway.sessions[&sid].thread.identity.clone();

        // A running turn IS armed — that is the watchdog's real job.
        gateway.submit_to_sid(&sid, "first".into()).await.unwrap();
        assert!(
            gateway.sessions[&sid]
                .watched_turn
                .lock()
                .unwrap()
                .is_some(),
            "a Started submission must be watched"
        );

        // Clear the arm the way the watchdog does once it has warned, then let
        // a second message degrade to FIFO behind the active turn.
        *gateway.sessions[&sid].watched_turn.lock().unwrap() = None;
        gateway.submit_to_sid(&sid, "queued".into()).await.unwrap();
        assert_eq!(
            fake.routings.lock().await.len(),
            2,
            "both messages reached the adapter"
        );
        assert!(
            gateway.sessions[&sid]
                .watched_turn
                .lock()
                .unwrap()
                .is_none(),
            "a queued turn has not started and must not be watched for silence"
        );

        // …and when it really starts, the pump arms it by its own turn id.
        fake.events.lock().await.push_back((
            identity,
            ThreadEvent::TurnStarted {
                turn_id: "queued-2".into(),
            },
        ));
        fake.events_notify.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let armed = gateway.sessions[&sid].watched_turn.lock().unwrap().clone();
                if let Some((turn_id, _)) = armed {
                    assert_eq!(turn_id, "queued-2");
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("canonical TurnStarted arms the watchdog for the turn that began");
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

    /// v0.8.23 review §1.3-D item 9 — `SessionView::waiting_approval` mirrors
    /// the shared pending registry: `false` for an ordinary session, flips to
    /// `true` the instant an External-origin HITL prompt is tagged with its
    /// sid (`PendingInteractions::tag_sid`, the exact step `hitl::ask_permission`
    /// takes), and back to `false` once the prompt is taken/resolved. Read via
    /// `try_lock` so `session_views()` stays a sync, non-blocking snapshot.
    #[tokio::test]
    async fn gateway_session_views_reports_waiting_approval() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha-waiting-approval");
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();

        assert!(
            !gateway.session_views()[0].waiting_approval,
            "no pending yet"
        );

        let token = "pwaiting001";
        let (tx, _rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            permission_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );
        shared.lock().await.tag_sid(token, sid.to_string());

        assert!(
            gateway.session_views()[0].waiting_approval,
            "tagged pending flips the flag"
        );

        // Resolving the approval consumes the pending — the flag drops again.
        gateway.resolve_web_selection(token, "allow").await.unwrap();
        assert!(
            !gateway.session_views()[0].waiting_approval,
            "resolved pending clears the flag"
        );
    }

    /// v0.9 T2 — plan → spawn → apply round-trip matches the old monolithic
    /// resume: same sid, one extra start_thread, child revived, no new session.
    #[tokio::test]
    async fn resume_dead_session_three_phase_round_trip() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        assert_eq!(sid, "s1");
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        fake.live.store(false, Ordering::SeqCst);
        let plan = gateway
            .plan_resume_dead_session(&sid)
            .unwrap()
            .expect("dead child needs a resume plan");
        assert_eq!(plan.session_id, "s1");
        assert_eq!(plan.project, "alpha");
        assert_eq!(plan.role, "reviewer");
        assert!(!plan.generation.is_empty());

        let thread = Gateway::spawn_for_resume_plan(&plan).await.unwrap();
        assert_eq!(fake.starts.load(Ordering::SeqCst), 2);
        assert!(
            fake.live.load(Ordering::SeqCst),
            "start_thread revived live"
        );

        gateway
            .apply_resume_dead_session(plan, thread)
            .await
            .unwrap();
        assert_eq!(gateway.session_views().len(), 1);
        assert_eq!(gateway.session_views()[0].sid, "s1");
        // Apply path does not close (the dead child was already gone).
        assert_eq!(fake.closes.load(Ordering::SeqCst), 0);
    }

    /// v0.9 T2 — concurrent `stop_session` during the lock-free resume spawn
    /// window must not deadlock and must not leave a zombie session in the map.
    /// Prefer stop-wins: apply sees generation mismatch / missing session and
    /// discards the freshly spawned thread via `close_thread`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_stop_during_resume_spawn_does_not_deadlock_or_leave_zombie() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(
            FakeAdapter::new(AgentVendor::Claude)
                .with_start_delay(std::time::Duration::from_millis(200)),
        );
        let mut gw = Gateway::new(fake.clone(), "alpha", tmp.path());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(tx);

        let sid = gw
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        assert_eq!(sid, "s1");
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        // Child dies; resume will cold-start with the 200ms delay.
        fake.live.store(false, Ordering::SeqCst);
        let gateway = Arc::new(tokio::sync::Mutex::new(gw));

        let gw_a = Arc::clone(&gateway);
        let sid_a = sid.clone();
        let resume_task =
            tokio::spawn(async move { Gateway::resume_dead_session_shared(gw_a, &sid_a).await });

        // Wait until the resume spawn is mid-flight (2nd start_thread entered).
        for _ in 0..100 {
            if fake.starts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            fake.starts.load(Ordering::SeqCst) >= 2,
            "resume spawn must be in flight before concurrent stop"
        );

        // Concurrent stop while resume holds NO gateway lock across spawn.
        let stop_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            gateway.lock().await.stop_session(&sid).await
        })
        .await
        .expect("stop_session must not hang behind resume spawn");
        assert!(
            stop_result.is_ok(),
            "stop_session must succeed: {stop_result:?}"
        );

        let resume_join = tokio::time::timeout(std::time::Duration::from_secs(5), resume_task)
            .await
            .expect("resume task must not hang");
        let resume_result = resume_join.expect("resume task must not panic");
        // Stop removed the session → apply discards the fresh thread.
        assert!(
            resume_result.is_err(),
            "resume apply must fail after concurrent stop (no zombie insert): {resume_result:?}"
        );

        // Preferred race outcome: session gone, no zombie in the map.
        let views = gateway.lock().await.session_views();
        assert!(
            views.is_empty(),
            "sessions map must not keep a zombie after stop-during-resume: {views:?}"
        );
        // stop closes the old thread; apply closes the discarded resume thread.
        assert!(
            fake.closes.load(Ordering::SeqCst) >= 2,
            "expected stop + discard closes, got {}",
            fake.closes.load(Ordering::SeqCst)
        );
    }

    /// v0.9 T2 review fix — mixed flavors must not ABBA-deadlock: a shared
    /// resume holds the per-sid claim and needs the gateway lock for its
    /// plan/apply, while a caller already holding the gateway lock runs the
    /// `&mut self` resume (which takes NO claim — the fix). The apply-phase
    /// generation check settles the race; both calls return within the
    /// timeout and the child ends live with no zombie.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mixed_flavor_resume_does_not_deadlock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(
            FakeAdapter::new(AgentVendor::Claude)
                .with_start_delay(std::time::Duration::from_millis(150)),
        );
        let mut gw = Gateway::new(fake.clone(), "alpha", tmp.path());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(tx);
        let sid = gw
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
        fake.live.store(false, Ordering::SeqCst);
        let gateway = Arc::new(tokio::sync::Mutex::new(gw));

        // Task A: shared flavor — owns the per-sid claim, spawns slowly.
        let gw_a = Arc::clone(&gateway);
        let sid_a = sid.clone();
        let shared_task =
            tokio::spawn(async move { Gateway::resume_dead_session_shared(gw_a, &sid_a).await });
        for _ in 0..100 {
            if fake.starts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            fake.starts.load(Ordering::SeqCst) >= 2,
            "shared resume must be mid-spawn before the &mut-self resume runs"
        );

        // Task B: `&mut self` flavor under the gateway lock while A holds the
        // claim. Pre-fix this awaited A's claim while blocking A's apply →
        // ABBA deadlock; post-fix it must return within the timeout.
        let mutself = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            gateway.lock().await.resume_dead_session(&sid).await
        })
        .await
        .expect("&mut-self resume must not deadlock behind the shared claim");
        let shared = tokio::time::timeout(std::time::Duration::from_secs(5), shared_task)
            .await
            .expect("shared resume must not hang")
            .expect("shared resume must not panic");
        assert!(
            mutself.is_ok() || shared.is_ok(),
            "one flavor must win the generation race: mutself={mutself:?} shared={shared:?}"
        );
        let views = gateway.lock().await.session_views();
        assert_eq!(views.len(), 1, "exactly one session, no zombie: {views:?}");
        assert!(
            fake.live.load(Ordering::SeqCst),
            "child must end live after the mixed resumes"
        );
    }

    /// v0.8.24 Track D — FakeRemoteHostProxy simulates a colocated satellite:
    /// online host → create session stamped with host id → one Q&A turn →
    /// dead-child resume keeps the same sid/host. Production
    /// HttpRemoteHostProxy fails closed instead (see remote_host tests).
    #[tokio::test]
    async fn remote_fake_host_one_turn_resume_and_host_stamp() {
        use ccteam_core::host_registry::{now_unix, HostRecord, HostRegistry};
        use ccteam_core::CcteamPaths;

        let tmp = tempfile::TempDir::new().unwrap();
        let ccteam_root = tmp.path().join(".ccteam");
        let project_dir = tmp.path().join("projects/alpha");
        std::fs::create_dir_all(&ccteam_root).unwrap();
        std::fs::create_dir_all(ccteam_root.join("hosts")).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let paths = CcteamPaths {
            root: ccteam_root.clone(),
            projects_root: tmp.path().join("projects"),
        };

        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat-lab".into(),
            hostname: "sat-lab".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.8.24".into(),
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix(),
            agents: vec![],
            // v0.9.0 W3 (G9) — the satellite must report `alpha` registered
            // (last heartbeat) before a remote spawn there is allowed.
            projects: vec![ccteam_core::HostProjectReport {
                slug: "wire-alpha".into(),
                path: project_dir.display().to_string(),
            }],
            joined_at: chrono::Utc::now().to_rfc3339(),
        });
        reg.save(&ccteam_core::host_registry::registry_path_in(&ccteam_root))
            .unwrap();
        ccteam_core::config::upsert_project(
            &ccteam_root,
            ccteam_core::ProjectEntry {
                slug: "alpha".into(),
                path: project_dir.clone(),
                host: "sat-lab".into(),
                remote_slug: Some("wire-alpha".into()),
                remote_path: Some(project_dir.clone()),
                team: "dev".into(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", &project_dir);
        gateway.enable_project_creation(paths);
        let proxy = Arc::new(crate::remote_host::FakeRemoteHostProxy::default());
        gateway.set_remote_host_proxy(proxy.clone());

        let outcome = gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
            )
            .await
            .expect("online fake remote create");
        assert_eq!(outcome.sid, "s1");
        assert_eq!(
            proxy.last_host.lock().unwrap().as_deref(),
            Some("sat-lab"),
            "proxy must be consulted before spawn"
        );

        let views = gateway.session_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].host, "sat-lab", "session view stamps host");
        let meta = read_session_meta(&project_dir, "s1").unwrap();
        assert_eq!(meta.host, "sat-lab", "meta.json stamps host");
        assert!(
            !project_dir.join(".claude").exists(),
            "remote data home must not become a vendor working tree"
        );
        assert_eq!(
            meta.trigger.as_deref(),
            Some("web"),
            "web create → trigger=web"
        );

        // One Q&A round.
        let turn = gateway
            .submit_to_sid("s1", "hello-remote".into())
            .await
            .unwrap();
        assert!(turn.starts_with("turn-"), "got {turn}");
        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[("alpha-reviewer-s1".to_string(), "hello-remote".to_string())]
        );

        // Stop+resume: dead child + next turn keeps sid and host.
        fake.live.store(false, Ordering::SeqCst);
        gateway
            .submit_to_sid("s1", "after-resume".into())
            .await
            .unwrap();
        assert_eq!(fake.starts.load(Ordering::SeqCst), 2, "resume restarted");
        let views = gateway.session_views();
        assert_eq!(views[0].sid, "s1");
        assert_eq!(views[0].host, "sat-lab", "host attribution survives resume");
        assert_eq!(
            fake.submissions.lock().await.last().map(|p| p.1.as_str()),
            Some("after-resume")
        );

        // A project rebind never moves an existing session. Rebuild/resume
        // fails readable and requires a fresh sid on the new binding.
        let mut config = ccteam_core::config::load(&ccteam_root).unwrap();
        let entry = config
            .projects
            .iter_mut()
            .find(|entry| entry.slug == "alpha")
            .unwrap();
        entry.host = ccteam_core::LOCAL_HOST.to_string();
        entry.remote_slug = None;
        entry.remote_path = None;
        ccteam_core::config::save(&ccteam_root, &config).unwrap();
        fake.live.store(false, Ordering::SeqCst);
        let err = gateway
            .plan_resume_dead_session("s1")
            .err()
            .expect("rebind must reject resume planning");
        assert!(
            err.to_string().contains("project host binding changed"),
            "got: {err}"
        );
    }

    /// v0.9.0 W3 (G10, safety-critical) — a session whose host has gone
    /// OFFLINE since it was created must NEVER be silently respawned on
    /// the main daemon: every rebuild path re-gates the host right before
    /// its spawn and fails readable instead. Covers BOTH funnels: the
    /// cold-start trio (`plan_session_rebuild` → `spawn_for_plan`, shared
    /// by `rebuild_session_from_meta` / `resume_restored_sessions[_shared]`)
    /// and dead-child resume (`plan_resume_dead_session` →
    /// `spawn_for_resume_plan`). In both cases the local `FakeAdapter` must
    /// NEVER be invoked again once offline (`starts` stays pinned at 1).
    #[tokio::test]
    async fn remote_session_never_respawns_locally_when_host_offline() {
        use ccteam_core::host_registry::{now_unix, HostRecord, HostRegistry};
        use ccteam_core::CcteamPaths;

        let tmp = tempfile::TempDir::new().unwrap();
        let ccteam_root = tmp.path().join(".ccteam");
        let project_dir = tmp.path().join("projects/alpha");
        std::fs::create_dir_all(&ccteam_root).unwrap();
        std::fs::create_dir_all(ccteam_root.join("hosts")).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let paths = CcteamPaths {
            root: ccteam_root.clone(),
            projects_root: tmp.path().join("projects"),
        };
        let reg_path = ccteam_core::host_registry::registry_path_in(&ccteam_root);

        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat-lab".into(),
            hostname: "sat-lab".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.9.0".into(),
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix(),
            agents: vec![],
            projects: vec![ccteam_core::HostProjectReport {
                slug: "wire-alpha".into(),
                path: project_dir.display().to_string(),
            }],
            joined_at: chrono::Utc::now().to_rfc3339(),
        });
        reg.save(&reg_path).unwrap();
        ccteam_core::config::upsert_project(
            &ccteam_root,
            ccteam_core::ProjectEntry {
                slug: "alpha".into(),
                path: project_dir.clone(),
                host: "sat-lab".into(),
                remote_slug: Some("wire-alpha".into()),
                remote_path: Some(project_dir.clone()),
                team: "dev".into(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", &project_dir);
        gateway.enable_project_creation(paths);
        let proxy = Arc::new(crate::remote_host::FakeRemoteHostProxy::default());
        gateway.set_remote_host_proxy(proxy.clone());

        let outcome = gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
            )
            .await
            .expect("online create");
        let sid = outcome.sid.clone();
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        // Satellite goes offline (crashed / lost network / never sent
        // another heartbeat within the TTL).
        let mut reg2 = HostRegistry::load(&reg_path).unwrap();
        reg2.get_mut("sat-lab").unwrap().last_heartbeat_unix = now_unix().saturating_sub(10_000);
        reg2.save(&reg_path).unwrap();

        // Path A: cold-start rebuild.
        let (slug, cwd, meta) = gateway.find_meta_for_sid(&sid).unwrap();
        assert_eq!(meta.host, "sat-lab");
        let reply_to = ChatKey::new("web", "web-api", "web-api");
        let plan = gateway
            .plan_session_rebuild(&slug, cwd.clone(), &meta, &reply_to)
            .unwrap();
        let err = Gateway::spawn_for_plan(&plan).await.unwrap_err();
        assert!(err.to_string().contains("offline"), "got: {err}");
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "cold-start rebuild must NOT spawn the adapter locally when offline"
        );

        // Path B: dead-child resume.
        fake.live.store(false, Ordering::SeqCst);
        let resume_plan = gateway
            .plan_resume_dead_session(&sid)
            .unwrap()
            .expect("session is dead -> plan expected");
        let err2 = Gateway::spawn_for_resume_plan(&resume_plan)
            .await
            .unwrap_err();
        assert!(err2.to_string().contains("offline"), "got: {err2}");
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "dead-child resume must NOT spawn the adapter locally when offline"
        );
    }

    /// v0.8.24 F5 — web API spawn writes `trigger=web` on meta.json.
    #[tokio::test]
    async fn web_create_session_meta_records_trigger_web() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let meta = read_session_meta(tmp.path(), &sid).unwrap();
        assert_eq!(meta.trigger.as_deref(), Some("web"));
    }

    /// v0.8.24 F5 — when `thread_is_live` is false, `submit_resolved` must
    /// **enqueue** the user text (production path calls `enqueue_pending_turn`)
    /// before resume, then drain FIFO after the child is live. Pre-seeded
    /// pending turns + the just-enqueued one both land, in order.
    #[tokio::test]
    async fn submit_while_not_live_enqueues_then_drains_fifo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());

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
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        // Cold-start queue already has one turn (e.g. concurrent web POST
        // while the child was still spawning).
        crate::pending_turns::enqueue_pending_turn(
            tmp.path(),
            &sid,
            "queued-first",
            Some("web".into()),
            false,
        )
        .unwrap();
        assert_eq!(
            crate::pending_turns::pending_turn_count(tmp.path(), &sid),
            1
        );

        // Child dies; next user turn must enqueue (not drop) then revive + drain.
        fake.live.store(false, Ordering::SeqCst);
        let turn = gateway
            .submit_to_sid(&sid, "queued-second".into())
            .await
            .unwrap();
        assert!(
            turn.starts_with("turn-alpha-reviewer-s1"),
            "drain surfaces the real turn id, got {turn}"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            2,
            "resume re-started the dead child"
        );
        assert!(fake.live.load(Ordering::SeqCst));
        assert_eq!(
            crate::pending_turns::pending_turn_count(tmp.path(), &sid),
            0,
            "queue drained after live"
        );
        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[
                ("alpha-reviewer-s1".to_string(), "queued-first".to_string()),
                ("alpha-reviewer-s1".to_string(), "queued-second".to_string()),
            ],
            "FIFO: pre-queued then just-enqueued"
        );
    }

    /// Dead-child recovery on the TURN path (reactive): when a session's child
    /// has exited out from under the held handle, `submit_turn` returns the
    /// recoverable `HarnessError::ThreadDied`, and the gateway transparently
    /// resumes-by-session-id (re-`start_thread`, SAME sid) and RETRIES once —
    /// the turn lands instead of failing "stream-json writer closed". The turn
    /// was never delivered on a ThreadDied, so the single retry can't
    /// double-submit. Killed twice to prove it's repeatable, not a one-shot, and
    /// that a LIVE session never re-spawns (3 starts for 3 deaths — no spurious
    /// resume on the healthy first turn).
    ///
    /// With v0.8.24 F5, a known-dead child (`thread_is_live` false) takes the
    /// enqueue→resume→drain path (same end state: turn lands, sid stable).
    #[tokio::test]
    async fn gateway_resumes_dead_session_on_next_turn() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

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

        // First turn lands on the freshly-spawned child (one start_thread).
        gateway.submit_to_sid("s1", "first".into()).await.unwrap();
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        // The child dies out from under the handle.
        fake.live.store(false, Ordering::SeqCst);

        // The next turn self-heals — resume (a 2nd start_thread) + submit, SAME
        // sid, single session (NOT a fresh `/new`).
        let turn = gateway.submit_to_sid("s1", "second".into()).await.unwrap();
        assert!(
            turn.starts_with("turn-alpha-reviewer-s1"),
            "resume keeps the sid stable: {turn}"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            2,
            "dead child resumed in place (start_thread called again)"
        );
        assert!(
            fake.live.load(Ordering::SeqCst),
            "the resume revived the child"
        );
        assert_eq!(gateway.session_views().len(), 1, "still one session");
        assert_eq!(gateway.session_views()[0].sid, "s1");

        // Repeatable: kill it again, drive another turn — a 3rd start_thread.
        fake.live.store(false, Ordering::SeqCst);
        gateway.submit_to_sid("s1", "third".into()).await.unwrap();
        assert_eq!(fake.starts.load(Ordering::SeqCst), 3, "resumed again");

        // Every turn reached the adapter against the stable identity (and a
        // LIVE session never re-spawns — exactly 3 starts for 3 deaths, no
        // spurious resumes on the healthy first turn).
        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[
                ("alpha-reviewer-s1".to_string(), "first".to_string()),
                ("alpha-reviewer-s1".to_string(), "second".to_string()),
                ("alpha-reviewer-s1".to_string(), "third".to_string()),
            ]
        );
    }

    /// Dead-child recovery on the DIRECTIVE path (proactive): a `/command` is
    /// NOT blindly retried (it may have side effects), so the gateway
    /// PROBES `thread_is_live` and resumes-by-sid BEFORE dispatching. A `/model`
    /// to a dead session therefore re-spawns (a 2nd start_thread) and the
    /// directive reaches the live thread, rather than failing on the corpse.
    #[tokio::test]
    async fn gateway_resumes_dead_session_before_directive() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

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
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);

        // The child dies; the next message is a directive, not a turn.
        fake.live.store(false, Ordering::SeqCst);
        let receipt = gateway
            .submit_to_sid("s1", "/model opus-x".into())
            .await
            .unwrap();
        assert_eq!(receipt, "directive:s1");

        // The probe resumed the dead child (2nd start_thread) and the directive
        // reached the resumed thread (recorded against the stable identity).
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            2,
            "directive probe resumed the dead child before dispatch"
        );
        assert!(fake.live.load(Ordering::SeqCst), "resume revived the child");
        let directives = fake.directives.lock().await;
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].0, "alpha-reviewer-s1");
        assert_eq!(directives[0].1.name, "model");
    }

    /// `/interrupt` stops the running turn WITHOUT destroying the session — the
    /// contrast with `/stop`. It (1) calls the adapter's `interrupt_turn`, (2)
    /// LEAVES the session in the gateway map (so a follow-up `/model` etc. still
    /// drives the same context — the whole point), and (3) enforces the
    /// own-session ACL (a foreign chat can't interrupt). Bare `/interrupt`
    /// targets the chat's CURRENT session.
    #[tokio::test]
    async fn gateway_interrupt_stops_turn_but_keeps_session_and_is_own_only() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        // chat-1 creates a session (its current).
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // Explicit `/interrupt s1` → receipt names the session, the adapter's
        // interrupt was called, and the session STILL EXISTS (contrast /stop).
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "/interrupt s1")
            .await
            .unwrap();
        assert_eq!(reply.len(), 1);
        assert!(
            reply[0].contains("已中断 session s1") && reply[0].contains("会话保留"),
            "interrupt receipt: {:?}",
            reply[0]
        );
        assert_eq!(
            fake.interrupts.lock().await.as_slice(),
            &["alpha-reviewer-s1".to_string()],
            "the adapter's interrupt_turn was invoked once"
        );
        // The session is NOT gone (this is the /stop contrast): it's still
        // listed + addressable, so /model can follow on the same context.
        assert_eq!(gateway.session_views().len(), 1);
        let used = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["using session s1\n↓ 查看状态 → /status"]);

        // Bare `/interrupt` targets the CURRENT session (s1) — non-destructive,
        // so no explicit sid is required (unlike /stop).
        let bare = gateway
            .handle_text("mock", "chat-1", "alice", "/interrupt")
            .await
            .unwrap();
        assert!(bare[0].contains("已中断 session s1"), "bare: {:?}", bare[0]);
        assert_eq!(
            fake.interrupts.lock().await.len(),
            2,
            "bare interrupt also drove interrupt_turn"
        );

        // Own-only ACL: a DIFFERENT chat cannot interrupt chat-1's session —
        // it reads as unknown (no existence leak), and interrupt_turn is NOT
        // called for it (still 2 total).
        let foreign = gateway
            .handle_text("mock", "chat-2", "bob", "/interrupt s1")
            .await
            .unwrap();
        assert_eq!(
            foreign,
            vec!["unknown session for this chat: s1\n↓ 查看状态 → /status"]
        );
        assert_eq!(
            fake.interrupts.lock().await.len(),
            2,
            "a foreign chat's interrupt must NOT reach the adapter"
        );

        // The web/REST core (`interrupt_session`, ACL applied by the route) also
        // keeps the session: an unknown sid errors so the edge can 404.
        gateway.interrupt_session("s1").await.unwrap();
        assert_eq!(
            gateway.session_views().len(),
            1,
            "still live after core interrupt"
        );
        assert!(gateway.interrupt_session("s99").await.is_err());
    }

    /// Web-path parity: a `/command` submitted via `submit_to_sid` (the network
    /// API leg) is interpreted as a session DIRECTIVE — exactly like the IM
    /// `submit_to_current` path — not shipped to the agent as literal text. Its
    /// synchronous receipt is delivered over the session's SSE stream as an
    /// Answer keyed on `sid`. Regression: `/model …` from the web console used
    /// to reach claude verbatim → "/model isn't available in this environment".
    #[tokio::test]
    async fn submit_to_sid_routes_slash_command_as_directive() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-directive");
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();

        // Subscribe before submitting so the broadcast tee catches the receipt.
        let mut rx = gateway.subscribe_events();

        let receipt_id = gateway
            .submit_to_sid(&sid, "/model opus-x".into())
            .await
            .unwrap();
        // A directive has no turn id — a synthetic marker, never "turn-…".
        assert_eq!(receipt_id, format!("directive:{sid}"));

        // Interpreted as a DIRECTIVE (recorded), NOT a literal turn submission.
        let directives = fake.directives.lock().await;
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].1.name, "model");
        assert_eq!(directives[0].1.args, "opus-x");
        drop(directives);
        assert!(
            fake.submissions.lock().await.is_empty(),
            "a /command must not be submitted to the agent as user text"
        );

        // The receipt streams back over the session's SSE (sid-keyed Answer).
        let ev = rx.try_recv().expect("directive receipt emitted to SSE");
        assert_eq!(ev.sid.as_deref(), Some(sid.as_str()));
        assert!(matches!(ev.kind, GatewayEventKind::Answer));
        assert!(
            ev.content.contains("directive: model"),
            "got: {}",
            ev.content
        );
    }

    /// Three-surface parity: a GATEWAY control command (`/status`) submitted via
    /// the web interactive path (`submit_web_sid`) is handled by the gateway
    /// control face — NOT shipped to the vendor as literal text (which replied
    /// "/status isn't available in this environment"). Its render is delivered
    /// over the session's SSE stream as a sid-keyed Answer, like a directive.
    #[tokio::test]
    async fn submit_web_sid_handles_gateway_status_command() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-webstatus");
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();

        let mut rx = gateway.subscribe_events();

        // Admin web console: `/status` is a gateway control command.
        let receipt = gateway
            .submit_web_sid(&sid, "/status".into(), true)
            .await
            .unwrap();
        assert_eq!(receipt, format!("command:{sid}"));

        // Never reached the vendor as a turn or a directive.
        assert!(
            fake.submissions.lock().await.is_empty(),
            "a gateway /status must not be submitted to the agent as user text"
        );
        assert!(
            fake.directives.lock().await.is_empty(),
            "/status is a gateway command, not a vendor directive"
        );

        // Rendered back over the session's SSE (sid-keyed Answer).
        let ev = rx.try_recv().expect("status render emitted to SSE");
        assert_eq!(ev.sid.as_deref(), Some(sid.as_str()));
        assert!(matches!(ev.kind, GatewayEventKind::Answer));
    }

    /// Non-gateway text and vendor directives still route to the vendor through
    /// the web path — `submit_web_sid` only intercepts GATEWAY control commands.
    #[tokio::test]
    async fn submit_web_sid_passes_turns_and_vendor_directives_through() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-webthru");
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();

        // Plain text → a real turn to the vendor.
        gateway
            .submit_web_sid(&sid, "hello".into(), true)
            .await
            .unwrap();
        assert_eq!(
            fake.submissions.lock().await.len(),
            1,
            "plain text is a turn to the agent"
        );

        // Vendor directive → recorded as a directive (not a gateway command).
        gateway
            .submit_web_sid(&sid, "/model opus-x".into(), true)
            .await
            .unwrap();
        let directives = fake.directives.lock().await;
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].1.name, "model");

        // A non-admin caller never gets the gateway control face: `/status`
        // falls through to `submit_to_sid` (today's behaviour; fleet renders
        // stay admin-only), so the receipt is NOT a gateway `command:` marker.
        drop(directives);
        let receipt = gateway
            .submit_web_sid(&sid, "/status".into(), false)
            .await
            .unwrap();
        assert_ne!(
            receipt,
            format!("command:{sid}"),
            "non-admin /status must NOT be handled by the gateway control face"
        );
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

    #[tokio::test]
    async fn curated_mcp_json_written_on_claude_stream_json_spawn() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let tmp = tempfile::tempdir().unwrap();
        let mut gw = Gateway::new(fake.clone(), "demo", tmp.path());
        let sid = gw
            .create_session_api(
                "demo".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let mcp_path =
            ccteam_harness::execution::mcp_config::session_mcp_config_path(tmp.path(), &sid);
        assert!(
            mcp_path.exists(),
            "expected curated mcp.json at {}",
            mcp_path.display()
        );
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        assert!(body["mcpServers"]["ccteam"].is_object());
        // HTTP form by default
        assert_eq!(body["mcpServers"]["ccteam"]["type"], "http");
        let auth = body["mcpServers"]["ccteam"]["headers"]["Authorization"]
            .as_str()
            .unwrap_or("");
        assert!(
            auth.starts_with(&format!("Bearer ccteam-sid:{sid}:")),
            "auth={auth}"
        );
    }

    /// v0.9.0 W1 (F1) — the gate authenticates the `(sid, secret)` PRINCIPAL and
    /// returns the resolved [`CallerCtx`] (server-side slug + role). Right
    /// principal → Some; wrong/empty secret or an unknown sid → None
    /// (fail-closed). Role is NOT part of authorization (audit label only).
    #[tokio::test]
    async fn verify_session_principal_authenticates_by_sid_and_secret() {
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

        // Correct (sid, secret) principal → CallerCtx with server-side slug+role.
        let ctx = gateway
            .verify_session_principal(sid.as_str(), &secret)
            .expect("principal ok");
        assert_eq!(ctx.sid.as_str(), sid.as_str());
        assert_eq!(ctx.slug, "alpha");
        assert_eq!(ctx.role, "cto");
        // Wrong secret → None.
        assert!(gateway
            .verify_session_principal(sid.as_str(), "deadbeefdeadbeefdeadbeefdeadbeef")
            .is_none());
        // Empty secret → None (fail-closed; never fall-open).
        assert!(gateway.verify_session_principal(sid.as_str(), "").is_none());
        // Unknown sid (even with a real secret) → None.
        assert!(gateway.verify_session_principal("s999", &secret).is_none());
    }

    /// v0.9.0 W1 (F1) — two sessions can run the SAME role; each mints its own
    /// per-session secret. The `(sid, secret)` principal STILL isolates: each
    /// secret authenticates ONLY under its OWN sid (resolving to that session's
    /// CallerCtx); the same secret presented under the WRONG sid is rejected.
    #[tokio::test]
    async fn verify_session_principal_isolates_two_same_role_sessions() {
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

        // Each secret authenticates ONLY under its own sid.
        assert_eq!(
            gateway
                .verify_session_principal(sid1.as_str(), &secret1)
                .unwrap()
                .sid
                .as_str(),
            sid1.as_str()
        );
        assert_eq!(
            gateway
                .verify_session_principal(sid2.as_str(), &secret2)
                .unwrap()
                .sid
                .as_str(),
            sid2.as_str()
        );
        // A secret presented under the WRONG sid → None (principal is per-sid).
        assert!(gateway
            .verify_session_principal(sid1.as_str(), &secret2)
            .is_none());
        assert!(gateway
            .verify_session_principal(sid2.as_str(), &secret1)
            .is_none());
        // Bogus secret → None even though the sid is live.
        assert!(gateway
            .verify_session_principal(sid1.as_str(), "deadbeefdeadbeefdeadbeefdeadbeef")
            .is_none());
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
            outcome: None,
            error_kind: None,
            error: None,
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

    /// v0.8.19 — the 👀 ack reaction lifecycle. Dispatching an IM turn emits
    /// `Reaction{on:true}` on the inbound `message_id` AND records the pending
    /// msg_id on the session (so the silent time-to-first-token gap is acked);
    /// the detached event pump then emits `Reaction{on:false}` on the turn's
    /// FIRST event AND clears the pending (fires exactly once). Both events
    /// carry the session's `sid` + the IM channel/chat for routing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn im_turn_adds_then_clears_eyes_reaction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir);
        // Wire the sink (production path) so the add-reaction is emitted there
        // synchronously and the pump runs (→ clear-reaction). Capture all events.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        // Create an IM session (telegram), then drive a turn with a real inbound
        // message_id (the ack is keyed on it).
        gateway
            .handle_text("telegram", "chat-7", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_message(
                "telegram",
                "chat-7",
                "alice",
                "tg-555",
                "do a thing",
                &[],
                None,
            )
            .await
            .unwrap();

        // The pending msg_id is recorded the instant the turn is dispatched, and
        // the detached pump TAKEs it on the first event. Collect emitted events
        // (bounded poll) until both the add + clear reactions arrive.
        let mut add: Option<(String, String, bool)> = None;
        let mut clear: Option<(String, String, bool)> = None;
        for _ in 0..200 {
            while let Ok(ev) = rx.try_recv() {
                if let GatewayEventKind::Reaction { message_id, on } = &ev.kind {
                    let tuple = (ev.channel.clone(), message_id.clone(), *on);
                    if *on {
                        add = Some(tuple);
                    } else {
                        clear = Some(tuple);
                    }
                }
            }
            if add.is_some() && clear.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let add = add.expect("dispatching an IM turn must emit Reaction{on:true}");
        assert_eq!(add.0, "telegram", "add reaction routes to the IM channel");
        assert_eq!(add.1, "tg-555", "ack reacts to the inbound message_id");
        assert!(add.2, "add => on:true");

        let clear = clear.expect("the turn's first event must emit Reaction{on:false}");
        assert_eq!(clear.0, "telegram");
        assert_eq!(clear.1, "tg-555", "clear targets the same message_id");
        assert!(!clear.2, "clear => on:false");

        // Pending is cleared after the first event fired the clear (fires once).
        let pending = {
            let s = gateway.sessions.values().next().expect("the session");
            s.pending_reaction.lock().unwrap().clone()
        };
        assert!(
            pending.is_none(),
            "pending_reaction must be taken after the first event"
        );
    }

    /// v0.8.19 — a WEB-driven turn emits NO 👀 reaction (web has its own UI; the
    /// gateway add-arm skips `channel == "web"` and the web leg passes an empty
    /// message_id). Regression guard so the IM-only ack never leaks to web.
    #[tokio::test]
    async fn web_turn_emits_no_reaction() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-web-noreact");
        let mut rx = gateway.subscribe_events();
        // Wire a sink too (emit_user_signal prefers it) so we'd SEE a stray
        // reaction if one were emitted; subscribe_events tees the broadcast.
        let (tx, _sink_rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

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
            .submit_to_sid(&sid, "do a thing".into())
            .await
            .unwrap();

        // Drain the broadcast tee: not a single Reaction event for a web turn.
        let mut saw_reaction = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev.kind, GatewayEventKind::Reaction { .. }) {
                saw_reaction = true;
            }
        }
        assert!(!saw_reaction, "a web turn must NOT emit a 👀 reaction");
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

    /// v0.8.23 review §3.2-5 (item 2a) — a FOCUSED session's IM answer
    /// carries a compact "→ slug/sid (role)" context echo (previously only
    /// the out-of-focus case carried any context at all), so a multi-session
    /// chat always knows which session just spoke.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn turn_answer_carries_context_echo_for_focused_im_session() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-echo-focused");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let mut events = gateway.subscribe_events();
        gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();

        let ev = recv_answer(&mut events).await;
        assert!(
            ev.content.contains("echo: hello"),
            "answer still carries the real reply text: {}",
            ev.content
        );
        assert!(
            ev.content.ends_with("\n\n→ alpha/s1 (reviewer)"),
            "context echo suffix present: {:?}",
            ev.content
        );
    }

    /// v0.8.23 review §3.2-5 (item 2a) — a roleless session's echo omits the
    /// `(role)` parens (own `FakeAdapter`/gateway — the fake's `events()`
    /// shares ONE `Notify` across every identity, so driving two live
    /// sessions' pumps to completion in the SAME test can misdirect a wakeup
    /// to the wrong session's still-parked pump; one live session per test
    /// sidesteps that test-double-only race entirely).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn turn_answer_context_echo_omits_role_when_roleless() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-echo-roleless");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude")
            .await
            .unwrap();
        let mut im_events = gateway.subscribe_events();
        gateway
            .handle_text("mock", "chat-1", "alice", "hi")
            .await
            .unwrap();
        let im_ev = recv_answer(&mut im_events).await;
        assert!(
            im_ev.content.ends_with("\n\n→ alpha/s1"),
            "roleless echo carries no (role) parens: {:?}",
            im_ev.content
        );
    }

    /// v0.8.23 review §3.2-5 (item 2a) — a WEB-owned session's answer gets NO
    /// echo at all (the web console already shows its session context in its
    /// own chrome). Own `FakeAdapter`/gateway, see the sibling roleless test
    /// for why.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn turn_answer_context_echo_skips_web_channel() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-echo-web");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        let mut web_events = gateway.subscribe_events();
        gateway.submit_to_sid(&sid, "ping".into()).await.unwrap();
        let web_ev = recv_answer(&mut web_events).await;
        assert!(
            !web_ev.content.contains('→'),
            "web answers must not carry the IM-only context echo: {:?}",
            web_ev.content
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn internal_web_turn_mirrors_once_to_admin_telegram() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_global_telegram(&paths, vec!["chat-42".into(), "chat-99".into()]);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir);
        gateway.enable_project_creation(paths);
        let mut broadcast = gateway.subscribe_events();
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        // No gateway submit at all: model a harness-internal background wakeup
        // with narration + final text inside one vendor turn. Unknown origin
        // must default to internal, and only the final boundary may mirror.
        let identity = format!("alpha-reviewer-{sid}");
        {
            let mut queued = fake.events.lock().await;
            queued.push_back((
                identity.clone(),
                agent_msg(
                    |item| ThreadEvent::ItemCompleted { item },
                    "background checkpoint",
                ),
            ));
            queued.push_back((
                identity.clone(),
                agent_msg(
                    |item| ThreadEvent::ItemCompleted { item },
                    "background final",
                ),
            ));
            queued.push_back((
                identity,
                ThreadEvent::TurnCompleted {
                    turn_id: "background-turn".into(),
                    usage: Default::default(),
                    model: None,
                },
            ));
        }
        fake.events_notify.notify_one();

        let answers = recv_sink_answers(&mut events, 3).await;
        assert_eq!(answers.len(), 3, "two web answers + one final IM mirror");
        let web: Vec<_> = answers
            .iter()
            .filter(|event| event.channel == "web")
            .collect();
        assert_eq!(web.len(), 2, "web delivery remains byte-for-byte complete");
        assert_eq!(web[0].chat_id, "web-api");
        assert_eq!(web[0].content, "background checkpoint");
        assert_eq!(web[1].content, "background final");
        let mirror = answers
            .iter()
            .find(|event| event.channel == "telegram")
            .unwrap();
        assert_eq!(mirror.chat_id, "chat-42", "first allowlisted chat wins");
        assert_eq!(
            mirror.content,
            "[s1 alpha claude reviewer] background final"
        );
        assert_eq!(mirror.sid.as_deref(), Some("s1"));
        assert!(
            events.try_recv().is_err(),
            "completed turn emits exactly one mirror"
        );
        let web_broadcast = [
            recv_answer(&mut broadcast).await,
            recv_answer(&mut broadcast).await,
        ];
        assert!(web_broadcast.iter().all(|event| event.channel == "web"));
        while let Ok(event) = broadcast.try_recv() {
            assert!(
                !matches!(event.kind, GatewayEventKind::Answer),
                "delivery-only mirror must not duplicate either web SSE ring: {event:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn poisoned_admin_owner_uses_project_owner_for_delivery_and_rebuild() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "alpha", Some("user:ualice"));
        seed_global_telegram(&paths, vec!["admin-chat".into()]);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake, "alpha", project_dir.clone());
        gateway.enable_project_creation(paths);
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        // The write path now stamps the project principal, so poison the
        // PERSISTED metadata by hand — that is the state this test is about:
        // a `meta.json` written before ownership inherited the project (the
        // operator's on-disk fleet), which no migration will ever rewrite.
        let mut meta = read_session_meta(&project_dir, &sid).unwrap();
        assert_eq!(
            meta.owner, "user:ualice",
            "the spawn path stamps the tenant project's principal"
        );
        meta.owner = "user:web-api".to_string();
        write_session_meta(&project_dir, &meta).unwrap();

        let rebuild = gateway
            .plan_session_rebuild("alpha", project_dir, &meta, &web_api_chat())
            .unwrap();
        assert_eq!(
            rebuild.owner.identity(),
            "user:ualice",
            "restart repairs the effective owner from the project SoT"
        );

        gateway
            .submit_to_sid(&sid, "tenant-only result".into())
            .await
            .unwrap();
        let answers = recv_sink_answers(&mut events, 1).await;
        wait_for_turn_idle(&gateway, &sid).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(answers[0].channel, "web");
        assert_eq!(answers[0].chat_id, "ualice");
        assert!(
            events.try_recv().is_err(),
            "poisoned metadata must not mirror tenant output to admin Telegram"
        );
    }

    #[tokio::test]
    async fn operator_project_keeps_delegated_session_caller_pool() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "alpha", Some("telegram:project-owner"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", project_dir.clone());
        gateway.enable_project_creation(paths);

        let sid = gateway
            .create_delegated_session(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                None,
                None,
            )
            .await
            .unwrap()
            .sid;
        let meta = read_session_meta(&project_dir, &sid).unwrap();
        assert_eq!(
            meta.owner, "user:web-api",
            "operator projects preserve the caller's session pool"
        );
    }

    /// THE REPORTED BUG (2026-07-30, real machine) — a tenant's IM `/status`
    /// rendered NO `👥 直接子会话` block and under-counted "本项目其他 N 个会话"
    /// while that tenant's WEB team page listed the very same children. The
    /// children (`s22..s30`) had been spawned by an ambient `session_spawn`, so
    /// they carried the admin pool's owner (`user:web-api`); the LIVE gate read
    /// that stored owner verbatim while its STOPPED twin (`chat_can_access_sid`)
    /// already inherited the project principal — same chat, same project, same
    /// session, visible when stopped and invisible when live. The web surface
    /// gates at PROJECT level only, so the two frontends disagreed, which is
    /// exactly what the §三 red line forbids ("ACL = 一个身份解析器 + 一套归属
    /// 策略,两个前端共用"). project 是归属单元,session 继承 — including live.
    #[tokio::test]
    async fn tenant_status_lists_live_children_owned_by_the_admin_pool() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "alpha", Some("user:ualice"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", &project_dir);
        gateway.enable_project_creation(paths);

        // The tenant drives its own project from its per-tenant bot; `/new`
        // focuses the chat on s1, the parent of the delegation tree.
        let tenant = ChatKey::new("telegram@ualice", "111", "alice");
        gateway
            .handle_text("telegram@ualice", "111", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let child = gateway
            .create_delegated_session(
                "alpha".into(),
                "researcher".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: "s1".into(),
                    depth: 0,
                    role: "reviewer".into(),
                }),
                Some("delegated investigation".into()),
            )
            .await
            .unwrap()
            .sid;
        // Reproduce the operator's LIVE fleet: children whose owner is the
        // caller's admin pool, not the project's tenant. Reachable whenever a
        // session predates its project's ownership (spawned before the
        // inheritance rule, or spawned in a project a tenant claimed later) —
        // the ownership decision belongs to the project, so the gate must
        // resolve it at read time rather than trust the stamped owner.
        gateway.sessions.get_mut(&child).unwrap().owner =
            ChatKey::new("user", "web-api", "web-api");

        assert!(
            gateway.chat_can_access(&tenant, gateway.sessions.get(&child).unwrap()),
            "a LIVE session in a tenant-owned project belongs to that tenant"
        );
        let status = gateway.render_status(&tenant).await;
        assert!(
            status.contains(&format!("👥 直接子会话:\n      · {child} · claude")),
            "the tenant's /status must list its project's delegated children: {status}"
        );
    }

    /// The fallback leg must stay untouched: an operator-owned (or unowned)
    /// project has NO tenant principal to inherit, so the stored owner still
    /// decides and two admin IM chats on one bot stay isolated from each other
    /// ("IM chat 之间互相隔离" — the 档0 rule the v0.8.13 same-project sharing
    /// was reverted for; do not let project inheritance quietly re-add it).
    #[tokio::test]
    async fn operator_and_unowned_projects_keep_per_im_chat_isolation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, _project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "ops", Some("user:web-api")); // operator pool
        seed_owned_project(&paths, "legacy", None); // legacy `ccteam init`
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "ops", paths.projects_root.join("ops"));
        // Both chats are NAMED in the allowlist ⇒ both are the operator.
        gateway.bind_operator_allowlist("telegram", ["111".to_string(), "222".to_string()]);
        gateway.enable_project_creation(paths);

        let rob = ChatKey::new("telegram", "111", "rob");
        let eve = ChatKey::new("telegram", "222", "eve");
        for slug in ["ops", "legacy"] {
            gateway.change_project(&rob, slug).unwrap();
            gateway.change_project(&eve, slug).unwrap();
            gateway
                .handle_text("telegram", "111", "rob", "/new claude reviewer")
                .await
                .unwrap();
            gateway
                .handle_text("telegram", "222", "eve", "/new claude reviewer")
                .await
                .unwrap();
            let focused = |chat: &ChatKey| {
                gateway
                    .current_session
                    .read()
                    .unwrap()
                    .get(chat)
                    .cloned()
                    .expect("/new focuses the chat")
            };
            let (mine, theirs) = (focused(&rob), focused(&eve));
            assert!(
                gateway.chat_can_access(&rob, gateway.sessions.get(&mine).unwrap()),
                "{slug}: a chat sees its own session"
            );
            assert!(
                !gateway.chat_can_access(&rob, gateway.sessions.get(&theirs).unwrap()),
                "{slug}: an admin chat must NOT see another admin chat's session"
            );
            assert!(
                !gateway.chat_can_access(&eve, gateway.sessions.get(&mine).unwrap()),
                "{slug}: isolation is symmetric"
            );
        }
    }

    /// Live and stopped must answer identically for the SAME (chat, session):
    /// the divergence IS the bug (a `/status` that hides what `/rename` reaches).
    #[tokio::test]
    async fn live_and_stopped_session_gates_agree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "alpha", Some("user:ualice"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", &project_dir);
        gateway.enable_project_creation(paths);

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        // Stamp the pre-inheritance owner on BOTH rungs of the ladder (live map
        // + on-disk meta), so the two gates read the same poisoned input.
        gateway.sessions.get_mut(&sid).unwrap().owner = ChatKey::new("user", "web-api", "web-api");
        let mut meta = read_session_meta(&project_dir, &sid).unwrap();
        meta.owner = "user:web-api".to_string();
        write_session_meta(&project_dir, &meta).unwrap();

        let tenant = ChatKey::new("telegram@ualice", "111", "alice");
        let live = gateway.chat_can_access(&tenant, gateway.sessions.get(&sid).unwrap());
        gateway.sessions.remove(&sid); // the session stops; meta.json stays
        let stopped = gateway.chat_can_access_sid(&tenant, &sid);
        assert_eq!(
            live, stopped,
            "one ownership policy: live and stopped cannot disagree"
        );
        assert!(live, "the tenant owns everything in its own project");
    }

    /// The WRITE path decides ownership once, in `plan_new_session`, so every
    /// spawn entry (IM `/new`, REST `POST …/sessions`, MCP `session_spawn`) is
    /// covered without its own patch (§五 总纲 判据②). The persisted
    /// `meta.owner` is what `ccteam-web` serves verbatim, so getting it right
    /// here is what keeps the two frontends telling one story.
    #[tokio::test]
    async fn fresh_session_meta_owner_follows_project_ownership() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, _project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "tenant-proj", Some("user:ualice"));
        seed_owned_project(&paths, "ops", Some("user:web-api"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "ops", paths.projects_root.join("ops"));
        gateway.enable_project_creation(paths.clone());

        let tenant_sid = gateway
            .create_session_api_tuned(
                "tenant-proj".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
            )
            .await
            .unwrap()
            .sid;
        assert_eq!(
            read_session_meta(&paths.projects_root.join("tenant-proj"), &tenant_sid)
                .unwrap()
                .owner,
            "user:ualice",
            "a tenant project stamps its own principal on every fresh session"
        );

        let ops_sid = gateway
            .create_session_api(
                "ops".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        assert_eq!(
            read_session_meta(&paths.projects_root.join("ops"), &ops_sid)
                .unwrap()
                .owner,
            "user:web-api",
            "an operator project still stamps the caller-derived owner"
        );
    }

    /// `owner` is the resource identity; `reply_to` is the concrete delivery
    /// frontend — inheriting the first must never rewrite the second. A tenant
    /// bot creating a session in its own project keeps its TELEGRAM route;
    /// pinning `reply_to` to `web:<tenant>` would silently kill IM delivery.
    #[tokio::test]
    async fn tenant_bot_spawn_keeps_its_im_reply_route() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_owned_project(&paths, "alpha", Some("user:ualice"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", &project_dir);
        gateway.enable_project_creation(paths);

        gateway
            .handle_text("telegram@ualice", "111", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let session = gateway.sessions.get("s1").expect("session s1");
        assert_eq!(session.owner.identity(), "user:ualice");
        let reply_to = session.reply_to.lock().unwrap().clone();
        assert_eq!(reply_to.channel, "telegram@ualice", "IM delivery survives");
        assert_eq!(reply_to.chat_id, "111");
        assert_eq!(
            read_session_meta(&project_dir, "s1").unwrap().owner,
            "user:ualice"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn internal_delegated_child_web_turn_does_not_mirror_to_admin_telegram() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_global_telegram(&paths, vec!["chat-42".into()]);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir);
        gateway.enable_project_creation(paths);
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let parent = gateway
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let sid = gateway
            .create_delegated_session(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: parent,
                    depth: 0,
                    role: String::new(),
                }),
                None,
            )
            .await
            .unwrap()
            .sid;
        gateway
            .submit_to_sid(&sid, "delegated task done".into())
            .await
            .unwrap();
        fake.events_notify.notify_waiters();

        let answers = recv_sink_answers(&mut events, 1).await;
        wait_for_turn_idle(&gateway, &sid).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(answers[0].channel, "web");
        assert!(
            events.try_recv().is_err(),
            "a delegated child's final answer must reach only its parent notification path"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn user_web_turn_does_not_mirror_to_admin_telegram() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        seed_global_telegram(&paths, vec!["chat-42".into()]);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake, "alpha", project_dir);
        gateway.enable_project_creation(paths);
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        gateway
            .submit_web_sid(&sid, "human typed this".into(), true)
            .await
            .unwrap();

        let answers = recv_sink_answers(&mut events, 1).await;
        wait_for_turn_idle(&gateway, &sid).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(answers[0].channel, "web");
        assert!(
            events.try_recv().is_err(),
            "a human web turn must not ping Telegram"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn internal_web_turn_without_global_telegram_is_silent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake, "alpha", project_dir);
        gateway.enable_project_creation(paths);
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        gateway
            .submit_to_sid(&sid, "background done".into())
            .await
            .unwrap();

        let answers = recv_sink_answers(&mut events, 1).await;
        wait_for_turn_idle(&gateway, &sid).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(answers[0].channel, "web");
        assert!(
            events.try_recv().is_err(),
            "missing creds are a silent skip"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn internal_tenant_web_turn_mirrors_to_its_linked_im() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        let mut tenants = ccteam_core::tenants::TenantRegistry::default();
        let alice = tenants.add("alice");
        tenants.link_chat(&alice.id, "telegram:alice-chat");
        tenants.set_telegram(
            &alice.id,
            Some(ccteam_core::tenants::TenantTelegram {
                bot_token: "456:alice".into(),
                allowed_chat_ids: Vec::new(),
            }),
        );
        tenants.save(&paths.users_dir()).unwrap();

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake, "alpha", project_dir);
        gateway.enable_project_creation(paths);
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let sid = gateway
            .create_session_api_tuned(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                alice.id.clone(),
                SpawnTuning::default(),
            )
            .await
            .unwrap()
            .sid;
        gateway
            .submit_to_sid(&sid, "tenant background done".into())
            .await
            .unwrap();

        let answers = recv_sink_answers(&mut events, 2).await;
        wait_for_turn_idle(&gateway, &sid).await;
        let mirror = answers
            .iter()
            .find(|event| event.channel == format!("telegram@{}", alice.id))
            .expect("tenant mirror uses its own bot channel");
        assert_eq!(mirror.chat_id, "alice-chat");
        assert_eq!(
            mirror.content,
            "[s1 alpha claude reviewer] alpha-reviewer-s1 echo: tenant background done"
        );
        assert!(
            events.try_recv().is_err(),
            "tenant receives exactly one mirror"
        );
    }

    /// v0.9 T5 — completed turn appends one `kind:turn` row to the project's
    /// experience.jsonl (derived index). Captures sid/turn_id/vendor + spawn
    /// fingerprints; stream-json fake also carries usage/model so cost can
    /// price when the model is known.
    #[tokio::test]
    async fn pump_appends_experience_turn_record() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        // Seed a role file so role_sha is non-None at spawn.
        let agents = project_dir.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("reviewer.md"), b"you are reviewer").unwrap();
        let skill = project_dir
            .join(".claude")
            .join("skills")
            .join("ci-watcher");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), b"watch ci").unwrap();

        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude).with_turn_boundary());
        let mut gateway = Gateway::new(fake, "alpha", project_dir.clone());
        gateway.enable_project_creation(paths);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);

        let created = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap();
        let sid = created.sid.clone();
        // Fingerprints landed on meta at spawn.
        let meta = read_session_meta(&project_dir, &sid).unwrap();
        assert!(
            meta.role_sha.is_some(),
            "spawn must snapshot role_sha: {:?}",
            meta.role_sha
        );
        assert!(
            meta.skills_sha
                .as_ref()
                .is_some_and(|m| m.contains_key("ci-watcher")),
            "spawn must snapshot skills_sha: {:?}",
            meta.skills_sha
        );

        gateway
            .submit_to_sid(&sid, "do a thing".into())
            .await
            .unwrap();

        let exp_path = ccteam_harness::execution::experience::experience_jsonl_path(&project_dir);
        let mut found = None;
        for _ in 0..100 {
            if let Ok(recs) =
                ccteam_harness::execution::experience::read_all_experience(&project_dir)
            {
                if let Some(r) = recs.into_iter().find(|r| {
                    matches!(
                        r,
                        ccteam_harness::execution::experience::ExperienceRecord::Turn(t)
                            if t.sid == sid
                    )
                }) {
                    found = Some(r);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let rec = found.unwrap_or_else(|| {
            panic!(
                "pump must append kind:turn experience to {}",
                exp_path.display()
            )
        });
        match rec {
            ccteam_harness::execution::experience::ExperienceRecord::Turn(t) => {
                assert_eq!(t.sid, sid);
                assert!(!t.turn_id.is_empty());
                assert_eq!(t.vendor, "claude");
                assert_eq!(t.role, "reviewer");
                assert_eq!(t.role_sha, meta.role_sha);
                assert_eq!(t.skills_sha, meta.skills_sha);
                // FakeAdapter emits usage + claude-sonnet-4-6 → priceable.
                assert!(t.usage.is_some(), "stream-json TurnCompleted carries usage");
                assert_eq!(t.model.as_deref(), Some("claude-sonnet-4-6"));
                assert!(
                    t.cost_usd.is_some(),
                    "known model must price (got None); usage={:?}",
                    t.usage
                );
            }
            other => panic!("expected turn record, got {other:?}"),
        }
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
    /// behind every stale restored session. v0.8.21 Wave-2 — restore now
    /// COLD-STARTS (`start_thread`, not `resume_thread`); the `_shared` path
    /// builds the plan under the lock, then spawns OUTSIDE it.
    #[tokio::test]
    async fn restored_session_resume_does_not_hold_gateway_lock_while_adapter_waits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().join("alpha");
        std::fs::create_dir_all(&project_dir).unwrap();

        // Seed: create s1 so its meta.json + routing.json (live_sids=[s1]) persist.
        let seed = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(seed, "alpha", project_dir.clone());
        gateway.enable_persistence(tmp.path()).unwrap();
        let sid = gateway
            .create_session_api_proto(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
            )
            .await
            .unwrap();
        assert_eq!(sid, "s1");
        drop(gateway);

        // Restart with a SLOW cold start (Wave-2 rebuilds via start_thread).
        let slow = Arc::new(
            FakeAdapter::new(AgentVendor::Claude)
                .with_start_delay(std::time::Duration::from_millis(250)),
        );
        let mut restored = Gateway::new(slow.clone(), "alpha", project_dir);
        restored.enable_persistence(tmp.path()).unwrap();
        let gateway = Arc::new(tokio::sync::Mutex::new(restored));

        let resume_task = tokio::spawn(Gateway::resume_restored_sessions_shared(Arc::clone(
            &gateway,
        )));
        // Wait until the cold-start spawn is in flight.
        for _ in 0..50 {
            if slow.starts.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(slow.starts.load(Ordering::SeqCst), 1);

        // The lock must stay obtainable while the 250ms spawn is in flight (the
        // spawn runs OUTSIDE the lock). The session is inserted only AFTER the
        // spawn completes, so it is not yet in the live map here.
        {
            let guard = tokio::time::timeout(std::time::Duration::from_millis(50), gateway.lock())
                .await
                .expect("gateway lock must stay available while restored spawn awaits adapter");
            assert!(
                guard.session_views().is_empty(),
                "rebuilt session is applied only after the spawn completes"
            );
        }

        resume_task.await.unwrap();
        // After restore completes, the session is live.
        assert_eq!(gateway.lock().await.session_views().len(), 1);
    }

    /// v0.8.x (concurrency review §4.1 P1) — head-of-line regression: chat A's
    /// message triggers the IMPLICIT first-message spawn (slow, via a
    /// delayed `FakeAdapter::start_thread`); chat B's message to an
    /// ALREADY-LIVE session must complete without waiting for A's spawn.
    /// Drives `handle_message_shared` directly — the same entry point the
    /// daemon's `spawn_inbound_consumer` uses for this exact shape — with A
    /// on its own task (mirroring the daemon backgrounding it) and B run
    /// inline (mirroring the daemon's loop moving straight on to the next
    /// chat). Timing assertion is loose (well under the 250ms spawn delay)
    /// to avoid flake.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_message_shared_does_not_block_other_chats_behind_a_slow_spawn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let slow = Arc::new(
            FakeAdapter::new(AgentVendor::Claude)
                .with_start_delay(std::time::Duration::from_millis(250)),
        );
        let mut gw = Gateway::new(slow.clone(), "alpha", tmp.path());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(tx);
        // Chat B already has a live session BEFORE the concurrent phase below
        // (its own spawn also pays the adapter's delay, but that is setup,
        // not what this test measures).
        gw.handle_text("telegram", "chat-b", "bob", "hello")
            .await
            .unwrap();
        assert_eq!(slow.starts.load(Ordering::SeqCst), 1);
        let gateway = Arc::new(tokio::sync::Mutex::new(gw));

        // Chat A: a brand-new chat's first message — implicit spawn, slow.
        let gw_a = Arc::clone(&gateway);
        let a_task = tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = Gateway::handle_message_shared(
                gw_a,
                "telegram",
                "chat-a",
                "alice",
                "a-1",
                "hi there",
                &[],
                None,
            )
            .await;
            (result, start.elapsed())
        });

        // Wait until chat A's spawn is actually in flight (entered
        // `start_thread`'s delay) before driving chat B, so this exercises
        // "B runs WHILE A's spawn is in flight" rather than an arbitrary race.
        for _ in 0..100 {
            if slow.starts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            slow.starts.load(Ordering::SeqCst),
            2,
            "chat A's spawn must have started"
        );

        // Chat B: existing session, plain text — must complete promptly, NOT
        // queue behind chat A's 250ms spawn.
        let b_start = std::time::Instant::now();
        let b_result = Gateway::handle_message_shared(
            Arc::clone(&gateway),
            "telegram",
            "chat-b",
            "bob",
            "b-1",
            "still there?",
            &[],
            None,
        )
        .await;
        let b_elapsed = b_start.elapsed();
        assert!(
            b_result.is_ok(),
            "chat B's message must succeed: {b_result:?}"
        );
        assert!(
            b_elapsed < std::time::Duration::from_millis(150),
            "chat B must not queue behind chat A's slow spawn (took {b_elapsed:?})"
        );

        let (a_result, a_elapsed) = a_task.await.unwrap();
        assert!(
            a_result.is_ok(),
            "chat A's message must eventually succeed: {a_result:?}"
        );
        assert!(
            a_elapsed >= std::time::Duration::from_millis(250),
            "sanity: chat A really did pay the spawn delay ({a_elapsed:?})"
        );
        // B finished (well) before A did — the actual head-of-line proof.
        assert!(b_elapsed < a_elapsed);
    }

    /// v0.8.x (concurrency review §4.1 P1) — two RAPID messages from the SAME
    /// brand-new chat (no session yet) must spawn exactly ONE session, not
    /// two: `SpawnClaims`'s per-chat single-flight serializes the racing
    /// `handle_message_shared` calls so the loser observes the session the
    /// winner just created instead of spawning a duplicate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_message_shared_does_not_double_spawn_for_concurrent_first_messages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(
            FakeAdapter::new(AgentVendor::Claude)
                .with_start_delay(std::time::Duration::from_millis(80)),
        );
        let mut gw = Gateway::new(fake.clone(), "alpha", tmp.path());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(tx);
        let gateway = Arc::new(tokio::sync::Mutex::new(gw));

        let mut tasks = Vec::new();
        for i in 0..2 {
            let gw = Arc::clone(&gateway);
            tasks.push(tokio::spawn(async move {
                Gateway::handle_message_shared(
                    gw,
                    "telegram",
                    "chat-new",
                    "carol",
                    &format!("m-{i}"),
                    &format!("msg {i}"),
                    &[],
                    None,
                )
                .await
            }));
        }
        for t in tasks {
            let r = t.await.unwrap();
            assert!(r.is_ok(), "both racing first messages must succeed: {r:?}");
        }

        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "exactly one session must be spawned for two racing first messages on the same chat"
        );
        assert_eq!(
            gateway.lock().await.session_views().len(),
            1,
            "exactly one session must be live"
        );
        // Neither message was silently dropped — both landed as turns on the
        // one session that got spawned.
        assert_eq!(
            fake.submissions.lock().await.len(),
            2,
            "both messages must have been submitted as turns"
        );
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
            receipt[0],
            "created session s2 (hitl: non-allowlist tools need IM approval)\n↓ 查看状态 → /status",
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

    /// v0.8.20 — `request_im_reload` is the web/CLI → daemon nudge that makes a
    /// newly-saved IM bot token apply WITHOUT a restart: it signals the daemon's
    /// reload task to rebuild the credential-driven channels. Locks the two-state
    /// contract the `config/im/*` handlers depend on — a safe no-op `false` on
    /// the standalone path (no trigger wired), and a delivered signal + `true`
    /// once the daemon wires its trigger. Regression guard for the bug where a
    /// web-saved token silently required `ccteam stop && start`.
    #[tokio::test]
    async fn request_im_reload_signals_only_when_trigger_wired() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw = Gateway::new(fake, "alpha", "/tmp/alpha");
        // Standalone (no daemon): a reload request is a safe no-op, never panics.
        assert!(
            !gw.request_im_reload(),
            "no trigger wired ⇒ false (standalone/test path)"
        );
        // Daemon path: wiring the trigger makes a request deliver a real signal.
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        gw.set_im_reload_trigger(tx);
        assert!(
            gw.request_im_reload(),
            "trigger wired ⇒ true (signal accepted)"
        );
        assert!(
            rx.try_recv().is_ok(),
            "the daemon's reload task actually received the nudge"
        );
    }

    /// v0.8.7 W2 (DB.1) — a hitl session's mode survives a daemon restart:
    /// persist → reload → cold-start rebuild → the restored session reports
    /// hitl. v0.8.21 Wave-2 — the posture round-trips through `meta.json`
    /// (`permission_mode`), and the live map is rebuilt by the async restore.
    #[tokio::test]
    async fn hitl_mode_persists_across_reload() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Project dir under the tempdir so the session's meta.json write is
        // isolated (the rebuild reads it back on restart).
        let project_dir = tmp.path().join("alpha");
        std::fs::create_dir_all(&project_dir).unwrap();
        {
            let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
            let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());
            gateway.enable_persistence(tmp.path()).unwrap();
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
        // Fresh gateway loading the same persisted root. Wave-2: load_state
        // restores routing (sync); the live map is cold-start rebuilt from
        // meta.json by the async restore step (mirrors daemon startup).
        let fake2 = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw2 = Gateway::new(fake2.clone(), "alpha", project_dir);
        gw2.enable_persistence(tmp.path()).unwrap();
        gw2.resume_restored_sessions().await;
        let views = gw2.session_views();
        assert_eq!(views.len(), 1, "the session restored from disk");
        assert_eq!(
            views[0].permission_mode, "hitl",
            "the hitl posture must survive the persist/reload round-trip (via meta.json)"
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
                outcome: None,
                error_kind: None,
                error: None,
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

    /// Clickable project picker: on Telegram, `/projects` is delivered as a
    /// header + one inline "switch" button per project (`nav:cd:<slug>`), so
    /// the command returns NO inline reply (the buttons ride the event sink).
    /// Non-button channels (the test mock, web, Lark) keep the plain slug list.
    #[tokio::test]
    async fn telegram_projects_delivers_switch_buttons() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        gateway.register_project("beta", proj.path());
        let mut events = gateway.subscribe_events();

        // Telegram → the reply is empty; the list + buttons arrive as an Answer.
        let replies = gateway
            .handle_text("telegram", "chat-1", "alice", "/projects")
            .await
            .unwrap();
        assert!(
            replies.is_empty(),
            "a button-capable /projects returns no inline reply: {replies:?}"
        );
        let ev = recv_answer(&mut events).await;
        assert!(ev.content.contains("项目"), "header: {}", ev.content);
        let datas: Vec<&str> = ev.options.iter().map(|o| o.data.as_str()).collect();
        assert!(datas.contains(&"nav:cd:alpha"), "options: {datas:?}");
        assert!(datas.contains(&"nav:cd:beta"), "options: {datas:?}");
        // The current project is marked with ✓ (default project = alpha).
        assert!(
            ev.options
                .iter()
                .any(|o| o.data == "nav:cd:alpha" && o.label.starts_with('✓')),
            "current project marked: {:?}",
            ev.options
        );

        // The mock channel has no buttons → the bare newline slug list, verbatim.
        let mock = gateway
            .handle_text("mock", "chat-2", "bob", "/projects")
            .await
            .unwrap();
        assert_eq!(mock, vec!["alpha\nbeta"]);
    }

    /// Clickable session picker: on Telegram, `/sessions` is delivered as the
    /// usual text list PLUS one inline "switch" button per live session
    /// (`nav:use:<sid>`), so the command returns no inline reply. The mock
    /// channel still gets the plain-text list (regression guard).
    #[tokio::test]
    async fn telegram_sessions_delivers_switch_buttons() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        gateway
            .handle_text("telegram", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        let mut events = gateway.subscribe_events();
        let replies = gateway
            .handle_text("telegram", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert!(
            replies.is_empty(),
            "a button-capable /sessions returns no inline reply: {replies:?}"
        );
        let ev = recv_answer(&mut events).await;
        assert!(ev.content.contains("s1"), "list text: {}", ev.content);
        assert_eq!(
            ev.options
                .iter()
                .map(|o| o.data.as_str())
                .collect::<Vec<_>>(),
            vec!["nav:use:s1"],
        );

        // Mock channel = plain text list, unchanged.
        gateway
            .handle_text("mock", "chat-2", "bob", "/new claude reviewer")
            .await
            .unwrap();
        let mock = gateway
            .handle_text("mock", "chat-2", "bob", "/sessions")
            .await
            .unwrap();
        assert_eq!(mock.len(), 1);
        assert!(mock[0].contains("s2 claude"), "{}", mock[0]);
    }

    /// Picker buttons are `sid vendor (title)` (sid → vendor → title), a `✓`
    /// marking the current session. The ROLE is NOT on the button (it stays on
    /// the information-rich text rows). Tail-padding stays visual-only so
    /// Telegram left-aligns the variable-width labels.
    #[tokio::test]
    async fn session_picker_labels_carry_sid_vendor_and_title() {
        use unicode_width::UnicodeWidthStr;
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        // One roleless untitled session + one titled session: the untitled
        // button is `sid vendor`, the titled one appends its (title).
        gateway
            .handle_text("telegram", "chat-1", "alice", "/new claude")
            .await
            .unwrap();
        gateway
            .handle_text("telegram", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .rename_session("s2", "A long review title")
            .await
            .unwrap();
        let chat = ChatKey::new("telegram", "chat-1", "alice");
        let opts = gateway.session_switch_options(&chat, false);
        assert_eq!(opts.len(), 2, "{opts:?}");
        let s1 = opts.iter().find(|o| o.id == "s1").unwrap();
        let s2 = opts.iter().find(|o| o.id == "s2").unwrap();
        let s1_text = s1.label.trim_end_matches('\u{2800}');
        let s2_text = s2.label.trim_end_matches('\u{2800}');
        // Untitled → `sid vendor`; titled + current → `✓ sid vendor (title)`.
        // Vendor is lowercase (`vendor_str`).
        assert_eq!(s1_text, "s1 claude");
        assert_eq!(s2_text, "✓ s2 claude (A long review title)");
        for label in [s1_text, s2_text] {
            assert!(
                !label.contains("reviewer"),
                "no role on the button: {label:?}"
            );
        }
        // Both labels padded to the same display width, with braille blanks.
        assert_eq!(
            s1.label.as_str().width(),
            s2.label.as_str().width(),
            "equal width: {opts:?}"
        );
        assert!(
            s1.label.ends_with('\u{2800}'),
            "shorter row is tail-padded: {:?}",
            s1.label
        );
    }

    /// A verbose title is clipped to `SESSION_BUTTON_TITLE_MAX_COLS` display
    /// columns (ellipsis included) so one long title can't widen every button.
    #[tokio::test]
    async fn session_picker_title_is_width_capped() {
        use unicode_width::UnicodeWidthStr;
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        gateway
            .handle_text("telegram", "chat-1", "alice", "/new claude")
            .await
            .unwrap();
        let long = "A really really long session title that keeps going";
        gateway.rename_session("s1", long).await.unwrap();
        let chat = ChatKey::new("telegram", "chat-1", "alice");
        let opts = gateway.session_switch_options(&chat, false);
        let label = opts[0].label.trim_end_matches('\u{2800}');
        let title = label
            .split_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .expect("titled button label");
        assert!(
            title.ends_with('…'),
            "clipped title ends with an ellipsis: {title:?}"
        );
        assert!(
            title.width() <= SESSION_BUTTON_TITLE_MAX_COLS,
            "within cap: {title:?}"
        );
        assert!(
            long.starts_with(title.trim_end_matches('…')),
            "prefix of the real title"
        );
    }

    /// `/sessions` text rows START with the sid and carry NO activity dot
    /// (🟢/🟡/🟠/🔴/⚪) or leading `[vendor]` tag — those lived on the text row
    /// pre-cleanup; activity now lives only on `/status`, and the vendor is
    /// still readable from the `:{vendor}:` colon field.
    #[tokio::test]
    async fn gateway_sessions_text_rows_start_with_sid_and_have_no_activity_dot() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        let row = listing[0].split('\n').nth(1).expect("a session row");
        assert!(
            row.starts_with("s1 claude"),
            "row starts with the sid: {row:?}"
        );
        for marker in ["🟢", "🟡", "🟠", "🔴", "⚪", "[claude]"] {
            assert!(
                !listing[0].contains(marker),
                "no activity dot / vendor tag on the text row: {listing:?}"
            );
        }
    }

    /// Picker-label padding equalizes display width across an option set.
    #[test]
    fn picker_label_padding_equalizes_display_width() {
        use unicode_width::UnicodeWidthStr;
        // Padding: mixed CJK/Latin rows end up the same display width.
        let mut opts = vec![
            MessageOption {
                data: "a".into(),
                label: "▸ s39 · grok · 「当前是什么模型」".into(),
                id: "s39".into(),
            },
            MessageOption {
                data: "b".into(),
                label: "▸ s43 · claude · 「Completed the full reques…".into(),
                id: "s43".into(),
            },
        ];
        left_align_option_labels(&mut opts);
        assert_eq!(
            opts[0].label.as_str().width(),
            opts[1].label.as_str().width()
        );
    }

    /// Tapping a picker button switches project / session through the SAME
    /// path `/cd` / `/use` use. A `nav:cd:<slug>` tap moves the current
    /// project; a `nav:use:<sid>` tap focuses the session (and moves the
    /// current project to that session's).
    #[tokio::test]
    async fn nav_button_tap_switches_project_and_session() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("telegram", ["chat-1".to_string()]);
        gateway.register_project("beta", proj.path());
        gateway
            .handle_text("telegram", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let chat = ChatKey::new("telegram", "chat-1", "alice");

        // Tap "switch to beta".
        let replies = gateway
            .handle_message(
                "telegram",
                "chat-1",
                "alice",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: "nav:cd:beta".to_string(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            replies,
            vec![
                "project set to beta (next message starts a session there)\n↓ 本项目会话 → /sessions"
            ]
        );
        assert_eq!(gateway.current_project_for(&chat).as_deref(), Some("beta"));

        // Tap "switch to s1" — focuses s1 and moves the project back to its own
        // (alpha).
        let replies = gateway
            .handle_message(
                "telegram",
                "chat-1",
                "alice",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: "nav:use:s1".to_string(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(replies, vec!["using session s1\n↓ 查看状态 → /status"]);
        assert_eq!(gateway.current_project_for(&chat).as_deref(), Some("alpha"));
        assert_eq!(
            gateway.current_session.read().unwrap().get(&chat).cloned(),
            Some("s1".to_string())
        );

        // A malformed nav payload is benign (never panics / switches).
        let replies = gateway
            .handle_message(
                "telegram",
                "chat-1",
                "alice",
                "",
                "",
                &[],
                Some(&ChoiceReply {
                    data: "nav:bogus".to_string(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(replies, vec!["invalid selection"]);
    }

    /// `strip_vendor_prefix` drops a leading `{vendor}` + separator from a
    /// model id that repeats it (`claude-opus-4-8` → `opus-4-8`), so the
    /// compact `/sessions` row (`sid vendor.model…`) doesn't read redundant
    /// (`claude.claude-opus-4-8`). A model that does NOT start with the
    /// vendor (codex's `gpt-5`), a partial-word false-positive (no separator
    /// boundary), and a match that would leave nothing after stripping all
    /// pass through unchanged — case-insensitive on the match, but the
    /// RETURNED text preserves the model's original casing/content.
    #[test]
    fn strip_vendor_prefix_drops_only_a_real_separator_bounded_match() {
        assert_eq!(
            strip_vendor_prefix("claude", "claude-opus-4-8[1m]"),
            "opus-4-8[1m]"
        );
        assert_eq!(strip_vendor_prefix("grok", "grok-4.5"), "4.5");
        assert_eq!(strip_vendor_prefix("claude", "Claude-Opus"), "Opus");
        // codex's models (gpt-*) never repeat "codex" — unchanged.
        assert_eq!(
            strip_vendor_prefix("codex", "gpt-5.5-codex"),
            "gpt-5.5-codex"
        );
        // "claudexyz" is a partial-word match, not "claude" + a real
        // separator — must NOT strip.
        assert_eq!(strip_vendor_prefix("claude", "claudexyz"), "claudexyz");
        // Stripping down to nothing falls back to the original.
        assert_eq!(strip_vendor_prefix("claude", "claude"), "claude");
        assert_eq!(strip_vendor_prefix("claude", "claude-"), "claude-");
    }

    /// P3 — `/sessions` appends each session's model + ctx from
    /// `thread_status`. With a `[1m]` model the window is 1M; with no
    /// status reported the legacy `id:project:vendor:role` row is unchanged.
    #[tokio::test]
    async fn gateway_sessions_shows_model_and_context() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // v0.8.22 P0-3/P0-4 — an isolated tempdir, not the shared literal
        // "/tmp/alpha": `render_sessions` now also scans on-disk `meta.json`
        // history for its "最近结束" section, so a fixed path shared with other
        // tests would leak cross-test session residue into this assertion.
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // Default status (all-None) → no suffix, legacy row verbatim.
        let bare = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(bare, vec!["📁 当前项目: alpha\ns1 claude"]);

        // Now report a model + effort + usage → suffix appears with the
        // TOTAL window (absolute, via `format_tokens`) + percent — no project
        // slug, no absolute USED count.
        fake.set_status(ThreadStatus {
            model: Some("claude-opus-4-8[1m]".into()),
            context: Some(ContextUsage::known(
                188_000,
                1_000_000,
                ContextSource::Derived,
            )),
            effort: Some("max".into()),
            goal: None,
        })
        .await;
        let with_status = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            with_status,
            vec!["📁 当前项目: alpha\ns1 claude.opus-4-8[1m].max.1M(19%)"]
        );

        // A non-[1m] model, no effort, renders against the 200k baseline.
        fake.set_status(ThreadStatus {
            model: Some("claude-sonnet-4-5".into()),
            effort: None,
            context: Some(ContextUsage::known(
                188_000,
                200_000,
                ContextSource::Derived,
            )),
            goal: None,
        })
        .await;
        let baseline = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            baseline,
            vec!["📁 当前项目: alpha\ns1 claude.sonnet-4-5.200k(94%)"]
        );
    }

    /// Every vendor's IM row carries its lowercase vendor right after the sid
    /// (`sid vendor · …`, the same opening as the switch button).
    #[tokio::test]
    async fn gateway_sessions_im_rows_lead_with_sid_then_vendor() {
        let factory: Arc<
            dyn Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = Arc::new(|vendor, _| Arc::new(FakeAdapter::new(vendor)));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new_with_factory(factory, "alpha", proj.path());
        for vendor in ["claude", "codex", "grok", "opencode", "kimi"] {
            gateway
                .handle_text("mock", "chat-1", "alice", &format!("/new {vendor}"))
                .await
                .unwrap();
        }
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        for vendor in ["claude", "codex", "grok", "opencode", "kimi"] {
            assert!(listing.contains(&format!(" {vendor}")), "{listing}");
        }
    }

    /// v0.8.23 review §1.3-D item 9 — IM `/sessions` pins a session with an
    /// outstanding HITL approval to the top of the live list (a ⏳ marker
    /// prefixes its row), even when it is LESS recent than its siblings.
    /// `s2` (qa) is created after `s1` (reviewer) so the default recency
    /// order is `s2` then `s1`; tagging `s1` with a pending approval must
    /// invert that.
    #[tokio::test]
    async fn gateway_sessions_pins_waiting_approval_to_top_with_marker() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude qa")
            .await
            .unwrap();

        // No pending yet — plain recency order (s2 newer, sorts first).
        let before = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(before, vec!["📁 当前项目: alpha\ns2 claude\ns1 claude"]);

        // Tag s1 (the OLDER session) with an outstanding approval.
        let token = "pwaitpin001";
        let (tx, _rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            permission_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );
        shared.lock().await.tag_sid(token, "s1".to_string());

        let after = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            after,
            vec!["📁 当前项目: alpha\n⏳ s1 claude\ns2 claude"],
            "s1 pinned to the top + ⏳-marked despite being less recent"
        );
    }

    /// v0.8.23 review §1.3-D item 9 — the web bare-row feed (`parse_sessions_reply`'s
    /// contract, fixed-shape `id:project:vendor:role` text) is UNCHANGED by
    /// the waiting-approval pin: no reorder, no ⏳ marker, even with a
    /// tagged pending outstanding. Sessions are created + queried from the
    /// SAME `web` chat so ownership (the shared ACL) isn't a confound.
    #[tokio::test]
    async fn gateway_sessions_web_bare_rows_unaffected_by_waiting_approval() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        let shared = Arc::new(Mutex::new(crate::pending::PendingInteractions::new()));
        gateway.set_pending(shared.clone());

        gateway
            .handle_text("web", "web-api", "web-api", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("web", "web-api", "web-api", "/new claude qa")
            .await
            .unwrap();

        let token = "pwaitweb001";
        let (tx, _rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
        shared.lock().await.register(
            token.to_string(),
            permission_prompt(token),
            InteractionOrigin::External { reply: tx },
            Instant::now() + std::time::Duration::from_secs(600),
        );
        shared.lock().await.tag_sid(token, "s1".to_string());

        let web_view = gateway
            .handle_text("web", "web-api", "web-api", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            web_view,
            vec!["s2:alpha:claude:qa\ns1:alpha:claude:reviewer"]
        );
    }

    /// v0.8.19 `/status` — fleet-health states derive deterministically from
    /// the per-session `turn_started_at` / `last_event_at` cells:
    /// - no turn in flight (`turn_started_at == None`) ⇒ 🟢 idle.
    /// - in flight + a recent event ⇒ 🔵 working (with elapsed).
    /// - in flight + last event stale past the idle window ⇒ 🔴 STUCK (matching
    ///   the watchdog's "silent for a full window" definition).
    /// Also asserts model · effort · ctx come from the real `thread_status`,
    /// and `ctx —` when no context is reported.
    #[tokio::test]
    async fn gateway_status_renders_idle_working_stuck() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        // A model + effort + context so the model·effort·ctx tail is exercised.
        fake.set_status(ThreadStatus {
            model: Some("claude-opus-4-8".into()),
            context: Some(ContextUsage::known(
                410_000,
                1_000_000,
                ContextSource::Derived,
            )),
            effort: Some("max".into()),
            goal: None,
        })
        .await;

        // (1) No turn in flight → 🟢 idle (seeded None at construction).
        let idle = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert_eq!(idle.len(), 1, "one message: {idle:?}");
        // /status = the CURRENT session deep view (📍 当前会话), NOT the fleet
        // list. The fake adapter's handle carries no `vendor_uuid` → `resume —`.
        assert!(
            idle[0].contains("📍 当前会话 s1 · alpha · claude · reviewer · 🟢 idle"),
            "current-session header: {idle:?}"
        );
        assert!(
            idle[0].contains("claude-opus-4-8 · max · ctx 41% · resume —"),
            "model·effort·ctx·resume line: {idle:?}"
        );
        // Owner req — /status ends by pointing at the full project list with a
        // live count (this gateway has one project, `alpha`), replacing the old
        // `/sessions all` cross-project pointer.
        assert!(
            idle[0].contains("↓ 所有 1 个项目 → /projects"),
            "/status footer points at /projects with a count: {idle:?}"
        );

        // (2) A turn in flight with a RECENT event → 🔵 working.
        let now = Instant::now();
        {
            let s = gateway.sessions.get("s1").expect("session s1");
            *s.turn_started_at.lock().unwrap() = Some(now);
            *s.last_event_at.lock().unwrap() = Some(now);
            *s.latest_activity.lock().unwrap() = Some("read×16·bash×8".to_string());
        }
        let working = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert!(
            working[0].contains("📍 当前会话 s1 · alpha · claude · reviewer · 🔵 working "),
            "working state: {working:?}"
        );
        assert!(
            working[0].contains("ctx 41%"),
            "ctx still shown: {working:?}"
        );

        // (3) In flight but the last event is stale past the idle window →
        // 🔴 STUCK. Use the SAME threshold the code reads so the test is
        // deterministic against any env-configured window.
        let mut window = gateway_turn_timeout_duration();
        if window.is_zero() {
            window = std::time::Duration::from_secs(300);
        }
        {
            let s = gateway.sessions.get("s1").expect("session s1");
            *s.turn_started_at.lock().unwrap() = Some(now);
            *s.last_event_at.lock().unwrap() =
                Some(now - (window + std::time::Duration::from_secs(60)));
        }
        let stuck = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert!(
            stuck[0].contains("📍 当前会话 s1 · alpha · claude · reviewer · 🔴 STUCK "),
            "stuck state: {stuck:?}"
        );
        assert!(stuck[0].contains("silent"), "silent duration: {stuck:?}");
    }

    #[tokio::test]
    async fn gateway_status_lists_a_working_direct_child() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, project_dir) = mirror_test_paths(&tmp);
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", &project_dir);
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
        gateway.enable_project_creation(paths.clone());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let child = gateway
            .create_delegated_session(
                "alpha".into(),
                "researcher".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: "s1".into(),
                    depth: 0,
                    role: "reviewer".into(),
                }),
                Some("delegated investigation".into()),
            )
            .await
            .unwrap()
            .sid;
        let progress = ccteam_core::progress::build_chat_turn_user_prompt_event(
            "researcher",
            &child,
            "child-turn",
            "investigate",
        );
        ccteam_core::progress::append_event(&paths.progress_jsonl("alpha"), &progress).unwrap();

        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert!(
            out[0].contains(
                "👥 直接子会话:\n      · s2 · claude · 🟡 working · delegated investigation"
            ),
            "working child is visible from its root status: {out:?}"
        );
    }

    #[tokio::test]
    async fn gateway_status_without_children_keeps_existing_card() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert_eq!(
            out,
            vec![format!(
                "🧭 → alpha/s1 (reviewer)\n📍 当前会话 s1 · alpha · claude · reviewer · 🟢 idle\n   📁 {}\n   — · — · ctx — · resume —\n   ↓ 所有 1 个项目 → /projects",
                proj.path().display()
            )]
        );
    }

    /// v0.8.19 `/status` — a roleless session shows `—` for the role, and a
    /// session whose adapter reports no context shows `ctx —` (deterministic,
    /// never fabricated). Default (all-None) status → model `—`, effort `—`.
    #[tokio::test]
    async fn gateway_status_roleless_and_no_context() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        // Roleless: `/new claude` with no role token.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude")
            .await
            .unwrap();
        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert!(
            out[0].contains("📍 当前会话 s1 · alpha · claude · — · 🟢 idle"),
            "roleless → role shows —, vendor still shown: {out:?}"
        );
        assert!(
            out[0].contains("— · — · ctx — · resume —"),
            "statusless + no-uuid → placeholders, never fabricated: {out:?}"
        );
    }

    /// v0.8.23 review §3.2-5 (item 2c) — `/status` leads with a standalone
    /// "你在哪" header line (project slug + current session sid/role) ahead
    /// of the existing `📍 当前会话` deep-view body, so the two-pointer
    /// (project × session) mental model has one line answering both.
    #[tokio::test]
    async fn gateway_status_leads_with_where_am_i_header() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-status-header");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert!(
            out[0].starts_with("🧭 → alpha/s1 (reviewer)\n📍 当前会话"),
            "leads with the you-are-here header before the existing body: {out:?}"
        );
    }

    /// v0.8.19 `/status` — empty fleet renders a friendly line, never an error.
    #[tokio::test]
    async fn gateway_status_empty_is_friendly() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert_eq!(out, vec!["no sessions — start one with /new"]);
    }

    /// v0.8.20 /status v2 ③ — the account-usage line renders 5h / weekly /
    /// credits with reset hints, a ⚠ on a `warning` weekly, and the
    /// subscription tail; an all-None usage renders the empty string.
    #[test]
    fn format_account_usage_renders_windows_resets_and_warning() {
        let u = AccountUsage {
            subscription: Some("max".into()),
            five_hour_pct: Some(17),
            five_hour_resets_at: Some("2026-06-25T19:00:00.4+00:00".into()),
            weekly_pct: Some(78),
            weekly_resets_at: Some("2026-06-29T18:59:59+00:00".into()),
            weekly_severity: Some("warning".into()),
            credits_pct: Some(46),
        };
        let s = format_account_usage(&u);
        assert!(s.contains("5h 17% (→19:00)"), "{s}");
        assert!(s.contains("周 78%⚠ (→06/29)"), "{s}");
        assert!(s.contains("额度 46%"), "{s}");
        assert!(s.ends_with("· max"), "{s}");
        assert_eq!(format_account_usage(&AccountUsage::default()), "");
    }

    /// `/status` running-task block — background workflows (`local_workflow`)
    /// are counted and labeled separately from subagents; a workflow's empty
    /// `subagent_type` must NOT fall back to the "subagent" label.
    #[test]
    fn format_running_tasks_distinguishes_workflows_from_subagents() {
        fn task(id: &str, kind: &str, desc: &str, task_type: &str) -> RunningTask {
            RunningTask {
                task_id: id.into(),
                kind: kind.into(),
                description: desc.into(),
                task_type: task_type.into(),
                started: std::time::Instant::now(),
            }
        }
        // Nothing running → nothing rendered.
        assert_eq!(format_running_tasks(&[]), "");
        // Subagents only → the pre-workflow header, kind from subagent_type.
        let subs = [task("a1", "code-reviewer", "review auth", "local_agent")];
        let s = format_running_tasks(&subs);
        assert!(s.contains("在跑 subagent (1):"), "{s}");
        assert!(s.contains("code-reviewer「review auth」"), "{s}");
        // Mixed → both kinds counted in the header; the workflow row is labeled
        // "workflow" even though its subagent_type is empty.
        let mixed = [
            task("a1", "", "find bugs", "local_agent"),
            task("w1", "", "audit the codebase", "local_workflow"),
        ];
        let s = format_running_tasks(&mixed);
        assert!(s.contains("在跑 subagent (1) + workflow (1):"), "{s}");
        assert!(s.contains("subagent「find bugs」"), "{s}");
        assert!(s.contains("workflow「audit the codebase」"), "{s}");
        // Workflows only (e.g. an idle session with a background run).
        let wf = [task("w1", "", "migrate call sites", "local_workflow")];
        let s = format_running_tasks(&wf);
        assert!(s.contains("在跑 workflow (1):"), "{s}");
        // Background shells (`local_bash` — Bash run_in_background / Monitor)
        // get their own bucket + row label; an idle session with an in-flight
        // `make test` renders it instead of a bare `🟢 idle`.
        let bg = [
            task("b1", "", "make test full suite", "local_bash"),
            task("b2", "", "watch /tmp/maketest.log", "local_bash"),
            task("a1", "code-reviewer", "review auth", "local_agent"),
        ];
        let s = format_running_tasks(&bg);
        assert!(s.contains("在跑 subagent (1) + 后台任务 (2):"), "{s}");
        assert!(s.contains("后台「make test full suite」"), "{s}");
        assert!(s.contains("后台「watch /tmp/maketest.log」"), "{s}");
    }

    /// The outlives-turn vocabulary the working-signal check shares with the
    /// harness turn-end eviction: background workflows AND background shells
    /// survive a turn boundary; sync subagents are turn-scoped.
    #[test]
    fn outlives_turn_vocabulary_covers_workflows_and_bg_shells() {
        fn t(task_type: &str) -> RunningTask {
            RunningTask {
                task_id: "x".into(),
                kind: String::new(),
                description: String::new(),
                task_type: task_type.into(),
                started: std::time::Instant::now(),
            }
        }
        assert!(t("local_workflow").outlives_turn());
        assert!(t("local_bash").outlives_turn());
        assert!(!t("local_agent").outlives_turn());
        assert!(!t("").outlives_turn());
    }

    /// v0.8.19 `/status` — ACL: a foreign IM chat does NOT see another chat's
    /// sessions (mirrors `/sessions` own-only). The owner sees its own.
    #[tokio::test]
    async fn gateway_status_acl_is_own_only() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        // tg-1 owns s1.
        gateway
            .handle_text("telegram", "tg-1", "rob", "/new claude reviewer")
            .await
            .unwrap();
        // A DIFFERENT telegram chat sees nothing (own-only isolation).
        let foreign = gateway
            .handle_text("telegram", "tg-2", "bob", "/status")
            .await
            .unwrap();
        assert_eq!(foreign, vec!["no sessions — start one with /new"]);
        // The web console (shared pool) DOES NOT see an IM-created session.
        let web = gateway
            .handle_text("web", "web-chat", "web-user", "/status")
            .await
            .unwrap();
        assert_eq!(web, vec!["no sessions — start one with /new"]);
        // The owner sees its own session.
        let owner = gateway
            .handle_text("telegram", "tg-1", "rob", "/status")
            .await
            .unwrap();
        assert_eq!(owner.len(), 1);
        assert!(
            owner[0].contains("📍 当前会话 s1 · alpha · claude · reviewer · 🟢 idle"),
            "got: {owner:?}"
        );
    }

    /// v0.8.19 `/status` — when the session's handle carries a stream-json
    /// `vendor_uuid` (the real Anthropic `--resume` id), `/status` surfaces it
    /// verbatim next to the sid as `resume <uuid>` (not the `resume —`
    /// fallback). Mirrors how the live daemon's persisted handle holds the id
    /// across restarts.
    #[tokio::test]
    async fn gateway_status_shows_real_vendor_resume_uuid() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        // Inject a vendor_uuid into the handle the way a stream-json spawn would
        // (the FakeAdapter returns an empty `raw_extras`).
        let uuid = "43e8b6b9-f233-4612-91e5-fc94e935c448";
        {
            let s = gateway.sessions.get_mut("s1").expect("session s1");
            s.thread.raw_extras = serde_json::json!({ "vendor_uuid": uuid });
        }
        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        assert!(
            out[0].contains(&format!("resume {uuid}")),
            "the real --resume uuid must show in the deep view: {out:?}"
        );
        assert!(
            out[0].contains("📍 当前会话 s1 · alpha · claude · reviewer · 🟢 idle"),
            "got: {out:?}"
        );
    }

    /// v0.8.19 `/status` — registered in the command set + dispatches via
    /// `handle_text` (it routes as a gateway command, never as turn text).
    #[tokio::test]
    async fn gateway_status_is_registered_and_dispatches() {
        assert!(
            GATEWAY_COMMANDS.iter().any(|c| c.name == "/status"),
            "/status must be registered in GATEWAY_COMMANDS"
        );
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        let out = gateway
            .handle_text("mock", "chat-1", "alice", "/status")
            .await
            .unwrap();
        // Dispatched as a command (the friendly empty-fleet reply), and the fake
        // adapter never received a turn submission.
        assert_eq!(out, vec!["no sessions — start one with /new"]);
        assert!(
            fake.submissions.lock().await.is_empty(),
            "/status must not submit a turn"
        );
    }

    /// v0.8.19 `/status` — the duration humanizer used on the working / stuck
    /// lines: compact `45s` / `1m12s` / `6m` / `2h3m`, seconds dropped at ≥ 1h.
    #[test]
    fn humanize_dur_is_compact() {
        use std::time::Duration;
        assert_eq!(humanize_dur(Duration::from_secs(0)), "0s");
        assert_eq!(humanize_dur(Duration::from_secs(45)), "45s");
        assert_eq!(humanize_dur(Duration::from_secs(72)), "1m12s");
        assert_eq!(humanize_dur(Duration::from_secs(360)), "6m");
        assert_eq!(humanize_dur(Duration::from_secs(7380)), "2h3m");
        assert_eq!(humanize_dur(Duration::from_secs(7200)), "2h");
        // Sub-second rounds down to 0s (never panics / never blank).
        assert_eq!(humanize_dur(Duration::from_millis(400)), "0s");
    }

    /// v0.8.18 柱2 (multi-user soft-partition 档0) — own-only isolation: a
    /// session created by one chat is NOT visible OR addressable from a
    /// DIFFERENT chat, even in the same project. This REVERSES the v0.8.13
    /// cross-frontend-by-project sharing so two IM chats (distinct `chat_id`s)
    /// on one machine never cross. The OWNER keeps full visibility + addressing.
    /// (Same-user web↔IM cross-frontend reach returns via 档1.) — the async
    /// integration check for this is `gateway_sessions_are_own_only_across_chats`
    /// below; the test directly here is the pure unit check of the same rule.
    ///
    /// v0.8.20 web↔IM convergence — a per-tenant IM bot (`telegram@<tid>`) and
    /// that tenant's web console are ONE identity (`user:<tid>`): the bot sees the
    /// tenant's WEB-created sessions (and the tenant's web sees the bot's). It
    /// still sees NOTHING of other tenants or the admin pool. The admin/global
    /// bot keeps the operator "own + all web" view.
    #[test]
    fn session_owner_visibility_converges_tenant_web_and_im() {
        let gw = acl_gateway();
        // A web-created session's OWNER is the canonical user identity
        // (`user:<id>`), derived by `canonical_owner` from the web frontend chat.
        let web_a = canonical_owner(&ChatKey::new("web", "uaaa", "uaaa")).identity(); // user:uaaa
        let web_b = canonical_owner(&ChatKey::new("web", "ubbb", "ubbb")).identity(); // user:ubbb
        let web_admin = canonical_owner(&ChatKey::new("web", "web-api", "web-api")).identity();
        let admin_tg = ChatKey::new("telegram", "339", "rob");
        let bot_a = ChatKey::new("telegram@uaaa", "111", "alice"); // uaaa's IM bot
        let bot_b = ChatKey::new("telegram@ubbb", "222", "bob");

        // CONVERGENCE: uaaa's bot canonicalizes to user:uaaa → it sees uaaa's
        // web-created sessions (owner user:uaaa).
        assert!(
            gw.owner_identity_visible(&bot_a, &web_a),
            "a tenant bot sees its tenant's web-created sessions (convergence)"
        );
        // ISOLATION: not the admin pool, not another tenant, not the admin IM.
        assert!(
            !gw.owner_identity_visible(&bot_a, &web_admin),
            "no admin pool"
        );
        assert!(
            !gw.owner_identity_visible(&bot_a, &web_b),
            "no other tenant"
        );
        assert!(
            !gw.owner_identity_visible(&bot_a, &admin_tg.identity()),
            "no admin IM"
        );
        assert!(
            !gw.owner_identity_visible(&bot_b, &web_a),
            "ubbb's bot doesn't see uaaa's sessions"
        );

        // The admin/global bot: own + its OWN web console pool — NOT a
        // tenant's. (the 2026-07-28 cross-user fix: the old blanket `user:*` pool
        // pushed tenants' sessions into the owner's IM bot, where a `/use` on a
        // listed sid then re-pointed that session's `reply_to` at the admin
        // chat — "IM receives another user's session messages".)
        assert!(gw.owner_identity_visible(&admin_tg, &admin_tg.identity()));
        assert!(gw.owner_identity_visible(&admin_tg, &web_admin));
        assert!(
            !gw.owner_identity_visible(&admin_tg, &web_a),
            "the admin/global bot must NOT see a tenant's web sessions"
        );
        // ... and symmetrically, a tenant's WEB console (channel "web", not a
        // bot channel) sees neither the admin pool nor another tenant.
        let web_console_a = ChatKey::new("web", "uaaa", "uaaa");
        assert!(gw.owner_identity_visible(&web_console_a, &web_a));
        assert!(!gw.owner_identity_visible(&web_console_a, &web_b));
        assert!(!gw.owner_identity_visible(&web_console_a, &web_admin));
        assert!(!gw.owner_identity_visible(&web_console_a, &admin_tg.identity()));
    }

    /// PRIVILEGE IS A NAME (2026-07-28 owner report, part 2). Reaching a bot
    /// must not make you the operator — the transport allowlist used to double
    /// as the privilege grant, so a bot open to the world (`allowed_user_ids:
    /// ["*"]`, which is what the reporter's Lark bot carried) served EVERY
    /// stranger as the box owner: all projects, all sessions, spawn anywhere.
    #[test]
    fn wildcard_allowlist_names_nobody_so_grants_nobody() {
        let owner = ChatKey::new("lark", "oc_room", "ou_owner");
        let stranger = ChatKey::new("lark", "oc_other", "ou_stranger");

        // `"*"` — an explicit "anyone may talk" — names nobody.
        let mut gw = acl_gateway();
        assert_eq!(
            gw.bind_operator_allowlist("lark", ["*".to_string()]),
            OperatorBindingKind::Wildcard
        );
        assert_eq!(
            gw.principal(&owner),
            Principal::Guest("lark:oc_room".into())
        );
        assert_eq!(
            gw.principal(&stranger),
            Principal::Guest("lark:oc_other".into())
        );
        // A guest sees no project — not the owner's, not a legacy unowned one.
        assert!(!gw.chat_can_see_project_owner(&stranger, None));
        assert!(!gw.chat_can_see_project_owner(&stranger, Some("user:web-api")));
        assert!(!gw.chat_can_see_project_owner(&stranger, Some("user:ualice")));

        // Naming the owner takes the bot back: Lark authorizes by SENDER
        // open_id, Telegram by chat id — either field may carry the name.
        let mut named = acl_gateway();
        assert_eq!(
            named.bind_operator_allowlist("lark", ["ou_owner".to_string()]),
            OperatorBindingKind::Named
        );
        assert_eq!(named.principal(&owner), Principal::Operator);
        assert_eq!(
            named.principal(&stranger),
            Principal::Guest("lark:oc_other".into())
        );
        assert!(named.chat_can_see_project_owner(&owner, Some("user:web-api")));

        // An UNCONFIGURED platform keeps the legacy single-operator assumption
        // (a half-configured owner must not be locked out of their own bot);
        // the daemon warns about it at startup instead.
        let plain = acl_gateway();
        assert_eq!(plain.principal(&owner), Principal::Operator);
    }

    /// A principal never inherits the box's DEFAULT project. This was the
    /// widest hole in the IM path: a tenant's bot (or any unnamed chat) that
    /// had never `/cd`'d anywhere silently resolved to the owner's default
    /// project, and the implicit first-message spawn then started an agent
    /// inside the owner's working tree.
    #[tokio::test]
    async fn spawn_never_falls_back_to_a_project_the_caller_cannot_see() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        seed_owned_project(&paths, "ops", Some("user:web-api"));
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "ops", paths.projects_root.join("ops"));
        gateway.enable_project_creation(paths.clone());
        gateway.bind_operator_allowlist("telegram", ["339".to_string()]);

        // A tenant's own bot: owns nothing yet → refused, with the next step.
        let err = gateway
            .handle_text("telegram@ualice", "111", "alice", "hello")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("/newproject"), "actionable refusal: {msg}");
        assert!(
            !msg.contains("ops"),
            "must not name the owner's project: {msg}"
        );

        // An unnamed chat on the owner's bot is a guest — same refusal, pointed
        // at the binding step instead.
        let guest = gateway
            .handle_text("telegram", "999", "eve", "hello")
            .await
            .unwrap_err();
        assert!(
            format!("{guest:#}").contains("绑定"),
            "guest is told to get bound: {guest:#}"
        );

        // Nothing was created in the owner's project.
        assert!(gateway.session_views().is_empty(), "no session may exist");

        // The owner's own named chat still spawns there.
        gateway
            .handle_text("telegram", "339", "rob", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(
            gateway
                .session_views()
                .iter()
                .map(|v| v.project.as_str())
                .collect::<Vec<_>>(),
            vec!["ops"]
        );
    }

    /// v0.8.21 cold-resume ACL — `owner_identity_visible` reads a persisted
    /// `meta.owner` when resuming a STOPPED session, and must agree with the
    /// live gate (`chat_can_access`). The regression it guards: the
    /// earlier cold path reconstructed a `ChatKey` via `from_identity` and
    /// compared with `==`, but `ChatKey` equality includes `user_id` while
    /// `identity()` drops it — so an admin IM bot (whose `canonical_owner` keeps
    /// the sender's `user_id`) was wrongly DENIED resume of its OWN session.
    #[test]
    fn owner_identity_visible_matches_live_acl_on_strings() {
        let gw = acl_gateway();
        // meta.owner is the canonical identity STRING (user_id dropped).
        let admin_tg = ChatKey::new("telegram", "339", "rob"); // user_id ≠ chat_id
        let admin_owns = canonical_owner(&admin_tg).identity(); // "telegram:339"

        // THE REGRESSION: the admin bot must see its own cold session even
        // though `meta.owner` ("telegram:339") lost the "rob" user_id.
        assert!(
            gw.owner_identity_visible(&admin_tg, &admin_owns),
            "owner must see its own stopped session (user_id round-trip must not deny)"
        );
        // A DIFFERENT admin chat (other chat_id) does not see it.
        let other_tg = ChatKey::new("telegram", "999", "eve");
        assert!(!gw.owner_identity_visible(&other_tg, &admin_owns));

        // Web/tenant convergence + isolation, mirrored from the live-ACL test.
        let web_a = canonical_owner(&ChatKey::new("web", "uaaa", "uaaa")).identity(); // user:uaaa
        let bot_a = ChatKey::new("telegram@uaaa", "111", "alice");
        let bot_b = ChatKey::new("telegram@ubbb", "222", "bob");
        assert!(
            gw.owner_identity_visible(&bot_a, &web_a),
            "a tenant bot sees its tenant's web-created (cold) sessions"
        );
        assert!(
            !gw.owner_identity_visible(&bot_b, &web_a),
            "another tenant's bot does NOT"
        );
        assert!(
            !gw.owner_identity_visible(&bot_a, &admin_owns),
            "a tenant bot gets no admin pool"
        );
        // The admin/global bot sees its OWN web pool — never a tenant's (the
        // cold-resume twin of the 2026-07-28 cross-user fix).
        assert!(gw.owner_identity_visible(
            &admin_tg,
            &canonical_owner(&ChatKey::new("web", "web-api", "web-api")).identity()
        ));
        assert!(
            !gw.owner_identity_visible(&admin_tg, &web_a),
            "the admin/global bot must NOT cold-resume a tenant's session"
        );
    }

    /// IM PROJECT ACL — the multi-user isolation the leaky unfiltered project
    /// list broke: a tenant must NOT see the admin's projects (the reported
    /// bug), and the operator must never peek into a tenant's — SYMMETRIC with
    /// the web REST list (`build_projects` → `Identity::can_see_owner`) and the
    /// MCP list (`visible_user_projects`), since all three now authorize off the
    /// same `ProjectState.owner` through the same core policy
    /// (`ccteam_core::identity::can_see_owner`). Pure-predicate twin of
    /// `session_owner_visibility_converges_tenant_web_and_im` (the session
    /// rule), keyed on PROJECT owners instead of session owners.
    #[test]
    fn project_acl_isolates_tenants_from_admin_and_each_other() {
        let gw = acl_gateway();
        // Project owner tags, as `/newproject` + web `POST /projects` stamp
        // them: a tenant's project is `user:<id>`, the admin's is the shared web
        // pool `user:web-api` or its own IM `telegram:<chat_id>`, a legacy
        // `ccteam init` project is unowned (`None`).
        let admin_web = Some("user:web-api");
        let admin_im = Some("telegram:339");
        let tenant_a = Some("user:ualice");
        let tenant_b = Some("user:ubob");
        let unowned: Option<&str> = None;

        // A per-tenant IM bot (`telegram@ualice`) — the "user" in the bug report
        // — and its own web console (channel "web", tenant id as chat_id) are ONE
        // identity, so BOTH frontends see exactly the same projects (web/IM
        // symmetry): ONLY the tenant's own, never the admin's (web OR IM), never
        // another tenant's, never a legacy unowned one.
        for viewer in [
            ChatKey::new("telegram@ualice", "111", "alice"),
            ChatKey::new("web", "ualice", "ualice"),
        ] {
            assert!(
                gw.chat_can_see_project_owner(&viewer, tenant_a),
                "a tenant sees its own project ({})",
                viewer.channel
            );
            assert!(
                !gw.chat_can_see_project_owner(&viewer, admin_web),
                "a tenant must NOT see the admin's web project ({})",
                viewer.channel
            );
            assert!(
                !gw.chat_can_see_project_owner(&viewer, admin_im),
                "a tenant must NOT see the admin's IM project ({})",
                viewer.channel
            );
            assert!(
                !gw.chat_can_see_project_owner(&viewer, tenant_b),
                "a tenant must NOT see another tenant's project ({})",
                viewer.channel
            );
            assert!(
                !gw.chat_can_see_project_owner(&viewer, unowned),
                "a tenant must NOT see a legacy unowned project ({})",
                viewer.channel
            );
        }

        // The operator (owner's global IM bot ≡ admin web console) sees every
        // NON-tenant project — its own web pool, its own IM projects, and legacy
        // unowned — but NEVER a per-user tenant's private project (exactly what
        // web admin sees via `can_see_owner`).
        for op in [
            ChatKey::new("telegram", "339", "rob"),
            ChatKey::new("web", "web-api", "web-api"),
        ] {
            assert!(
                gw.chat_can_see_project_owner(&op, admin_web),
                "operator sees its own web pool ({})",
                op.channel
            );
            assert!(
                gw.chat_can_see_project_owner(&op, admin_im),
                "operator sees its own IM project ({})",
                op.channel
            );
            assert!(
                gw.chat_can_see_project_owner(&op, unowned),
                "operator sees legacy unowned projects ({})",
                op.channel
            );
            assert!(
                !gw.chat_can_see_project_owner(&op, tenant_a),
                "operator must NOT peek into a tenant's project ({})",
                op.channel
            );
        }
    }

    /// IM `/projects` + `/cd` end-to-end — proves the owner ACL is WIRED at the
    /// command surface, not just correct as a predicate: a tenant's `/projects`
    /// lists only its OWN project, and it cannot `/cd` into — then spawn sessions
    /// in — the admin's project by typing the slug. Seeds real owned
    /// `state.json`s read through `collect_projects` (the SAME source the web
    /// list reads), so a future refactor that keeps `chat_can_see_project_owner`
    /// but stops `visible_project_slugs` / `change_project` from calling it would
    /// still fail here — the gap the pure-predicate test above can't catch.
    #[tokio::test]
    async fn im_project_list_and_cd_hide_other_owners_projects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        seed_owned_project(&paths, "tenant-proj", Some("user:ualice"));
        seed_owned_project(&paths, "admin-proj", Some("user:web-api"));
        seed_owned_project(&paths, "cli-proj", None); // legacy, unowned

        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "seed", tmp.path().join("seed"));
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("telegram", ["339".to_string()]);
        gateway.enable_project_creation(paths);

        let tenant = ChatKey::new("telegram@ualice", "111", "alice");
        let admin = ChatKey::new("telegram", "339", "rob");

        // THE BUG: the tenant's `/projects` lists ONLY its own project.
        assert_eq!(
            gateway.visible_project_slugs(&tenant),
            vec!["tenant-proj".to_string()],
            "a tenant's /projects must not include the admin's or legacy projects"
        );
        // The operator sees the admin + legacy projects, not the tenant's private.
        let admin_visible = gateway.visible_project_slugs(&admin);
        assert!(
            admin_visible.contains(&"admin-proj".to_string())
                && admin_visible.contains(&"cli-proj".to_string()),
            "operator sees its own + legacy, got {admin_visible:?}"
        );
        assert!(
            !admin_visible.contains(&"tenant-proj".to_string()),
            "operator must not peek into a tenant's project, got {admin_visible:?}"
        );

        // Addressing is gated too: the tenant can't `/cd` into the admin's
        // project by typing the slug — it reads identically to a nonexistent one
        // ("unknown project"), leaking no existence — but CAN `/cd` into its own.
        let err = gateway
            .change_project(&tenant, "admin-proj")
            .expect_err("tenant must not /cd into the admin's project");
        assert!(
            err.to_string().contains("unknown project"),
            "expected an unknown-project error, got {err}"
        );
        assert!(
            gateway.change_project(&tenant, "tenant-proj").is_ok(),
            "a tenant must be able to /cd into its OWN project"
        );
    }

    /// v0.8.21 cold-resume ACL (web path) — `resume_stopped_session` resolves a
    /// sid across ALL registered projects (`find_meta_for_sid`), so the web
    /// caller's authorised slug MUST bind the resolved project. Without the
    /// `expected_slug` guard a tenant authorised for project B could resume a
    /// session belonging to project A by POSTing A's sid under B's slug. This
    /// proves a mismatched slug is rejected BEFORE any child is spawned.
    #[tokio::test]
    async fn resume_stopped_session_rejects_cross_project_slug_before_spawn() {
        let alpha_dir = tempfile::TempDir::new().unwrap();
        let beta_dir = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", alpha_dir.path());
        gateway.register_project("beta", beta_dir.path());

        // A stopped session s1 belongs to project alpha (meta.json on disk).
        let meta = SessionMeta {
            sid: "s1".into(),
            slug: "alpha".into(),
            vendor: AgentVendor::Claude,
            protocol: SessionProtocol::StreamJson,
            role: String::new(),
            permission_mode: PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            effort: None,
            host: "local".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            last_active: "2026-01-01T00:00:00Z".into(),
            origin: SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        write_session_meta(alpha_dir.path(), &meta).unwrap();

        // Resuming s1 under the WRONG slug (beta) must be rejected, and crucially
        // must NOT spawn the child.
        let denied = gateway
            .resume_stopped_session("s1", "user:web-api", Some("beta"))
            .await;
        assert!(
            denied.is_err(),
            "resume must reject a sid that doesn't belong to the authorised slug"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            0,
            "a rejected cross-project resume must not spawn a child"
        );

        // The correct slug (alpha) proceeds to spawn (the guard doesn't over-block).
        let allowed = gateway
            .resume_stopped_session("s1", "user:web-api", Some("alpha"))
            .await;
        assert!(
            allowed.is_ok(),
            "the owning project's slug is allowed: {allowed:?}"
        );
        assert_eq!(
            fake.starts.load(Ordering::SeqCst),
            1,
            "correct slug spawns once"
        );
    }

    /// Web send-resume-by-sid — the two helpers the web turn handler leans on to
    /// cold-resume a session that "disappeared" (evicted for capacity, dropped
    /// by a daemon restart whose rebuild failed, or stopped) instead of 404-ing:
    /// `is_session_live` tracks live-map membership, and `project_slug_for_sid`
    /// resolves the owning project of a STOPPED session from its on-disk
    /// `meta.json` (uncapped — unlike the web rail's history list) so `gate_sid`
    /// admits the caller and the turn can resume it.
    #[tokio::test]
    async fn stopped_session_still_resolves_its_project_for_send_resume() {
        let alpha_dir = tempfile::TempDir::new().unwrap();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", alpha_dir.path());

        // A stopped session s1 (meta.json on disk, never spawned → not live).
        let meta = SessionMeta {
            sid: "s1".into(),
            slug: "alpha".into(),
            vendor: AgentVendor::Claude,
            protocol: SessionProtocol::StreamJson,
            role: String::new(),
            permission_mode: PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            effort: None,
            host: "local".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            last_active: "2026-01-01T00:00:00Z".into(),
            origin: SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        write_session_meta(alpha_dir.path(), &meta).unwrap();

        // Not in the live map...
        assert!(
            !gateway.is_session_live("s1"),
            "a stopped session is not live"
        );
        // ...but the turn handler can still resolve its project from meta.json
        // (the fix: resolve → cold-resume rather than 404 the "vanished" sid).
        assert_eq!(
            gateway.project_slug_for_sid("s1").as_deref(),
            Some("alpha"),
            "a stopped session still resolves its owning project from meta.json"
        );
        // A genuinely unknown sid resolves to nothing → a real 404.
        assert!(
            gateway.project_slug_for_sid("s999").is_none(),
            "an unknown sid resolves to no project"
        );

        // Resuming it (what the web turn now does before submitting) re-admits
        // it into the live map, so the subsequent submit finds a live session.
        gateway
            .resume_stopped_session("s1", "user:web-api", Some("alpha"))
            .await
            .unwrap();
        assert!(
            gateway.is_session_live("s1"),
            "resume re-admits s1 into the live map, so the turn can proceed"
        );
        assert_eq!(
            gateway.project_slug_for_sid("s1").as_deref(),
            Some("alpha"),
            "a live session resolves its project from the live map"
        );
    }

    /// v0.8.21 IM cold-resume — `/use <sid>` on a STOPPED session (meta.json on
    /// disk, no live thread) re-activates it, and the own-only ACL still holds:
    /// a chat that doesn't own it reads it as unknown (no existence leak) and
    /// the denied `/use` must NOT resurrect it; the owner resumes it. The IM
    /// peer of the web `POST .../resume` path.
    #[tokio::test]
    async fn im_use_cold_resumes_stopped_session_own_only() {
        let proj = tempfile::TempDir::new().unwrap();
        let agents = proj.path().join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("reviewer.md"),
            "---\nname: reviewer\n---\nbody\n",
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());

        // Owner creates s1, then stops it: the live thread is gone but meta.json
        // survives, so it shows up in history.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert!(
            gateway.session_views().iter().any(|v| v.sid == "s1"),
            "created s1"
        );
        gateway
            .handle_text("mock", "chat-1", "alice", "/stop s1")
            .await
            .unwrap();
        assert!(
            !gateway.session_views().iter().any(|v| v.sid == "s1"),
            "s1 is stopped (no longer live)"
        );
        assert!(
            gateway
                .list_history_sessions("alpha")
                .iter()
                .any(|m| m.sid == "s1"),
            "stopped s1 survives in history (meta.json kept)"
        );

        // A different chat cannot cold-resume it — own-only ACL reads it as
        // unknown (no existence leak), and the denied /use must not resurrect it.
        let denied = gateway
            .handle_text("mock", "chat-2", "bob", "/use s1")
            .await
            .unwrap();
        assert_eq!(
            denied,
            vec!["unknown session for this chat: s1\n↓ 查看状态 → /status".to_string()]
        );
        assert!(
            !gateway.session_views().iter().any(|v| v.sid == "s1"),
            "a denied /use must not resurrect the session"
        );

        // The owner /use s1 cold-resumes it from meta.json → live again.
        let resumed = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(
            resumed,
            vec!["resumed session s1\n↓ 查看状态 → /status".to_string()]
        );
        assert!(
            gateway.session_views().iter().any(|v| v.sid == "s1"),
            "owner cold-resumed s1 from meta.json"
        );
    }

    /// Send-resume symmetry (the architectural twin of the web-turn fix) — the
    /// shared `submit_to_sid` core (which the web turn, MCP `session_dispatch`
    /// and the `@handle` mirror all funnel through) COLD-RESUMES a session that
    /// left the live map, instead of erroring "current session missing". This is
    /// the deepest resume-by-sid rung now living in the submit core alongside the
    /// in-map dead-child revive — so every frontend revives a "disappeared"
    /// session identically, not just the web handler.
    #[tokio::test]
    async fn submit_to_sid_cold_resumes_a_stopped_session() {
        let proj = tempfile::TempDir::new().unwrap();
        let agents = proj.path().join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("reviewer.md"),
            "---\nname: reviewer\n---\nbody\n",
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());

        // Create s1, then stop it — live thread gone, meta.json kept.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert!(gateway.is_session_live("s1"), "created s1");
        gateway
            .handle_text("mock", "chat-1", "alice", "/stop s1")
            .await
            .unwrap();
        assert!(
            !gateway.is_session_live("s1"),
            "s1 stopped (left the live map)"
        );
        let starts_before = fake.starts.load(Ordering::SeqCst);

        // A turn addressed by sid must revive it (not "current session missing").
        let turn = gateway.submit_to_sid("s1", "back to work".into()).await;
        assert!(
            turn.is_ok(),
            "submit_to_sid must cold-resume a stopped session: {turn:?}"
        );
        assert!(gateway.is_session_live("s1"), "the send revived s1");
        assert!(
            fake.starts.load(Ordering::SeqCst) > starts_before,
            "cold-resume re-spawned the child"
        );

        // A genuinely unknown sid still errors (no meta → nothing to resume).
        assert!(
            gateway.submit_to_sid("s404", "x".into()).await.is_err(),
            "an unknown sid has no meta to resume from"
        );
    }

    /// v0.8.23 review §3.2-5 (item 2b) — `/use @<role>` resolves to the ONE
    /// chat-visible session carrying that role (unambiguous case).
    #[tokio::test]
    async fn use_at_role_shorthand_resolves_unambiguous_role() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-use-role-happy");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap(); // s1
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude qa")
            .await
            .unwrap(); // s2, now current
        let used = gateway
            .handle_text("mock", "chat-1", "alice", "/use @reviewer")
            .await
            .unwrap();
        assert_eq!(
            used,
            vec!["using session s1\n↓ 查看状态 → /status".to_string()]
        );
    }

    /// v0.8.23 review §3.2-5 (item 2b) — two sessions share a role: `/use
    /// @role` resolves the ambiguity SILENTLY by recency (most-recent
    /// `last_active` wins; sid-desc tiebreaks when `last_active` is
    /// unavailable, as here with no real project backing meta.json) rather
    /// than erroring.
    #[tokio::test]
    async fn use_at_role_shorthand_ambiguous_picks_most_recent() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-use-role-ambiguous");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap(); // s1
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap(); // s2 — same role, spawned later
        let used = gateway
            .handle_text("mock", "chat-1", "alice", "/use @reviewer")
            .await
            .unwrap();
        assert_eq!(
            used,
            vec!["using session s2\n↓ 查看状态 → /status".to_string()],
            "ambiguous role resolves to the most-recently-active session"
        );
    }

    /// v0.8.23 review §3.2-5 (item 2b) — an unmatched role is a clear usage
    /// error listing the roles this chat CAN see, not a silent no-op.
    #[tokio::test]
    async fn use_at_role_shorthand_unknown_role_lists_available_roles() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-use-role-unknown");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let err = gateway
            .handle_text("mock", "chat-1", "alice", "/use @qa")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("qa"), "names the unmatched role: {msg}");
        assert!(msg.contains("reviewer"), "lists the available roles: {msg}");
    }

    /// v0.8.23 review §3.2-5 (item 2b) — `@role` visibility follows the SAME
    /// own-only ACL as `/sessions`/`/status` (`chat_can_access`): a foreign
    /// chat's session with a matching role must not resolve, and must not
    /// leak into the "available roles" list either.
    #[tokio::test]
    async fn use_at_role_shorthand_respects_chat_acl() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-use-role-acl");
        // tg-1 owns a `reviewer` session.
        gateway
            .handle_text("telegram", "tg-1", "rob", "/new claude reviewer")
            .await
            .unwrap();
        // A different IM chat cannot see it via @role — own-only isolation.
        let err = gateway
            .handle_text("telegram", "tg-2", "bob", "/use @reviewer")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("reviewer"),
            "another chat's role must not leak into the available-roles hint: {msg}"
        );
    }

    #[tokio::test]
    async fn gateway_sessions_are_own_only_across_chats() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // v0.8.22 P0-3/P0-4 — isolated tempdir (see
        // `gateway_sessions_shows_model_and_context` for why the shared
        // "/tmp/alpha" literal is unsafe now that `/sessions` also reads
        // on-disk history).
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());
        // chat-1 creates a session in the default project "alpha".
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // chat-2 (a different chat/user) in the SAME default project "alpha"
        // does NOT see chat-1's session (own-only; pre-0.8.18 the same-project
        // leak would have shown it).
        let seen = gateway
            .handle_text("mock", "chat-2", "bob", "/sessions")
            .await
            .unwrap();
        assert_eq!(seen, vec!["📁 当前项目: alpha\n暂无会话 —— /new 开一个"]);

        // …and cannot ADDRESS it: /use is refused for a non-owner and reads as
        // unknown (no existence leak).
        let used = gateway
            .handle_text("mock", "chat-2", "bob", "/use s1")
            .await
            .unwrap();
        assert_eq!(
            used,
            vec!["unknown session for this chat: s1\n↓ 查看状态 → /status"]
        );

        // The OWNER still sees + uses its own session.
        let owner_sees = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(owner_sees, vec!["📁 当前项目: alpha\ns1 claude"]);
        let owner_uses = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(owner_uses, vec!["using session s1\n↓ 查看状态 → /status"]);
    }

    /// v0.8.18 柱2 档0 (regression fix) — the web console is a SHARED operator
    /// pool: a session created from the web console (`owner.channel == "user"`)
    /// is visible AND addressable from an IM chat. This is the common
    /// single-user flow (create it on web, drive it from your phone) and the
    /// exact case the first cut of own-only broke. IM-created sessions instead
    /// stay private to their chat (see `gateway_sessions_are_own_only_across_chats`).
    #[tokio::test]
    async fn gateway_web_owned_session_visible_from_im() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        // v0.8.22 P0-3/P0-4 — isolated tempdir (see
        // `gateway_sessions_shows_model_and_context` for why the shared
        // "/tmp/alpha" literal is unsafe now that `/sessions` also reads
        // on-disk history).
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake.clone(), "alpha", proj.path());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("telegram", ["339498819".to_string()]);
        let mut events = gateway.subscribe_events();
        // A session created from the web console (channel "web").
        gateway
            .handle_text("web", "web-api", "web-api", "/new claude reviewer")
            .await
            .unwrap();

        // A telegram chat SEES it (shared user pool) and can /use it. On
        // Telegram the list rides the event sink (text + switch buttons).
        let seen = list_text(
            &mut gateway,
            &mut events,
            "telegram",
            "339498819",
            "rob",
            "/sessions",
        )
        .await;
        assert_eq!(seen, vec!["📁 当前项目: alpha\ns1 claude"]);
        let used = gateway
            .handle_text("telegram", "339498819", "rob", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["using session s1\n↓ 查看状态 → /status"]);
    }

    /// v0.8.20 web↔IM convergence — a tenant's web console and their OWN IM bot
    /// (`telegram@<tid>`) are ONE identity (`user:<tid>`): the bot sees the
    /// tenant's web-created sessions AND its own; a DIFFERENT tenant's bot sees
    /// neither. (The admin/global bot keeps the shared-pool operator view, tested
    /// in `gateway_web_owned_session_visible_from_im`.)
    #[tokio::test]
    async fn gateway_web_and_tenant_bot_converge() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        // `alpha` belongs to tenant uaaa: a tenant reaches only what it owns, so
        // the fixture has to grant it a project rather than let it inherit the
        // daemon default (the cross-user hole `current_project_for` closed).
        seed_owned_project(&paths, "alpha", Some("user:uaaa"));
        let mut gateway = Gateway::new(fake.clone(), "alpha", paths.projects_root.join("alpha"));
        gateway.enable_project_creation(paths.clone());
        let mut events = gateway.subscribe_events();

        // Tenant uaaa creates a session on the WEB → owned user:uaaa.
        gateway
            .handle_text("web", "uaaa", "uaaa", "/new claude reviewer")
            .await
            .unwrap();
        // uaaa's OWN IM bot SEES it (convergence — the forward direction). On
        // Telegram the list rides the event sink (text + switch buttons).
        let seen = list_text(
            &mut gateway,
            &mut events,
            "telegram@uaaa",
            "111",
            "alice",
            "/sessions",
        )
        .await;
        assert!(
            seen.iter().any(|m| m.contains("s1")),
            "uaaa's bot sees its tenant's web session: {seen:?}"
        );

        // uaaa's bot creates a SECOND session → also user:uaaa.
        gateway
            .handle_text("telegram@uaaa", "111", "alice", "/new claude api")
            .await
            .unwrap();
        let both = list_text(
            &mut gateway,
            &mut events,
            "telegram@uaaa",
            "111",
            "alice",
            "/sessions",
        )
        .await;
        assert!(
            both.iter().any(|m| m.contains("s1")) && both.iter().any(|m| m.contains("s2")),
            "uaaa's bot sees BOTH its web + its own sessions: {both:?}"
        );

        // A DIFFERENT tenant's bot sees NEITHER (isolation holds).
        let other = list_text(
            &mut gateway,
            &mut events,
            "telegram@ubbb",
            "222",
            "bob",
            "/sessions",
        )
        .await;
        // Isolated down to the BANNER: ubbb owns nothing, so it reads
        // "(无项目)" instead of borrowing uaaa's slug — the project name of
        // another tenant used to leak here through the default-project fallback.
        assert_eq!(
            other,
            vec!["📁 当前项目: (无项目)\n暂无会话 —— /new 开一个"],
            "ubbb's bot is isolated from uaaa"
        );
    }

    /// CROSS-USER REGRESSION (2026-07-28 owner report: "IM receives other
    /// users' session messages") — a TENANT's web-created session must not surface in
    /// the admin/global IM bot. The old pool rule was a blanket "any `user:*`
    /// owner is shared", written when the web console had one (admin) identity;
    /// per-user web tokens then made every tenant's session visible there, and
    /// `/use` on a listed sid re-points that session's `reply_to` at the admin
    /// chat — so the tenant's answers/progress started landing in the owner's
    /// Telegram. The legit admin case (`user:web-api` ↔ IM) is unchanged and
    /// covered by `gateway_web_owned_session_visible_from_im`.
    #[tokio::test]
    async fn gateway_tenant_web_session_is_hidden_from_admin_im_bot() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        // Each identity owns its own project — nobody borrows the daemon default.
        seed_owned_project(&paths, "alpha", Some("user:uaaa"));
        seed_owned_project(&paths, "ops", Some("user:web-api"));
        let mut gateway = Gateway::new(fake.clone(), "alpha", paths.projects_root.join("alpha"));
        gateway.enable_project_creation(paths.clone());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("telegram", ["339498819".to_string()]);
        let mut events = gateway.subscribe_events();

        // Tenant uaaa creates a session on ITS web console → owner user:uaaa.
        gateway
            .handle_text("web", "uaaa", "uaaa", "/new claude reviewer")
            .await
            .unwrap();

        // The owner's global Telegram bot must NOT list it …
        let seen = list_text(
            &mut gateway,
            &mut events,
            "telegram",
            "339498819",
            "rob",
            "/sessions",
        )
        .await;
        // The owner's bot sees an empty fleet — and its banner names ITS OWN
        // project (`ops`), never the tenant's `alpha`.
        assert_eq!(
            seen,
            vec!["📁 当前项目: ops\n暂无会话 —— /new 开一个"],
            "a tenant's web session must not reach the admin/global IM bot: {seen:?}"
        );

        // … and must not be able to ADDRESS it (which is what re-points
        // `reply_to` and starts the cross-user push). Reads as unknown, so the
        // sid's existence leaks nothing either.
        let used = gateway
            .handle_text("telegram", "339498819", "rob", "/use s1")
            .await
            .unwrap();
        assert_eq!(
            used,
            vec!["unknown session for this chat: s1\n↓ 查看状态 → /status"]
        );

        // Another TENANT's web console is equally blind to it.
        let other_tenant = gateway
            .handle_text("web", "ubbb", "ubbb", "/sessions")
            .await
            .unwrap();
        assert!(
            !other_tenant.iter().any(|m| m.contains("s1")),
            "one tenant's console must not see another's sessions: {other_tenant:?}"
        );

        // The owner keeps their own web console pool (no over-correction).
        gateway
            .handle_text("web", "web-api", "web-api", "/new claude api")
            .await
            .unwrap();
        let admin_sees = list_text(
            &mut gateway,
            &mut events,
            "telegram",
            "339498819",
            "rob",
            "/sessions",
        )
        .await;
        assert!(
            admin_sees.iter().any(|m| m.contains("s2")),
            "the admin bot still drives its OWN web console sessions: {admin_sees:?}"
        );
    }

    /// v0.8.18 (owner) — IM `/new` with NO role token creates a ROLELESS session
    /// (bare claude). `hitl`/`terminal` are flags, never mistaken for a role, so
    /// `/new claude` and `/new claude hitl` are both roleless; an explicit role
    /// still works.
    #[tokio::test]
    async fn gateway_new_without_role_is_roleless() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        gateway
            .handle_text("tg", "c1", "u1", "/new claude")
            .await
            .unwrap();
        gateway
            .handle_text("tg", "c1", "u1", "/new claude hitl")
            .await
            .unwrap();
        gateway
            .handle_text("tg", "c1", "u1", "/new claude reviewer")
            .await
            .unwrap();

        let views = gateway.session_views();
        let s1 = views.iter().find(|v| v.sid == "s1").expect("s1");
        let s2 = views.iter().find(|v| v.sid == "s2").expect("s2");
        let s3 = views.iter().find(|v| v.sid == "s3").expect("s3");
        assert_eq!(s1.role, "", "/new claude → roleless");
        assert_eq!(s2.role, "", "/new claude hitl → roleless (hitl is a flag)");
        assert_eq!(s2.permission_mode, "hitl", "the hitl flag still applies");
        assert_eq!(s3.role, "reviewer", "/new claude reviewer → explicit role");
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
    async fn gateway_commands_switch_project_and_session() {
        let fake = Arc::new(FakeAdapter::default());
        // v0.8.22 P0-3/P0-4 — isolated tempdirs (see
        // `gateway_sessions_shows_model_and_context` for why the shared
        // "/tmp/alpha"/"/tmp/beta" literals are unsafe now that `/sessions`
        // also reads on-disk history).
        let tmp_alpha = tempfile::TempDir::new().unwrap();
        let tmp_beta = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp_alpha.path());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
        gateway.register_project("beta", tmp_beta.path());

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
            vec!["project set to beta (next message starts a session there)\n↓ 本项目会话 → /sessions"]
        );

        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        // v0.8.22 P0-3 — ordered by last_active desc: s2 (spawned after s1)
        // sorts first.
        let sessions = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["📁 当前项目: beta\ns2 claude\ns1 codex"]);

        let use_first = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(use_first, vec!["using session s1\n↓ 查看状态 → /status"]);
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
                        AgentVendor::Grok => codex.clone(), // tests: no dedicated grok fake
                        AgentVendor::Opencode => codex.clone(),
                        AgentVendor::Kimi => codex.clone(),
                    }
                },
            )
        };
        // v0.8.22 P0-3/P0-4 — isolated tempdirs (see
        // `gateway_sessions_shows_model_and_context` for why the shared
        // "/tmp/alpha"/"/tmp/beta" literals are unsafe now that `/sessions`
        // also reads on-disk history).
        let tmp_alpha = tempfile::TempDir::new().unwrap();
        let tmp_beta = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new_with_factory(factory, "alpha", tmp_alpha.path());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string(), "chat-2".to_string()]);
        gateway.register_project("beta", tmp_beta.path());

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

        // `all` lists the full cross-project fleet (default `/sessions` would now
        // scope to the current project, `beta`). v0.8.22 P0-3 — ordered by
        // last_active desc: s4 (most recently spawned) first, s1 last.
        let sessions = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions all")
            .await
            .unwrap();
        assert_eq!(
            sessions,
            vec!["📁 当前项目: beta\ns4 claude\ns3 codex\ns2 codex\ns1 claude"]
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
                        AgentVendor::Grok => codex.clone(),
                        AgentVendor::Opencode => codex.clone(),
                        AgentVendor::Kimi => codex.clone(),
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

    #[tokio::test]
    async fn im_model_receipt_appends_status_hint() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        fake.directive_script
            .lock()
            .await
            .push_back(DirectiveOutcome::Done {
                receipt: "已切换 model → opus（live）".into(),
            });
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha-model-hint-im");
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "/model opus")
            .await
            .unwrap();
        assert_eq!(
            replies,
            vec!["已切换 model → opus（live）\n↓ 查看状态 → /status"]
        );
    }

    #[tokio::test]
    async fn web_model_receipt_stays_byte_identical() {
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        fake.directive_script
            .lock()
            .await
            .push_back(DirectiveOutcome::Done {
                receipt: "已切换 model → opus（live）".into(),
            });
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha-model-hint-web");
        gateway
            .handle_text("web", "web-api", "web-api", "/new claude reviewer")
            .await
            .unwrap();

        let replies = gateway
            .handle_text("web", "web-api", "web-api", "/model opus")
            .await
            .unwrap();
        assert_eq!(replies, vec!["已切换 model → opus（live）"]);
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
        // Project dirs under the tempdir so each session's meta.json (the
        // Wave-2 SoT the restart rebuilds from) is isolated + auto-cleaned.
        let alpha = tmp.path().join("alpha");
        let beta = tmp.path().join("beta");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        let fake = Arc::new(FakeAdapter::default());

        let original_secret_s1;
        let original_secret_s2;
        {
            let mut gateway = Gateway::new(fake.clone(), "alpha", alpha.clone());
            // The owner's own chat(s): named in the bot allowlist ⇒ operator.
            gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
            gateway.register_project("beta", beta.clone());
            gateway.enable_persistence(tmp.path()).unwrap();
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
            original_secret_s1 = gateway.sessions.get("s1").unwrap().secret.clone();
            original_secret_s2 = gateway.sessions.get("s2").unwrap().secret.clone();
            assert_eq!(original_secret_s1.len(), 32);
            assert_eq!(original_secret_s2.len(), 32);
            assert_ne!(original_secret_s1, original_secret_s2);
        }

        let mut restored = Gateway::new(fake.clone(), "alpha", alpha);
        restored.register_project("beta", beta);
        restored.enable_persistence(tmp.path()).unwrap();
        // Wave-2 — load_state restores routing (sync); the live map is cold-start
        // rebuilt from each live sid's meta.json by the async restore step.
        restored.resume_restored_sessions().await;

        // v0.8.21 Wave-2 — the cto-gate secret is NOT persisted; a cold-start
        // rebuild MINTS a fresh one (the prior child died with the prior process,
        // so its secret is gone). The re-spawned child's env gets this new value,
        // so pane-env + the gate map stay in lockstep at a fresh 32-char secret
        // that differs from the pre-restart one.
        let restored_s1 = restored.sessions.get("s1").unwrap().secret.clone();
        let restored_s2 = restored.sessions.get("s2").unwrap().secret.clone();
        assert_eq!(restored_s1.len(), 32);
        assert_eq!(restored_s2.len(), 32);
        assert_ne!(
            restored_s1, original_secret_s1,
            "secret is re-minted on cold-start rebuild, not persisted"
        );
        assert_ne!(
            restored_s2, original_secret_s2,
            "secret is re-minted on cold-start rebuild, not persisted"
        );

        // v0.9.0 W5 (real-machine smoke fix) — the curated Claude stream-json
        // mcp.json MUST be re-written on cold-start rebuild with the FRESH
        // secret, else the resumed session keeps a stale bearer and loses every
        // ccteam MCP tool (can no longer delegate). Assert each session's
        // mcp.json now carries the RESTORED secret and NOT the original one.
        for (sid, restored_secret, original_secret) in [
            ("s1", &restored_s1, &original_secret_s1),
            ("s2", &restored_s2, &original_secret_s2),
        ] {
            let mcp = tmp
                .path()
                .join("beta")
                .join(".ccteam")
                .join("chat")
                .join(sid)
                .join("mcp.json");
            let body = std::fs::read_to_string(&mcp)
                .unwrap_or_else(|e| panic!("read {} after rebuild: {e}", mcp.display()));
            assert!(
                body.contains(restored_secret.as_str()),
                "{sid} mcp.json must be rewritten with the fresh secret after resume"
            );
            assert!(
                !body.contains(original_secret.as_str()),
                "{sid} mcp.json must NOT keep the stale pre-restart secret"
            );
        }

        // v0.8.22 P0-3 — ordered by last_active desc: s2 (spawned after s1,
        // pre-restart) sorts first.
        let sessions = restored
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["📁 当前项目: beta\ns2 claude\ns1 claude"]);

        assert_eq!(
            restored
                .handle_text("mock", "chat-1", "alice", "/use s1")
                .await
                .unwrap(),
            vec!["using session s1\n↓ 查看状态 → /status"]
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
            vec!["using session s2\n↓ 查看状态 → /status"]
        );
        let reply_s2 = restored
            .handle_text("mock", "chat-1", "alice", "after restart two")
            .await
            .unwrap();
        assert_eq!(reply_s2, vec!["beta-reviewer-s2 echo: after restart two"]);
        // 2 original creates + 2 cold-start rebuilds on restart (Wave-2 re-spawns
        // each live session; both were live, so both restart via start_thread).
        // /use s1 + /use s2 then hit the already-rebuilt sessions (no new spawn).
        assert_eq!(fake.starts.load(Ordering::SeqCst), 4);
    }

    /// Guards the fix for the incident where a breaking routing-schema jump
    /// (or any other loss of `routing.json`) made a live chat session read as
    /// "vanished": with no routing.json to load, `load_state` must fall back
    /// to each project's `meta.json` to rebuild the live-set, so the session
    /// is findable again via `/sessions` + `/use` instead of gone for good.
    #[tokio::test]
    async fn gateway_recovers_live_sessions_from_meta_when_routing_json_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let beta = tmp.path().join("beta");
        std::fs::create_dir_all(&beta).unwrap();
        let fake = Arc::new(FakeAdapter::default());

        {
            let mut gateway = Gateway::new(fake.clone(), "beta", beta.clone());
            gateway.enable_persistence(tmp.path()).unwrap();
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap();
        }

        // Simulate the incident: routing.json is gone (never written yet under
        // the new schema, or lost some other way), but the session's meta.json
        // — an independent per-project file — is untouched.
        std::fs::remove_file(crate::routing_state_path_in(tmp.path())).unwrap();
        assert!(ccteam_harness::execution::session_meta::session_meta_path(&beta, "s1").exists());

        let mut restored = Gateway::new(fake.clone(), "beta", beta);
        restored.enable_persistence(tmp.path()).unwrap();
        restored.resume_restored_sessions().await;

        // s1 is live again and listed — not silently gone — even though
        // routing.json never said so. The chat's FOCUS wasn't (and can't
        // losslessly be) restored, so `/use` is still needed once.
        let sessions = restored
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["📁 当前项目: beta\ns1 claude"]);

        assert_eq!(
            restored
                .handle_text("mock", "chat-1", "alice", "/use s1")
                .await
                .unwrap(),
            vec!["using session s1\n↓ 查看状态 → /status"]
        );
        let reply = restored
            .handle_text("mock", "chat-1", "alice", "after routing.json loss")
            .await
            .unwrap();
        assert_eq!(
            reply,
            vec!["beta-reviewer-s1 echo: after routing.json loss"]
        );
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
        // Project dir under the tempdir so the sessions' meta.json (Wave-2 SoT)
        // is isolated — the restart rebuilds the live set from it.
        let project_dir = tmp.path().join("alpha-reuse");
        std::fs::create_dir_all(&project_dir).unwrap();
        let fake = Arc::new(FakeAdapter::default());

        {
            let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());
            gateway.enable_persistence(tmp.path()).unwrap();
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
            // Free s1 (persists the removal from live_sids + the bumped counter).
            gateway.stop_session("s1").await.unwrap();
            assert!(gateway.session_resolve("s1").is_none());
            assert!(gateway.session_resolve("s2").is_some());
        }

        // Rebuild from the same on-disk state (routing.json + next-sid + meta).
        let mut restored = Gateway::new(fake.clone(), "alpha", project_dir);
        restored.enable_persistence(tmp.path()).unwrap();
        // Wave-2 — cold-start rebuild the live set (only s2; s1 was stopped, so
        // it left live_sids and is not rebuilt — its meta.json lingers as history).
        restored.resume_restored_sessions().await;
        assert!(
            restored.session_resolve("s2").is_some(),
            "the surviving same-role session must restore after restart"
        );
        assert!(
            restored.session_resolve("s1").is_none(),
            "the stopped session must not resurrect (left live_sids; history only)"
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

    /// v0.8.21 Wave-2 — the monotonic sid counter lives in its OWN file
    /// (`state/sessions/next-sid`), NOT derived from `max(meta sid)` and NOT
    /// inside routing.json. So even if routing.json AND every `meta.json` are
    /// wiped (a state purge / `rm -rf .ccteam/chat`), the next create never
    /// RE-USES a freed sid — the "sid monotonic, never reused" red line holds
    /// independently of the routing table and the session history on disk.
    #[tokio::test]
    async fn next_sid_monotonic_survives_routing_and_meta_wipe() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("alpha");
        std::fs::create_dir_all(&project_dir).unwrap();
        let fake = Arc::new(FakeAdapter::default());
        {
            let mut gateway = Gateway::new(fake.clone(), "alpha", project_dir.clone());
            gateway.enable_persistence(tmp.path()).unwrap();
            for _ in 0..3 {
                gateway
                    .create_session_api(
                        "alpha".into(),
                        "reviewer".into(),
                        AgentVendor::Claude,
                        ccteam_harness::PermissionMode::Skip,
                    )
                    .await
                    .unwrap();
            }
            // s1, s2, s3 created → the next-sid counter file now reads 3.
        }
        // Wipe BOTH the routing snapshot AND every session's meta.json, leaving
        // ONLY the next-sid counter file behind.
        let _ = std::fs::remove_file(crate::routing_state_path_in(tmp.path()));
        let _ = std::fs::remove_dir_all(project_dir.join(".ccteam").join("chat"));

        let mut restored = Gateway::new(fake.clone(), "alpha", project_dir);
        restored.enable_persistence(tmp.path()).unwrap();
        // Nothing to rebuild (routing + meta gone) — but the counter survives.
        restored.resume_restored_sessions().await;
        let next = restored
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap();
        assert_eq!(
            next, "s4",
            "next-sid persists independently of routing.json + meta.json — a wiped \
             state must never recycle s1/s2/s3"
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
        // v0.8.21 Wave-2 — the out-of-process reader enumerates projects from
        // config.yaml under the ccteam_root, then resolves each routing.json
        // live sid to its meta.json. So register the project in config + spawn.
        let root = tmp.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        let beta = tmp.path().join("beta");
        std::fs::create_dir_all(&beta).unwrap();
        ccteam_core::config::upsert_project(
            &root,
            ccteam_core::config::ProjectEntry {
                slug: "beta".to_string(),
                path: beta.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "beta", beta);
        gateway.enable_persistence(&root).unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        // v0.8.8 F1 — the canonical pane name is keyed by the session sid
        // (`s1`), not the role: a same-role second session would otherwise
        // collide on one name. The first `/new` minted s1.
        let names = tracked_chat_session_names(&root).unwrap();
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
        // gateway, persist, then read the flat rows back out-of-process. Wave-2:
        // the reader resolves routing.json live_sids ⋈ each session's meta.json,
        // enumerating project dirs from config.yaml under the ccteam_root.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        let alpha = tmp.path().join("alpha");
        std::fs::create_dir_all(&alpha).unwrap();
        ccteam_core::config::upsert_project(
            &root,
            ccteam_core::config::ProjectEntry {
                slug: "alpha".to_string(),
                path: alpha.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", alpha);
        gateway.enable_persistence(&root).unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex builder")
            .await
            .unwrap();

        let rows = tracked_chat_sessions(&root).unwrap();
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
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
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
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
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
        // v0.8.22 P0-3/P0-4 — isolated tempdirs (see
        // `gateway_sessions_shows_model_and_context` for why the shared
        // "/tmp/alpha"/"/tmp/beta" literals are unsafe now that `/sessions`
        // also reads on-disk history).
        let tmp_alpha = tempfile::TempDir::new().unwrap();
        let tmp_beta = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp_alpha.path());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
        gateway.register_project("beta", tmp_beta.path());

        // Active session s1 lives in project alpha.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let before = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(before, vec!["📁 当前项目: alpha\ns1 claude"]);

        // /cd to beta, where no session exists yet, clears the active session.
        let cd = gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        assert_eq!(
            cd,
            vec!["project set to beta (next message starts a session there)\n↓ 本项目会话 → /sessions"]
        );

        // The next plain message must route into a beta session, not back s1.
        // v0.9.0 neutralization — the implicit spawn is roleless (`beta--s2`).
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "where am i")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta--s2 echo: where am i"]);

        // `all` shows both projects (default `/sessions` now scopes to `beta`).
        // v0.8.22 P0-3 — ordered by last_active desc: s2 (just spawned) sorts
        // before s1.
        let after = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions all")
            .await
            .unwrap();
        // s2's first plain message ("where am i") auto-titles it, but the title
        // now rides the switch BUTTON, not the text row (see
        // `session_switch_options`); s1 never sent a plain message, so it is
        // untitled either way. s2 is roleless → empty role field
        // (`s2 claude.beta`).
        assert_eq!(after, vec!["📁 当前项目: beta\ns2 claude\ns1 claude"]);
    }

    #[tokio::test]
    async fn gateway_cd_adopts_existing_session_in_target_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
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
        assert_eq!(
            cd_back,
            vec!["project set to alpha (switched to s1)\n↓ 本项目会话 → /sessions"]
        );

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
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("mock", ["chat-1".to_string()]);
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
        // so the next message spawns a ROLELESS agent in beta (v0.9.0
        // neutralization — the implicit first-message spawn seeds no role), not
        // the bot. Fake echo format is `{project}-{role}-{sid}`, so an empty
        // role renders as `beta--s1`.
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta--s1 echo: hello"]);
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
            rendered.contains("s1:alpha:claude:lead"),
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
    async fn gateway_sessions_isolated_across_frontends_own_only() {
        // v0.8.18 柱2 档0 — own-only across frontends: a web chat (and another
        // IM chat) does NOT see or address a session a DIFFERENT chat created.
        // The pre-0.8.18 "web is a global operator view" + cross-frontend-by-
        // project sharing are gone; both return for ONE user via 档1 (a shared
        // identity linking the web token to the chat_id).
        let fake = Arc::new(FakeAdapter::default());
        // v0.8.22 P0-3/P0-4 — isolated tempdir (see
        // `gateway_sessions_shows_model_and_context` for why the shared
        // "/tmp/alpha" literal is unsafe now that `/sessions` also reads
        // on-disk history).
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());
        let mut events = gateway.subscribe_events();

        // A Telegram chat creates a session.
        gateway
            .handle_text("telegram", "tg-1", "rob", "/new claude assistant")
            .await
            .unwrap();

        // The web console no longer sees a session it didn't create (own-only).
        let listing = gateway
            .handle_text("web", "web-api", "web-api", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            listing,
            vec!["no sessions"],
            "web should NOT see the IM-created session under own-only: {listing:?}"
        );

        // …and cannot /use it (refused as unknown — no existence leak).
        let used = gateway
            .handle_text("web", "web-chat", "web-user", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["unknown session for this chat: s1"]);

        // A different Telegram chat in the same default project also does NOT
        // see it (the same-project sharing leak is gone). On Telegram the list
        // rides the event sink (text + switch buttons).
        let other = list_text(
            &mut gateway,
            &mut events,
            "telegram",
            "tg-2",
            "bob",
            "/sessions",
        )
        .await;
        assert_eq!(other, vec!["📁 当前项目: alpha\n暂无会话 —— /new 开一个"]);

        // The OWNER (tg-1) still sees AND addresses its own session — isolation
        // doesn't break the owner's own flow.
        let owner_sees = list_text(
            &mut gateway,
            &mut events,
            "telegram",
            "tg-1",
            "rob",
            "/sessions",
        )
        .await;
        assert!(
            owner_sees.iter().any(|r| r.contains("s1 claude")),
            "owner should see its own session: {owner_sees:?}"
        );
        let owner_uses = gateway
            .handle_text("telegram", "tg-1", "rob", "/use s1")
            .await
            .unwrap();
        assert_eq!(owner_uses, vec!["using session s1\n↓ 查看状态 → /status"]);
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
        // v0.8.22 P0-3/P0-4 — isolated tempdir: the count-based assertion
        // below (`starts_with('s')`) would over-count if a "最近结束" history
        // row leaked in from another test sharing the literal "/tmp/alpha".
        let proj = tempfile::TempDir::new().unwrap();
        let mut gateway = Gateway::new(fake, "alpha", proj.path());

        let first = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude assistant")
            .await
            .unwrap();
        assert_eq!(first, vec!["created session s1\n↓ 查看状态 → /status"]);
        // Same project + role → a SECOND, distinct session s2 (no reuse).
        let again = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude assistant")
            .await
            .unwrap();
        assert_eq!(
            again,
            vec!["created session s2\n↓ 查看状态 → /status"],
            "F1: a repeat /new of the same role must mint a NEW sid, not reuse s1"
        );
        // A third /new (different role) → s3.
        let other_role = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(other_role, vec!["created session s3\n↓ 查看状态 → /status"]);

        // Three sessions tracked — two same-role (s1, s2) + one (s3).
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            listing[0].lines().skip(1).count(),
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
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
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
        assert_eq!(
            switched,
            vec!["switched session s1 to role reviewer\n↓ 查看状态 → /status"]
        );

        // A follow-up turn now routes to the reviewer pane under the SAME sid.
        let after = gateway
            .handle_text("mock", "chat-1", "alice", "still here?")
            .await
            .unwrap();
        assert_eq!(after, vec!["alpha-reviewer-s1 echo: still here?"]);

        // The session list shows the new role bound to the same sid (no s2).
        // (The auto-title from "hi" is sid-scoped and survives the `/role`
        // re-spawn, but now rides the switch button, not this text row.)
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(listing, vec!["📁 当前项目: alpha\ns1 claude"]);

        // `/use s1` still resolves the same (now-reviewer) session.
        let used = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(used, vec!["using session s1\n↓ 查看状态 → /status"]);

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
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
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

        // The session is still resolvable, still s1, still `cto`. (The title
        // auto-set from "hi" now rides the switch button, not this text row.)
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(listing, vec!["📁 当前项目: alpha\ns1 claude"]);
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
        assert_eq!(
            switched,
            vec!["switched session s1 to role reviewer\n↓ 查看状态 → /status"]
        );
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

    // ===== v0.8.22 P1 — session-title system: `/rename` =====

    /// `/rename` with no active session gives the usage/context error (not a
    /// system-fault message) — mirrors `/role`'s no-active-session path.
    #[tokio::test]
    async fn gateway_rename_with_no_active_session_errors() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha-rename-noop");

        let err = gateway
            .handle_text("mock", "chat-1", "alice", "/rename hello")
            .await
            .expect_err("/rename with no active session should error");
        assert!(
            format!("{err:#}").contains("活动会话"),
            "expected the no-active-session hint: {err:#}"
        );
    }

    /// `/rename` with a blank title is rejected with a usage hint (never a
    /// silent no-op or a system-fault message).
    #[tokio::test]
    async fn gateway_rename_blank_title_is_rejected() {
        let fake = Arc::new(FakeAdapter::default());
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();

        let err = gateway
            .handle_text("mock", "chat-1", "alice", "/rename    ")
            .await
            .expect_err("/rename with a blank title should error");
        assert!(
            format!("{err:#}").contains("用法"),
            "expected the usage hint: {err:#}"
        );
    }

    /// `/rename` on the current session sets its title (rule-based, no LLM —
    /// verified by the input surviving verbatim short and getting collapsed
    /// when it has extra whitespace), and the title is STICKY: a later plain
    /// message must NOT overwrite it via the first-message auto-title path
    /// (the precedence `apply_title` enforces).
    #[tokio::test]
    async fn gateway_rename_sets_sticky_title_surfaced_in_sessions() {
        let fake = Arc::new(FakeAdapter::default());
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();

        let renamed = gateway
            .handle_text("mock", "chat-1", "alice", "/rename  my   custom title  ")
            .await
            .unwrap();
        // The receipt names the session, the new title, and — always — what
        // the vendor did with it (this fake vendor has no title surface, so
        // the honest answer is "ccteam-side only").
        assert_eq!(
            renamed,
            vec![
                "已重命名 s1 →「my custom title」\n· 仅 ccteam 侧(claude 无会话标题接口)\
                 \n↓ 本项目会话 → /sessions"
            ]
        );

        // The title moved off the /sessions text row onto the switch button.
        let listing = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(listing, vec!["📁 当前项目: alpha\ns1 claude"]);
        // …and the rename SURFACES on the picker button.
        let chat = ChatKey::new("mock", "chat-1", "alice");
        let s1_button = |g: &Gateway| {
            g.session_switch_options(&chat, false)
                .into_iter()
                .find(|o| o.id == "s1")
                .map(|o| o.label.trim_end_matches('\u{2800}').to_string())
        };
        assert_eq!(
            s1_button(&gateway).as_deref(),
            Some("✓ s1 claude (my custom title)")
        );

        // A later plain message must NOT clobber the rename via auto-title.
        gateway
            .handle_text("mock", "chat-1", "alice", "totally different first message")
            .await
            .unwrap();
        let listing2 = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(listing2, vec!["📁 当前项目: alpha\ns1 claude"]);
        assert_eq!(
            s1_button(&gateway).as_deref(),
            Some("✓ s1 claude (my custom title)"),
            "an explicit /rename must survive a later message (sticky user title)"
        );
    }

    /// `[<sid>] <title>` parsing: a leading `s<N>` is a TARGET only when a
    /// title follows it, so `/rename s3` still titles the CURRENT session
    /// "s3" instead of silently renaming a different session to nothing.
    #[test]
    fn rename_arg_splits_a_leading_sid_only_when_a_title_follows() {
        assert_eq!(
            split_leading_sid("s12 release checklist"),
            Some(("s12", "release checklist"))
        );
        // Extra whitespace around the title is the caller's to trim.
        assert_eq!(
            split_leading_sid("s3   spaced  out "),
            Some(("s3", "spaced  out"))
        );
        // No remainder ⇒ the whole thing is a title for the current session.
        assert_eq!(split_leading_sid("s3"), None);
        assert_eq!(split_leading_sid("s3   "), None);
        // Not a sid shape ⇒ plain title, even when it starts with `s`.
        assert_eq!(split_leading_sid("session cleanup"), None);
        assert_eq!(split_leading_sid("s12a bad shape"), None);
        assert_eq!(split_leading_sid("ship the rename"), None);
    }

    /// `/rename <sid> <title>` renames a session that is NOT the current one —
    /// the same leading-id convention `/stop` / `/use` / `/interrupt` take —
    /// and the receipt names the session actually renamed (so a mis-parse is
    /// visible immediately rather than silently retitling the current chat).
    #[tokio::test]
    async fn gateway_rename_targets_an_explicit_sid() {
        let fake = Arc::new(FakeAdapter::default());
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();
        // s2 is current; rename s1 by id.
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "/rename s1 the older one")
            .await
            .unwrap();
        assert!(
            reply[0].starts_with("已重命名 s1 →「the older one」"),
            "receipt must name the targeted sid: {:?}",
            reply
        );
        let s1 = read_session_meta(tmp.path(), "s1").unwrap();
        let s2 = read_session_meta(tmp.path(), "s2").unwrap();
        assert_eq!(s1.title.as_deref(), Some("the older one"));
        assert_eq!(s1.title_source, Some(TitleSource::User));
        assert!(s2.title.is_none(), "the current session must be untouched");
    }

    /// A sid another chat owns is not renameable — and reads exactly like an
    /// unknown one (no existence disclosure), matching `/stop`'s answer.
    #[tokio::test]
    async fn gateway_rename_refuses_a_foreign_sid() {
        let fake = Arc::new(FakeAdapter::default());
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-2", "bob", "/new claude cto")
            .await
            .unwrap();

        let reply = gateway
            .handle_text("mock", "chat-2", "bob", "/rename s1 not mine")
            .await
            .unwrap();
        assert!(
            reply[0].starts_with("unknown session for this chat: s1"),
            "{:?}",
            reply
        );
        let unknown = gateway
            .handle_text("mock", "chat-2", "bob", "/rename s99 nope")
            .await
            .unwrap();
        assert!(
            unknown[0].starts_with("unknown session for this chat: s99"),
            "a foreign sid and an unknown sid must read identically: {:?}",
            unknown
        );
        assert!(read_session_meta(tmp.path(), "s1").unwrap().title.is_none());
    }

    /// A STOPPED session is renameable: `meta.json` outlives the live map, and
    /// the web rail offers rename on history rows. The vendor push still runs,
    /// with NO live thread — which is exactly what tells a vendor whose title
    /// surface is an RPC (codex) to answer `Deferred`.
    #[tokio::test]
    async fn gateway_rename_works_on_a_stopped_session() {
        let fake = Arc::new(FakeAdapter::default());
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/stop s1")
            .await
            .unwrap();
        assert!(!gateway.is_session_live("s1"));

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "/rename s1 archived work")
            .await
            .unwrap();
        assert!(
            reply[0].starts_with("已重命名 s1 →「archived work」"),
            "a stopped session must rename like a live one: {:?}",
            reply
        );
        let meta = read_session_meta(tmp.path(), "s1").unwrap();
        assert_eq!(meta.title.as_deref(), Some("archived work"));
        let pushes = fake.title_pushes.lock().await.clone();
        assert_eq!(
            pushes,
            vec![("s1".to_string(), "archived work".to_string(), false)],
            "the vendor push must run with no live thread"
        );
    }

    /// The rename reaches the VENDOR's own title surface, and the receipt says
    /// so — the whole point of wiring it through the adapter rather than only
    /// writing ccteam's meta.json.
    #[tokio::test]
    async fn gateway_rename_pushes_the_title_to_the_vendor() {
        let fake = Arc::new(FakeAdapter::default().with_title_surface());
        let tmp = tempfile::tempdir().unwrap();
        seed_role(tmp.path(), "cto");
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude cto")
            .await
            .unwrap();

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "/rename ship it")
            .await
            .unwrap();
        assert!(
            reply[0].contains("已同步到 claude 自己的会话标题"),
            "a real push must be reported as synced: {:?}",
            reply
        );
        let pushes = fake.title_pushes.lock().await.clone();
        assert_eq!(
            pushes,
            vec![("s1".to_string(), "ship it".to_string(), true)],
            "the live session's push carries its thread handle"
        );

        // Renaming again shows what it replaced.
        let again = gateway
            .handle_text("mock", "chat-1", "alice", "/rename ship it twice")
            .await
            .unwrap();
        assert!(
            again[0].starts_with("已重命名 s1 「ship it」→「ship it twice」"),
            "the receipt must show the previous title: {:?}",
            again
        );
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

    // ========================================================================
    // v0.9.0 W2 (F2/F5/F7) — delegation semantics, guardrails, reliability.
    // ========================================================================

    /// Build a delegation-wired gateway behind an `Arc<Mutex>`: event sink
    /// (drained) + delegation notifier tx + the notifier task. Mirrors the
    /// daemon startup order (delegation_tx BEFORE set_event_sink so pumps
    /// capture it). Returns the shared handle.
    async fn delegation_gateway_with_factory(
        project_dir: &std::path::Path,
        factory: crate::daemon::AdapterFactory,
    ) -> Arc<tokio::sync::Mutex<Gateway>> {
        let mut gw = Gateway::new_with_factory(factory, "alpha", project_dir);
        gw.register_project("alpha", project_dir);
        let (dtx, drx) = tokio::sync::mpsc::unbounded_channel();
        gw.set_delegation_notifier_tx(dtx);
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });
        let gateway = Arc::new(tokio::sync::Mutex::new(gw));
        // Production runs reconciliation before draining live signals. Finish
        // that empty-project startup phase before returning the fixture too,
        // so a newly-created watch cannot race a historical replay.
        Gateway::reconcile_delegations(Arc::clone(&gateway)).await;
        let notifier_gateway = Arc::clone(&gateway);
        tokio::spawn(async move {
            let mut drx = drx;
            while let Some(signal) = drx.recv().await {
                notifier_gateway
                    .lock()
                    .await
                    .deliver_delegation_signal(signal)
                    .await;
            }
        });
        gateway
    }

    async fn delegation_gateway(project_dir: &std::path::Path) -> Arc<tokio::sync::Mutex<Gateway>> {
        // A FRESH fake per spawn so each session's pump has its OWN
        // `events_notify` — a single shared fake's `notify_one` could wake the
        // wrong pump when two sessions (parent + child) run concurrently.
        let factory: crate::daemon::AdapterFactory = Arc::new(|vendor, _protocol| {
            // `with_turn_boundary` mirrors every REAL adapter (all five emit
            // `TurnCompleted`) — required since v0.9.5: the delegation
            // notification fires on the turn boundary, not per answer.
            Arc::new(FakeAdapter::new(vendor).with_turn_boundary())
                as Arc<dyn HarnessAdapter + Send + Sync>
        });
        delegation_gateway_with_factory(project_dir, factory).await
    }

    /// e2e: an Ambient spawn records the parent lineage + trigger + title; a
    /// notifying dispatch wakes the parent with a `[ccteam]` turn; the child's
    /// turn is durably on disk by the time the notification lands (ordering).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn delegation_e2e_spawn_dispatch_notifies_parent_and_ordering() {
        use ccteam_harness::execution::turns_mirror::read_all_turns;
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let gateway = delegation_gateway(&project_dir).await;

        let parent_sid = {
            let mut gw = gateway.lock().await;
            gw.create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid
        };
        let child_sid = {
            let mut gw = gateway.lock().await;
            gw.create_delegated_session(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: parent_sid.clone(),
                    depth: 0,
                    role: String::new(),
                }),
                Some("research task".into()),
            )
            .await
            .unwrap()
            .sid
        };
        // Child meta records the delegation lineage + spawn trigger + title.
        let cmeta = read_session_meta(&project_dir, &child_sid).unwrap();
        assert_eq!(cmeta.parent_sid.as_deref(), Some(parent_sid.as_str()));
        assert_eq!(cmeta.delegation_depth, 1);
        assert_eq!(cmeta.trigger.as_deref(), Some("session_spawn"));
        assert_eq!(cmeta.title.as_deref(), Some("research task"));

        // Dispatch (arm the watch + drive the child's turn).
        {
            let mut gw = gateway.lock().await;
            gw.arm_delegation_watch(
                &child_sid,
                &parent_sid,
                ccteam_harness::NotifyMode::Final,
                Some("research task".into()),
                None,
            );
            gw.submit_to_sid(&child_sid, "do the research".into())
                .await
                .unwrap();
        }

        // The notifier delivers a completion turn to the parent (poll off-lock).
        let mut notified = false;
        for _ in 0..200 {
            let turns = read_all_turns(&project_dir, &parent_sid).unwrap_or_default();
            if turns.iter().any(|t| {
                t.user.contains("[ccteam] delegated session")
                    || t.assistant.contains("[ccteam] delegated session")
            }) {
                notified = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            notified,
            "parent must receive the `[ccteam]` completion notification"
        );
        // ORDERING: the child's turn is durably on disk (collect sees it).
        let child_turns = read_all_turns(&project_dir, &child_sid).unwrap_or_default();
        assert!(
            child_turns
                .iter()
                .any(|t| t.assistant.contains("echo: do the research")),
            "child turn is durably appended BEFORE the notification (read-your-writes)"
        );
    }

    /// Count the `[ccteam]` notification turns delivered to `sid`.
    fn ccteam_notification_turns(
        project_dir: &std::path::Path,
        sid: &str,
    ) -> Vec<ccteam_harness::execution::turns_mirror::TurnRecord> {
        ccteam_harness::execution::turns_mirror::read_all_turns(project_dir, sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.user.contains("[ccteam] delegated session"))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn delegation_vendor_fatal_turn_is_explicitly_marked_for_parent() {
        const CAPACITY_ERROR: &str = "Selected model is at capacity. Please try a different model.";
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let factory: crate::daemon::AdapterFactory = Arc::new(|vendor, _protocol| {
            let fake = if vendor == AgentVendor::Codex {
                FakeAdapter::new(vendor).with_turn_failure(CAPACITY_ERROR)
            } else {
                FakeAdapter::new(vendor).with_turn_boundary()
            };
            Arc::new(fake) as Arc<dyn HarnessAdapter + Send + Sync>
        });
        let gateway = delegation_gateway_with_factory(&project_dir, factory).await;

        let (parent_sid, child_sid) = {
            let mut gw = gateway.lock().await;
            let parent = gw
                .create_session_api(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Claude,
                    PermissionMode::Skip,
                )
                .await
                .unwrap()
                .sid;
            let child = gw
                .create_delegated_session(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Codex,
                    PermissionMode::Skip,
                    SessionProtocol::StreamJson,
                    "web-api".into(),
                    SpawnTuning::default(),
                    Some(DelegationParent {
                        sid: parent.clone(),
                        depth: 0,
                        role: String::new(),
                    }),
                    Some("capacity probe".into()),
                )
                .await
                .unwrap()
                .sid;
            gw.arm_delegation_watch(
                &child,
                &parent,
                ccteam_harness::NotifyMode::Final,
                Some("capacity probe".into()),
                None,
            );
            gw.submit_to_sid(&child, "run the task".into())
                .await
                .unwrap();
            (parent, child)
        };

        let mut notification = None;
        for _ in 0..200 {
            notification =
                ccteam_harness::execution::turns_mirror::read_all_turns(&project_dir, &parent_sid)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|turn| turn.user.contains(CAPACITY_ERROR));
            if notification.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let notification = notification.expect("parent receives vendor-fatal notification");
        assert!(
            notification
                .user
                .starts_with("[ccteam] [delegation completed with VENDOR ERROR]"),
            "vendor-fatal notification must lead with an explicit marker: {}",
            notification.user
        );
        assert!(notification.user.contains(CAPACITY_ERROR));
        assert!(notification
            .user
            .contains(&format!("session_collect{{sid:{child_sid}, tail:true}}")));

        let child_failure =
            ccteam_harness::execution::turns_mirror::read_all_turns(&project_dir, &child_sid)
                .unwrap()
                .into_iter()
                .find(|turn| turn.assistant == CAPACITY_ERROR)
                .expect("vendor failure is durably mirrored in the child ledger");
        let child_failure = serde_json::to_value(child_failure).unwrap();
        assert_eq!(child_failure["outcome"], "failed");
        assert_eq!(child_failure["error_kind"], "turn_failed");
        assert_eq!(child_failure["error"], CAPACITY_ERROR);
    }

    /// v0.9.5 feedback fix (P0) — a codex-shaped chatty child (several
    /// mirrored assistant messages inside ONE vendor turn) must produce
    /// exactly ONE notification for the default `final` watch: at the turn
    /// boundary, explicitly marked idle, folding the interim count. Interim
    /// signals must not notify; a replayed boundary must dedup.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn delegation_final_mode_notifies_once_per_turn_and_marks_idle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let gateway = delegation_gateway(&project_dir).await;

        let (parent_sid, child_sid) = {
            let mut gw = gateway.lock().await;
            let parent = gw
                .create_session_api(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Claude,
                    PermissionMode::Skip,
                )
                .await
                .unwrap()
                .sid;
            let child = gw
                .create_delegated_session(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Codex,
                    PermissionMode::Skip,
                    SessionProtocol::StreamJson,
                    "web-api".into(),
                    SpawnTuning::default(),
                    Some(DelegationParent {
                        sid: parent.clone(),
                        depth: 0,
                        role: String::new(),
                    }),
                    Some("long wave".into()),
                )
                .await
                .unwrap()
                .sid;
            gw.arm_delegation_watch(
                &child,
                &parent,
                ccteam_harness::NotifyMode::Final,
                Some("long wave".into()),
                None,
            );
            (parent, child)
        };

        let interim = |n: u32| crate::delegation::DelegationSignal {
            child_sid: child_sid.clone(),
            turn_id: format!("{child_sid}-{n}"),
            tail: format!("narration checkpoint {n}"),
            vendor: AgentVendor::Codex,
            host: "local".into(),
            boundary: false,
            vendor_error: false,
            interim_notes: 0,
            covered_turns: vec![format!("{child_sid}-{n}")],
        };
        // Three interim narration messages inside the running turn → silence.
        {
            let mut gw = gateway.lock().await;
            for n in 1..=3 {
                gw.deliver_delegation_signal(interim(n)).await;
            }
        }
        assert!(
            ccteam_notification_turns(&project_dir, &parent_sid).is_empty(),
            "interim narration must NOT wake a `final` watch"
        );

        // The vendor turn boundary → exactly one notification, marked idle.
        let boundary = crate::delegation::DelegationSignal {
            child_sid: child_sid.clone(),
            turn_id: format!("{child_sid}-4"),
            tail: "wave finished: 3 cards done".into(),
            vendor: AgentVendor::Codex,
            host: "local".into(),
            boundary: true,
            vendor_error: false,
            interim_notes: 3,
            covered_turns: (1..=4).map(|n| format!("{child_sid}-{n}")).collect(),
        };
        gateway
            .lock()
            .await
            .deliver_delegation_signal(boundary.clone())
            .await;
        let notes = ccteam_notification_turns(&project_dir, &parent_sid);
        assert_eq!(notes.len(), 1, "exactly ONE notification per vendor turn");
        assert!(
            notes[0]
                .user
                .contains("is now IDLE, waiting for the next dispatch"),
            "notification states the child went idle: {}",
            notes[0].user
        );
        assert!(
            notes[0].user.contains("3 interim note(s)"),
            "notification folds the interim count: {}",
            notes[0].user
        );
        assert!(notes[0].user.contains("wave finished: 3 cards done"));

        // Replay (at-least-once upstream) → deduped, still one notification.
        gateway
            .lock()
            .await
            .deliver_delegation_signal(boundary)
            .await;
        assert_eq!(
            ccteam_notification_turns(&project_dir, &parent_sid).len(),
            1,
            "boundary replay must dedup"
        );
    }

    /// v0.9.5 feedback fix — an `all` watch still gets the per-message
    /// firehose (labeled still-working) PLUS the idle boundary wake-up; an
    /// `off` watch gets nothing while `delegation_completed` still lands in
    /// progress.jsonl at the boundary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn delegation_all_and_off_modes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let gateway = delegation_gateway(&project_dir).await;

        let (parent_sid, child_sid) = {
            let mut gw = gateway.lock().await;
            let parent = gw
                .create_session_api(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Claude,
                    PermissionMode::Skip,
                )
                .await
                .unwrap()
                .sid;
            let child = gw
                .create_delegated_session(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Codex,
                    PermissionMode::Skip,
                    SessionProtocol::StreamJson,
                    "web-api".into(),
                    SpawnTuning::default(),
                    Some(DelegationParent {
                        sid: parent.clone(),
                        depth: 0,
                        role: String::new(),
                    }),
                    None,
                )
                .await
                .unwrap()
                .sid;
            gw.arm_delegation_watch(&child, &parent, ccteam_harness::NotifyMode::All, None, None);
            (parent, child)
        };

        let signal = |n: u32, boundary: bool| crate::delegation::DelegationSignal {
            child_sid: child_sid.clone(),
            turn_id: format!("{child_sid}-{n}"),
            tail: format!("message {n}"),
            vendor: AgentVendor::Codex,
            host: "local".into(),
            boundary,
            vendor_error: false,
            interim_notes: if boundary { n as usize - 1 } else { 0 },
            covered_turns: vec![format!("{child_sid}-{n}")],
        };
        {
            let mut gw = gateway.lock().await;
            gw.deliver_delegation_signal(signal(1, false)).await;
            gw.deliver_delegation_signal(signal(2, false)).await;
            // Boundary carries the same turn_id as the last interim (the
            // final answer IS the last mirrored message) — the distinct
            // `#final` dedup key must still let the idle wake-up through.
            let mut b = signal(2, true);
            b.interim_notes = 1;
            gw.deliver_delegation_signal(b).await;
        }
        let notes = ccteam_notification_turns(&project_dir, &parent_sid);
        assert_eq!(notes.len(), 3, "all-mode: 2 interim + 1 boundary");
        assert!(notes[0].user.contains("still WORKING, no action needed"));
        assert!(notes[1].user.contains("still WORKING, no action needed"));
        assert!(notes[2].user.contains("is now IDLE"));

        // `off` mode: nothing notifies, but the boundary still lands
        // delegation_completed in progress.jsonl.
        let (parent2, child2) = {
            let mut gw = gateway.lock().await;
            let parent = gw
                .create_session_api(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Claude,
                    PermissionMode::Skip,
                )
                .await
                .unwrap()
                .sid;
            let child = gw
                .create_delegated_session(
                    "alpha".into(),
                    String::new(),
                    AgentVendor::Codex,
                    PermissionMode::Skip,
                    SessionProtocol::StreamJson,
                    "web-api".into(),
                    SpawnTuning::default(),
                    Some(DelegationParent {
                        sid: parent.clone(),
                        depth: 0,
                        role: String::new(),
                    }),
                    None,
                )
                .await
                .unwrap()
                .sid;
            gw.arm_delegation_watch(&child, &parent, ccteam_harness::NotifyMode::Off, None, None);
            (parent, child)
        };
        {
            let mut gw = gateway.lock().await;
            gw.deliver_delegation_signal(crate::delegation::DelegationSignal {
                child_sid: child2.clone(),
                turn_id: format!("{child2}-1"),
                tail: "done".into(),
                vendor: AgentVendor::Codex,
                host: "local".into(),
                boundary: true,
                vendor_error: false,
                interim_notes: 0,
                covered_turns: vec![format!("{child2}-1")],
            })
            .await;
        }
        assert!(
            ccteam_notification_turns(&project_dir, &parent2).is_empty(),
            "off-mode must never notify"
        );
    }

    /// Spawn a claude child in `alpha` under `parent` (test helper — a named fn
    /// so the borrowed future's lifetime is well-formed, unlike an async closure).
    async fn spawn_child(
        gw: &mut Gateway,
        parent: DelegationParent,
    ) -> Result<CreateSessionOutcome> {
        gw.create_delegated_session(
            "alpha".into(),
            String::new(),
            AgentVendor::Claude,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".into(),
            SpawnTuning::default(),
            Some(parent),
            None,
        )
        .await
    }

    /// Each count guardrail (depth / children / delegated) rejects one spawn
    /// with a readable error AND emits a `delegation_denied{reason}` event.
    #[tokio::test]
    async fn delegation_count_guardrails_deny_and_emit_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ccteam_root = tmp.path().join(".ccteam");
        let projects_root = tmp.path().join("projects");
        let project_dir = projects_root.join("alpha");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(ccteam_root.join("state")).unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: ccteam_root.clone(),
            projects_root: projects_root.clone(),
        };
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw = Gateway::new(fake, "alpha", &project_dir);
        gw.enable_project_creation(paths.clone());
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });

        let mk = |depth: u32| DelegationParent {
            sid: "sPARENT".into(),
            depth,
            role: String::new(),
        };

        // depth: parent at depth 1 → child depth 2 > max_depth 1.
        gw.set_delegation_config(ccteam_core::DelegationConfig {
            max_depth: 1,
            max_children: 99,
            max_delegated: 99,
        });
        let e = spawn_child(&mut gw, mk(1)).await.unwrap_err();
        assert!(e.to_string().contains("depth"), "depth: {e}");

        // children: a real parent with 1 child → 2nd child denied.
        gw.set_delegation_config(ccteam_core::DelegationConfig {
            max_depth: 9,
            max_children: 1,
            max_delegated: 99,
        });
        let parent = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let pctx = DelegationParent {
            sid: parent.clone(),
            depth: 0,
            role: String::new(),
        };
        spawn_child(&mut gw, pctx.clone()).await.unwrap(); // 1st child ok
        let e2 = spawn_child(&mut gw, pctx.clone()).await.unwrap_err();
        assert!(
            e2.to_string().contains("fan-out"),
            "children (fan-out): {e2}"
        );

        // delegated: 1 delegated child now lives → ceiling 1 → next denied.
        gw.set_delegation_config(ccteam_core::DelegationConfig {
            max_depth: 9,
            max_children: 9,
            max_delegated: 1,
        });
        let parent2 = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let e3 = spawn_child(
            &mut gw,
            DelegationParent {
                sid: parent2,
                depth: 0,
                role: String::new(),
            },
        )
        .await
        .unwrap_err();
        assert!(e3.to_string().contains("ceiling"), "delegated: {e3}");

        // All three emitted a `delegation_denied` with the right reason.
        let events =
            ccteam_core::progress::read_all_events(&paths.progress_jsonl("alpha")).unwrap();
        let reasons: Vec<&str> = events
            .iter()
            .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("delegation_denied"))
            .filter_map(|e| e.get("reason").and_then(|v| v.as_str()))
            .collect();
        assert!(reasons.contains(&"depth"), "reasons: {reasons:?}");
        assert!(reasons.contains(&"children"), "reasons: {reasons:?}");
        assert!(reasons.contains(&"delegated"), "reasons: {reasons:?}");
    }

    /// The Ambient budget gate denies a spawn when the vendor's 24h project
    /// cost has reached its cap (a `0.0` cap = no spend allowed) + emits
    /// `delegation_denied{reason:"budget"}`.
    #[tokio::test]
    async fn delegation_budget_gate_denies_and_emits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ccteam_root = tmp.path().join(".ccteam");
        let projects_root = tmp.path().join("projects");
        let project_dir = projects_root.join("alpha");
        std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
        std::fs::create_dir_all(ccteam_root.join("state")).unwrap();
        // A zero claude cap → any cost (incl. 0) is "reached" → deny.
        std::fs::write(
            project_dir.join(".ccteam").join("workflow.yaml"),
            "budgets_v060:\n  claude:\n    max_cost_usd_per_24h: 0.0\n",
        )
        .unwrap();
        let paths = ccteam_core::CcteamPaths {
            root: ccteam_root.clone(),
            projects_root: projects_root.clone(),
        };
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw = Gateway::new(fake, "alpha", &project_dir);
        gw.enable_project_creation(paths.clone());
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });

        let e = gw
            .create_delegated_session(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: "sP".into(),
                    depth: 0,
                    role: String::new(),
                }),
                None,
            )
            .await
            .unwrap_err();
        assert!(e.to_string().contains("budget"), "budget: {e}");
        let events =
            ccteam_core::progress::read_all_events(&paths.progress_jsonl("alpha")).unwrap();
        assert!(events.iter().any(|e| {
            e.get("event").and_then(|v| v.as_str()) == Some("delegation_denied")
                && e.get("reason").and_then(|v| v.as_str()) == Some("budget")
        }));
    }

    /// Chaos: a durable watch + a completed child turn on disk, loaded by a
    /// FRESH gateway (daemon-restart), delivers the missed notification EXACTLY
    /// once; a second reconcile over the same state delivers none.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn delegation_reconcile_delivers_missed_notification_exactly_once() {
        use ccteam_harness::execution::turns_mirror::{append_turn, read_all_turns, TurnRecord};
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let gateway = delegation_gateway(&project_dir).await;

        // A live parent to receive the notification.
        let parent_sid = {
            let mut gw = gateway.lock().await;
            gw.create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid
        };
        // Simulate a child that completed a turn while the daemon was down: its
        // turns.jsonl has one assistant turn + an armed, un-notified watch.
        let child_sid = "s99";
        append_turn(
            &project_dir,
            child_sid,
            &TurnRecord {
                turn_id: format!("{child_sid}-1"),
                ts: chrono::Utc::now(),
                vendor: "claude".into(),
                role: String::new(),
                user: String::new(),
                assistant: "the research is done".into(),
                usage: serde_json::Value::Null,
                tool_calls: vec![],
                outcome: None,
                error_kind: None,
                error: None,
            },
        )
        .unwrap();
        ccteam_harness::write_delegation_watch(
            &project_dir,
            child_sid,
            &ccteam_harness::DelegationWatch::armed(
                &parent_sid,
                ccteam_harness::NotifyMode::Final,
                Some("research".into()),
                Some(format!("{child_sid}-1")),
            ),
        )
        .unwrap();

        // Reconcile #1 delivers the missed notification.
        Gateway::reconcile_delegations(Arc::clone(&gateway)).await;
        // Count the DELIVERED notifications = the mirrored USER turns (the
        // parent's own echo of it is a separate assistant turn — don't
        // double-count one notification).
        let count_notifications = |dir: &std::path::Path, psid: &str| {
            read_all_turns(dir, psid)
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.user.contains("[ccteam] delegated session"))
                .count()
        };
        let mut delivered = 0;
        for _ in 0..200 {
            delivered = count_notifications(&project_dir, &parent_sid);
            if delivered >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            delivered, 1,
            "reconcile delivers the missed notification once"
        );

        // Reconcile #2 delivers nothing new (deduped by notified_turns).
        Gateway::reconcile_delegations(Arc::clone(&gateway)).await;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert_eq!(
            count_notifications(&project_dir, &parent_sid),
            1,
            "a second reconcile is a no-op (exactly-once)"
        );
    }

    /// v0.9.5 feedback fix — a restart reconcile FOLDS a chatty child's missed
    /// backlog (N un-notified turns) into ONE idle-marked notification carrying
    /// the latest text, instead of replaying the narration turn by turn; a
    /// second reconcile stays a no-op.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn delegation_reconcile_folds_missed_backlog_into_one_notification() {
        use ccteam_harness::execution::turns_mirror::{append_turn, read_all_turns, TurnRecord};
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let gateway = delegation_gateway(&project_dir).await;

        let parent_sid = {
            let mut gw = gateway.lock().await;
            gw.create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid
        };
        let child_sid = "s98";
        for n in 1..=3 {
            append_turn(
                &project_dir,
                child_sid,
                &TurnRecord {
                    turn_id: format!("{child_sid}-{n}"),
                    ts: chrono::Utc::now(),
                    vendor: "codex".into(),
                    role: String::new(),
                    user: String::new(),
                    assistant: format!("checkpoint {n}"),
                    usage: serde_json::Value::Null,
                    tool_calls: vec![],
                    outcome: None,
                    error_kind: None,
                    error: None,
                },
            )
            .unwrap();
        }
        ccteam_harness::write_delegation_watch(
            &project_dir,
            child_sid,
            &ccteam_harness::DelegationWatch::armed(
                &parent_sid,
                ccteam_harness::NotifyMode::Final,
                None,
                Some(format!("{child_sid}-1")),
            ),
        )
        .unwrap();

        Gateway::reconcile_delegations(Arc::clone(&gateway)).await;
        let notifications = |dir: &std::path::Path, psid: &str| {
            read_all_turns(dir, psid)
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.user.contains("[ccteam] delegated session"))
                .collect::<Vec<_>>()
        };
        let mut got: Vec<TurnRecord> = vec![];
        for _ in 0..200 {
            got = notifications(&project_dir, &parent_sid);
            if !got.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(got.len(), 1, "backlog folds into ONE notification");
        assert!(
            got[0].user.contains("checkpoint 3"),
            "the folded notification carries the LATEST text: {}",
            got[0].user
        );
        assert!(got[0].user.contains("2 interim note(s)"));

        Gateway::reconcile_delegations(Arc::clone(&gateway)).await;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert_eq!(
            notifications(&project_dir, &parent_sid).len(),
            1,
            "a second reconcile is a no-op (covered turns all recorded)"
        );
    }

    /// `/sessions` renders a delegation tree: children indented `└─ ` under
    /// their parent; a non-local host + title annotate the row.
    #[tokio::test]
    async fn delegation_sessions_tree_indents_children() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw = Gateway::new(fake, "alpha", &project_dir);
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gw.bind_operator_allowlist("mock", ["chat-1".to_string()]);
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });

        let parent = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let child = gw
            .create_delegated_session(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: parent.clone(),
                    depth: 0,
                    role: String::new(),
                }),
                None,
            )
            .await
            .unwrap()
            .sid;
        // Web chat drives the fleet (`all` scope, own-pool visibility).
        let out = gw
            .render_sessions(&ChatKey::new("mock", "chat-1", "alice"), true)
            .await;
        // Parent row precedes the indented child row (roleless → `sid claude`).
        let pline = format!("{parent} claude");
        let cline = format!("└─ {child} claude");
        let pi = out
            .find(&pline)
            .unwrap_or_else(|| panic!("parent row: {out}"));
        let ci = out
            .find(&cline)
            .unwrap_or_else(|| panic!("indented child row: {out}"));
        assert!(pi < ci, "child indented under parent:\n{out}");
    }

    /// `ancestor_chain` (the stop-descendant + cycle basis): a child's chain
    /// includes its parent; a sibling's does not.
    #[tokio::test]
    async fn delegation_ancestor_chain_walks_parent_links() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw = Gateway::new(fake, "alpha", &project_dir);
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });

        let parent = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let child = gw
            .create_delegated_session(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: parent.clone(),
                    depth: 0,
                    role: String::new(),
                }),
                None,
            )
            .await
            .unwrap()
            .sid;
        let sibling = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let chain = gw.ancestor_chain(&child);
        assert!(chain.contains(&parent), "child's chain reaches its parent");
        assert!(chain.contains(&child), "chain includes self");
        assert!(
            !chain.contains(&sibling),
            "an unrelated sibling is NOT an ancestor"
        );
    }

    // ========================================================================
    // v0.9.0 W4 (F4) — GatewayEventKind::Delegation broadcast (team view SSE).
    // ========================================================================

    /// A delegated spawn's `delegation_spawned` emit point ALSO broadcasts a
    /// `GatewayEventKind::Delegation` (the team view's live twin) — fires even
    /// with `project_paths` unset (no progress.jsonl write side effect needed
    /// for the broadcast leg; see `emit_delegation_progress`'s doc). `sid` is
    /// `None` (not a per-session event) and `slug` names the project — the
    /// team view's global SSE ACL key.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delegated_spawn_broadcasts_delegation_event_with_slug() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gw = Gateway::new(fake, "alpha", &project_dir);
        let mut events = gw.subscribe_events();
        // No mpsc sink wired: `emit_user_signal` falls back to the raw
        // broadcast (still exercises the SAME `Delegation` construction path
        // `emit_delegation_progress` uses at every call site).

        let parent = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let child = gw
            .create_delegated_session(
                "alpha".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".into(),
                SpawnTuning::default(),
                Some(DelegationParent {
                    sid: parent.clone(),
                    depth: 0,
                    role: "brain".into(),
                }),
                Some("research task".into()),
            )
            .await
            .unwrap()
            .sid;

        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let ev = events.recv().await.expect("broadcast still open");
                if matches!(ev.kind, GatewayEventKind::Delegation { .. }) {
                    return ev;
                }
            }
        })
        .await
        .expect("a Delegation broadcast arrives");

        assert_eq!(
            ev.sid, None,
            "a Delegation event names two sessions, not one"
        );
        assert_eq!(ev.slug.as_deref(), Some("alpha"));
        match ev.kind {
            GatewayEventKind::Delegation {
                relation,
                parent_sid,
                child_sid,
                title,
                reason,
            } => {
                assert_eq!(relation, "spawned");
                assert_eq!(parent_sid, parent);
                assert_eq!(child_sid, child);
                assert_eq!(title.as_deref(), Some("research task"));
                assert_eq!(reason, None);
            }
            _ => unreachable!("filtered above"),
        }
    }

    #[tokio::test]
    async fn inbox_create_list_and_cancel_use_current_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role(tmp.path(), "reviewer");
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        gateway
            .enable_persistence(tmp.path().join("ccteam-home"))
            .unwrap();

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let created = gateway
            .handle_text(
                "mock",
                "chat-1",
                "alice",
                "/inbox +30m review the release notes",
            )
            .await
            .unwrap();
        assert!(created[0].contains("scheduled d1 → s1"), "{created:?}");

        let listed = gateway
            .handle_text("mock", "chat-1", "alice", "/inbox")
            .await
            .unwrap();
        assert!(listed[0].contains("d1 · s1"), "{listed:?}");
        assert!(listed[0].contains("review the release notes"));

        let cancelled = gateway
            .handle_text("mock", "chat-1", "alice", "/inbox cancel d1")
            .await
            .unwrap();
        assert_eq!(cancelled, vec!["cancelled d1"]);
        assert!(gateway.scheduled_items_for_sid("s1").unwrap().is_empty());
    }

    #[tokio::test]
    async fn inbox_list_reuses_own_plus_web_pool_acl() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role(tmp.path(), "reviewer");
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", tmp.path());
        // The owner's own chat(s): named in the bot allowlist ⇒ operator.
        gateway.bind_operator_allowlist("telegram", ["chat-1".to_string(), "chat-2".to_string()]);

        gateway
            .handle_text("telegram", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("telegram", "chat-1", "alice", "/inbox +30m own-message")
            .await
            .unwrap();
        gateway
            .handle_text("telegram", "chat-2", "bob", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("telegram", "chat-2", "bob", "/inbox +30m foreign-message")
            .await
            .unwrap();
        let web_sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        gateway
            .create_scheduled_message(
                &web_sid,
                "web-pool-message".into(),
                chrono::Utc::now() + chrono::Duration::minutes(30),
                "user:web-api".into(),
                None,
            )
            .unwrap();

        let listed = gateway
            .handle_text("telegram", "chat-1", "alice", "/inbox")
            .await
            .unwrap();
        assert!(listed[0].contains("own-message"), "{listed:?}");
        assert!(listed[0].contains("web-pool-message"), "{listed:?}");
        assert!(!listed[0].contains("foreign-message"), "{listed:?}");
    }

    #[tokio::test]
    async fn scheduled_slash_body_fires_as_literal_user_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role(tmp.path(), "reviewer");
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let item = gateway
            .create_scheduled_message(
                &sid,
                "/model opus".into(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
                "user:web-api".into(),
                None,
            )
            .unwrap();
        gateway
            .scheduled_items
            .get_mut(&item.id)
            .unwrap()
            .item
            .send_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        // Exercise the cold-start pending_turns leg too: the literal flag must
        // survive enqueue/drain rather than turning `/model` into a directive.
        fake.live.store(false, Ordering::SeqCst);

        gateway.fire_due_scheduled(chrono::Utc::now()).await;

        let submissions = fake.submissions.lock().await.clone();
        assert!(submissions.iter().any(|(_, text)| text == "/model opus"));
        assert!(fake.directives.lock().await.is_empty());
        assert!(gateway.scheduled_items_for_sid(&sid).unwrap().is_empty());
    }

    #[tokio::test]
    async fn scheduled_id_counter_survives_cancel_and_daemon_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        seed_role(&project_dir, "reviewer");
        let root = tmp.path().join("home");
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake, "alpha", &project_dir);
        gateway.enable_persistence(&root).unwrap();
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let first = gateway
            .create_scheduled_message(
                &sid,
                "first".into(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
                "user:web-api".into(),
                None,
            )
            .unwrap();
        assert_eq!(first.id, "d1");
        gateway.cancel_scheduled_message(&sid, "d1").unwrap();
        drop(gateway);

        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut restored = Gateway::new(fake, "alpha", &project_dir);
        restored.enable_persistence(&root).unwrap();
        let second = restored
            .create_scheduled_message(
                &sid,
                "second".into(),
                chrono::Utc::now() + chrono::Duration::minutes(10),
                "user:web-api".into(),
                None,
            )
            .unwrap();
        assert_eq!(second.id, "d2", "cancelled ids are never reused");
    }

    #[tokio::test]
    async fn scheduled_restart_catch_up_marks_over_24h_failed_then_gc_expires_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_role(tmp.path(), "reviewer");
        let fake = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let mut gateway = Gateway::new(fake.clone(), "alpha", tmp.path());
        let sid = gateway
            .create_session_api(
                "alpha".into(),
                "reviewer".into(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let item = gateway
            .create_scheduled_message(
                &sid,
                "too old".into(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
                "user:web-api".into(),
                None,
            )
            .unwrap();
        gateway
            .scheduled_items
            .get_mut(&item.id)
            .unwrap()
            .item
            .send_at = chrono::Utc::now() - chrono::Duration::hours(25);

        gateway.fire_due_scheduled(chrono::Utc::now()).await;
        let failed = gateway.scheduled_items_for_sid(&sid).unwrap();
        assert_eq!(failed[0].status, crate::scheduled::ScheduledStatus::Failed);
        assert!(failed[0]
            .fail_reason
            .as_deref()
            .unwrap()
            .contains("older than 24 hours"));
        assert!(fake.submissions.lock().await.is_empty());

        gateway
            .scheduled_items
            .get_mut(&item.id)
            .unwrap()
            .item
            .failed_at = Some(chrono::Utc::now() - chrono::Duration::hours(25));
        assert!(gateway.scheduled_items_for_sid(&sid).unwrap().is_empty());
    }
}
