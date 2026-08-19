//! V0.6.0 Wave 3 F112 — `CodexAppServerAdapter` (mode-3 codex bot path).
//!
//! Talks to `codex app-server` over a Unix Domain Socket via the
//! `thread/start`, `thread/resume`, `turn/start`, `thread/archive`
//! JSON-RPC v2-lite methods (see [`super::codex_jsonrpc`]).
//!
//! ## Lifecycle
//!
//! - `start_thread`: ensure a `codex app-server` daemon is running →
//!   connect to its UDS → `initialize` handshake (negotiating
//!   `experimentalApi: true`) + `initialized` notification → `thread/start`
//!   with model + cwd hint → return [`ThreadHandle`] whose `identity`
//!   carries the codex `thread_id`. The handshake runs once per cached
//!   client inside `client()` (W3b catalog §7.2 — without it the server
//!   keeps `experimental_api = false` and silently filters ~30% of the
//!   notification surface, including `turn/plan/updated`).
//! - `submit_turn`: `turn/start` with `[{type:"text", text:...}]`.
//! - `events`: subscribe to broadcast notifications, translate
//!   `item/*` + `turn/*` notifications → [`ThreadEvent`]. **V0.6.1 F122**:
//!   also mirror the key boundary events (`turn/completed` + the `error`
//!   notification — the real wire name for turn failures, NOT `turn/failed`)
//!   into the project's `progress.jsonl` as `agent_done`
//!   entries tagged `vendor: codex` so the `cost_24h_by_vendor["codex"]`
//!   roll-up + budget cap surfaces stay live without the orchestrator
//!   needing to wire a separate poller (the V0.6.0 Wave 3 D9 retained
//!   risk).
//! - `resume_thread`: `thread/resume` with the persistent id.
//! - `close_thread`: `thread/archive` + `thread/unsubscribe` (best-effort).
//!
//! ## Socket discovery
//!
//! Default: `$CODEX_HOME/app-server-control/app-server-control.sock`
//! (CODEX_HOME falls back to `~/.codex`). Override via env
//! `CCTEAM_CODEX_APP_SERVER_SOCKET`. Tests use a tempdir socket served
//! by a hand-rolled scripted JSON-RPC peer.
//!
//! Wave 1 (V0.6) decision: mode 3 codex bot is **not** an end-user
//! configuration today (Wave 1 mode-3 ships claude-only). This adapter
//! exists so the trait stack is uniform and `/ccteam-advise` can dual-
//! probe codex without touching tmux. The orchestrator's mode-3
//! dispatch (e2e-wiring's territory) decides which adapter to mount.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::execution::codex_jsonrpc::{CodexJsonRpcClient, JsonRpcError, Notification};
use crate::execution::progress_bridge::{
    append_event, build_agent_done_completed_event, build_agent_done_errored_event,
    build_chat_session_reset_event_with_reason, build_codex_plan_updated_event,
    build_codex_rate_limit_event, build_codex_thread_status_event, build_codex_token_usage_event,
    progress_jsonl_from_env, semantic_payload_is_all_null,
};
#[cfg(test)]
use crate::execution::progress_bridge::{
    CODEX_PLAN_UPDATED, CODEX_RATE_LIMIT, CODEX_THREAD_STATUS, CODEX_TOKEN_USAGE,
};
use crate::execution::session_meta::read_session_meta;
use crate::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, PermissionMode,
    SpawnCtx, ThreadErrorEvent, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, TurnId,
    TurnInput, TurnRouting, TurnSubmission, UnifiedTokenUsage,
};
use crate::{
    ChoiceOption, ChoicePrompt, ChoiceSelection, ContextSource, ContextUsage, Directive,
    DirectiveOutcome, ThreadStatus,
};

/// Env override for the UDS path the adapter dials. Setting it is the
/// explicit power-user override: it selects the [`CodexTransport::Socket`]
/// transport and points at a self-managed `codex app-server` daemon
/// socket (tests set this to a tempdir socket served by a scripted peer).
/// Unset → the default [`CodexTransport::Stdio`] transport, which only
/// needs a `codex` binary on `PATH`.
pub const APP_SERVER_SOCKET_ENV: &str = "CCTEAM_CODEX_APP_SERVER_SOCKET";

/// How the adapter reaches `codex app-server`. Resolved ONCE at adapter
/// construction (see [`resolve_codex_transport`]) and stored on
/// [`CodexAppServerAdapter`], so `client()` is a plain `match` with no
/// per-call env sniffing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexTransport {
    /// Default: spawn `codex app-server --listen stdio://` and speak
    /// JSON-RPC over its stdio pipes. `program` is `CCTEAM_CODEX_BIN`
    /// (or `"codex"` on `PATH`). This is the path that "just works"
    /// once `codex` is installed — no external daemon required.
    Stdio { program: String },
    /// Power-user override: dial an already-running `codex app-server`
    /// over the UDS at `path`. Selected when [`APP_SERVER_SOCKET_ENV`]
    /// is set.
    Socket { path: PathBuf },
}

/// Pure, single-axis transport selection.
///
/// - [`APP_SERVER_SOCKET_ENV`] set → [`CodexTransport::Socket`] with that
///   path (the explicit power-user override pointing at a self-managed
///   `codex app-server` daemon).
/// - otherwise → [`CodexTransport::Stdio`] with `CCTEAM_CODEX_BIN` (or
///   `"codex"`), the default that just needs `codex` on `PATH`.
///
/// No second axis: the old `CCTEAM_CODEX_APP_SERVER_TRANSPORT` env (which
/// produced a 4-state matrix with two dead cells) is gone.
pub fn resolve_codex_transport() -> CodexTransport {
    if let Some(p) = std::env::var_os(APP_SERVER_SOCKET_ENV) {
        return CodexTransport::Socket {
            path: PathBuf::from(p),
        };
    }
    let program = std::env::var("CCTEAM_CODEX_BIN").unwrap_or_else(|_| "codex".to_string());
    CodexTransport::Stdio { program }
}

/// Fault-injection hook for real IM smoke tests: when set to `1`, the
/// next `submit_turn` terminates the cached stdio Codex app-server
/// child before issuing the JSON-RPC request, exercising the normal
/// submit-error path against a real binary.
pub const APP_SERVER_FAULT_KILL_BEFORE_TURN_ENV: &str =
    "CCTEAM_CODEX_APP_SERVER_FAULT_KILL_BEFORE_TURN";

/// V0.6.1 F122 — per-thread context the adapter consults when bridging
/// boundary events into `progress.jsonl`. Populated by `start_thread`
/// from the [`SpawnCtx`]; consumed by the `events()` stream the first
/// time a notification matches the thread.
///
/// `progress_path` lands at `~/.ccteam/progress/<slug>.jsonl`
/// (resolved from `CCTEAM_HOME`) so the bridge stays
/// consistent with the orchestrator's own writes. Tests inject a
/// custom ctx via [`CodexAppServerAdapter::register_bridge_for_test`].
#[derive(Debug, Clone)]
pub struct ProgressBridgeCtx {
    pub progress_path: PathBuf,
    pub role: String,
    pub sid: String,
    pub slug: String,
    pub model: Option<String>,
}

/// v0.8.5 D2.4 — per-thread live state the harness owns directly (NOT a
/// by-product of the `events()` stream). Maintained by the single
/// notification dispatcher spawned once per cached client (see
/// [`CodexAppServerAdapter::spawn_tracker_dispatcher`]).
///
/// `usage` comes ONLY from the `thread/tokenUsage/updated` notification
/// (the real `turn/completed` wire has NO usage field — see
/// `translate_notification`). `active_turn` is set on `turn/started` and
/// cleared on `turn/completed` + terminal `error`. `model` is seeded from
/// the spawn ctx / `thread/start` response.
#[derive(Debug, Clone, Default)]
pub struct ThreadLive {
    /// Latest context usage (total tokens + model context window) from
    /// `thread/tokenUsage/updated`.
    pub usage: Option<ContextUsage>,
    /// The in-flight turn id, if any. Drives steer-vs-start (D2.2) and
    /// `/interrupt`'s `expectedTurnId`.
    pub active_turn: Option<String>,
    /// Model id for this thread. Seeded deterministically from the spawn
    /// ctx (user's explicit intent) else codex's resolved `result.model`
    /// echoed by the `thread/start` / `thread/resume` response — see
    /// [`pluck_model`]. Never inferred; `None` only if codex reports none.
    pub model: Option<String>,
    /// Reasoning-effort the thread runs at (codex `reasoningEffort`,
    /// lowercase: `none`/`minimal`/`low`/`medium`/`high`/`xhigh`/custom).
    /// Seeded from the `thread/start` / `thread/resume` response
    /// ([`pluck_effort`]) or a `/model <id> <effort>` directive. `None`
    /// when codex reports no effort. Surfaced in the `/sessions` statusline.
    pub effort: Option<String>,
}

/// v0.8.5 D2.4 — the harness-level, vendor-scoped runtime state cache.
/// Keyed by codex `thread_id`. One per adapter instance (which is itself a
/// per-vendor singleton, arch §1.1), fed by ONE dispatcher task per cached
/// client — so opening multiple `events()` streams never double-counts.
#[derive(Debug, Default)]
pub struct CodexThreadTracker {
    threads: HashMap<String, ThreadLive>,
}

impl CodexThreadTracker {
    fn entry(&mut self, thread_id: &str) -> &mut ThreadLive {
        self.threads.entry(thread_id.to_string()).or_default()
    }

    /// Snapshot the live state for `thread_id` (None if never seen).
    pub fn snapshot(&self, thread_id: &str) -> Option<ThreadLive> {
        self.threads.get(thread_id).cloned()
    }

    /// True if any tracked thread has an in-flight turn. Gates the
    /// config-change app-server re-spawn: dropping the shared connection
    /// aborts a live turn (turns are not auto-retried), so we only re-spawn
    /// when the child is idle.
    fn any_active_turn(&self) -> bool {
        self.threads.values().any(|t| t.active_turn.is_some())
    }
}

/// v0.8.5 D2.1 — per-session command overrides applied on the NEXT
/// `turn/start` (model/effort/personality/collaboration mode/approval +
/// sandbox policy). Keyed by `thread_id`, mirroring the `bridges` map
/// pattern. Daemon-restart loss is acceptable (PRD §3-D2.1 / arch §7-2).
#[derive(Debug, Clone, Default)]
pub struct SessionOverride {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub personality: Option<String>,
    pub collaboration_mode: Option<String>,
    /// `AskForApproval` wire string (kebab-case: `on-request` / `never` /
    /// `untrusted` / `on-failure`), `shared.rs:162`.
    pub approval_policy: Option<String>,
    /// `SandboxPolicy` is an internally-tagged OBJECT on the wire
    /// (`{"type":"readOnly"}` / `{"type":"workspaceWrite",..}` /
    /// `{"type":"dangerFullAccess"}`, `permissions.rs` `SandboxPolicy`), NOT
    /// a bare string — so we store the full JSON value.
    pub sandbox_policy: Option<Value>,
}

impl SessionOverride {
    fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.effort.is_none()
            && self.personality.is_none()
            && self.collaboration_mode.is_none()
            && self.approval_policy.is_none()
            && self.sandbox_policy.is_none()
    }
}

/// V0.6.0 F112 [`HarnessAdapter`] that drives mode-3 codex bot sessions
/// via `codex app-server` UDS. The adapter is stateless across threads
/// — each `start_thread` lazily connects (and caches) a client per
/// process so reused for `submit_turn` / `events` / `close_thread`.
///
/// **V0.6.1 F122**: holds an optional `bridges` map keyed by codex
/// `thread_id`. Each entry carries the project's `progress.jsonl`
/// path + role/sid/slug/model so the `events()` stream can mirror
/// `turn/completed` / `turn/failed` notifications into `agent_done`
/// rows tagged `vendor: codex`. Without an entry the stream behaves
/// exactly like V0.6.0 (translation only — no IO side effect).
///
/// **v0.8.5 D2.4**: additionally holds a [`CodexThreadTracker`]
/// (per-thread usage / active-turn / model) fed by a single dispatcher
/// task spawned once per cached client, a per-session [`SessionOverride`]
/// map applied on `turn/start`, and a `skills/list` cache invalidated by
/// the `skills/changed` notification.
#[derive(Clone)]
pub struct CodexAppServerAdapter {
    /// The cached app-server connection. Bundles the JSON-RPC `client` with
    /// the set of thread ids loaded ON THAT connection (see [`CachedConn`]),
    /// so replacing the connection (transport death / re-dial) atomically
    /// resets the load-tracking — the per-connection epoch `ensure_thread_loaded`
    /// keys off of.
    inner: Arc<Mutex<Option<CachedConn>>>,
    bridges: Arc<Mutex<HashMap<String, ProgressBridgeCtx>>>,
    /// v0.8.5 D2.4 — harness-owned per-thread live state (usage /
    /// active-turn / model). Fed by ONE dispatcher per cached client.
    tracker: Arc<Mutex<CodexThreadTracker>>,
    /// v0.8.5 D2.1 — per-session command overrides applied on `turn/start`.
    overrides: Arc<Mutex<HashMap<String, SessionOverride>>>,
    /// v0.8.5 D2 — cached `skills/list` result (flattened `(name, path)`),
    /// invalidated by the `skills/changed` notification. `None` = cold.
    skills_cache: Arc<Mutex<Option<Vec<CachedSkill>>>>,
    /// v0.8.20 — latest account-scoped rate-limit snapshot (the inner
    /// `rateLimits` object of `account/rateLimits/updated`). Fed by the same
    /// single per-client dispatcher as `tracker`; account-, not thread-,
    /// scoped (the notification carries no thread_id), so ONE slot. Read by
    /// `account_usage()` so a Codex session surfaces ITS OWN 5h/weekly limits
    /// in the IM `/status` line instead of borrowing another vendor's.
    rate_limits: Arc<Mutex<Option<Value>>>,
    /// Transport resolved ONCE at construction (see
    /// [`resolve_codex_transport`]). `client()` matches on this — no
    /// per-call env sniffing.
    transport: CodexTransport,
}

/// One live app-server connection plus the set of thread ids ccteam has
/// loaded on it. codex keeps a thread resident only while a connection is
/// subscribed to it (`thread_lifecycle.rs`), and ccteam shares ONE child
/// connection across all codex sessions — so whenever the connection is
/// replaced (a transport-death `forget_client`, a child crash, a daemon
/// restart) every previously-loaded thread is gone from the new connection.
/// `loaded` is the per-connection epoch: it is created empty with the
/// connection and dropped wholesale when the connection is forgotten, so
/// [`CodexAppServerAdapter::ensure_thread_loaded`] knows deterministically
/// whether `tid` still needs a `thread/resume` before any turn-producing RPC.
#[derive(Clone)]
struct CachedConn {
    client: Arc<CodexJsonRpcClient>,
    loaded: Arc<Mutex<HashSet<String>>>,
    /// mtime of `$CODEX_HOME/config.toml` captured when this app-server child
    /// was spawned. `codex app-server` snapshots its config at process start
    /// and never re-reads the file, so a long-lived child serves every new
    /// `thread/start` from the config it saw at boot. `start_thread` compares
    /// this against the live on-disk mtime and re-spawns the child when they
    /// differ, so a new session picks up edited config without a ccteam
    /// restart. `None` when the file couldn't be stat'd at dial time.
    config_mtime: Option<std::time::SystemTime>,
}

/// v0.8.5 D2 — one entry of the flattened `skills/list` cache.
#[derive(Debug, Clone)]
pub struct CachedSkill {
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

impl Default for CodexAppServerAdapter {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            bridges: Arc::new(Mutex::new(HashMap::new())),
            tracker: Arc::new(Mutex::new(CodexThreadTracker::default())),
            overrides: Arc::new(Mutex::new(HashMap::new())),
            skills_cache: Arc::new(Mutex::new(None)),
            rate_limits: Arc::new(Mutex::new(None)),
            transport: resolve_codex_transport(),
        }
    }
}

impl std::fmt::Debug for CodexAppServerAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexAppServerAdapter")
            .finish_non_exhaustive()
    }
}

impl CodexAppServerAdapter {
    /// Construct the adapter, resolving the [`CodexTransport`] ONCE (env
    /// is read here, never in `client()`). A single adapter therefore
    /// memoises a single transport + a single `codex app-server` child
    /// (stdio) for its whole lifetime.
    pub fn new() -> Self {
        Self::default()
    }

    /// The resolved transport, decided at construction.
    pub fn transport(&self) -> &CodexTransport {
        &self.transport
    }

    /// String tag for `ThreadHandle.raw_extras.transport`: `"stdio"` for
    /// the default child-spawn path, `"socket"` for the UDS override.
    fn transport_tag(&self) -> &'static str {
        match self.transport {
            CodexTransport::Stdio { .. } => "stdio",
            CodexTransport::Socket { .. } => "socket",
        }
    }

    /// Resolve the UDS path the adapter should dial. Env override wins;
    /// otherwise `$CODEX_HOME/app-server-control/app-server-control.sock`
    /// with `CODEX_HOME` falling back to `~/.codex`.
    pub fn resolve_socket_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(APP_SERVER_SOCKET_ENV) {
            return Some(PathBuf::from(p));
        }
        let home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
        Some(
            home.join("app-server-control")
                .join("app-server-control.sock"),
        )
    }

    /// Lazily connect (or reuse) the JSON-RPC client. Spawning the
    /// daemon on demand is deliberately out-of-scope — callers (the
    /// orchestrator wiring + `ccteam doctor --check-codex-auth`) verify
    /// the daemon is running before enabling mode-3 codex. The adapter
    /// returns a clean [`HarnessError::SpawnFailed`] if the socket is
    /// missing, so the caller surfaces the right diagnostic.
    async fn client(&self) -> Result<Arc<CodexJsonRpcClient>, HarnessError> {
        Ok(self.conn().await?.client)
    }

    /// Lazily connect (or reuse) the full cached connection — the JSON-RPC
    /// client plus its per-connection `loaded` set. A fresh connection always
    /// gets an empty `loaded` set (new epoch); `forget_client` drops the whole
    /// [`CachedConn`], so the next `conn()` re-dials with a clean epoch.
    async fn conn(&self) -> Result<CachedConn, HarnessError> {
        let mut guard = self.inner.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        // F10: transport is resolved once at construction; here we just
        // `match` on it. `Stdio` spawns a child app-server; `Socket`
        // dials the power-user UDS override.
        let client = match &self.transport {
            CodexTransport::Stdio { program } => CodexJsonRpcClient::connect_stdio_command(program)
                .await
                .map_err(|err| {
                    HarnessError::SpawnFailed(format!(
                        "spawn codex app-server stdio ({program}): {err:#}"
                    ))
                })?,
            CodexTransport::Socket { path } => {
                CodexJsonRpcClient::connect_uds(path).await.map_err(|err| {
                    HarnessError::SpawnFailed(format!(
                        "connect codex app-server at {}: {err:#}",
                        path.display()
                    ))
                })?
            }
        };
        let shared = Arc::new(client);
        // W3b catalog §7.2 defect fix: complete the `initialize` handshake
        // BEFORE returning the cached client (i.e. before the first
        // `thread/start` or `events()` subscribe). Without it the server
        // keeps `experimental_api = false` and silently filters ~30% of the
        // server→client notification surface — including `turn/plan/updated`
        // (the structured plan tree V0.6.1 F124 HITL needs),
        // `thread/tokenUsage/updated`, `thread/goal/*`, and `item/plan/delta`.
        // We do this once per cached client (client() memoises), so all
        // subsequent calls reuse the negotiated capabilities.
        Self::handshake(&shared).await?;
        // v0.8.5 D2.4 — spawn the ONE tracker dispatcher per cached client,
        // BEFORE returning it (so it is subscribed before the first
        // `thread/start` / `turn/start`). It owns a single broadcast
        // subscription and is the sole writer of the tracker — opening
        // multiple `events()` streams never double-counts (arch §1.3).
        // The task exits when the broadcast closes (client dropped /
        // `forget_client`); a subsequent re-dial spawns a fresh one.
        self.spawn_tracker_dispatcher(&shared);
        let conn = CachedConn {
            client: Arc::clone(&shared),
            loaded: Arc::new(Mutex::new(HashSet::new())),
            config_mtime: codex_config_mtime(),
        };
        *guard = Some(conn.clone());
        Ok(conn)
    }

    /// `codex app-server` reads `config.toml` once at process start and never
    /// re-reads it, so on-disk edits are invisible to the long-lived child ccteam
    /// caches — every new `thread/start` inherits the config from when the child
    /// was spawned. Detect an edit (config mtime differs from the dial-time
    /// snapshot) and drop the cached connection so the next `conn()` re-spawns a
    /// fresh child that reads the new config.
    ///
    /// Two guards keep it safe and honest:
    /// - **Stdio transport only** — ccteam owns that child. The `Socket` override
    ///   dials an external app-server ccteam can't restart; bouncing would just
    ///   reconnect to the same stale daemon, so we skip it.
    /// - **Idle only** — dropping the shared connection aborts any in-flight turn
    ///   (turns are not auto-retried, see `submit_turn`). If any thread is
    ///   mid-turn we skip and warn; the new session uses the previous config until
    ///   the child is idle (or ccteam restarts).
    ///
    /// What the idle guard does NOT protect is every OTHER session's inbound
    /// subscription: dropping the connection closes their broadcast too. That
    /// is deliberate and safe only because the read path is rebuildable —
    /// `event_attachment() == Rebuildable`, and the gateway pump re-attaches
    /// (2026-08-09: before that contract existed, this exact line silently
    /// blinded every attached codex session).
    ///
    /// Called from `start_thread`. Existing resumed threads keep codex's
    /// per-thread config regardless (codex resume semantics), so this targets new
    /// threads only.
    async fn reload_app_server_if_config_changed(&self) {
        let is_stdio = matches!(self.transport, CodexTransport::Stdio { .. });
        let disk = codex_config_mtime();
        let (had_child, stored) = {
            let guard = self.inner.lock().await;
            (guard.is_some(), guard.as_ref().and_then(|c| c.config_mtime))
        };
        let busy = self.tracker.lock().await.any_active_turn();
        match decide_config_reload(is_stdio, had_child, stored, disk, busy) {
            ConfigReloadDecision::Respawn => {
                tracing::info!(
                    "codex config.toml changed on disk; re-spawning the app-server so \
                     the new session reads the updated config"
                );
                self.forget_client().await;
            }
            ConfigReloadDecision::Busy => tracing::warn!(
                "codex config.toml changed on disk but the shared app-server has an \
                 in-flight turn; the new session will use the previously-loaded config \
                 until the app-server is idle (or ccteam restarts)"
            ),
            ConfigReloadDecision::Skip => {}
        }
    }

    /// Deterministic precondition for every turn-producing / thread-scoped RPC:
    /// guarantee `tid` is resident in the CURRENT app-server connection before
    /// the call, and return that connection's client.
    ///
    /// codex keeps a thread loaded only while a connection is subscribed to it,
    /// and `turn/start` resolves the thread purely from the app-server's
    /// in-memory map (no on-disk auto-load) — so a turn issued against a fresh
    /// connection that never loaded `tid` fails with `thread not found`. Rather
    /// than send the turn and react to that error, we track (per connection
    /// epoch) which threads we have loaded and `thread/resume` exactly once,
    /// up front, when `tid` is missing. The turn RPC then only ever runs where
    /// the thread is guaranteed resident — `thread not found` is structurally
    /// impossible in steady state, with no error-string matching and no
    /// optimistic-send-then-retry fallback.
    async fn ensure_thread_loaded(
        &self,
        tid: &str,
    ) -> Result<Arc<CodexJsonRpcClient>, HarnessError> {
        let conn = self.conn().await?;
        let mut loaded = conn.loaded.lock().await;
        if loaded.contains(tid) {
            return Ok(conn.client);
        }
        // Not loaded on this connection epoch — resume it (loads from the
        // on-disk rollout AND subscribes this connection, so codex keeps it
        // resident for the connection's life). Holding `loaded` across the RPC
        // makes the resume exactly-once per (thread, connection).
        self.call_or_drop_dead(&conn.client, "thread/resume", json!({ "threadId": tid }))
            .await
            .map_err(|e| {
                HarnessError::SubmitFailed(format!("thread/resume (ensure-loaded): {e:#}"))
            })?;
        loaded.insert(tid.to_string());
        drop(loaded);
        // A freshly (re)loaded thread has no in-flight turn; clear any stale
        // `active_turn` carried over from a previous connection so submit_turn
        // picks `turn/start` rather than steering a turn that no longer exists.
        self.tracker.lock().await.entry(tid).active_turn = None;
        Ok(conn.client)
    }

    /// `thread/name/set` (`thread.rs:660`) — the ONE place codex's thread name
    /// is written, shared by the user-typed `/rename` directive and the
    /// cross-surface [`HarnessAdapter::set_session_title`] push, so an IM/web
    /// rename and an in-thread `/rename` can never drift apart.
    async fn set_thread_name(&self, tid: &str, name: &str) -> Result<(), HarnessError> {
        let client = self.client().await?;
        client
            .call("thread/name/set", json!({ "threadId": tid, "name": name }))
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("thread/name/set: {e:#}")))?;
        Ok(())
    }

    /// Mark `tid` as loaded on the current connection (called after a
    /// successful `thread/start` / `thread/resume`). Best-effort: a connection
    /// failure here just means the next [`ensure_thread_loaded`] re-resumes.
    async fn mark_loaded(&self, tid: &str) {
        if let Ok(conn) = self.conn().await {
            conn.loaded.lock().await.insert(tid.to_string());
        }
    }

    /// Issue an RPC on `client`; if it fails with a *transport death* (the
    /// connection / child is gone — see [`is_transport_death`]) drop the cached
    /// connection so the NEXT call re-dials a fresh app-server. A logical error
    /// from a LIVE peer leaves the shared connection — and every other session's
    /// loaded thread — untouched. Returns the raw result for the caller to map
    /// into its own [`HarnessError`] variant. This is the single home of the
    /// "forget the shared connection iff the transport died" policy, so every
    /// RPC path (turn/start, thread/resume, thread/start) recovers identically.
    async fn call_or_drop_dead(
        &self,
        client: &Arc<CodexJsonRpcClient>,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        match client.call(method, params).await {
            Ok(v) => Ok(v),
            Err(e) => {
                if is_transport_death(&e) {
                    self.forget_client().await;
                }
                Err(e)
            }
        }
    }

    /// v0.8.5 D2.4 — spawn the single per-client notification dispatcher
    /// that feeds the [`CodexThreadTracker`] and invalidates the skills
    /// cache. Deliberately NOT hung on `events()` (which stays a final-only
    /// presentation translator); the progress.jsonl mirror in `events()`
    /// is unaffected. Returns the [`JoinHandle`] (mostly for tests; the
    /// task self-terminates on broadcast close).
    fn spawn_tracker_dispatcher(
        &self,
        client: &Arc<CodexJsonRpcClient>,
    ) -> tokio::task::JoinHandle<()> {
        let mut rx = client.subscribe();
        let tracker = Arc::clone(&self.tracker);
        let skills_cache = Arc::clone(&self.skills_cache);
        let rate_limits = Arc::clone(&self.rate_limits);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(notif) => {
                        apply_notification_to_tracker(&tracker, &notif).await;
                        if notif.method == "skills/changed" {
                            *skills_cache.lock().await = None;
                        }
                        // Account-scoped rate-limit snapshot (no thread_id) —
                        // cache the latest so `account_usage()` reports THIS
                        // vendor's own 5h/weekly windows.
                        if notif.method == "account/rateLimits/updated" {
                            if let Some(snap) =
                                pluck_val(&notif.params, "rate_limits", "rateLimits")
                            {
                                *rate_limits.lock().await = Some(snap);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(n, "codex tracker dispatcher lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    /// Test hook: seed a thread's model in the tracker (production seeds it
    /// from `start_thread`'s spawn ctx).
    #[doc(hidden)]
    pub async fn tracker_seed_model_for_test(&self, thread_id: &str, model: Option<String>) {
        self.tracker.lock().await.entry(thread_id).model = model;
    }

    /// Snapshot a thread's tracker state (test + `thread_status` use it).
    pub async fn tracker_snapshot(&self, thread_id: &str) -> Option<ThreadLive> {
        self.tracker.lock().await.snapshot(thread_id)
    }

    /// v0.8.5 D2.1 — fold the per-session override map for `thread_id` into a
    /// `turn/start` params object. Each present field becomes the matching
    /// per-turn override (`turn/start` carries them per
    /// `app-server-protocol/src/protocol/v2/turn.rs`:
    /// model:114, effort:126, personality:132, approval_policy:99,
    /// sandbox_policy:106, collaboration_mode:145). Overrides "stick" on the
    /// server for this + subsequent turns, so we only need to send them when
    /// changed; resending every turn is harmless and idempotent.
    async fn apply_overrides(&self, thread_id: &str, params: &mut Value) {
        let ov = {
            let map = self.overrides.lock().await;
            match map.get(thread_id) {
                Some(o) if !o.is_empty() => o.clone(),
                _ => return,
            }
        };
        // Resolve the effective model BEFORE the mutable borrow of params:
        // override model wins, else the tracker's seeded model. Needed for
        // the (EXPERIMENTAL) collaboration mode object whose `settings.model`
        // is required.
        let effective_model = match ov.model.clone() {
            Some(m) => Some(m),
            None => self.tracker_snapshot(thread_id).await.and_then(|t| t.model),
        };
        let obj = match params.as_object_mut() {
            Some(o) => o,
            None => return,
        };
        if let Some(m) = ov.model {
            obj.insert("model".into(), Value::String(m));
        }
        if let Some(e) = ov.effort {
            obj.insert("effort".into(), Value::String(e));
        }
        if let Some(p) = ov.personality {
            obj.insert("personality".into(), Value::String(p));
        }
        if let Some(c) = ov.collaboration_mode {
            // CollaborationMode = { mode: ModeKind, settings: { model,
            // reasoning_effort?, developer_instructions? } } (config_types.rs:622).
            // settings.model is REQUIRED; without an effective model we cannot
            // build a valid object, so skip the field (degrade rather than
            // send a malformed turn — honest partial support for this
            // EXPERIMENTAL knob).
            if let Some(model) = effective_model {
                obj.insert(
                    "collaborationMode".into(),
                    json!({ "mode": c, "settings": { "model": model } }),
                );
            } else {
                tracing::warn!(
                    thread_id,
                    "collaboration mode override skipped: no effective model for required \
                     settings.model"
                );
            }
        }
        if let Some(a) = ov.approval_policy {
            obj.insert("approvalPolicy".into(), Value::String(a));
        }
        if let Some(s) = ov.sandbox_policy {
            // SandboxPolicy is an internally-tagged object, not a string.
            obj.insert("sandboxPolicy".into(), s);
        }
    }

    /// W3b catalog §4.1 — send the Codex `initialize` request (negotiating
    /// `experimentalApi: true`) followed by the one-way `initialized`
    /// notification. Mirrors the handshake the official Codex clients run:
    /// `InitializeParams { clientInfo, capabilities }` per
    /// `references/codex/codex-rs/app-server-protocol/src/protocol/v1.rs:26-56`
    /// (camelCase wire), then `ClientNotification::Initialized` →
    /// `{"method":"initialized"}` per `common.rs:1519-1521`.
    ///
    /// We opt OUT of the realtime/voice + Windows-only + admin-UI noise
    /// notifications (server-side filter is cheaper than ccteam-side) but
    /// keep every business-critical surface (turn/*, item/*, account/*).
    async fn handshake(client: &CodexJsonRpcClient) -> Result<(), HarnessError> {
        let params = json!({
            "clientInfo": {
                "name": "ccteam",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "experimentalApi": true,
                "requestAttestation": false,
                "optOutNotificationMethods": [
                    "thread/realtime/started",
                    "thread/realtime/itemAdded",
                    "thread/realtime/transcript/delta",
                    "thread/realtime/transcript/done",
                    "thread/realtime/outputAudio/delta",
                    "thread/realtime/sdp",
                    "thread/realtime/error",
                    "thread/realtime/closed",
                    "windows/worldWritableWarning",
                    "windowsSandbox/setupCompleted",
                    "app/list/updated",
                    // NOTE (v0.8.5 D2): `skills/changed` is deliberately NOT
                    // opted out — the tracker dispatcher consumes it to
                    // invalidate the `skills/list` cache (arch §1.3). If you
                    // re-add it here the cache goes stale-forever.
                    "fuzzyFileSearch/sessionUpdated",
                    "fuzzyFileSearch/sessionCompleted",
                    "remoteControl/status/changed"
                ]
            }
        });
        client
            .call("initialize", params)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("codex initialize handshake: {e:#}")))?;
        // `initialized` is a one-way client notification (no id, no
        // response) signalling readiness to receive server-initiated
        // requests + notifications.
        client
            .notify("initialized", Value::Null)
            .await
            .map_err(|e| {
                HarnessError::SpawnFailed(format!("codex initialized notification: {e:#}"))
            })?;
        Ok(())
    }

    /// One-shot helper: drop the cached client (e.g. after detecting a
    /// dead reader task). Next call to `client()` will re-dial.
    pub async fn forget_client(&self) {
        *self.inner.lock().await = None;
    }

    /// V0.6.1 F122 — register a progress bridge for `thread_id`. Called
    /// from `start_thread` after the codex `thread/start` response lands;
    /// also exposed for tests that skip the spawn dance and want to
    /// drive the events stream directly.
    pub async fn register_bridge(&self, thread_id: String, ctx: ProgressBridgeCtx) {
        self.bridges.lock().await.insert(thread_id, ctx);
    }

    /// V0.6.1 F122 — test escape hatch. Equivalent to [`register_bridge`]
    /// but named so production call sites (orchestrator wiring) don't
    /// reach for it by accident.
    #[doc(hidden)]
    pub async fn register_bridge_for_test(&self, thread_id: String, ctx: ProgressBridgeCtx) {
        self.register_bridge(thread_id, ctx).await;
    }

    async fn bridge_for(&self, thread_id: &str) -> Option<ProgressBridgeCtx> {
        self.bridges.lock().await.get(thread_id).cloned()
    }

    async fn drop_bridge(&self, thread_id: &str) {
        self.bridges.lock().await.remove(thread_id);
    }

    /// v0.8.5 D2.1 — the builtin command table. Returns `Ok(Some(outcome))`
    /// for a recognised builtin (the six classes), `Ok(None)` when `name` is
    /// not a builtin (the caller then tries the skills layer). Errors only on
    /// transport/RPC failure (propagated verbatim, D2.1 error class).
    ///
    /// b2344d8 anchors for every RPC are cited inline (file:line in
    /// `references/codex/codex-rs/`).
    async fn builtin_directive(
        &self,
        h: &ThreadHandle,
        name: &str,
        d: &Directive,
    ) -> Result<Option<DirectiveOutcome>, HarnessError> {
        let tid = h.identity.as_str();
        let args = d.args.trim();
        let outcome = match name {
            // ---- RPC direct-map (→ Turn unless noted) -------------------
            // thread/compact/start — common.rs:541
            "compact" => {
                let client = self.client().await?;
                client
                    .call("thread/compact/start", json!({ "threadId": tid }))
                    .await
                    .map_err(|e| {
                        HarnessError::SubmitFailed(format!("thread/compact/start: {e:#}"))
                    })?;
                DirectiveOutcome::Turn(synthetic_command_turn_id("compact", tid))
            }
            // review/start — common.rs:797; ReviewTarget — review.rs:43-65.
            // D2 apply: `branch X` → baseBranch; `commit X` → commit;
            // else → custom{instructions}; (a bare arg here means
            // uncommittedChanges). D4: a BARE `/review` (no args, no choice)
            // becomes a 4-option NeedsChoice; the `branch`/`commit` picks are
            // 2nd-hop (they need a value), surfaced as a follow-up NeedsChoice
            // whose free_text the user supplies (or `/review branch X` direct).
            "review" => {
                // D4 re-entry: a choice carrying one of the 4 ReviewTarget ids.
                if let Some(sel) = &d.choice {
                    return self.review_apply_choice(tid, sel).await.map(Some);
                }
                // D4 bare → NeedsChoice with the 4 fixed ReviewTarget options.
                if args.is_empty() {
                    return Ok(Some(DirectiveOutcome::NeedsChoice(choice_prompt(
                        "What should Codex review?",
                        review_options(),
                    ))));
                }
                // D2 with-args → direct apply (unchanged).
                let target = parse_review_target(args);
                self.review_start(tid, target).await?
            }
            // turn/interrupt — common.rs:762; params {threadId, turnId}
            // (turn.rs:188-191). Needs the active turn id from the tracker.
            "interrupt" => {
                let active = self.tracker_snapshot(tid).await.and_then(|t| t.active_turn);
                match active {
                    Some(turn_id) => {
                        let client = self.client().await?;
                        client
                            .call(
                                "turn/interrupt",
                                json!({ "threadId": tid, "turnId": turn_id }),
                            )
                            .await
                            .map_err(|e| {
                                HarnessError::SubmitFailed(format!("turn/interrupt: {e:#}"))
                            })?;
                        DirectiveOutcome::Done {
                            receipt: "interrupted the active turn.".to_string(),
                        }
                    }
                    None => DirectiveOutcome::Done {
                        receipt: "no active turn to interrupt.".to_string(),
                    },
                }
            }
            // thread/fork — common.rs:457; response thread.id (thread.rs:553).
            // The new thread id is registered as a new gateway session by the
            // gateway; we surface it in the receipt (wiring hook noted).
            "fork" => {
                let client = self.client().await?;
                let result = client
                    .call("thread/fork", json!({ "threadId": tid }))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("thread/fork: {e:#}")))?;
                let new_id = pluck_thread_id(&result).unwrap_or_default();
                DirectiveOutcome::Done {
                    receipt: format!("forked thread → {new_id} (use /use {new_id} to switch)."),
                }
            }
            // thread/rollback — common.rs:562; params {threadId, numTurns}
            // (thread.rs:938-947, numTurns >= 1).
            "rollback" => {
                let n: u32 = args.parse().unwrap_or(1).max(1);
                let client = self.client().await?;
                client
                    .call("thread/rollback", json!({ "threadId": tid, "numTurns": n }))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("thread/rollback: {e:#}")))?;
                DirectiveOutcome::Done {
                    receipt: format!("rolled back {n} turn(s)."),
                }
            }
            // thread/name/set — common.rs:492; params {threadId, name}
            // (thread.rs:660-663).
            "rename" => {
                if args.is_empty() {
                    DirectiveOutcome::Rejected {
                        reason: "usage: /rename <new name>".to_string(),
                    }
                } else {
                    self.set_thread_name(tid, args).await?;
                    DirectiveOutcome::Done {
                        receipt: format!("renamed thread to \"{args}\"."),
                    }
                }
            }
            // thread/goal/{set,get,clear} — common.rs:497/502/507.
            // no args → get; "clear" → clear; else → set objective.
            "goal" => {
                let client = self.client().await?;
                if args.is_empty() {
                    let result = client
                        .call("thread/goal/get", json!({ "threadId": tid }))
                        .await
                        .map_err(|e| {
                            HarnessError::SubmitFailed(format!("thread/goal/get: {e:#}"))
                        })?;
                    let objective = result
                        .get("goal")
                        .and_then(|g| g.get("objective"))
                        .and_then(|v| v.as_str());
                    DirectiveOutcome::Done {
                        receipt: match objective {
                            Some(o) => format!("goal: {o}"),
                            None => "no goal set.".to_string(),
                        },
                    }
                } else if args.eq_ignore_ascii_case("clear") {
                    client
                        .call("thread/goal/clear", json!({ "threadId": tid }))
                        .await
                        .map_err(|e| {
                            HarnessError::SubmitFailed(format!("thread/goal/clear: {e:#}"))
                        })?;
                    DirectiveOutcome::Done {
                        receipt: "goal cleared.".to_string(),
                    }
                } else {
                    client
                        .call(
                            "thread/goal/set",
                            json!({ "threadId": tid, "objective": args }),
                        )
                        .await
                        .map_err(|e| {
                            HarnessError::SubmitFailed(format!("thread/goal/set: {e:#}"))
                        })?;
                    DirectiveOutcome::Done {
                        receipt: format!("goal set: {args}"),
                    }
                }
            }
            // thread/backgroundTerminals/clean — common.rs:556.
            "stop" => {
                let client = self.client().await?;
                client
                    .call(
                        "thread/backgroundTerminals/clean",
                        json!({ "threadId": tid }),
                    )
                    .await
                    .map_err(|e| {
                        HarnessError::SubmitFailed(format!(
                            "thread/backgroundTerminals/clean: {e:#}"
                        ))
                    })?;
                DirectiveOutcome::Done {
                    receipt: "cleaned background terminals.".to_string(),
                }
            }
            // thread/memoryMode/set — common.rs:524; mode "enabled"|"disabled"
            // (thread.rs:830-836, lowercase enum). D4: bare → NeedsChoice
            // (enabled/disabled); a choice re-enters with the picked on/off id;
            // with-args keeps the D2 direct apply.
            "memories" => {
                // D4 re-entry: fold the picked id into the args parse below.
                let effective = match &d.choice {
                    Some(sel) => picked_id(sel).unwrap_or_default(),
                    None if args.is_empty() => {
                        return Ok(Some(DirectiveOutcome::NeedsChoice(choice_prompt(
                            "Memory mode?",
                            memory_mode_options(),
                        ))));
                    }
                    None => args.to_string(),
                };
                let mode = match effective.to_ascii_lowercase().as_str() {
                    "on" | "enable" | "enabled" => Some("enabled"),
                    "off" | "disable" | "disabled" => Some("disabled"),
                    _ => None,
                };
                match mode {
                    Some(m) => {
                        let client = self.client().await?;
                        client
                            .call(
                                "thread/memoryMode/set",
                                json!({ "threadId": tid, "mode": m }),
                            )
                            .await
                            .map_err(|e| {
                                HarnessError::SubmitFailed(format!("thread/memoryMode/set: {e:#}"))
                            })?;
                        DirectiveOutcome::Done {
                            receipt: format!("memory mode → {m}."),
                        }
                    }
                    None => DirectiveOutcome::Rejected {
                        reason: "usage: /memories <on|off>".to_string(),
                    },
                }
            }
            // command/exec — common.rs:965; params {command: [argv...]}
            // (command_exec.rs:30-33). Run a one-off `git diff`.
            "diff" => {
                let client = self.client().await?;
                let mut argv = vec!["git".to_string(), "diff".to_string()];
                if !args.is_empty() {
                    argv.extend(args.split_whitespace().map(|s| s.to_string()));
                }
                let result = client
                    .call("command/exec", json!({ "command": argv }))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("command/exec: {e:#}")))?;
                let out = result
                    .get("stdout")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                DirectiveOutcome::Done {
                    receipt: if out.trim().is_empty() {
                        "no changes.".to_string()
                    } else {
                        out
                    },
                }
            }
            // /init → a fixed-prompt turn (turn/start, common.rs:756).
            "init" => {
                let client = self.client().await?;
                let prompt = "Analyze this codebase and create an AGENTS.md file \
                    capturing build/test/lint commands, code style, and architecture \
                    so future agents can be productive immediately.";
                let mut params = json!({
                    "threadId": tid,
                    "input": [{ "type": "text", "text": prompt }],
                });
                self.apply_overrides(tid, &mut params).await;
                let result = client
                    .call("turn/start", params)
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("turn/start (init): {e:#}")))?;
                let turn_id = pluck_turn_id(&result).ok_or_else(|| {
                    HarnessError::SubmitFailed(format!(
                        "turn/start (init) response missing turn.id: {result}"
                    ))
                })?;
                DirectiveOutcome::Turn(TurnId(turn_id))
            }
            // account/login/start + account/logout — common.rs:911/931
            // (admin-gated at the gateway; the RPC itself is unconditional).
            "login" => {
                let client = self.client().await?;
                client
                    .call("account/login/start", json!({}))
                    .await
                    .map_err(|e| {
                        HarnessError::SubmitFailed(format!("account/login/start: {e:#}"))
                    })?;
                DirectiveOutcome::Done {
                    receipt: "login started (follow the codex CLI prompts).".to_string(),
                }
            }
            "logout" => {
                let client = self.client().await?;
                client
                    .call("account/logout", Value::Null)
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("account/logout: {e:#}")))?;
                DirectiveOutcome::Done {
                    receipt: "logged out.".to_string(),
                }
            }

            // ---- query-synth (→ Done{receipt}) --------------------------
            // /status: tracker (model + ctx) — no RPC needed; D2.4.
            "status" => {
                let live = self.tracker_snapshot(tid).await.unwrap_or_default();
                DirectiveOutcome::Done {
                    receipt: render_status_receipt(&live),
                }
            }
            // D4: bare /model (no args, no choice) → model/list (common.rs:803)
            // rendered as a NeedsChoice picker (one option per model+effort).
            // This SUPERSEDES D2's query-synth text receipt for the bare case.
            // A `choice` re-entry (empty args + d.choice) and the `/model <id>`
            // with-args case both fall through to the override arm below.
            "model" if args.is_empty() && d.choice.is_none() => {
                let client = self.client().await?;
                let result = client
                    .call("model/list", json!({}))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("model/list: {e:#}")))?;
                DirectiveOutcome::NeedsChoice(choice_prompt(
                    "Choose a model + reasoning effort:",
                    model_options(&result),
                ))
            }
            // /skills → skills/list (common.rs:608). D4: a choice re-entry
            // shows the picked skill's detail (minimal). Bare → NeedsChoice
            // listing skills; with-args → the D2 text receipt (a filtered
            // list still reads fine and avoids a useless 1-option picker).
            "skills" => {
                let skills = self.skills(true).await?;
                if let Some(sel) = &d.choice {
                    let id = picked_id(sel).unwrap_or_default();
                    let detail = skills
                        .iter()
                        .find(|s| s.name.eq_ignore_ascii_case(&id))
                        .map(|s| {
                            format!(
                                "/{}{}\n{}",
                                s.name,
                                if s.enabled { "" } else { " (disabled)" },
                                s.path
                            )
                        })
                        .unwrap_or_else(|| format!("skill not found: {id}"));
                    DirectiveOutcome::Done { receipt: detail }
                } else if args.is_empty() && !skills.is_empty() {
                    let options = skills
                        .iter()
                        .map(|s| ChoiceOption {
                            id: s.name.clone(),
                            label: if s.enabled {
                                format!("/{}", s.name)
                            } else {
                                format!("/{} (disabled)", s.name)
                            },
                        })
                        .collect();
                    DirectiveOutcome::NeedsChoice(choice_prompt("Skills (pick to view):", options))
                } else {
                    DirectiveOutcome::Done {
                        receipt: render_skills_list(&skills),
                    }
                }
            }
            // /mcp → mcpServerStatus/list (common.rs:898).
            "mcp" => {
                let client = self.client().await?;
                let result = client
                    .call("mcpServerStatus/list", json!({}))
                    .await
                    .map_err(|e| {
                        HarnessError::SubmitFailed(format!("mcpServerStatus/list: {e:#}"))
                    })?;
                DirectiveOutcome::Done {
                    receipt: render_count_receipt("MCP server", &result, "data"),
                }
            }
            // /hooks → hooks/list (common.rs:618).
            "hooks" => {
                let client = self.client().await?;
                let result = client
                    .call("hooks/list", json!({}))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("hooks/list: {e:#}")))?;
                DirectiveOutcome::Done {
                    receipt: render_count_receipt("hook source", &result, "data"),
                }
            }
            // /apps → app/list (common.rs:683).
            "apps" => {
                let client = self.client().await?;
                let result = client
                    .call("app/list", json!({}))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("app/list: {e:#}")))?;
                DirectiveOutcome::Done {
                    receipt: render_count_receipt("app", &result, "data"),
                }
            }

            // ---- per-session override (→ Done) --------------------------
            // /model <id> [effort] — turn.rs:114/126. D4: a choice re-entry
            // arrives with empty args + d.choice carrying the picked
            // "<id> [effort]" id; parse that instead.
            "model" => {
                let picked = d.choice.as_ref().and_then(picked_id);
                let effective = picked.as_deref().unwrap_or(args);
                let mut parts = effective.split_whitespace();
                let model = parts.next().map(|s| s.to_string());
                let effort = parts.next().map(|s| s.to_ascii_lowercase());
                self.set_override(tid, |o| {
                    o.model = model.clone();
                    if effort.is_some() {
                        o.effort = effort.clone();
                    }
                })
                .await;
                DirectiveOutcome::Done {
                    receipt: match (&model, &effort) {
                        (Some(m), Some(e)) => {
                            format!("model → {m} (effort {e}); applies next turn.")
                        }
                        (Some(m), None) => format!("model → {m}; applies next turn."),
                        _ => "model override cleared.".to_string(),
                    },
                }
            }
            // /personality <p> — turn.rs:132 (lowercase: none/friendly/pragmatic).
            // D4: bare → NeedsChoice (static Personality enum); a choice
            // re-enters with the picked id; with-args keeps the D2 apply.
            "personality" => {
                let picked = d.choice.as_ref().and_then(picked_id);
                let p = match (picked, args.is_empty()) {
                    (Some(p), _) => p.to_ascii_lowercase(),
                    (None, true) => {
                        return Ok(Some(DirectiveOutcome::NeedsChoice(choice_prompt(
                            "Communication style?",
                            personality_options(),
                        ))));
                    }
                    (None, false) => args.to_ascii_lowercase(),
                };
                self.set_override(tid, |o| o.personality = Some(p.clone()))
                    .await;
                DirectiveOutcome::Done {
                    receipt: format!("personality → {p}; applies next turn."),
                }
            }
            // /plan, /collab <m> — turn.rs:145 collaboration_mode (EXPERIMENTAL).
            // `CollaborationMode` is `{mode: ModeKind, settings: {model,..}}`
            // (config_types.rs:622) — `settings.model` is REQUIRED, so the
            // full object is built at apply time from the effective model
            // (see `apply_overrides`). Here we only store the `ModeKind`
            // (snake_case: `plan` / `default`; config_types.rs:576 accepts
            // `code`/`execute`/`custom` as aliases for `default`).
            // D4: `/collab` (bare, no choice) → NeedsChoice from the
            // EXPERIMENTAL `collaborationMode/list` (common.rs:864); a choice
            // re-enters with the picked ModeKind/name; `/collab <m>` applies
            // directly. `/plan` is a directed alias for the `plan` ModeKind —
            // it always applies directly (no popup), matching the TUI "switch
            // to Plan mode" semantics.
            "collab" | "plan" => {
                let picked = d.choice.as_ref().and_then(picked_id);
                let kind = if name == "plan" {
                    "plan".to_string()
                } else if let Some(p) = picked {
                    p.to_ascii_lowercase()
                } else if args.is_empty() {
                    // EXPERIMENTAL list; if the server doesn't implement it the
                    // RPC errors → propagate as SubmitFailed (honest).
                    let client = self.client().await?;
                    let result = client
                        .call("collaborationMode/list", json!({}))
                        .await
                        .map_err(|e| {
                            HarnessError::SubmitFailed(format!("collaborationMode/list: {e:#}"))
                        })?;
                    let options = collab_options(&result);
                    return Ok(Some(DirectiveOutcome::NeedsChoice(choice_prompt(
                        "Collaboration mode?",
                        options,
                    ))));
                } else {
                    args.to_ascii_lowercase()
                };
                self.set_override(tid, |o| o.collaboration_mode = Some(kind.clone()))
                    .await;
                DirectiveOutcome::Done {
                    receipt: format!("collaboration mode → {kind}; applies next turn."),
                }
            }
            // /permissions <preset> — turn.rs:99 approval_policy + :106
            // sandbox_policy (admin-gated at the gateway). We map a small set
            // of presets to (approval_policy, sandbox_policy). D4: bare →
            // NeedsChoice (static AskForApproval/SandboxMode presets); a choice
            // re-enters with the picked preset id; with-args keeps D2's apply.
            "permissions" => {
                if d.choice.is_none() && args.is_empty() {
                    return Ok(Some(DirectiveOutcome::NeedsChoice(choice_prompt(
                        "What is Codex allowed to do?",
                        permissions_options(),
                    ))));
                }
                let effective = match d.choice.as_ref().and_then(picked_id) {
                    Some(p) => p,
                    None => args.to_string(),
                };
                match permissions_preset(&effective) {
                    Some((approval, sandbox, label)) => {
                        let a = approval.to_string();
                        self.set_override(tid, |o| {
                            o.approval_policy = Some(a.clone());
                            o.sandbox_policy = Some(sandbox.clone());
                        })
                        .await;
                        DirectiveOutcome::Done {
                            receipt: format!(
                                "permissions → {label} (approval={approval}); applies next turn."
                            ),
                        }
                    }
                    None => DirectiveOutcome::Rejected {
                        reason: "usage: /permissions <read-only|auto|full-access>".to_string(),
                    },
                }
            }

            // ---- semantic redirect (→ Redirect) -------------------------
            // Codex has no in-thread /new /clear — point at the gateway's
            // session commands.
            "new" | "clear" => DirectiveOutcome::Redirect {
                hint: "Codex has no in-thread equivalent — use the gateway's /new (fresh \
                       session) or /use (switch session)."
                    .to_string(),
            },
            // D4: /resume bare → NeedsChoice from thread/list (common.rs:567);
            // a choice re-enters with the picked thread id, surfaced as a
            // Redirect carrying `/use <id>` (the gateway owns session switching
            // — Codex has no in-thread resume). `/resume <id>` short-circuits
            // straight to the same redirect.
            "resume" => {
                if let Some(sel) = &d.choice {
                    let id = picked_id(sel).unwrap_or_default();
                    return Ok(Some(resume_redirect(&id)));
                }
                if !args.is_empty() {
                    return Ok(Some(resume_redirect(args)));
                }
                let client = self.client().await?;
                let result = client
                    .call("thread/list", json!({}))
                    .await
                    .map_err(|e| HarnessError::SubmitFailed(format!("thread/list: {e:#}")))?;
                let options = resume_options(&result);
                if options.is_empty() {
                    DirectiveOutcome::Done {
                        receipt: "no saved threads to resume.".to_string(),
                    }
                } else {
                    DirectiveOutcome::NeedsChoice(choice_prompt("Resume which thread?", options))
                }
            }

            // ---- TUI-only (→ Rejected) ----------------------------------
            n if is_codex_tui_only(n) => DirectiveOutcome::Rejected {
                reason: format!(
                    "/{n} is a Codex TUI-only command with no app-server equivalent; \
                     it cannot run from chat."
                ),
            },

            // not a builtin → let the caller try the skills layer.
            _ => return Ok(None),
        };
        Ok(Some(outcome))
    }

    /// v0.8.5 D2/D4 — fire `review/start` for a resolved [`ReviewTarget`]
    /// (review.rs:43-65) and surface the resulting turn.
    async fn review_start(
        &self,
        thread_id: &str,
        target: Value,
    ) -> Result<DirectiveOutcome, HarnessError> {
        let client = self.client().await?;
        let result = client
            .call(
                "review/start",
                json!({ "threadId": thread_id, "target": target }),
            )
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("review/start: {e:#}")))?;
        let turn_id = pluck_turn_id(&result).ok_or_else(|| {
            HarnessError::SubmitFailed(format!("review/start response missing turn.id: {result}"))
        })?;
        Ok(DirectiveOutcome::Turn(TurnId(turn_id)))
    }

    /// v0.8.5 D4 — apply a `/review` ChoicePrompt selection. The 4 fixed
    /// `ReviewTarget` ids map to:
    /// - `uncommitted` → fire `review/start` { uncommittedChanges }.
    /// - `custom` → if `free_text` was supplied, fire { custom }; else a
    ///   2nd-hop NeedsChoice asking for instructions (free-text answer).
    /// - `branch` / `commit` → need a value: if `free_text` carries it, fire
    ///   the target; else a 2nd-hop NeedsChoice prompting for the branch/sha
    ///   (the user answers with free text — `parse_review_target` then folds
    ///   `"branch <x>"` / `"commit <x>"`).
    async fn review_apply_choice(
        &self,
        thread_id: &str,
        sel: &ChoiceSelection,
    ) -> Result<DirectiveOutcome, HarnessError> {
        let id = sel.ids.first().map(|s| s.as_str()).unwrap_or("");
        let free = sel.free_text.as_deref().map(str::trim).unwrap_or("");
        match id {
            "uncommitted" => {
                self.review_start(thread_id, json!({ "type": "uncommittedChanges" }))
                    .await
            }
            "branch" | "commit" if !free.is_empty() => {
                self.review_start(thread_id, parse_review_target(&format!("{id} {free}")))
                    .await
            }
            "custom" if !free.is_empty() => {
                self.review_start(thread_id, json!({ "type": "custom", "instructions": free }))
                    .await
            }
            // 2nd-hop: branch/commit/custom picked without a value yet → ask
            // for it as a free-text follow-up (the channel renders a text
            // prompt; the answer comes back as `free_text`).
            "branch" | "commit" | "custom" => {
                let title = match id {
                    "branch" => "Which base branch? (reply with the branch name)",
                    "commit" => "Which commit? (reply with the sha)",
                    _ => "Review instructions? (reply with what to focus on)",
                };
                // A no-option prompt signals "free-text only"; the channel's
                // numbered-text / chips fallback still accepts a typed reply,
                // which the gateway folds back as `free_text` on re-entry.
                Ok(DirectiveOutcome::NeedsChoice(ChoicePrompt {
                    token: mint_choice_token(),
                    title: title.to_string(),
                    options: vec![ChoiceOption {
                        id: id.to_string(),
                        label: format!("(reply with the {id})"),
                    }],
                    multi: false,
                }))
            }
            other => Ok(DirectiveOutcome::Rejected {
                reason: format!("unknown review target: {other}"),
            }),
        }
    }

    /// v0.8.5 D2.1 — mutate (or create) the per-session override entry.
    async fn set_override(&self, thread_id: &str, f: impl FnOnce(&mut SessionOverride)) {
        let mut map = self.overrides.lock().await;
        f(map.entry(thread_id.to_string()).or_default());
    }

    /// v0.8.5 D2 — return the (cached) flattened skills list, fetching from
    /// `skills/list` (common.rs:608) on a cold cache. `force` bypasses the
    /// cache (used by `/skills` so a manual query is always fresh; the
    /// resolution layer uses the cache). Cache is invalidated by the
    /// dispatcher on `skills/changed`.
    async fn skills(&self, force: bool) -> Result<Vec<CachedSkill>, HarnessError> {
        if !force {
            if let Some(c) = self.skills_cache.lock().await.as_ref() {
                return Ok(c.clone());
            }
        }
        let client = self.client().await?;
        let result = client
            .call("skills/list", json!({}))
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("skills/list: {e:#}")))?;
        let flat = flatten_skills(&result);
        *self.skills_cache.lock().await = Some(flat.clone());
        Ok(flat)
    }

    /// v0.8.5 D2 — case-insensitive skill lookup (resolution layer 2).
    async fn find_skill(&self, name: &str) -> Result<Option<CachedSkill>, HarnessError> {
        let skills = self.skills(false).await?;
        Ok(skills
            .into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(name)))
    }

    /// v0.8.5 D2 — up to 3 nearest skill names (substring / shared-prefix)
    /// for a `Rejected` hint. Best-effort; an RPC failure yields no hints.
    async fn nearest_skill_candidates(&self, name: &str) -> Vec<String> {
        let skills = match self.skills(false).await {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let lname = name.to_ascii_lowercase();
        let mut hits: Vec<String> = skills
            .iter()
            .filter(|s| {
                let sn = s.name.to_ascii_lowercase();
                sn.contains(&lname)
                    || lname.contains(&sn)
                    || sn
                        .chars()
                        .next()
                        .zip(lname.chars().next())
                        .map(|(a, b)| a == b)
                        .unwrap_or(false)
            })
            .map(|s| format!("/{}", s.name))
            .collect();
        hits.sort();
        hits.dedup();
        hits.truncate(3);
        hits
    }

    /// Test hook: read the per-session override for a thread.
    #[doc(hidden)]
    pub async fn override_for_test(&self, thread_id: &str) -> SessionOverride {
        self.overrides
            .lock()
            .await
            .get(thread_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Test hook: prime the skills cache directly (skips the `skills/list`
    /// RPC) so resolution tests don't need a scripted peer for the cache.
    #[doc(hidden)]
    pub async fn prime_skills_cache_for_test(&self, skills: Vec<CachedSkill>) {
        *self.skills_cache.lock().await = Some(skills);
    }

    /// Test hook: is the skills cache currently populated?
    #[doc(hidden)]
    pub async fn skills_cache_is_some_for_test(&self) -> bool {
        self.skills_cache.lock().await.is_some()
    }

    /// Test hook: clear the current connection's loaded-thread set WITHOUT
    /// dropping the connection — models a thread that left the app-server's
    /// memory (the bug) so the next turn must `ensure_thread_loaded` → resume
    /// it before `turn/start`. No-op when not connected.
    #[doc(hidden)]
    pub async fn forget_loaded_for_test(&self) {
        if let Some(conn) = self.inner.lock().await.as_ref() {
            conn.loaded.lock().await.clear();
        }
    }
}

/// Map a codex `account/rateLimits/updated` snapshot (the inner `rateLimits`
/// object) → the vendor-neutral [`crate::AccountUsage`]. Codex's `primary`
/// window is the 5-hour limit and `secondary` the weekly (per the codex
/// protocol `windowDurationMins` 300 / 10080), mirroring Claude's two windows.
/// `resetsAt` is an absolute Unix epoch (seconds) → ISO-8601 for the renderer;
/// `planType` → subscription tier; a non-null `rateLimitReachedType` ⇒ weekly
/// ⚠. `credits_pct` stays `None` — codex's `credits` is a balance object, not a
/// utilization %, so the `/status` line correctly omits 额度 for codex.
/// All-`None` ⇒ `None`.
fn account_usage_from_codex_snapshot(snapshot: &Value) -> Option<crate::AccountUsage> {
    // dual-key (camelCase wire / snake_case) numeric lookup within a window.
    let win_i64 = |w: Option<&Value>, snake: &str, camel: &str| -> Option<i64> {
        w.and_then(|x| x.get(camel).or_else(|| x.get(snake)))
            .and_then(|v| v.as_i64())
    };
    let pct = |w: Option<&Value>| {
        win_i64(w, "used_percent", "usedPercent").map(|n| n.clamp(0, 100) as u8)
    };
    let reset_iso = |w: Option<&Value>| {
        win_i64(w, "resets_at", "resetsAt")
            .and_then(|secs| chrono::DateTime::<Utc>::from_timestamp(secs, 0))
            .map(|dt| dt.to_rfc3339())
    };
    let primary = snapshot.get("primary");
    let secondary = snapshot.get("secondary");
    let weekly_severity = snapshot
        .get("rate_limit_reached_type")
        .or_else(|| snapshot.get("rateLimitReachedType"))
        .filter(|v| !v.is_null())
        .map(|_| "warning".to_string());
    let subscription = snapshot
        .get("plan_type")
        .or_else(|| snapshot.get("planType"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let usage = crate::AccountUsage {
        subscription,
        five_hour_pct: pct(primary),
        five_hour_resets_at: reset_iso(primary),
        weekly_pct: pct(secondary),
        weekly_resets_at: reset_iso(secondary),
        weekly_severity,
        credits_pct: None,
    };
    if usage == crate::AccountUsage::default() {
        None
    } else {
        Some(usage)
    }
}

#[async_trait]
impl HarnessAdapter for CodexAppServerAdapter {
    fn name(&self) -> &'static str {
        "codex-app-server"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Codex
    }

    /// Report THIS codex account's usage from the cached
    /// `account/rateLimits/updated` snapshot (account-, not thread-, scoped —
    /// `_h` is ignored). `None` until codex has pushed a snapshot (first
    /// connect / turn); the IM `/status` line then omits the usage row or, via
    /// the gateway's same-vendor fallback, borrows another live codex session.
    async fn account_usage(&self, _h: &ThreadHandle) -> Option<crate::AccountUsage> {
        let snapshot = self.rate_limits.lock().await.clone()?;
        account_usage_from_codex_snapshot(&snapshot)
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if let Some(mode) = ctx.mode.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            return Err(crate::execution::acp::spawn_pick_refused(
                "mode",
                mode,
                "Codex has no session-mode axis (DSH agent presets only today)",
            ));
        }
        // v0.9.0 W3 (F3) — remote execution is claude-only in this version
        // (PRD §五 explicit non-goal: codex/opencode/grok remote is
        // best-effort, verified only by compiling). Fail clean + readable
        // rather than silently spawning the daemon-singleton app-server
        // locally under a remote host id.
        if ctx.remote.is_some() {
            return Err(HarnessError::NotImplemented {
                reason: "remote execution (host != local) is not yet supported for codex; \
                         use host=local"
                    .to_string(),
            });
        }
        // Pick up on-disk `~/.codex/config.toml` edits: `codex app-server`
        // snapshots its config at process start, so re-spawn the shared child
        // when the file changed since it was dialed. Otherwise this new session
        // would silently inherit the config from when the daemon last started.
        self.reload_app_server_if_config_changed().await;
        let client = self.client().await?;
        let cwd_str = ctx.cwd.to_string_lossy().to_string();
        // Inject the ccteam MCP server for THIS thread via
        // `config.mcp_servers.ccteam` (snake_case Codex config schema). The
        // HTTP entry carries this session's sid/secret bearer directly to the
        // daemon; codex 0.144.3 real-machine smoke verifies thread/start,
        // tools/list, and the Authorization header. The global Codex entry is
        // HTTP too, because Codex deep-merges same-named global/thread entries
        // and rejects a mixed `command + url` transport. `None` (no secret) →
        // fall through to the global admin HTTP entry. NOT a system-prompt
        // injection; the developerInstructions line is unchanged.
        let mcp_config =
            crate::execution::mcp_config::codex_thread_mcp_config(&ctx.sid, &ctx.secret);
        let mut start_params = json!({
            "cwd": cwd_str,
            "threadSource": "user",
            "sessionStartSource": "startup",
            "serviceName": format!("ccteam/{}", ctx.slug),
            "developerInstructions": format!(
                "ccteam role: {} (slug={}, sid={})",
                spec.role, ctx.slug, ctx.sid
            ),
        });
        if let Some(cfg) = mcp_config.clone() {
            start_params["config"] = cfg;
        }
        // v0.9.0 W1 (G5) — resume-first. codex `vendor_uuid` is now persisted
        // (raw_extras below), so a daemon rebuild reads the prior thread id from
        // meta and `thread/resume`s it (context preserved) instead of silently
        // starting fresh and dropping the conversation. On resume failure, fall
        // back to a fresh `thread/start` and emit a `chat_session_reset`
        // progress event (the honest context-loss signal), mirroring the
        // claude / opencode ladders.
        let prior_uuid = read_session_meta(&ctx.project_dir, &ctx.sid)
            .ok()
            .map(|m| m.vendor_uuid)
            .filter(|u| !u.trim().is_empty());
        let (thread_id, result) = match prior_uuid {
            Some(uuid) => {
                let mut resume_params = json!({ "threadId": uuid });
                if let Some(cfg) = mcp_config.clone() {
                    resume_params["config"] = cfg;
                }
                match self
                    .call_or_drop_dead(&client, "thread/resume", resume_params)
                    .await
                {
                    Ok(result) => {
                        let tid = pluck_thread_id(&result).unwrap_or(uuid);
                        (tid, result)
                    }
                    Err(resume_err) => {
                        tracing::warn!(
                            sid = %ctx.sid,
                            slug = %ctx.slug,
                            error = %resume_err,
                            "codex thread/resume failed; falling back to fresh thread/start"
                        );
                        if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
                            let ev = build_chat_session_reset_event_with_reason(
                                &spec.role,
                                &ctx.sid,
                                "resume_failed_fallback_to_fresh",
                            );
                            if let Err(err) = append_event(&progress_path, &ev) {
                                tracing::warn!(error = %err, "codex: append reset event failed");
                            }
                        }
                        let result = self
                            .call_or_drop_dead(&client, "thread/start", start_params.clone())
                            .await
                            .map_err(|e| {
                                HarnessError::SpawnFailed(format!("thread/start: {e:#}"))
                            })?;
                        let tid = pluck_thread_id(&result).ok_or_else(|| {
                            HarnessError::SpawnFailed(format!(
                                "thread/start response missing thread.thread_id: {result}"
                            ))
                        })?;
                        (tid, result)
                    }
                }
            }
            None => {
                let result = self
                    .call_or_drop_dead(&client, "thread/start", start_params.clone())
                    .await
                    .map_err(|e| HarnessError::SpawnFailed(format!("thread/start: {e:#}")))?;
                let tid = pluck_thread_id(&result).ok_or_else(|| {
                    HarnessError::SpawnFailed(format!(
                        "thread/start response missing thread.thread_id: {result}"
                    ))
                })?;
                (tid, result)
            }
        };
        // Advisory catalog capture: one cheap RPC on this thread's EXISTING
        // app-server connection. Older servers may not implement model/list;
        // either RPC or cache failures are deliberately silent and never alter
        // the successful thread start/resume above.
        if let Ok(catalog) = client.call("model/list", json!({})).await {
            crate::model_catalog::record_vendor_models_best_effort(
                "codex",
                "codex model/list",
                catalog_models(&catalog),
            );
        }
        // The started / resumed thread is now resident + subscribed on this
        // connection — record it so subsequent turns skip the resume.
        self.mark_loaded(&thread_id).await;
        // v0.8.10 — make codex sessions writable by default. Codex's server
        // default thread posture is restrictive (read-only / approval
        // on-request); combined with ccteam's auto-deny approval callback (no
        // codex→IM HITL wired yet, see codex_jsonrpc.rs) the agent cannot write
        // files. Mirror the Claude `Skip` default (`--dangerously-skip-permissions`
        // — full access incl. network, no prompts): seed every turn with
        // sandbox=dangerFullAccess + approval=never, consistent with ccteam's
        // single-uid full-trust model and overridable per session via
        // `/permissions`. `Hitl` sessions stay on codex's restricted default —
        // codex approval routing to the IM HITL is not wired, so they remain
        // locked down rather than silently bypassing approval.
        if matches!(ctx.permission_mode, PermissionMode::Skip) {
            self.set_override(&thread_id, |o| {
                o.approval_policy = Some("never".to_string());
                o.sandbox_policy = Some(json!({ "type": "dangerFullAccess" }));
            })
            .await;
        }
        // v0.8.24 A-U3 — an explicit spawn-time model / effort choice becomes
        // a sticky `turn/start` override (the SAME machinery the `/model`
        // directive uses — `apply_overrides` folds `model:114` / `effort:126`
        // into every turn/start). Without this the ctx choice was
        // display-only (tracker seeding below); now it actually reaches
        // codex. Effort values are codex's `ReasoningEffort` set
        // (`none|minimal|low|medium|high|xhigh`).
        {
            let model = ctx.model_id.clone().filter(|m| !m.trim().is_empty());
            let effort = ctx.effort.clone().filter(|e| !e.trim().is_empty());
            if model.is_some() || effort.is_some() {
                self.set_override(&thread_id, move |o| {
                    if let Some(m) = model {
                        o.model = Some(m);
                    }
                    if let Some(e) = effort {
                        o.effort = Some(e);
                    }
                })
                .await;
            }
        }
        // v0.8.5 D2.4 / v0.8.19 — seed the tracker's model + effort for this
        // thread so `/status` + `thread_status` can report them before the
        // first tokenUsage notification arrives. DETERMINISTIC precedence:
        // the user's explicit `ctx.model_id` wins; otherwise codex's RESOLVED
        // model echoed in the `thread/start` response (`result.model` — see
        // [`pluck_model`]). Never inferred. This fixes the blank statusline
        // model for sessions started without an explicit model (codex's
        // server default). Effort comes only from the response (codex owns
        // it; `result.reasoningEffort`).
        {
            let model = ctx.model_id.clone().or_else(|| pluck_model(&result));
            // v0.8.24 A-U3 — an explicit ctx effort wins (it is now also an
            // override, so it is what codex will run); else the response echo.
            let effort = ctx
                .effort
                .clone()
                .filter(|e| !e.trim().is_empty())
                .or_else(|| pluck_effort(&result));
            let mut tracker = self.tracker.lock().await;
            let entry = tracker.entry(&thread_id);
            if model.is_some() {
                entry.model = model;
            }
            if effort.is_some() {
                entry.effort = effort;
            }
        }
        // V0.6.1 F122 — register a progress bridge so the events()
        // stream can mirror turn boundaries into progress.jsonl.
        // progress path resolution honours CCTEAM_HOME so test runs land
        // in their tempdir layout; production lands in ~/.ccteam/progress/.
        if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
            self.register_bridge(
                thread_id.clone(),
                ProgressBridgeCtx {
                    progress_path: progress_path.clone(),
                    role: spec.role.clone(),
                    sid: ctx.sid.clone(),
                    slug: ctx.slug.clone(),
                    model: ctx.model_id.clone(),
                },
            )
            .await;
            // V0.8 rmux Slice 4 — Codex mode-3 typed-event producer.
            // Gated on `CCTEAM_TYPED_EVENTS`; subscribes to JSON-RPC
            // notifications and writes `typed_event` rows directly to
            // the same `progress.jsonl`. Bypasses `EventMerger`
            // (no pane base side) — see module docs at
            // `execution/codex_typed_events.rs`.
            let _ = crate::execution::codex_typed_events::maybe_start_codex_typed_event_tap(
                Arc::clone(&client),
                progress_path,
            );
        }
        Ok(ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Chat,
            identity: thread_id.clone(),
            started_at: Utc::now(),
            raw_extras: json!({
                "thread_id": thread_id,
                // v0.9.0 W1 (G5) — persist the codex thread id as the vendor
                // resume key. `apply_new_session` reads `raw_extras.vendor_uuid`
                // into meta.json, so a daemon rebuild `thread/resume`s this
                // exact thread (see the resume-first ladder above) instead of
                // silently starting fresh.
                "vendor_uuid": thread_id,
                // F10: the RESOLVED transport, not an env echo —
                // "stdio" (default child-spawn) or "socket" (UDS override).
                "transport": self.transport_tag(),
                "socket": Self::resolve_socket_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            }),
        })
    }

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        if routing == TurnRouting::Queue {
            return Err(HarnessError::NotImplemented {
                reason: "codex app-server does not expose a distinct queued-turn channel".into(),
            });
        }
        // Deterministic precondition: guarantee this thread is resident on the
        // current connection (resume-once-per-epoch) BEFORE sending the turn,
        // so `turn/start` can never hit `thread not found`.
        let client = self.ensure_thread_loaded(&h.identity).await?;
        if std::env::var(APP_SERVER_FAULT_KILL_BEFORE_TURN_ENV)
            .ok()
            .as_deref()
            == Some("1")
        {
            client.terminate_stdio_child().await.map_err(|e| {
                HarnessError::SubmitFailed(format!("codex app-server fault injection: {e:#}"))
            })?;
        }
        let items = turn_input_to_items(input)?;
        let input_id = TurnSubmission::mint_input_id();
        // v0.8.5 D2.2 — active-turn interjection: if the tracker shows an
        // in-flight turn for this thread, steer it (`turn/steer` with the
        // required `expectedTurnId` precondition) rather than starting a new
        // one. This mirrors the Claude send-keys "talk to the running turn"
        // experience. With no active turn we fall through to `turn/start`
        // (applying any per-session overrides, D2.1).
        let active_turn = self
            .tracker_snapshot(&h.identity)
            .await
            .and_then(|t| t.active_turn);
        let was_active = active_turn.is_some();
        let (method, params) = if let Some(expected) = active_turn {
            (
                "turn/steer",
                json!({
                    "threadId": h.identity,
                    "clientUserMessageId": input_id.clone(),
                    "input": items,
                    "expectedTurnId": expected,
                }),
            )
        } else {
            let mut params = json!({
                "threadId": h.identity,
                "clientUserMessageId": input_id.clone(),
                "input": items,
            });
            self.apply_overrides(&h.identity, &mut params).await;
            ("turn/start", params)
        };
        // F10 self-heal hook: if the turn RPC fails (e.g. the stdio codex
        // child crashed — every codex session shares one child), drop the
        // cached client so the NEXT turn re-dials a fresh app-server.
        // Without this the dead client stays memoised and every subsequent
        // turn fails. The error is still surfaced to the caller (the current
        // in-flight turn is not auto-retried — crash recovery semantics,
        // see arch §1.1).
        // Drop the SHARED connection only on a genuine transport death (via
        // `call_or_drop_dead`); a logical error from a live peer leaves it — and
        // every other session's loaded thread — intact. The current turn is not
        // auto-retried; the next turn re-dials + `ensure_thread_loaded` re-resumes.
        let result = self
            .call_or_drop_dead(&client, method, params)
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("{method}: {e:#}")))?;
        let turn_id = pluck_turn_id(&result).ok_or_else(|| {
            HarnessError::SubmitFailed(format!("{method} response missing turn.id: {result}"))
        })?;
        let turn_id = TurnId(turn_id);
        if was_active {
            Ok(TurnSubmission::injected_with_input_id(turn_id, input_id))
        } else {
            Ok(TurnSubmission::started_with_input_id(turn_id, input_id))
        }
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let adapter_setup = self.clone();
        let adapter_bridge = self.clone();
        let thread_id = h.identity.clone();
        // Build a futures stream by chaining: (1) one-shot setup that
        // either yields an Error event or returns a broadcast receiver,
        // then (2) the receiver-driven event flow with thread-id
        // filtering. If we can't get a client yet, surface a single
        // Error event and stop — orchestrator's progress.jsonl poller
        // remains the state-transition SoT (Wave 1 contract).
        let setup = async move {
            match adapter_setup.client().await {
                Ok(c) => Ok(c.subscribe()),
                Err(err) => Err(ThreadErrorEvent {
                    kind: "connect".into(),
                    message: err.to_string(),
                }),
            }
        };
        let s = stream::once(setup).flat_map(move |outcome| {
            let thread_id = thread_id.clone();
            let adapter_bridge = adapter_bridge.clone();
            match outcome {
                Err(err) => {
                    // F122: connect failures still bridge to progress.jsonl
                    // when a bridge ctx is registered (e.g. a test that
                    // registers manually and then drops the peer). Fire
                    // a best-effort write before yielding the Error event.
                    let adapter_for_err = adapter_bridge.clone();
                    let wanted = thread_id.clone();
                    let err_for_evt = err.clone();
                    let s = stream::once(async move {
                        if let Some(ctx) = adapter_for_err.bridge_for(&wanted).await {
                            if let Some(line) = build_progress_line(
                                &ThreadEvent::Error(err_for_evt.clone()),
                                &wanted,
                                &ctx,
                            ) {
                                let _ = append_event(&ctx.progress_path, &line);
                            }
                            adapter_for_err.drop_bridge(&wanted).await;
                        }
                        ThreadEvent::Error(err_for_evt)
                    });
                    s.boxed()
                }
                Ok(rx) => {
                    let s = stream::unfold(
                        (rx, adapter_bridge, None),
                        move |(mut rx, bridge, mut turn_usage)| {
                            let wanted = thread_id.clone();
                            async move {
                                loop {
                                    match rx.recv().await {
                                        Ok(notif) => {
                                            if notif.method == "turn/started" {
                                                turn_usage = None;
                                            }
                                            if let Some(usage) =
                                                codex_turn_usage_from_notification(&notif)
                                            {
                                                turn_usage = Some(usage);
                                            }
                                            if let Some(mut evt) =
                                                translate_notification(&notif, &wanted)
                                            {
                                                let ctx = bridge.bridge_for(&wanted).await;
                                                if matches!(evt, ThreadEvent::TurnFailed { .. }) {
                                                    let fallback_model = match ctx
                                                        .as_ref()
                                                        .and_then(|ctx| ctx.model.clone())
                                                    {
                                                        Some(model) => Some(model),
                                                        None => bridge
                                                            .tracker_snapshot(&wanted)
                                                            .await
                                                            .and_then(|live| live.model),
                                                    };
                                                    enrich_codex_turn_failed(
                                                        &mut evt,
                                                        turn_usage,
                                                        fallback_model,
                                                    );
                                                }
                                                let terminal = is_terminal_progress(&evt);
                                                if let Some(ctx) = ctx {
                                                if let Some(line) =
                                                    build_progress_line(&evt, &wanted, &ctx)
                                                {
                                                    if let Err(err) = append_event(
                                                        &ctx.progress_path,
                                                        &line,
                                                    ) {
                                                        tracing::warn!(
                                                            thread_id = %wanted,
                                                            error = %err,
                                                            "codex bridge: append progress.jsonl failed"
                                                        );
                                                    }
                                                    // Terminal events: drop
                                                    // the bridge so we don't
                                                    // double-write if codex
                                                    // re-fires the same
                                                    // notification.
                                                    if terminal {
                                                        bridge.drop_bridge(&wanted).await;
                                                    }
                                                }
                                            }
                                                if terminal {
                                                    turn_usage = None;
                                                }
                                                return Some((evt, (rx, bridge, turn_usage)));
                                            }
                                        // V0.8 rmux W4-fu — some Codex
                                        // notifications (plan/tokenUsage/
                                        // status/rateLimits) carry no
                                        // `ThreadEvent` variant but must
                                        // still land in progress.jsonl as
                                        // additive observability rows. They
                                        // are NOT terminal and NOT yielded
                                        // into the event stream; the bridge
                                        // mirrors them and the loop keeps
                                        // pumping the next notification.
                                            if let Some(ctx) = bridge.bridge_for(&wanted).await {
                                                if let Some(line) =
                                                    build_codex_notification_progress_line(
                                                        &notif, &wanted,
                                                    )
                                                {
                                                    if let Err(err) =
                                                        append_event(&ctx.progress_path, &line)
                                                    {
                                                        tracing::warn!(
                                                            thread_id = %wanted,
                                                            error = %err,
                                                            "codex bridge: append codex-notif progress.jsonl failed"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        Err(
                                            tokio::sync::broadcast::error::RecvError::Lagged(n),
                                        ) => {
                                            tracing::warn!(
                                                n,
                                                "codex app-server event subscriber lagged"
                                            );
                                            continue;
                                        }
                                        Err(
                                            tokio::sync::broadcast::error::RecvError::Closed,
                                        ) => {
                                            return None;
                                        }
                                    }
                                }
                            }
                        },
                    );
                    s.boxed()
                }
            }
        });
        Box::pin(s)
    }

    fn event_attachment(&self) -> crate::EventAttachment {
        // THE case this exists for. Every codex session shares ONE app-server
        // connection, and that connection is replaced out from under them by
        // design — `forget_client` on transport death, and the planned respawn
        // when `config.toml` changes. A re-call re-dials (`client()`) and
        // re-subscribes to the current broadcast, exactly like
        // `ensure_thread_loaded` re-resumes the thread on the write side.
        crate::EventAttachment::Rebuildable
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<crate::ToolSurfaceRebuild, HarnessError> {
        Ok(crate::ToolSurfaceRebuild::RespawnRequired {
            reason: "codex applies ccteam's MCP config when a thread is started or resumed \
             (`thread/start` / `thread/resume` config override) and the app-server \
             offers no mid-thread re-apply — `/new` rebuilds the tool face"
                .to_string(),
        })
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        let client = self.client().await?;
        let result = self
            .call_or_drop_dead(
                &client,
                "thread/resume",
                json!({ "threadId": persistent_id }),
            )
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("thread/resume: {e:#}")))?;
        let thread_id = pluck_thread_id(&result).unwrap_or_else(|| persistent_id.to_string());
        // Resumed thread is resident + subscribed on this connection — record
        // it so the first turn doesn't redundantly resume again.
        self.mark_loaded(&thread_id).await;
        // v0.8.19 — seed the tracker's model + effort from the resume
        // response (`result.model` / `result.reasoningEffort` — see
        // [`pluck_model`]). Resumed threads have no `ctx.model_id` (resume
        // takes a bare persistent id), so codex's RESOLVED model is the sole
        // deterministic source. This is what fixes the blank statusline model
        // on a daemon-restart-resumed codex session (e.g. the live s28).
        // Never inferred; only set from a real value codex reports.
        {
            let model = pluck_model(&result);
            let effort = pluck_effort(&result);
            let mut tracker = self.tracker.lock().await;
            let entry = tracker.entry(&thread_id);
            if model.is_some() {
                entry.model = model;
            }
            if effort.is_some() {
                entry.effort = effort;
            }
        }
        Ok(ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Chat,
            identity: thread_id.clone(),
            started_at: Utc::now(),
            raw_extras: json!({ "thread_id": thread_id, "vendor_uuid": thread_id, "resumed": true }),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        // Best-effort archive — codex's `thread/archive` is the
        // "release server-side state" hook. Failure is logged but
        // never escalated (idempotent close semantics).
        let Ok(client) = self.client().await else {
            // No socket = nothing to close; matches V0.5.x missing-tmux
            // semantics for close_thread.
            return Ok(());
        };
        let archive = client
            .call("thread/archive", json!({ "threadId": h.identity }))
            .await;
        if let Err(err) = archive {
            tracing::warn!(thread_id = %h.identity, error = %err, "thread/archive failed (best-effort)");
        }
        let _ = client
            .call("thread/unsubscribe", json!({ "threadId": h.identity }))
            .await;
        // Archived + unsubscribed → codex unloads it; drop it from the
        // loaded set so a stale entry never suppresses a future resume.
        if let Some(conn) = self.inner.lock().await.as_ref() {
            conn.loaded.lock().await.remove(&h.identity);
        }
        Ok(())
    }

    /// v0.8.5 D2 — the full Codex command surface, three-layer resolution:
    ///
    /// 1. builtin mapping table ([`Self::builtin_directive`]) → RPC /
    ///    query-synth (`Done`) / per-session override (`Done`) / `Redirect` /
    ///    `Rejected` (TUI-only).
    /// 2. miss → `skills/list` cache (case-insensitive) → `turn/start` with
    ///    a `Skill` input → `Turn`.
    /// 3. still miss → `Rejected` with nearest candidates + `/skills` hint.
    ///
    /// Every server-side state-machine error (e.g. a thread-busy `/compact`)
    /// propagates verbatim as `SubmitFailed` — we deliberately do NOT
    /// reimplement the TUI's `available_during_task` guard (PRD §3-D2.1).
    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();
        if name.is_empty() {
            return Ok(DirectiveOutcome::Rejected {
                reason: "empty command".to_string(),
            });
        }
        // Every directive below is a thread-scoped RPC (compact / review / init
        // / fork / skill / …); guarantee the thread is resident on the current
        // connection first so none of them can hit `thread not found`.
        let _ = self.ensure_thread_loaded(&h.identity).await?;
        // Layer 1: builtin table. `None` = not a builtin → fall through.
        if let Some(outcome) = self.builtin_directive(h, &name, &d).await? {
            return Ok(outcome);
        }
        // Layer 2: dynamic skill match.
        if let Some(skill) = self.find_skill(&name).await? {
            if !skill.enabled {
                // D2.3 — a known-but-disabled skill is a clear receipt, not a
                // silent passthrough.
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!(
                        "/{name} is a skill but is not enabled — enable it then retry."
                    ),
                });
            }
            let client = self.client().await?;
            let mut input = vec![json!({
                "type": "skill",
                "name": skill.name,
                "path": skill.path,
            })];
            if !d.args.trim().is_empty() {
                input.push(json!({ "type": "text", "text": d.args.trim() }));
            }
            let mut params = json!({ "threadId": h.identity, "input": input });
            self.apply_overrides(&h.identity, &mut params).await;
            let result = client
                .call("turn/start", params)
                .await
                .map_err(|e| HarnessError::SubmitFailed(format!("turn/start (skill): {e:#}")))?;
            let turn_id = pluck_turn_id(&result).ok_or_else(|| {
                HarnessError::SubmitFailed(format!(
                    "turn/start (skill) response missing turn.id: {result}"
                ))
            })?;
            return Ok(DirectiveOutcome::Turn(TurnId(turn_id)));
        }
        // Layer 3: reject with nearest candidates.
        let candidates = self.nearest_skill_candidates(&name).await;
        let hint = if candidates.is_empty() {
            "Use /skills to see available skills.".to_string()
        } else {
            format!(
                "Did you mean: {}? Use /skills to see all.",
                candidates.join(", ")
            )
        };
        Ok(DirectiveOutcome::Rejected {
            reason: format!("/{name} is not a Codex command or known skill. {hint}"),
        })
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // P3 (D2.4) — read the harness-owned tracker, fed by the single
        // dispatcher from `thread/tokenUsage/updated` (usage) + spawn ctx
        // (model). No RPC: this is a pure in-memory read.
        let live = self.tracker_snapshot(&h.identity).await.unwrap_or_default();
        Ok(ThreadStatus {
            model: live.model,
            context: live.usage,
            // v0.8.19 — codex's RESOLVED reasoning effort, captured
            // deterministically from the `thread/start` / `thread/resume`
            // response (`result.reasoningEffort`). `None` when codex reports
            // none (keeps the Codex suffix unchanged in that case).
            effort: live.effort,
            // Codex has a native `/goal` (thread/goal/*); surfacing it in the
            // statusline is a follow-up — None for now.
            goal: None,
        })
    }

    /// Interrupt the in-flight turn via codex's `turn/interrupt` RPC (the same
    /// call the `/interrupt` directive already wired). The active turn id comes
    /// from the harness-owned tracker (`active_turn`, set on `turn/started`).
    /// The RPC rides the JSON-RPC client OUT-OF-BAND (it is not a `turn/start`
    /// in the turn queue), so it reaches codex while the turn is mid-stream and
    /// leaves the thread alive: no `thread/archive`, no unsubscribe. No active
    /// turn → a clean no-op (nothing to interrupt), never an error — so a
    /// gateway `/interrupt` on an idle codex session is harmless.
    async fn interrupt_turn(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let active = self
            .tracker_snapshot(&h.identity)
            .await
            .and_then(|t| t.active_turn);
        let Some(turn_id) = active else {
            // No in-flight turn — nothing to stop. Idempotent success.
            return Ok(());
        };
        let client = self.client().await?;
        client
            .call(
                "turn/interrupt",
                json!({ "threadId": h.identity, "turnId": turn_id }),
            )
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("turn/interrupt: {e:#}")))?;
        Ok(())
    }

    /// Codex's title surface is the `thread/name/set` RPC (`thread.rs:660`) —
    /// the same call the `/rename` directive makes, so an IM/web rename and a
    /// user-typed `/rename` land on one implementation. It needs a live
    /// connection with the thread resident, so a STOPPED session is an honest
    /// [`TitleSync::Deferred`]: unlike Claude there is no file to write.
    async fn set_session_title(
        &self,
        target: &crate::SessionTitleTarget,
        title: &str,
    ) -> Result<crate::TitleSync, HarnessError> {
        let Some(thread) = target.thread.as_ref() else {
            return Ok(crate::TitleSync::Deferred(
                "codex thread names are set over a live connection; resume the session to sync"
                    .into(),
            ));
        };
        let tid = thread.identity.as_str();
        // Same precondition every thread-scoped directive takes: the thread
        // must be resident on THIS connection or the RPC 404s.
        let _ = self.ensure_thread_loaded(tid).await?;
        self.set_thread_name(tid, title).await?;
        Ok(crate::TitleSync::Pushed)
    }
}

/// Whether a [`CodexJsonRpcClient::call`] error is a *transport death*
/// (connection lost / writer-reader task gone) rather than a logical error
/// response from a live peer. Used to scope `forget_client`: tearing down the
/// SHARED child connection on a logical error (e.g. a model/state error, or a
/// `thread not found`) would evict every other codex session's loaded thread.
///
/// Deterministic discriminator: a real JSON-RPC error *response* from a live
/// app-server always carries a numeric `code` (`JsonRpcError.code == Some`).
/// codex's `fail_pending` connection-loss path sets `code: None`, and any
/// non-RPC failure (writer channel closed, request send error) is not a
/// `JsonRpcError` at all — both mean the transport is gone.
/// Resolve `$CODEX_HOME/config.toml`, falling back to `~/.codex/config.toml`
/// — the same file `codex app-server` reads. ccteam inherits `CODEX_HOME` into
/// the child, so both resolve identically.
fn codex_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    Some(home.join("config.toml"))
}

/// Last-modified time of codex's `config.toml`, or `None` if it can't be
/// resolved / stat'd (treated as "unknown" — never forces a re-spawn).
fn codex_config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(codex_config_path()?)
        .ok()?
        .modified()
        .ok()
}

/// Outcome of the config-change check in [`CodexAppServerAdapter::reload_app_server_if_config_changed`].
#[derive(Debug, PartialEq, Eq)]
enum ConfigReloadDecision {
    /// Wrong transport, no child yet, unreadable file, or unchanged mtime.
    Skip,
    /// Config changed and the child is idle → re-spawn.
    Respawn,
    /// Config changed but a turn is in flight → skip and warn.
    Busy,
}

/// Pure decision for whether an on-disk `config.toml` edit should re-spawn the
/// cached codex app-server child. Split out so the branching is unit-testable
/// without a live child. See [`CodexAppServerAdapter::reload_app_server_if_config_changed`]
/// for the guard rationale.
fn decide_config_reload(
    is_stdio: bool,
    had_child: bool,
    stored_mtime: Option<std::time::SystemTime>,
    disk_mtime: Option<std::time::SystemTime>,
    any_active_turn: bool,
) -> ConfigReloadDecision {
    // Only the ccteam-owned Stdio child can be re-spawned; a change is only
    // actionable when we have a child, can read the file now, and its mtime
    // differs from the dial-time snapshot.
    if !is_stdio || !had_child || disk_mtime.is_none() || stored_mtime == disk_mtime {
        return ConfigReloadDecision::Skip;
    }
    if any_active_turn {
        ConfigReloadDecision::Busy
    } else {
        ConfigReloadDecision::Respawn
    }
}

fn is_transport_death(err: &anyhow::Error) -> bool {
    match err.downcast_ref::<JsonRpcError>() {
        Some(e) => e.code.is_none(),
        None => true,
    }
}

fn synthetic_command_turn_id(command: &str, thread_id: &str) -> TurnId {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    TurnId::new(format!("codex-app-server-{command}-{thread_id}-{nanos:x}"))
}

// =====================================================================
// v0.8.5 D4 — Codex bare-popup → two-step `NeedsChoice`.
//
// For the eight popup commands a BARE invocation (no args, no choice)
// returns `DirectiveOutcome::NeedsChoice(ChoicePrompt)` built from the
// list source (a list RPC or a static enum); the gateway renders it,
// the user picks, and `handle_directive` is re-entered with the same
// (bare) name + `d.choice` set to the picked id, which the apply arm
// folds into the D2 path (override / RPC / gateway redirect). An
// invocation WITH args skips the list and applies directly (D2 behaviour,
// unchanged).
//
// `mint_choice_token` produces the ≤16-byte ASCII, `:`-free correlation
// id the gateway packs into `"{token}:{idx}"` (arch §D3). The gateway
// resolves callbacks token-globally, so uniqueness is all that matters.
// =====================================================================

/// v0.8.5 D4 — mint a short opaque ChoicePrompt token. `cx` + the low 40
/// bits of the nanosecond clock as hex → ≤12 ASCII bytes, no `:`
/// (satisfies the `ChoicePrompt::token` contract: ASCII, ≤16 bytes, no
/// `:`). Token globality means collisions only matter within the TTL
/// window; 40 bits of clock entropy is ample.
fn mint_choice_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("cx{:x}", (nanos as u64) & 0xff_ffff_ffff)
}

/// v0.8.5 D4 — build a single-select [`ChoicePrompt`] with a fresh token.
fn choice_prompt(title: impl Into<String>, options: Vec<ChoiceOption>) -> ChoicePrompt {
    ChoicePrompt {
        token: mint_choice_token(),
        title: title.into(),
        options,
        multi: false,
    }
}

/// v0.8.5 D4 — the picked id from a re-entry selection (first id wins; we
/// only mint single-select prompts). Falls back to `free_text` (the
/// "Other" path) when no id was chosen.
fn picked_id(sel: &ChoiceSelection) -> Option<String> {
    sel.ids
        .first()
        .cloned()
        .or_else(|| sel.free_text.clone().filter(|s| !s.trim().is_empty()))
}

/// v0.8.5 D4 — static `Personality` options (config_types.rs:293,
/// `#[serde(rename_all="lowercase")]`).
fn personality_options() -> Vec<ChoiceOption> {
    ["none", "friendly", "pragmatic"]
        .into_iter()
        .map(|p| ChoiceOption {
            id: p.to_string(),
            label: p.to_string(),
        })
        .collect()
}

/// v0.8.5 D4 — static memory-mode options (`thread/memoryMode/set` mode,
/// thread.rs:830, lowercase `enabled`/`disabled`). The option `id` is the
/// `/memories` arg form (`on`/`off`) the apply arm already parses.
fn memory_mode_options() -> Vec<ChoiceOption> {
    vec![
        ChoiceOption {
            id: "on".to_string(),
            label: "enabled".to_string(),
        },
        ChoiceOption {
            id: "off".to_string(),
            label: "disabled".to_string(),
        },
    ]
}

/// v0.8.5 D4 — static `/permissions` preset options. The option `id` is
/// the preset name the existing `permissions_preset` parser accepts, so
/// the apply arm is unchanged. Authorities: `AskForApproval` (shared.rs:162)
/// + `SandboxMode` (shared.rs:292).
fn permissions_options() -> Vec<ChoiceOption> {
    vec![
        ChoiceOption {
            id: "read-only".to_string(),
            label: "read-only (approval on-request)".to_string(),
        },
        ChoiceOption {
            id: "auto".to_string(),
            label: "workspace-write (approval on-request)".to_string(),
        },
        ChoiceOption {
            id: "full-access".to_string(),
            label: "full-access (approval never)".to_string(),
        },
    ]
}

/// v0.8.5 D4 — the four `ReviewTarget` fixed options (review.rs:43-65).
/// `uncommitted` / `branch` / `commit` / `custom` map to the `/review`
/// arg form; `branch` + `commit` are 2nd-hop (they need a branch/sha),
/// so their ids carry the keyword the apply arm re-prompts on.
fn review_options() -> Vec<ChoiceOption> {
    vec![
        ChoiceOption {
            id: "uncommitted".to_string(),
            label: "uncommitted changes".to_string(),
        },
        ChoiceOption {
            id: "branch".to_string(),
            label: "against a base branch…".to_string(),
        },
        ChoiceOption {
            id: "commit".to_string(),
            label: "a specific commit…".to_string(),
        },
        ChoiceOption {
            id: "custom".to_string(),
            label: "custom instructions…".to_string(),
        },
    ]
}

/// v0.8.5 D4 — map a `model/list` response (model.rs:90
/// `supportedReasoningEfforts`) into `ChoiceOption`s. One option per
/// (model, effort) so the picked id is the exact `/model <id> [effort]`
/// arg form the override arm parses; a model with no efforts yields a
/// single bare-id option.
fn model_options(result: &Value) -> Vec<ChoiceOption> {
    let mut out = Vec::new();
    let Some(models) = result.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for m in models {
        let Some(id) = m.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let efforts: Vec<&str> = m
            .get("supportedReasoningEfforts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("reasoningEffort").and_then(|v| v.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        if efforts.is_empty() {
            out.push(ChoiceOption {
                id: id.to_string(),
                label: id.to_string(),
            });
        } else {
            for e in efforts {
                out.push(ChoiceOption {
                    id: format!("{id} {e}"),
                    label: format!("{id} ({e})"),
                });
            }
        }
    }
    out
}

/// Map the same vendor `model/list` response into the neutral last-seen cache.
/// This is presentation-only; ids remain opaque and are never validated.
fn catalog_models(result: &Value) -> Vec<crate::model_catalog::CatalogModel> {
    let Some(models) = result.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }
            let display_name = model
                .get("displayName")
                .or_else(|| model.get("display_name"))
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string);
            let efforts = model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .map(|efforts| {
                    efforts
                        .iter()
                        .filter_map(|effort| {
                            effort
                                .get("reasoningEffort")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(crate::model_catalog::CatalogModel {
                id: id.to_string(),
                display_name,
                efforts,
            })
        })
        .collect()
}

/// v0.8.5 D4 — map a `collaborationMode/list` response (EXPERIMENTAL,
/// collaboration_mode.rs:43 `CollaborationModeMask{name, mode, ..}`) into
/// `ChoiceOption`s. The picked id is the `ModeKind` (preferred) or the
/// preset `name`, which the `/collab <m>` override arm parses.
fn collab_options(result: &Value) -> Vec<ChoiceOption> {
    let mut out = Vec::new();
    let Some(modes) = result.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for m in modes {
        // Prefer the ModeKind (snake_case wire) as the apply id; fall back
        // to the human `name` when `mode` is absent.
        let mode = m.get("mode").and_then(|v| v.as_str());
        let name = m.get("name").and_then(|v| v.as_str());
        let (id, label) = match (mode, name) {
            (Some(mode), Some(name)) => (mode.to_string(), format!("{name} ({mode})")),
            (Some(mode), None) => (mode.to_string(), mode.to_string()),
            (None, Some(name)) => (name.to_string(), name.to_string()),
            (None, None) => continue,
        };
        out.push(ChoiceOption { id, label });
    }
    out
}

/// v0.8.5 D4 — map a `thread/list` response (thread.rs:1073
/// `ThreadListResponse{data: Vec<Thread>}`) into `ChoiceOption`s. The
/// picked id is the codex thread id (the gateway switches to it via
/// `/use <id>`); the label prefers the thread `name`, else its `preview`.
fn resume_options(result: &Value) -> Vec<ChoiceOption> {
    let mut out = Vec::new();
    let Some(threads) = result.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for t in threads {
        let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let label = t
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                t.get("preview")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .map(|s| {
                // Keep labels short for inline-keyboard buttons.
                if s.chars().count() > 48 {
                    let truncated: String = s.chars().take(45).collect();
                    format!("{truncated}…")
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| id.to_string());
        out.push(ChoiceOption {
            id: id.to_string(),
            label,
        });
    }
    out
}

/// v0.8.5 D4 — the `/resume` redirect: Codex has no in-thread resume, so a
/// picked thread id is surfaced as a `Redirect` instructing the gateway's
/// `/use <id>` session switch (the gateway owns session lifecycle).
fn resume_redirect(thread_id: &str) -> DirectiveOutcome {
    DirectiveOutcome::Redirect {
        hint: format!("/use {thread_id}"),
    }
}

/// v0.8.5 D2.1 — parse `/review` args into a `ReviewTarget` JSON value.
/// Mirrors `ReviewTarget` (`review.rs:43-65`, `#[serde(tag="type",
/// rename_all="camelCase")]`):
/// - bare → `{"type":"uncommittedChanges"}`
/// - `branch <b>` → `{"type":"baseBranch","branch":<b>}`
/// - `commit <sha>` → `{"type":"commit","sha":<sha>,"title":null}`
/// - anything else → `{"type":"custom","instructions":<args>}`
pub fn parse_review_target(args: &str) -> Value {
    let args = args.trim();
    if args.is_empty() {
        return json!({ "type": "uncommittedChanges" });
    }
    let mut parts = args.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match head.to_ascii_lowercase().as_str() {
        "branch" if !rest.is_empty() => json!({ "type": "baseBranch", "branch": rest }),
        "commit" if !rest.is_empty() => {
            json!({ "type": "commit", "sha": rest, "title": Value::Null })
        }
        _ => json!({ "type": "custom", "instructions": args }),
    }
}

/// v0.8.5 D2.1 — map a `/permissions` preset to `(approval_policy wire
/// string, sandbox_policy wire OBJECT, human label)`. Authorities:
/// `AskForApproval` is kebab-case strings (`shared.rs:162`); `SandboxPolicy`
/// is an internally-tagged `{"type":...}` object (`permissions.rs`
/// `SandboxPolicy`, camelCase variant tags). We expose a small safe preset
/// set rather than the full enum surface.
fn permissions_preset(args: &str) -> Option<(&'static str, Value, &'static str)> {
    match args.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "read-only" | "readonly" | "ro" => {
            Some(("on-request", json!({ "type": "readOnly" }), "read-only"))
        }
        "auto" | "workspace" | "workspace-write" => Some((
            "on-request",
            json!({ "type": "workspaceWrite" }),
            "workspace-write",
        )),
        "full-access" | "full" | "danger" => Some((
            "never",
            json!({ "type": "dangerFullAccess" }),
            "full-access",
        )),
        _ => None,
    }
}

/// v0.8.5 D2.1/D2.5 — the Codex TUI-only / unsupported-in-chat slash
/// commands (no meaningful app-server line); these are explicitly
/// `Rejected` from chat rather than passed through. Sourced from
/// `tui/src/slash_command.rs` (the TUI `SlashCommand` enum, b2344d8) —
/// `available_during_task()`/`supports_inline_args()` are TUI internals
/// ccteam cannot call, so the list is held here (PRD §3-D2.5).
///
/// The D2.5 drift test (`codex_app_server_test.rs`) asserts every
/// `SlashCommand` enum name is covered by EITHER this reject list OR the
/// builtin table ([`is_builtin_command`]); a new codex command that lands
/// in neither fails that test, forcing a classification decision here.
///
/// Categories rejected:
/// - pure TUI surface (theme/vim/keymap/statusline/title/copy/raw/mention/
///   ide/settings/realtime/quit/exit/feedback/rollout/ps/pets);
/// - TUI session/agent navigation with no in-thread chat equivalent
///   (agent/subagents/side/btw/archive — the gateway owns session
///   lifecycle, so these would be misleading);
/// - TUI config toggles (experimental/setup-default-sandbox/
///   sandbox-add-read-dir);
/// - the auto-review retry approval flow (approve);
/// - plugins browser + debug-only commands (plugins/debug-config/
///   test-approval/debug-m-drop/debug-m-update).
fn is_codex_tui_only(name: &str) -> bool {
    matches!(
        name,
        // pure TUI surface
        "theme"
            | "vim"
            | "keymap"
            | "statusline"
            | "title"
            | "copy"
            | "raw"
            | "mention"
            | "ide"
            | "settings"
            | "realtime"
            | "quit"
            | "exit"
            | "feedback"
            | "rollout"
            | "ps"
            | "pets"
            // TUI session/agent navigation (gateway owns session lifecycle)
            | "agent"
            | "subagents"
            | "side"
            | "btw"
            | "archive"
            // TUI config toggles
            | "experimental"
            | "setup-default-sandbox"
            | "sandbox-add-read-dir"
            // auto-review retry approval flow
            | "approve"
            // plugins browser + debug-only
            | "plugins"
            | "debug-config"
            | "test-approval"
            | "debug-m-drop"
            | "debug-m-update"
    )
}

/// v0.8.5 D2.5 — drift-test classifier: `true` for a command token the
/// builtin table ([`CodexAppServerAdapter::builtin_directive`]) handles
/// with a meaningful outcome (RPC / query-synth / override / redirect).
/// This list MUST stay in lockstep with the `match name` arms there; the
/// D2.5 snapshot test relies on it to prove no codex command is silently
/// dropped. (Free fn so the test can call it without an adapter; it has no
/// state.)
pub fn is_builtin_command(name: &str) -> bool {
    matches!(
        name,
        // RPC direct-map
        "compact"
            | "review"
            | "interrupt"
            | "fork"
            | "rollback"
            | "rename"
            | "goal"
            | "stop"
            | "memories"
            | "diff"
            | "init"
            | "login"
            | "logout"
            // query-synth
            | "status"
            | "model"
            | "skills"
            | "mcp"
            | "hooks"
            | "apps"
            // per-session override
            | "personality"
            | "collab"
            | "plan"
            | "permissions"
            // semantic redirect
            | "new"
            | "clear"
            | "resume"
    )
}

/// v0.8.5 D2.5 — drift-test classifier: `true` for a command token the
/// adapter explicitly rejects from chat ([`is_codex_tui_only`]). Exposed
/// alongside [`is_builtin_command`] so the snapshot test can assert every
/// codex `SlashCommand` enum name lands in exactly one bucket.
pub fn is_rejected_command(name: &str) -> bool {
    is_codex_tui_only(name)
}

/// v0.8.5 D2 — flatten a `skills/list` response (`SkillsListResponse {
/// data: Vec<SkillsListEntry { cwd, skills: Vec<SkillMetadata>, .. }> }`,
/// `plugin.rs:34-36` + `:489`) into `(name, path, enabled)`. We read
/// fields tolerantly (name/path/enabled) since `SkillMetadata`'s exact
/// shape is not pinned here; an absent `enabled` defaults to `true`.
pub fn flatten_skills(result: &Value) -> Vec<CachedSkill> {
    let mut out = Vec::new();
    let Some(entries) = result.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for entry in entries {
        let Some(skills) = entry.get("skills").and_then(|s| s.as_array()) else {
            continue;
        };
        for s in skills {
            let name = s
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let path = s
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            out.push(CachedSkill {
                name,
                path,
                enabled,
            });
        }
    }
    out
}

/// v0.8.5 D2.1 — render the `/status` receipt from the tracker. The
/// context line goes through the shared [`ContextUsage::render`] (P3) so
/// `/status` and `/sessions` show the same absolute+percent form.
fn render_status_receipt(live: &ThreadLive) -> String {
    let model = live.model.as_deref().unwrap_or("(unknown)");
    match &live.usage {
        Some(u) => format!("model: {model}\ncontext: {}", u.render()),
        None => format!("model: {model}\ncontext: (no usage yet)"),
    }
}

// NOTE (v0.8.5 D4): the former `render_model_list` text receipt for a bare
// `/model` was superseded by the `NeedsChoice` picker (`model_options`); the
// model+effort list is now rendered as inline-keyboard options, not text.

/// Render the flattened skills list as a receipt (name + disabled marker).
fn render_skills_list(skills: &[CachedSkill]) -> String {
    if skills.is_empty() {
        return "no skills available.".to_string();
    }
    let mut lines = vec!["available skills:".to_string()];
    for s in skills {
        if s.enabled {
            lines.push(format!("• /{}", s.name));
        } else {
            lines.push(format!("• /{} (disabled)", s.name));
        }
    }
    lines.join("\n")
}

/// Render a count receipt for a list-shaped query response (`/mcp`,
/// `/hooks`, `/apps`): "<n> <label>(s) configured".
fn render_count_receipt(label: &str, result: &Value, key: &str) -> String {
    let n = result
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {label}{plural} configured.")
}

/// Translate a [`TurnInput`] into the codex `UserInput[]` payload
/// shape. Mirrors `references/codex/codex-rs/app-server-protocol/src/
/// protocol/v2/turn.rs::UserInput` variants.
pub fn turn_input_to_items(input: TurnInput) -> Result<Value, HarnessError> {
    let items = match input {
        TurnInput::UserText(text) => json!([{ "type": "text", "text": text }]),
        TurnInput::Artifact(path) => {
            let body = std::fs::read_to_string(&path)
                .map_err(|e| HarnessError::SubmitFailed(format!("read artifact: {e}")))?;
            json!([
                { "type": "text", "text": format!("<artifact path=\"{}\">\n{}\n</artifact>", path.display(), body) },
            ])
        }
        TurnInput::Image(path) => json!([
            { "type": "localImage", "path": path.to_string_lossy() },
        ]),
        TurnInput::ToolResult { call_id, content } => json!([
            {
                "type": "text",
                "text": serde_json::to_string(&json!({ "call_id": call_id, "content": content }))
                    .unwrap_or_default(),
            }
        ]),
    };
    Ok(items)
}

/// Translate a single codex notification → [`ThreadEvent`]. Filters
/// notifications whose `params.thread_id` doesn't match `wanted`.
/// Returns `None` for notification methods we don't yet propagate
/// (e.g. `thread/status/changed` — orchestrator state polling owns
/// that today).
pub fn translate_notification(notif: &Notification, wanted: &str) -> Option<ThreadEvent> {
    // thread_id filter (some notifications carry the id, some don't —
    // we only filter when present so we don't drop turn-scoped events
    // that omit it). The real Codex v2 wire is camelCase (`threadId`),
    // so read it dual-key — otherwise the filter never fires against a
    // live binary and foreign-thread events would be falsely accepted.
    //
    // `turn/*` + `item/*` carry a top-level `threadId` (verified
    // `references/codex/codex-rs/app-server-protocol/src/protocol/v2/turn.rs:312-315`
    // + `.../v2/item.rs:1059-1066`), so the flat lookup gates them.
    // `thread/started` is the exception: its only id is nested at
    // `params.thread.id` (`ThreadStartedNotification { thread: Thread }`,
    // `.../v2/thread.rs:1122-1124` + `.../v2/thread_data.rs:105-106`).
    // Without consulting the nested id a foreign `thread/started` slipped
    // the gate, and its arm's `unwrap_or_else(|| wanted)` laundered the
    // foreign id into the wanted slot. Resolve the nested id as a fallback
    // so the foreign-thread filter fires uniformly.
    let resolved_tid = pluck_str(&notif.params, "thread_id", "threadId")
        .map(str::to_string)
        .or_else(|| notif.params.get("thread").and_then(pluck_id));
    if let Some(tid) = resolved_tid {
        if tid != wanted {
            return None;
        }
    }
    match notif.method.as_str() {
        // Real wire: `ThreadStartedNotification { thread: Thread }`, so the
        // id is `params.thread.id` (camelCase Thread). Fall back to a flat
        // `thread_id`/`threadId` (test fixtures) and finally `wanted`. The
        // foreign-thread gate above already consulted the nested
        // `thread.id`, so a non-matching id was filtered out before reaching
        // this arm — it never launders a foreign id into the wanted slot.
        "thread/started" => Some(ThreadEvent::ThreadStarted {
            thread_id: notif
                .params
                .get("thread")
                .and_then(pluck_id)
                .or_else(|| pluck_str(&notif.params, "thread_id", "threadId").map(str::to_string))
                .unwrap_or_else(|| wanted.to_string()),
        }),
        // Real wire: `TurnStartedNotification { threadId, turn: Turn }`,
        // where the turn id is `turn.id` (NOT `turn.turn_id`, and there is
        // no top-level `turnId`). `pluck_turn_id_from_params` resolves the
        // real shape first, then the snake/camel flat fallbacks the test
        // fixtures use.
        "turn/started" => Some(ThreadEvent::TurnStarted {
            turn_id: pluck_turn_id_from_params(&notif.params),
        }),
        // The turn's VERDICT lives in `turn.status` (+ `turn.error`), not in
        // the method name: codex reports a failed or interrupted turn through
        // this same notification (`emit_turn_completed_with_status`).
        // Translating on the method alone made success the DEFAULT branch, so
        // a turn killed upstream (`server_overloaded`) was booked as a clean
        // answer — ledger row, parent completion notification and chat reply
        // all lying at once. `codex_turn_outcome` states the outcome
        // explicitly and never falls through to `Ok`.
        //
        // NOTE (both arms): the real `turn/completed` wire has NO `usage`
        // field anywhere (the `Turn` struct carries id/items/status/error/
        // timing only). Token accounting flows through the separate
        // `thread/tokenUsage/updated` notification (W4-fu bridge), and a
        // failed turn is re-enriched from it by `enrich_codex_turn_failed`.
        // The lookup here returns `None` against a live binary → default
        // usage; it stays only to satisfy synthetic test fixtures that inline
        // `usage`. Do NOT "fix" it to read the turn object — there is nothing
        // there to read. Codex per-turn cost is priced from `ctx.model` in
        // `build_progress_line` (the wire carries no model here).
        "turn/completed" => Some(match codex_turn_outcome(&notif.params) {
            CodexTurnOutcome::Ok => ThreadEvent::TurnCompleted {
                turn_id: pluck_turn_id_from_params(&notif.params),
                usage: pluck_usage(&notif.params).unwrap_or_default(),
                model: None,
            },
            CodexTurnOutcome::Failed { kind, message } => ThreadEvent::TurnFailed {
                turn_id: pluck_turn_id_from_params(&notif.params),
                err: ThreadErrorEvent { kind, message },
                usage: pluck_usage(&notif.params).unwrap_or_default(),
                model: None,
            },
        }),
        // W3b catalog §8.4 defect fix: the mode-3 app-server protocol has
        // **no** `turn/failed` notification. The real wire name for a turn
        // failure is `"error"` carrying an `ErrorNotification` payload
        // (`references/codex/codex-rs/app-server-protocol/src/protocol/v2/notification.rs:41`):
        //   { error: TurnError { message, .. }, will_retry: bool,
        //     thread_id, turn_id }
        // The former `"turn/failed"` arm was dead code (the catalog notes
        // turn failures were silently routed into warn_unknown_vendor_token),
        // so terminal Codex failures never surfaced as `agent_done
        // {status:"errored"}`.
        //
        // `will_retry == true` means the app-server will transparently
        // retry the turn (a transient upstream blip) and does NOT interrupt
        // the turn — surfacing it as TurnFailed would prematurely tear down
        // the progress bridge (is_terminal_progress drops it), so a later
        // `turn/completed` would never write its `agent_done`. We therefore
        // skip retryable errors and only emit TurnFailed on terminal ones.
        "error" => {
            // Real wire `ErrorNotification { error, willRetry, threadId,
            // turnId }` — read `willRetry`/`turnId` dual-key so a live
            // codex binary's terminal failure surfaces as TurnFailed
            // (snake_case kept for the in-module test fixtures).
            let will_retry = pluck_bool(&notif.params, "will_retry", "willRetry").unwrap_or(false);
            if will_retry {
                return None;
            }
            Some(ThreadEvent::TurnFailed {
                turn_id: pluck_str(&notif.params, "turn_id", "turnId")
                    .unwrap_or("")
                    .to_string(),
                err: ThreadErrorEvent {
                    kind: codex_error_kind(&notif.params)
                        .unwrap_or_else(|| "turn_failed".to_string()),
                    message: notif
                        .params
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no message)")
                        .to_string(),
                },
                usage: pluck_usage(&notif.params).unwrap_or_default(),
                model: pluck_model(&notif.params),
            })
        }
        "item/started" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemStarted { item })
        }
        // NOTE (W3b catalog §8.2): there is **no** `item/updated`
        // notification in the mode-3 app-server protocol — the
        // server_notification_definitions! registry at
        // `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1425-1517`
        // splits item state changes into typed `*Delta` notifications
        // (`item/agentMessage/delta`, `item/reasoning/textDelta`, ...) +
        // `item/completed`. The dot-named `item.updated` exists only in
        // the mode-2 `codex exec --json` stream (see codex_exec.rs). The
        // former arm here was a copy-paste artefact that never fired;
        // removed so the dispatch reflects the real wire surface.
        "item/completed" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemCompleted { item })
        }
        "item/agentMessage/delta" => {
            let delta = notif
                .params
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Real wire `AgentMessageDeltaNotification` carries `itemId`.
            let id = pluck_str(&notif.params, "item_id", "itemId")
                .unwrap_or("")
                .to_string();
            Some(ThreadEvent::ItemUpdated {
                item: ThreadItem {
                    id,
                    details: ThreadItemDetails::AgentMessage(delta.to_string()),
                },
            })
        }
        // V0.8 rmux W4-fu — these four W4-unlocked notifications carry no
        // `ThreadEvent` variant; they are mirrored to `progress.jsonl` by
        // `build_codex_notification_progress_line` in the `events()` loop.
        // Return `None` *silently* here (NOT via the unknown-method warn
        // below) — they are explicitly handled, not protocol drift, and
        // `thread/tokenUsage/updated` in particular fires several times per
        // turn, so routing them through `warn_unknown_vendor_token` would
        // spam the logs and defeat its "surface real drift" purpose.
        "turn/plan/updated"
        | "thread/tokenUsage/updated"
        | "thread/status/changed"
        | "account/rateLimits/updated" => None,
        // v0.8.5 D2 — `skills/changed` is consumed by the tracker dispatcher
        // (it invalidates the skills/list cache, arch §1.3). It carries no
        // ThreadEvent and no progress.jsonl row; skip it silently here so it
        // doesn't hit the unknown-method warn path.
        "skills/changed" => None,
        // V0.6.3 F144 — forward-compat: a `codex app-server` notification
        // `method` we don't yet propagate is **skipped** (`None`) so the
        // event stream is never broken — the orchestrator's
        // `progress.jsonl` poller stays the state-transition SoT for
        // anything we don't translate. Warn once per unknown method so a
        // Codex app-server protocol drift surfaces in the logs.
        other => {
            crate::warn_unknown_vendor_token(
                "codex_app_server_notification",
                other,
                "skipping this notification; event stream continues",
            );
            None
        }
    }
}

/// Preserve Codex's typed terminal error discriminator instead of forcing
/// every failure to `turn_failed`. The live v2 wire is camelCase while stored
/// rollout fixtures may be snake_case; normalize either to ccteam's stable
/// snake_case event vocabulary. Payload-bearing enum variants are objects, so
/// their sole top-level key is the discriminator.
fn codex_error_kind(params: &Value) -> Option<String> {
    let error = params.get("error")?;
    let info = error
        .get("codexErrorInfo")
        .or_else(|| error.get("codex_error_info"))?;
    match info {
        Value::String(kind) if !kind.is_empty() => Some(camel_to_snake(kind)),
        Value::Object(fields) => fields.keys().next().map(|kind| camel_to_snake(kind)),
        _ => None,
    }
}

/// The explicit outcome of a codex `turn/completed`. There is deliberately no
/// `Unknown`-that-means-success: any terminal shape ccteam cannot read as a
/// success is a failure, so an unrecognized future status surfaces instead of
/// silently becoming a clean answer.
enum CodexTurnOutcome {
    Ok,
    Failed { kind: String, message: String },
}

/// Read the verdict a `turn/completed` carries.
///
/// `params.turn.status` is `completed` | `interrupted` | `failed` (camelCase
/// on the wire, `TurnStatus`), with the detail in `params.turn.error`
/// (`TurnError { message, codexErrorInfo }`).
fn codex_turn_outcome(params: &Value) -> CodexTurnOutcome {
    // No `turn` object / no `status` at all: a fixture or a build with no
    // failure channel on this notification — there is no verdict to demote.
    let Some(turn) = params.get("turn") else {
        return CodexTurnOutcome::Ok;
    };
    let status = turn
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if status.is_empty() || status.eq_ignore_ascii_case("completed") {
        return CodexTurnOutcome::Ok;
    }
    let error_message = turn
        .get("error")
        .and_then(|err| err.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let kind = match status {
        "interrupted" => "interrupted".to_string(),
        "failed" => codex_error_kind(turn).unwrap_or_else(|| "turn_failed".to_string()),
        _ => "unknown_turn_status".to_string(),
    };
    CodexTurnOutcome::Failed {
        message: error_message
            .unwrap_or_else(|| format!("codex turn ended with status `{status}`")),
        kind,
    }
}

/// V0.6.1 F122 — translate a [`ThreadEvent`] into the `progress.jsonl`
/// row the cost/budget pipelines consume. Mirrors the orchestrator's
/// own `translate_thread_event` shape (vendor-tagged `agent_done`) so
/// `compute_cost_summary` rolls codex turns into
/// `cost_24h_by_vendor["codex"]` without any consumer-side changes.
///
/// Returns `None` for events the bridge intentionally does **not**
/// surface (`ThreadStarted` is covered by `agent_spawn`; `Item*` /
/// `TurnStarted` are presentation-only and noisy).
pub fn build_progress_line(
    evt: &ThreadEvent,
    thread_id: &str,
    ctx: &ProgressBridgeCtx,
) -> Option<Value> {
    match evt {
        ThreadEvent::TurnCompleted {
            turn_id,
            usage,
            model,
        } => {
            // Prefer the turn's own canonical model; fall back to the spawn
            // ctx model. Determinism: an unknown / absent model prices to
            // `None` → `cost_usd` is OMITTED (excluded from sums), never
            // billed at a fallback rate. The WARN-once in `estimate_cost`
            // surfaces the unpriced model.
            let priced_model = model.as_deref().or(ctx.model.as_deref()).unwrap_or("");
            let cost = ccteam_cost::estimate_cost(usage, ccteam_cost::Vendor::Codex, priced_model);
            Some(build_agent_done_completed_event(
                &ctx.role, &ctx.sid, &ctx.slug, "codex", thread_id, turn_id, usage, cost,
            ))
        }
        ThreadEvent::TurnFailed { turn_id, err, .. } => Some(build_agent_done_errored_event(
            &ctx.role,
            &ctx.sid,
            &ctx.slug,
            "codex",
            thread_id,
            Some(turn_id),
            &err.kind,
            &err.message,
        )),
        ThreadEvent::Error(err) => Some(build_agent_done_errored_event(
            &ctx.role,
            &ctx.sid,
            &ctx.slug,
            "codex",
            thread_id,
            None,
            &err.kind,
            &err.message,
        )),
        ThreadEvent::ThreadStarted { .. }
        | ThreadEvent::TurnStarted { .. }
        | ThreadEvent::ItemStarted { .. }
        | ThreadEvent::ItemUpdated { .. }
        | ThreadEvent::ItemCompleted { .. } => None,
    }
}

/// V0.6.1 F122 — return `true` for events that close out a thread from
/// the bridge's point of view. After a terminal write the bridge drops
/// its ctx so a duplicate `turn/completed` notification (codex
/// app-server may re-broadcast on resubscribe) doesn't double-count.
fn is_terminal_progress(evt: &ThreadEvent) -> bool {
    matches!(
        evt,
        ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. } | ThreadEvent::Error(_)
    )
}

fn translate_item_event(
    params: &Value,
    ctor: fn(ThreadItem) -> ThreadEvent,
) -> Option<ThreadEvent> {
    let item_val = params
        .get("item")
        .cloned()
        .unwrap_or_else(|| params.clone());
    // Real wire: the id lives at `item.id` (single-word, no casing
    // issue). The flat `item_id`/`itemId` fallback only matters for
    // hand-rolled fixtures that omit the `item` wrapper.
    let id = item_val
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| pluck_str(params, "item_id", "itemId"))
        .unwrap_or("")
        .to_string();
    let details = match item_val.get("type").and_then(|v| v.as_str()) {
        Some("agent_message") | Some("agentMessage") => ThreadItemDetails::AgentMessage(
            item_val
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        Some("reasoning") => ThreadItemDetails::Reasoning(
            item_val
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        Some("command_execution") | Some("commandExecution") => {
            ThreadItemDetails::CommandExecution {
                cmd: item_val
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                // Real wire: `CommandExecutionStatus` is a
                // `#[serde(rename_all = "camelCase")]` enum, so the live
                // binary sends `"inProgress"` / `"completed"` / `"failed"`
                // / `"declined"` (verified
                // `references/codex/codex-rs/app-server-protocol/src/protocol/v2/item.rs:870-878`).
                // Fold through `camel_to_snake` so progress.jsonl reads in
                // ccteam's snake_case house style (`in_progress`); the
                // single-word variants pass through unchanged, and the
                // already-snake `in_progress` default is idempotent.
                status: camel_to_snake(
                    item_val
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("in_progress"),
                ),
            }
        }
        Some("file_change") | Some("fileChange") => {
            // Real wire: `FileChange { changes: Vec<FileUpdateChange> }`
            // where `FileUpdateChange { path, kind: PatchChangeKind, diff }`
            // and `PatchChangeKind` is an INTERNALLY-TAGGED enum
            // `#[serde(tag = "type", rename_all = "camelCase")]` →
            // `{"type":"add"}` / `{"type":"delete"}` /
            // `{"type":"update","movePath":<opt>}` (verified
            // `references/codex/codex-rs/app-server-protocol/src/protocol/v2/item.rs:918-935`).
            // The prior `changes[0].kind` string read always yielded `None`
            // against the live binary (kind is an object, not a string) →
            // every patch silently defaulted to `"update"`.
            let change = item_val.get("changes").and_then(|c| c.get(0));
            let path = change
                .and_then(|c| c.get("path"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_default();
            let kind_obj = change.and_then(|c| c.get("kind"));
            // The tag field is `type`; fold through `camel_to_snake` for
            // house style. An `update` carrying a `movePath` is a rename
            // (there is no distinct `rename` variant on the wire), so
            // surface the richer `"rename"` kind in that case.
            let kind = match kind_obj
                .and_then(|k| k.get("type"))
                .and_then(|v| v.as_str())
            {
                Some("update") | Some("Update") => {
                    let has_move = kind_obj
                        .and_then(|k| k.get("movePath").or_else(|| k.get("move_path")))
                        .map(|v| !v.is_null())
                        .unwrap_or(false);
                    if has_move {
                        "rename".to_string()
                    } else {
                        "update".to_string()
                    }
                }
                Some(other) => camel_to_snake(other),
                None => "update".to_string(),
            };
            ThreadItemDetails::FileChange { path, kind }
        }
        Some("mcp_tool_call") | Some("mcpToolCall") => ThreadItemDetails::ToolCall {
            name: item_val
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            args: item_val.get("arguments").cloned().unwrap_or(Value::Null),
        },
        Some("web_search") | Some("webSearch") => ThreadItemDetails::WebSearch {
            query: item_val
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        Some("error") => ThreadItemDetails::Error(
            item_val
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        // V0.6.3 F144 — forward-compat: a present-but-unrecognised item
        // `type` degrades to an empty agent message and warns once. A
        // missing `type` (`None`) is a shape gap, not a vocabulary
        // drift, so it stays silent.
        Some(other) => {
            crate::warn_unknown_vendor_token(
                "codex_app_server_item",
                other,
                "degraded to empty agent message",
            );
            ThreadItemDetails::AgentMessage(String::new())
        }
        None => ThreadItemDetails::AgentMessage(String::new()),
    };
    Some(ctor(ThreadItem { id, details }))
}

/// V0.8 rmux W4-fu — read a string field tolerating both the real
/// Codex v2 wire casing (camelCase, e.g. `threadId`) and the snake_case
/// the existing arms / test fixtures use. The real `codex app-server`
/// notifications serialize with `#[serde(rename_all = "camelCase")]`
/// (verified at `app-server-protocol/src/protocol/common.rs:2833`
/// `serialize_thread_status_changed_notification`), so a camelCase-first
/// lookup is required for the live wire while snake_case keeps the
/// in-module test fixtures consistent. Scoped to the four W4-fu arms.
fn pluck_str<'a>(params: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .and_then(|v| v.as_str())
}

/// V0.8 rmux W4-fu — read a JSON sub-value tolerating both wire casings.
fn pluck_val(params: &Value, snake: &str, camel: &str) -> Option<Value> {
    params.get(camel).or_else(|| params.get(snake)).cloned()
}

/// V0.8 rmux — bool sibling of [`pluck_str`] for the `error`
/// notification's `willRetry` (real wire) / `will_retry` (test fixture).
fn pluck_bool(params: &Value, snake: &str, camel: &str) -> Option<bool> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .and_then(|v| v.as_bool())
}

/// V0.8 rmux — pull a `*Notification`'s nested object id. The real Codex
/// v2 `Thread`/`Turn` structs name their id field plain `id` (camelCase
/// rename leaves single-word `id` untouched); older ccteam test fixtures
/// used the redundant `thread_id`/`turn_id` inside the object, so accept
/// any of the three. Used for the `thread`/`turn` sub-objects that
/// `thread/started` + `turn/*` notifications carry.
fn pluck_id(obj: &Value) -> Option<String> {
    obj.get("id")
        .or_else(|| obj.get("thread_id"))
        .or_else(|| obj.get("turn_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// V0.8 rmux — resolve a turn id from a `turn/*` notification's params.
/// Real wire: `{ threadId, turn: { id, .. } }` (id at `turn.id`). Falls
/// back to a flat `turn_id`/`turnId` for the in-module fixtures. Empty
/// string when nothing matches (preserving the prior unwrap_or("")).
fn pluck_turn_id_from_params(params: &Value) -> String {
    params
        .get("turn")
        .and_then(pluck_id)
        .or_else(|| pluck_str(params, "turn_id", "turnId").map(str::to_string))
        .unwrap_or_default()
}

/// V0.8 rmux W4-fu — translate the four Codex app-server notifications
/// that have no [`ThreadEvent`] variant (`turn/plan/updated`,
/// `thread/tokenUsage/updated`, `thread/status/changed`,
/// `account/rateLimits/updated`) into additive `progress.jsonl` rows.
/// These were silently dropped by [`translate_notification`]'s
/// forward-compat `other` arm until the W4 `initialize` handshake
/// (`experimentalApi: true`) put them on the wire.
///
/// Returns `None` for any other method (so [`translate_notification`]'s
/// own dispatch still owns the `ThreadEvent`-bearing notifications) and
/// for notifications whose thread_id doesn't match `wanted`
/// (`account/rateLimits/updated` is thread-agnostic, so it is never
/// filtered out).
///
/// IMPORTANT (semantics): `turn/plan/updated` is Codex's `update_plan`
/// todo/checklist tool — the upstream source comments
/// "`update_plan` is a todo/checklist tool; it is not related to
/// plan-mode updates"
/// (`references/codex/codex-rs/app-server/src/bespoke_event_handling.rs`,
/// `handle_turn_plan_update`). It is fire-and-forget; Codex never awaits
/// a client response. We therefore map it to the observability-only
/// `codex_plan_updated` event, NOT the F98 `plan_pending` HITL event.
pub fn build_codex_notification_progress_line(notif: &Notification, wanted: &str) -> Option<Value> {
    // thread_id filter for the thread-scoped notifications (rate-limit
    // carries no thread_id, so skip the gate when absent).
    let matches_thread = |params: &Value| -> bool {
        match pluck_str(params, "thread_id", "threadId") {
            Some(tid) => tid == wanted,
            None => true,
        }
    };

    match notif.method.as_str() {
        "turn/plan/updated" => {
            if !matches_thread(&notif.params) {
                return None;
            }
            let plan = pluck_val(&notif.params, "plan", "plan").unwrap_or(Value::Array(vec![]));
            Some(build_codex_plan_updated_event(
                pluck_str(&notif.params, "thread_id", "threadId").unwrap_or(wanted),
                pluck_str(&notif.params, "turn_id", "turnId").unwrap_or(""),
                pluck_str(&notif.params, "explanation", "explanation"),
                plan,
            ))
        }
        "thread/tokenUsage/updated" => {
            if !matches_thread(&notif.params) {
                return None;
            }
            let usage = pluck_val(&notif.params, "token_usage", "tokenUsage")
                .unwrap_or(Value::Object(Default::default()));
            let total = usage
                .get("total")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let last = usage
                .get("last")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let window = usage
                .get("modelContextWindow")
                .or_else(|| usage.get("model_context_window"))
                .and_then(|v| v.as_i64());
            Some(build_codex_token_usage_event(
                pluck_str(&notif.params, "thread_id", "threadId").unwrap_or(wanted),
                pluck_str(&notif.params, "turn_id", "turnId").unwrap_or(""),
                total,
                last,
                window,
            ))
        }
        "thread/status/changed" => {
            if !matches_thread(&notif.params) {
                return None;
            }
            let status_obj = pluck_val(&notif.params, "status", "status")
                .unwrap_or(Value::Object(Default::default()));
            // ThreadStatus is internally tagged: {"type":"idle"} /
            // {"type":"active","activeFlags":["waitingOnApproval"]}.
            let status = status_obj
                .get("type")
                .and_then(|v| v.as_str())
                .map(camel_to_snake)
                .unwrap_or_default();
            let active_flags = status_obj
                .get("activeFlags")
                .or_else(|| status_obj.get("active_flags"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| f.as_str())
                        .map(camel_to_snake)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(build_codex_thread_status_event(
                pluck_str(&notif.params, "thread_id", "threadId").unwrap_or(wanted),
                &status,
                active_flags,
            ))
        }
        "account/rateLimits/updated" => {
            // No thread_id on this notification — it is account-scoped.
            let snapshot = pluck_val(&notif.params, "rate_limits", "rateLimits")
                .unwrap_or(Value::Object(Default::default()));
            (!semantic_payload_is_all_null(&snapshot))
                .then(|| build_codex_rate_limit_event(snapshot))
        }
        _ => None,
    }
}

/// v0.8.5 D2.4 — fold a single codex notification into the
/// [`CodexThreadTracker`]. This is the SOLE writer of the tracker (the
/// dispatcher calls it); `events()` never touches the tracker.
///
/// - `turn/started` → set `active_turn` (turn id at `turn.id`, real wire).
/// - `turn/completed` → clear `active_turn`.
/// - `error` with `willRetry == false` (terminal) → clear `active_turn`.
///   Retryable errors leave it set (the turn is still alive).
/// - `thread/tokenUsage/updated` → set `usage` from `tokenUsage.last`
///   (`total_tokens` = current active context size) + `tokenUsage.modelContextWindow`.
///   NOT `tokenUsage.total` (the cumulative session sum, which over-counts the
///   re-sent context every turn). This is the ONLY source of usage — the real
///   `turn/completed` wire carries none.
async fn apply_notification_to_tracker(
    tracker: &Arc<Mutex<CodexThreadTracker>>,
    notif: &Notification,
) {
    // thread_id is needed to key every tracker entry; account-scoped
    // notifications (no thread_id) are ignored by the tracker.
    let Some(tid) = pluck_str(&notif.params, "thread_id", "threadId").map(str::to_string) else {
        // `thread/started` carries the id nested at `thread.id` only.
        if notif.method == "thread/started" {
            if let Some(tid) = notif.params.get("thread").and_then(pluck_id) {
                tracker.lock().await.entry(&tid);
            }
        }
        return;
    };
    match notif.method.as_str() {
        "turn/started" => {
            let turn_id = pluck_turn_id_from_params(&notif.params);
            tracker.lock().await.entry(&tid).active_turn = Some(turn_id);
        }
        "turn/completed" => {
            tracker.lock().await.entry(&tid).active_turn = None;
        }
        "error" => {
            // Terminal failures (willRetry=false) clear the active turn;
            // retryable ones do not (the turn lives on until completion).
            let will_retry = pluck_bool(&notif.params, "will_retry", "willRetry").unwrap_or(false);
            if !will_retry {
                tracker.lock().await.entry(&tid).active_turn = None;
            }
        }
        "thread/tokenUsage/updated" => {
            let usage = pluck_val(&notif.params, "token_usage", "tokenUsage")
                .unwrap_or(Value::Object(Default::default()));
            // `last.total_tokens` = the latest turn's active context size (how
            // full the window is right now). NOT `total.total_tokens`, which is
            // the cumulative session sum — it re-counts the re-sent context
            // every turn and balloons into the millions, yielding nonsense like
            // `4.0M / 258.4k (1535%)`. Codex's own TUI derives context-window
            // occupancy from `last_token_usage` for exactly this reason.
            let used = usage
                .get("last")
                .and_then(|t| t.get("total_tokens").or_else(|| t.get("totalTokens")))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0) as u64;
            let window = usage
                .get("modelContextWindow")
                .or_else(|| usage.get("model_context_window"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0) as u64;
            tracker.lock().await.entry(&tid).usage =
                Some(ContextUsage::known(used, window, ContextSource::Reported));
        }
        _ => {}
    }
}

/// V0.8 rmux W4-fu — fold a camelCase identifier to snake_case so the
/// emitted `progress.jsonl` `status` / `active_flags` values read in
/// ccteam's snake_case house style regardless of the Codex wire casing
/// (`waitingOnApproval` → `waiting_on_approval`, `systemError` →
/// `system_error`). ASCII-only; Codex status/flag tokens are all ASCII.
fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Pull the thread id from a `thread/start` / `thread/resume` response.
/// Real wire: `ThreadStartResponse { thread: Thread }` where the id is
/// `thread.id` (camelCase Thread). [`pluck_id`] tolerates the older
/// `thread.thread_id` fixture shape; the flat `thread_id`/`threadId`
/// fallbacks cover responses that inline the id.
fn pluck_thread_id(v: &Value) -> Option<String> {
    v.get("thread")
        .and_then(pluck_id)
        .or_else(|| pluck_str(v, "thread_id", "threadId").map(str::to_string))
}

/// Pull the turn id from a `turn/start` response. Real wire:
/// `TurnStartResponse { turn: Turn }` with the id at `turn.id`.
fn pluck_turn_id(v: &Value) -> Option<String> {
    v.get("turn")
        .and_then(pluck_id)
        .or_else(|| pluck_str(v, "turn_id", "turnId").map(str::to_string))
}

/// v0.8.19 — pull codex's RESOLVED model id from a `thread/start` /
/// `thread/resume` response so the `/sessions` statusline can show it for
/// sessions started without an explicit `ctx.model_id` (codex's server
/// default). DETERMINISTIC, never inferred: the real wire (verified live
/// against codex-cli 0.141.0 + `app-server-protocol/.../v2/thread.rs`
/// `ThreadStartResponse` / `ThreadResumeResponse`) puts the model at the
/// response TOP LEVEL `result.model` (a sibling of `result.thread`, NOT
/// inside the `Thread` object — `Thread` carries only `modelProvider`).
/// Camel-case-first via [`pluck_str`]; the defensive `thread.model` /
/// `threadSettings.model` paths cover a `ThreadSettings`-style payload
/// (`thread/settings/updated`) without ever fabricating a value. Empty
/// strings are treated as absent. Returns `None` (→ blank model) when
/// codex genuinely reports none.
fn pluck_model(v: &Value) -> Option<String> {
    pluck_str(v, "model", "model")
        .or_else(|| v.get("thread").and_then(|t| pluck_str(t, "model", "model")))
        .or_else(|| {
            v.get("threadSettings")
                .or_else(|| v.get("thread_settings"))
                .and_then(|s| pluck_str(s, "model", "model"))
        })
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// v0.8.19 — pull codex's resolved reasoning effort from a `thread/start`
/// / `thread/resume` response. Real wire: `result.reasoningEffort`
/// (`ReasoningEffort` serializes as a lowercase string — `none` /
/// `minimal` / `low` / `medium` / `high` / `xhigh` / custom — verified
/// live). The `threadSettings.effort` fallback covers a
/// `ThreadSettings`-style payload (it names the field `effort`, not
/// `reasoningEffort`). Lowercased to match the `/model <id> <effort>`
/// directive's stored form (`SessionOverride.effort`). `None` when codex
/// reports none (statusline omits effort).
fn pluck_effort(v: &Value) -> Option<String> {
    pluck_str(v, "reasoning_effort", "reasoningEffort")
        .or_else(|| {
            v.get("threadSettings")
                .or_else(|| v.get("thread_settings"))
                .and_then(|s| pluck_str(s, "effort", "effort"))
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
}

fn pluck_usage(v: &Value) -> Option<UnifiedTokenUsage> {
    let raw = v
        .get("usage")
        .cloned()
        .or_else(|| v.get("turn").and_then(|t| t.get("usage")).cloned())?;
    serde_json::from_value(raw).ok()
}

/// Extract the current turn's token buckets from Codex's dedicated usage
/// notification. The terminal `error` notification carries no usage, so the
/// per-subscriber event stream retains this `last` block until the terminal
/// boundary and attaches it there.
fn codex_turn_usage_from_notification(notif: &Notification) -> Option<UnifiedTokenUsage> {
    if notif.method != "thread/tokenUsage/updated" {
        return None;
    }
    let token_usage = pluck_val(&notif.params, "token_usage", "tokenUsage")?;
    let last = token_usage.get("last")?.clone();
    serde_json::from_value(last).ok()
}

/// Fill accounting omitted by Codex's terminal `error` wire shape from the
/// latest usage notification and the thread's resolved model. Inline values
/// win for synthetic fixtures and future protocol additions.
fn enrich_codex_turn_failed(
    evt: &mut ThreadEvent,
    fallback_usage: Option<UnifiedTokenUsage>,
    fallback_model: Option<String>,
) {
    let ThreadEvent::TurnFailed { usage, model, .. } = evt else {
        return;
    };
    if usage.total() == 0 && usage.reported_cost_usd.is_none() {
        if let Some(value) = fallback_usage {
            *usage = value;
        }
    }
    if model.is_none() {
        *model = fallback_model;
    }
}

/// Convenience: build a placeholder client-less adapter. Test-only;
/// production callers go through `client()` which dials the socket.
#[cfg(test)]
fn _placeholder() -> CodexAppServerAdapter {
    CodexAppServerAdapter::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolve_socket_env_override_wins() {
        std::env::set_var(APP_SERVER_SOCKET_ENV, "/tmp/ccteam-test-codex.sock");
        let p = CodexAppServerAdapter::resolve_socket_path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/ccteam-test-codex.sock"));
        std::env::remove_var(APP_SERVER_SOCKET_ENV);
    }

    #[test]
    fn is_transport_death_discriminates_logical_vs_transport() {
        // A real error RESPONSE from a live peer always carries a numeric code
        // (e.g. -32600 "thread not found") → NOT transport death → keep the
        // shared connection (and every other session's loaded thread).
        let logical = anyhow::Error::new(JsonRpcError {
            code: Some(-32600),
            message: "thread not found: t-1".into(),
            data: None,
        });
        assert!(!is_transport_death(&logical));

        // Connection loss surfaces via `fail_pending` as a code-less JsonRpcError
        // → transport death → forget the client so the next call re-dials.
        let closed = anyhow::Error::new(JsonRpcError {
            code: None,
            message: "jsonrpc peer closed".into(),
            data: None,
        });
        assert!(is_transport_death(&closed));

        // A non-RPC failure (writer channel gone / send error) isn't a
        // JsonRpcError at all → also transport death.
        let io = anyhow::anyhow!("send jsonrpc request turn/start: channel closed");
        assert!(is_transport_death(&io));
    }

    #[test]
    fn config_reload_decision_matrix() {
        let t0 = std::time::SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(1);

        // Changed + idle + stdio + child present → re-spawn.
        assert_eq!(
            decide_config_reload(true, true, Some(t0), Some(t1), false),
            ConfigReloadDecision::Respawn
        );
        // Changed but a turn is in flight → don't bounce the shared child.
        assert_eq!(
            decide_config_reload(true, true, Some(t0), Some(t1), true),
            ConfigReloadDecision::Busy
        );
        // Unchanged mtime → nothing to do.
        assert_eq!(
            decide_config_reload(true, true, Some(t0), Some(t0), false),
            ConfigReloadDecision::Skip
        );
        // Socket transport (external app-server ccteam can't restart) → skip.
        assert_eq!(
            decide_config_reload(false, true, Some(t0), Some(t1), false),
            ConfigReloadDecision::Skip
        );
        // No child dialed yet → next conn() reads fresh config anyway → skip.
        assert_eq!(
            decide_config_reload(true, false, None, Some(t1), false),
            ConfigReloadDecision::Skip
        );
        // config.toml unreadable now → treat as unknown, never force a bounce.
        assert_eq!(
            decide_config_reload(true, true, Some(t0), None, false),
            ConfigReloadDecision::Skip
        );
        // File appeared after dial (dialed with none, now present) → changed.
        assert_eq!(
            decide_config_reload(true, true, None, Some(t1), false),
            ConfigReloadDecision::Respawn
        );
    }

    #[test]
    fn tracker_any_active_turn() {
        let mut tracker = CodexThreadTracker::default();
        assert!(!tracker.any_active_turn());
        // A seen-but-idle thread doesn't count.
        tracker.entry("t-1");
        assert!(!tracker.any_active_turn());
        // An in-flight turn does.
        tracker.entry("t-2").active_turn = Some("turn-9".into());
        assert!(tracker.any_active_turn());
    }

    #[test]
    fn turn_input_user_text_shape() {
        let v = turn_input_to_items(TurnInput::UserText("hi".into())).unwrap();
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[0]["text"], "hi");
    }

    #[test]
    fn turn_input_image_shape() {
        let v = turn_input_to_items(TurnInput::Image(PathBuf::from("/img.png"))).unwrap();
        assert_eq!(v[0]["type"], "localImage");
        assert_eq!(v[0]["path"], "/img.png");
    }

    #[test]
    fn translate_thread_started() {
        let n = Notification {
            method: "thread/started".into(),
            params: json!({ "thread_id": "t-1" }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ThreadStarted { thread_id } => assert_eq!(thread_id, "t-1"),
            _ => panic!("expected ThreadStarted"),
        }
    }

    #[test]
    fn translate_filters_foreign_thread() {
        let n = Notification {
            method: "turn/started".into(),
            params: json!({ "thread_id": "other", "turn_id": "x" }),
        };
        assert!(translate_notification(&n, "ours").is_none());
    }

    #[test]
    fn translate_turn_completed_extracts_usage() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cached_input_tokens": 0,
                },
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnCompleted { turn_id, usage, .. } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            _ => panic!("expected TurnCompleted"),
        }
    }

    /// 2026-08-09 incident — three consecutive turns died upstream
    /// (`server_overloaded`) and codex reported each one through
    /// `turn/completed` carrying `status: "failed"`. Reading only the method
    /// name booked all three as clean answers.
    #[test]
    fn translate_turn_completed_failed_status_is_a_failure() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "threadId": "t-1",
                "turn": {
                    "id": "u-9",
                    "items": [],
                    "status": "failed",
                    "error": {
                        "message": "Selected model is at capacity. Try again later.",
                        "codexErrorInfo": { "serverOverloaded": {} },
                    },
                },
            }),
        };
        match translate_notification(&n, "t-1").expect("a failed turn must surface") {
            ThreadEvent::TurnFailed { turn_id, err, .. } => {
                assert_eq!(turn_id, "u-9");
                assert_eq!(err.kind, "server_overloaded");
                assert!(err.message.contains("at capacity"), "{}", err.message);
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_completed_interrupted_is_not_a_success() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "threadId": "t-1",
                "turn": { "id": "u-2", "items": [], "status": "interrupted", "error": null },
            }),
        };
        match translate_notification(&n, "t-1").expect("an interrupted turn must surface") {
            ThreadEvent::TurnFailed { turn_id, err, .. } => {
                assert_eq!(turn_id, "u-2");
                assert_eq!(err.kind, "interrupted");
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    /// No default-success fall-through: a status ccteam does not recognize is
    /// reported, never laundered into a clean answer.
    #[test]
    fn translate_turn_completed_unknown_status_is_a_failure() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "threadId": "t-1",
                "turn": { "id": "u-3", "items": [], "status": "quantumCollapsed" },
            }),
        };
        match translate_notification(&n, "t-1").expect("an unknown status must surface") {
            ThreadEvent::TurnFailed { err, .. } => {
                assert_eq!(err.kind, "unknown_turn_status");
                assert!(err.message.contains("quantumCollapsed"), "{}", err.message);
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_completed_success_status_stays_completed() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "threadId": "t-1",
                "turn": { "id": "u-4", "items": [], "status": "completed", "error": null },
            }),
        };
        match translate_notification(&n, "t-1").expect("a completed turn must surface") {
            ThreadEvent::TurnCompleted { turn_id, .. } => assert_eq!(turn_id, "u-4"),
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn translate_item_completed_agent_message() {
        let n = Notification {
            method: "item/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "item": { "id": "i-1", "type": "agent_message", "text": "hello" }
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemCompleted { item } => {
                assert_eq!(item.id, "i-1");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hello"),
                    _ => panic!("expected agent_message"),
                }
            }
            _ => panic!("expected ItemCompleted"),
        }
    }

    // W3b catalog §8.4 defect fix — turn failures arrive as the `"error"`
    // notification (NOT a `"turn/failed"` method, which does not exist in
    // the mode-3 protocol). A terminal `error` (will_retry=false) must
    // surface as TurnFailed so the bridge writes `agent_done
    // {status:"errored"}`; a transient `error` (will_retry=true) must be
    // skipped so the progress bridge isn't torn down mid-retry.

    #[test]
    fn translate_error_notification_terminal_surfaces_turn_failed() {
        let n = Notification {
            method: "error".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "will_retry": false,
                "model": "gpt-5.3-codex",
                "usage": {
                    "input_tokens": 50,
                    "output_tokens": 20,
                    "cached_input_tokens": 5
                },
                "error": { "message": "context window exceeded" },
            }),
        };
        let e = translate_notification(&n, "t-1").expect("terminal error must surface");
        match e {
            ThreadEvent::TurnFailed {
                turn_id,
                err,
                usage,
                model,
            } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(err.message, "context window exceeded");
                assert_eq!(err.kind, "turn_failed");
                assert_eq!(usage.input_tokens, 50);
                assert_eq!(usage.output_tokens, 20);
                assert_eq!(usage.cached_input_tokens, 5);
                assert_eq!(model.as_deref(), Some("gpt-5.3-codex"));
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn translate_error_notification_preserves_typed_server_overloaded_kind() {
        let n = Notification {
            method: "error".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "willRetry": false,
                "error": {
                    "message": "Selected model is at capacity. Please try a different model.",
                    "codexErrorInfo": "serverOverloaded"
                },
            }),
        };
        let e = translate_notification(&n, "t-1").expect("terminal error must surface");
        match &e {
            ThreadEvent::TurnFailed { err, .. } => {
                assert_eq!(err.kind, "server_overloaded");
                assert_eq!(
                    err.message,
                    "Selected model is at capacity. Please try a different model."
                );
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
        let progress = build_progress_line(
            &e,
            "t-1",
            &ProgressBridgeCtx {
                progress_path: PathBuf::new(),
                role: String::new(),
                sid: "s1".into(),
                slug: "demo".into(),
                model: None,
            },
        )
        .unwrap();
        assert_eq!(progress["status"], "errored");
        assert_eq!(progress["error_kind"], "server_overloaded");
    }

    #[test]
    fn translate_error_notification_retryable_is_skipped() {
        let n = Notification {
            method: "error".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "will_retry": true,
                "error": { "message": "transient upstream 503" },
            }),
        };
        assert!(
            translate_notification(&n, "t-1").is_none(),
            "retryable error must be skipped so the bridge survives until turn/completed"
        );
    }

    #[test]
    fn translate_legacy_turn_failed_method_is_now_unknown() {
        // The dead `turn/failed` arm was removed; the (non-existent) wire
        // name now falls through to the forward-compat skip path.
        let n = Notification {
            method: "turn/failed".into(),
            params: json!({ "thread_id": "t-1", "turn_id": "u-1" }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
    }

    #[test]
    fn translate_item_updated_method_is_now_unknown() {
        // The dead `item/updated` arm (mode-2-only wire shape) was removed;
        // it must now fall through to the forward-compat skip path, not
        // produce a ThreadEvent.
        let n = Notification {
            method: "item/updated".into(),
            params: json!({
                "thread_id": "t-1",
                "item": { "id": "i-1", "type": "agent_message", "text": "x" }
            }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
    }

    // V0.6.3 F144 — forward-compat regression tests. Codex's app-server
    // protocol may grow a notification method or item type ccteam
    // doesn't translate; the seam must skip it (no panic, stream keeps
    // flowing) and warn once.

    #[test]
    fn translate_unknown_notification_method_is_skipped() {
        let n = Notification {
            method: "thread/checkpoint/created".into(),
            params: json!({ "thread_id": "t-1", "checkpoint_id": "c-1" }),
        };
        assert!(
            translate_notification(&n, "t-1").is_none(),
            "unknown notification method must be skipped"
        );
    }

    #[test]
    fn translate_known_notification_with_future_fields_does_not_panic() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    // A future usage field codex may add.
                    "speculative_tokens": 99,
                },
                // A future top-level field.
                "carbon_grams": 0.001,
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnCompleted { turn_id, usage, .. } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(usage.input_tokens, 10);
            }
            _ => panic!("expected TurnCompleted"),
        }
    }

    // V0.8 rmux W4-fu — the four notifications the W4 `initialize`
    // handshake unlocked. They have no ThreadEvent variant, so
    // translate_notification still skips them (None); the additive
    // progress.jsonl rows come from build_codex_notification_progress_line.

    #[test]
    fn turn_plan_updated_maps_to_codex_plan_updated_not_plan_pending() {
        // CRITICAL: Codex's `turn/plan/updated` is its `update_plan`
        // todo/checklist tool (upstream comment: "not related to plan-mode
        // updates"); it is fire-and-forget and must NOT trigger the F98
        // `plan_pending` HITL round-trip.
        let n = Notification {
            method: "turn/plan/updated".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "explanation": "drafting",
                "plan": [
                    { "step": "read repo", "status": "completed" },
                    { "step": "write code", "status": "inProgress" },
                ],
            }),
        };
        // No ThreadEvent variant.
        assert!(translate_notification(&n, "t-1").is_none());
        let line = build_codex_notification_progress_line(&n, "t-1").expect("plan row");
        assert_eq!(line["event"], CODEX_PLAN_UPDATED);
        assert_ne!(line["event"], "plan_pending");
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["turn_id"], "u-1");
        assert_eq!(line["explanation"], "drafting");
        assert_eq!(line["plan"][0]["step"], "read repo");
        assert_eq!(line["plan"][1]["status"], "inProgress");
    }

    #[test]
    fn w4fu_methods_return_none_silently_not_via_unknown_warn() {
        // These four are handled by build_codex_notification_progress_line,
        // so translate_notification must skip them via the explicit no-op
        // arms (silent None), NOT the forward-compat unknown-method warn
        // path — tokenUsage especially fires many times per turn.
        for method in [
            "turn/plan/updated",
            "thread/tokenUsage/updated",
            "thread/status/changed",
            "account/rateLimits/updated",
        ] {
            let n = Notification {
                method: method.into(),
                params: json!({ "thread_id": "t-1" }),
            };
            assert!(
                translate_notification(&n, "t-1").is_none(),
                "{method} must be skipped by translate_notification"
            );
        }
    }

    #[test]
    fn turn_plan_updated_camelcase_wire_is_handled() {
        // The real Codex v2 wire serializes params in camelCase
        // (`threadId`/`turnId`), per common.rs:2833. The dual-key helper
        // must accept it.
        let n = Notification {
            method: "turn/plan/updated".into(),
            params: json!({
                "threadId": "t-9",
                "turnId": "u-9",
                "plan": [ { "step": "x", "status": "pending" } ],
            }),
        };
        let line = build_codex_notification_progress_line(&n, "t-9").expect("camel plan row");
        assert_eq!(line["event"], CODEX_PLAN_UPDATED);
        assert_eq!(line["thread_id"], "t-9");
        assert_eq!(line["turn_id"], "u-9");
    }

    #[test]
    fn turn_plan_updated_foreign_thread_filtered() {
        let n = Notification {
            method: "turn/plan/updated".into(),
            params: json!({ "threadId": "other", "turnId": "u", "plan": [] }),
        };
        assert!(build_codex_notification_progress_line(&n, "ours").is_none());
    }

    #[test]
    fn thread_token_usage_maps_to_codex_token_usage() {
        let n = Notification {
            method: "thread/tokenUsage/updated".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "token_usage": {
                    "total": { "total_tokens": 300, "input_tokens": 200, "output_tokens": 100,
                               "cached_input_tokens": 0, "reasoning_output_tokens": 0 },
                    "last":  { "total_tokens": 30,  "input_tokens": 20,  "output_tokens": 10,
                               "cached_input_tokens": 0, "reasoning_output_tokens": 0 },
                    "model_context_window": 200000,
                },
            }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
        let line = build_codex_notification_progress_line(&n, "t-1").expect("usage row");
        assert_eq!(line["event"], CODEX_TOKEN_USAGE);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["turn_id"], "u-1");
        assert_eq!(line["total"]["total_tokens"], 300);
        assert_eq!(line["last"]["output_tokens"], 10);
        assert_eq!(line["model_context_window"], 200000);
    }

    #[test]
    fn terminal_error_uses_preceding_token_usage_and_thread_model() {
        let usage_notification = Notification {
            method: "thread/tokenUsage/updated".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "tokenUsage": {
                    "last": {
                        "input_tokens": 80,
                        "output_tokens": 21,
                        "cached_input_tokens": 7,
                        "reasoning_output_tokens": 3
                    }
                }
            }),
        };
        let usage = codex_turn_usage_from_notification(&usage_notification)
            .expect("token usage notification must expose the last turn");
        let error_notification = Notification {
            method: "error".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "willRetry": false,
                "error": { "message": "output limit reached" }
            }),
        };
        let mut event =
            translate_notification(&error_notification, "t-1").expect("terminal error event");
        enrich_codex_turn_failed(&mut event, Some(usage), Some("gpt-5.3-codex".into()));
        match event {
            ThreadEvent::TurnFailed { usage, model, .. } => {
                assert_eq!(usage.input_tokens, 80);
                assert_eq!(usage.output_tokens, 21);
                assert_eq!(usage.cached_input_tokens, 7);
                assert_eq!(usage.reasoning_output_tokens, Some(3));
                assert_eq!(model.as_deref(), Some("gpt-5.3-codex"));
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn thread_token_usage_camelcase_wire() {
        let n = Notification {
            method: "thread/tokenUsage/updated".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "tokenUsage": {
                    "total": { "total_tokens": 5 },
                    "last":  { "total_tokens": 1 },
                    "modelContextWindow": 128000,
                },
            }),
        };
        let line = build_codex_notification_progress_line(&n, "t-1").expect("usage row");
        assert_eq!(line["event"], CODEX_TOKEN_USAGE);
        assert_eq!(line["total"]["total_tokens"], 5);
        assert_eq!(line["model_context_window"], 128000);
    }

    #[test]
    fn thread_status_active_waiting_on_approval() {
        // Internally-tagged ThreadStatus: {"type":"active","activeFlags":[...]}.
        let n = Notification {
            method: "thread/status/changed".into(),
            params: json!({
                "threadId": "t-1",
                "status": { "type": "active", "activeFlags": ["waitingOnApproval"] },
            }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
        let line = build_codex_notification_progress_line(&n, "t-1").expect("status row");
        assert_eq!(line["event"], CODEX_THREAD_STATUS);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["status"], "active");
        assert_eq!(line["active_flags"][0], "waiting_on_approval");
    }

    #[test]
    fn thread_status_idle_has_no_flags() {
        let n = Notification {
            method: "thread/status/changed".into(),
            params: json!({ "threadId": "t-1", "status": { "type": "idle" } }),
        };
        let line = build_codex_notification_progress_line(&n, "t-1").expect("status row");
        assert_eq!(line["status"], "idle");
        assert_eq!(line["active_flags"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn account_rate_limits_maps_to_codex_rate_limit() {
        // No thread_id — account-scoped; never filtered by thread.
        let n = Notification {
            method: "account/rateLimits/updated".into(),
            params: json!({
                "rateLimits": {
                    "primary": { "usedPercent": 80, "windowDurationMins": 60, "resetsAt": 123 },
                    "rateLimitReachedType": null,
                },
            }),
        };
        assert!(translate_notification(&n, "any-thread").is_none());
        let line =
            build_codex_notification_progress_line(&n, "any-thread").expect("rate-limit row");
        assert_eq!(line["event"], CODEX_RATE_LIMIT);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["snapshot"]["primary"]["usedPercent"], 80);
    }

    #[test]
    fn account_rate_limits_drops_null_only_snapshot_before_building() {
        let notification = Notification {
            method: "account/rateLimits/updated".into(),
            params: json!({
                "rateLimits": {
                    "primary": {"usedPercent": null, "resetsAt": null},
                    "secondary": null,
                    "credits": {},
                },
            }),
        };

        assert!(build_codex_notification_progress_line(&notification, "any-thread").is_none());
    }

    #[test]
    fn codex_snapshot_maps_to_account_usage() {
        // Real wire (camelCase): primary = 5-hour window, secondary = weekly.
        // `resetsAt` is an absolute Unix epoch (seconds) → ISO-8601.
        let snap = json!({
            "primary":   { "usedPercent": 12, "windowDurationMins": 300,   "resetsAt": 1_700_000_000_i64 },
            "secondary": { "usedPercent": 98, "windowDurationMins": 10080, "resetsAt": 1_700_500_000_i64 },
            "planType": "pro",
            "rateLimitReachedType": "rate_limit_reached",
            "credits": { "hasCredits": true, "unlimited": false, "balance": "5.00" },
        });
        let u = account_usage_from_codex_snapshot(&snap).expect("maps to usage");
        assert_eq!(u.five_hour_pct, Some(12));
        assert_eq!(u.weekly_pct, Some(98));
        assert_eq!(u.subscription.as_deref(), Some("pro"));
        // a reached limit surfaces the weekly ⚠ in the renderer.
        assert_eq!(u.weekly_severity.as_deref(), Some("warning"));
        // codex `credits` is a balance, not a %, so 额度 is intentionally omitted.
        assert_eq!(u.credits_pct, None);
        assert_eq!(
            u.five_hour_resets_at.as_deref(),
            Some("2023-11-14T22:13:20+00:00")
        );
        assert!(u.weekly_resets_at.is_some());

        // No windows reported ⇒ nothing to show (clean omission, not a borrow).
        assert_eq!(account_usage_from_codex_snapshot(&json!({})), None);
        // snake_case keys also accepted (defensive dual-key).
        let snake = json!({ "primary": { "used_percent": 5, "resets_at": 1_700_000_000_i64 } });
        assert_eq!(
            account_usage_from_codex_snapshot(&snake)
                .unwrap()
                .five_hour_pct,
            Some(5)
        );
    }

    #[test]
    fn camel_to_snake_folds_codex_tokens() {
        assert_eq!(camel_to_snake("waitingOnApproval"), "waiting_on_approval");
        assert_eq!(camel_to_snake("systemError"), "system_error");
        assert_eq!(camel_to_snake("idle"), "idle");
        assert_eq!(camel_to_snake("NotLoaded"), "not_loaded");
    }

    // V0.8 rmux task #18 — Codex wire camelCase sweep. The real
    // `codex app-server` v2 wire serializes every multi-word field in
    // camelCase (`#[serde(rename_all = "camelCase")]`, verified in
    // `references/codex/codex-rs/app-server-protocol/src/protocol/`).
    // The arms below previously read snake_case only and silently failed
    // against a live binary. These tests feed the REAL wire shape; the
    // pre-existing snake_case tests above still pass (dual-key).

    #[test]
    fn translate_thread_started_real_wire_nested_camel() {
        // Real wire: ThreadStartedNotification { thread: Thread { id, .. } }.
        let n = Notification {
            method: "thread/started".into(),
            params: json!({ "thread": { "id": "t-1", "sessionId": "s-1" } }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ThreadStarted { thread_id } => assert_eq!(thread_id, "t-1"),
            other => panic!("expected ThreadStarted, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_started_real_wire_turn_dot_id() {
        // Real wire: TurnStartedNotification { threadId, turn: { id, .. } }.
        // The turn id is `turn.id` — NOT `turn.turn_id`, NOT top-level.
        let n = Notification {
            method: "turn/started".into(),
            params: json!({ "threadId": "t-1", "turn": { "id": "u-7", "status": "inProgress" } }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnStarted { turn_id } => assert_eq!(turn_id, "u-7"),
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_completed_real_wire_turn_dot_id_no_usage() {
        // Real wire `turn/completed` has the id at `turn.id` and NO usage
        // field anywhere (token data flows via thread/tokenUsage/updated).
        // Must extract the id and default the usage without panicking.
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({ "threadId": "t-1", "turn": { "id": "u-9", "status": "completed" } }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnCompleted { turn_id, usage, .. } => {
                assert_eq!(turn_id, "u-9");
                // No usage on the real wire → defaulted to zero.
                assert_eq!(usage.input_tokens, 0);
                assert_eq!(usage.output_tokens, 0);
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_completed_real_wire_foreign_thread_filtered() {
        // camelCase threadId on a foreign thread must be filtered out —
        // proves the thread filter reads the camelCase key (regression:
        // a snake-only filter would falsely accept this).
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({ "threadId": "other", "turn": { "id": "u-9" } }),
        };
        assert!(
            translate_notification(&n, "ours").is_none(),
            "foreign camelCase threadId must be filtered"
        );
    }

    #[test]
    fn translate_error_notification_camelcase_terminal_surfaces_turn_failed() {
        // THE critical case (task #18): the real wire ErrorNotification is
        // { error, willRetry, threadId, turnId } in camelCase. A terminal
        // failure (willRetry=false) MUST surface as TurnFailed so the
        // bridge writes agent_done{status:"errored"}. A snake-only
        // `will_retry` read would default to false too, but a snake-only
        // `turn_id` would lose the id — assert both.
        let n = Notification {
            method: "error".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "willRetry": false,
                "error": { "message": "context window exceeded" },
            }),
        };
        let e = translate_notification(&n, "t-1").expect("terminal camelCase error must surface");
        match e {
            ThreadEvent::TurnFailed { turn_id, err, .. } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(err.message, "context window exceeded");
                assert_eq!(err.kind, "turn_failed");
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn translate_error_notification_camelcase_retryable_is_skipped() {
        // Real wire camelCase willRetry=true must still be read as true so
        // the retryable error is skipped (bridge survives until completion).
        let n = Notification {
            method: "error".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "willRetry": true,
                "error": { "message": "transient 503" },
            }),
        };
        assert!(
            translate_notification(&n, "t-1").is_none(),
            "camelCase retryable error must be skipped"
        );
    }

    #[test]
    fn translate_agent_message_delta_camelcase_item_id() {
        // Real wire AgentMessageDeltaNotification { threadId, turnId,
        // itemId, delta }. The item id must come from `itemId`.
        let n = Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "itemId": "i-42",
                "delta": "hel",
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemUpdated { item } => {
                assert_eq!(item.id, "i-42");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hel"),
                    other => panic!("expected agent message, got {other:?}"),
                }
            }
            other => panic!("expected ItemUpdated, got {other:?}"),
        }
    }

    #[test]
    fn translate_item_completed_camelcase_type_tag() {
        // Real wire ThreadItem is #[serde(tag="type", rename_all="camelCase")]
        // → type tag `agentMessage`; id at `item.id`. Carried inside
        // ItemCompletedNotification { item, threadId, turnId }.
        let n = Notification {
            method: "item/completed".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "item": { "id": "i-1", "type": "agentMessage", "text": "hello" }
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemCompleted { item } => {
                assert_eq!(item.id, "i-1");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hello"),
                    other => panic!("expected agent message, got {other:?}"),
                }
            }
            other => panic!("expected ItemCompleted, got {other:?}"),
        }
    }

    #[test]
    fn pluck_thread_id_resolves_real_wire_thread_dot_id() {
        // thread/start response: ThreadStartResponse { thread: Thread{ id } }.
        let resp = json!({ "thread": { "id": "thr_abc", "sessionId": "s" } });
        assert_eq!(pluck_thread_id(&resp), Some("thr_abc".to_string()));
        // Older fixture shape (thread.thread_id) still works.
        let legacy = json!({ "thread": { "thread_id": "thr_legacy" } });
        assert_eq!(pluck_thread_id(&legacy), Some("thr_legacy".to_string()));
        // Flat fallbacks.
        assert_eq!(
            pluck_thread_id(&json!({ "threadId": "thr_flat" })),
            Some("thr_flat".to_string())
        );
    }

    #[test]
    fn pluck_turn_id_resolves_real_wire_turn_dot_id() {
        // turn/start response: TurnStartResponse { turn: Turn{ id } }.
        let resp = json!({ "turn": { "id": "turn_abc", "status": "inProgress" } });
        assert_eq!(pluck_turn_id(&resp), Some("turn_abc".to_string()));
        let legacy = json!({ "turn": { "turn_id": "turn_legacy" } });
        assert_eq!(pluck_turn_id(&legacy), Some("turn_legacy".to_string()));
        assert_eq!(
            pluck_turn_id(&json!({ "turnId": "turn_flat" })),
            Some("turn_flat".to_string())
        );
    }

    #[test]
    fn pluck_model_resolves_real_wire_top_level_model() {
        // v0.8.19 — realistic `thread/start` response shape captured live
        // against codex-cli 0.141.0: the RESOLVED model is at the response
        // TOP LEVEL `result.model` (sibling of `thread`), NOT inside the
        // `thread` object (which carries only `modelProvider`).
        let resp = json!({
            "thread": {
                "id": "019ef6de-960a-7d93-86a4-8e37c9bfbdb0",
                "modelProvider": "openai",
                "status": { "type": "idle" }
            },
            "model": "gpt-5.5",
            "modelProvider": "openai",
            "reasoningEffort": "high"
        });
        assert_eq!(pluck_model(&resp), Some("gpt-5.5".to_string()));
        assert_eq!(pluck_effort(&resp), Some("high".to_string()));

        // Defensive fallback: a `ThreadSettings`-style payload nests the
        // model under `threadSettings.model` and effort under
        // `threadSettings.effort` (the short field name, not reasoningEffort).
        let settings = json!({
            "threadId": "t1",
            "threadSettings": { "model": "gpt-5.5-codex", "effort": "Low" }
        });
        assert_eq!(pluck_model(&settings), Some("gpt-5.5-codex".to_string()));
        // Effort is lowercased to match the /model directive's stored form.
        assert_eq!(pluck_effort(&settings), Some("low".to_string()));

        // Defensive: a model inlined on the thread object is still found.
        let on_thread = json!({ "thread": { "model": "o3" } });
        assert_eq!(pluck_model(&on_thread), Some("o3".to_string()));

        // Honest None: codex reports no model / no effort → blank statusline,
        // never inferred. Empty strings are treated as absent.
        assert_eq!(
            pluck_model(&json!({ "thread": { "modelProvider": "openai" } })),
            None
        );
        assert_eq!(pluck_model(&json!({ "model": "" })), None);
        assert_eq!(pluck_effort(&json!({ "model": "gpt-5.5" })), None);
        assert_eq!(pluck_effort(&json!({ "reasoningEffort": "" })), None);
    }

    #[test]
    fn catalog_models_keeps_vendor_ids_labels_and_efforts() {
        let rows = catalog_models(&json!({"data": [
            {
                "id": "gpt-future",
                "displayName": "GPT Future",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low"},
                    {"reasoningEffort": "high"}
                ]
            },
            {"id": "plain"},
            {"displayName": "missing id"}
        ]}));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "gpt-future");
        assert_eq!(rows[0].display_name.as_deref(), Some("GPT Future"));
        assert_eq!(rows[0].efforts, ["low", "high"]);
        assert_eq!(rows[1].id, "plain");
    }

    #[test]
    fn ctx_model_id_takes_precedence_over_resolved_model() {
        // v0.8.19 — the seeding expression is
        //   ctx.model_id.clone().or_else(|| pluck_model(&result))
        // The user's explicit intent (ctx.model_id) wins; codex's resolved
        // model only fills the gap. Deterministic on both arms.
        let result = json!({ "model": "gpt-5.5", "reasoningEffort": "high" });

        // Explicit ctx model present → it wins (codex's resolved model ignored).
        let ctx_model: Option<String> = Some("gpt-5.5-codex".to_string());
        assert_eq!(
            ctx_model.clone().or_else(|| pluck_model(&result)),
            Some("gpt-5.5-codex".to_string())
        );

        // No ctx model (codex server default) → fall back to resolved model.
        let no_ctx: Option<String> = None;
        assert_eq!(
            no_ctx.or_else(|| pluck_model(&result)),
            Some("gpt-5.5".to_string())
        );
    }

    #[test]
    fn translate_item_unknown_type_degrades_to_empty_message() {
        let n = Notification {
            method: "item/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "item": { "id": "i-2", "type": "quantum_blob", "data": [1, 2] }
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemCompleted { item } => {
                assert_eq!(item.id, "i-2");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, ""),
                    other => panic!("expected empty agent message, got {other:?}"),
                }
            }
            _ => panic!("expected ItemCompleted"),
        }
    }
}
