//! Per-bot tmux session supervisor + heartbeat.
//!
//! V0.6.0 Wave 2 F116. The supervisor:
//!
//! 1. Refreshes the daemon-global heartbeat file each tick
//!    (`~/.ccteam/state/imd.heartbeat`).
//! 2. For every registered bot, checks the per-bot heartbeat
//!    (`<project>/.ccteam/chat/<bot>/heartbeat`) — V0.6.1 F136
//!    writes this file via a 5s-tick task spawned inside
//!    [`BotSupervisor::ensure_started`] (lifetime tied to the
//!    adapter handle; aborted on `shutdown` / `restart`). Before
//!    F136 the production daemon never wrote it, so `decide()` ran
//!    a ~65s restart loop on every healthy bot.
//! 3. If a per-bot heartbeat is missing or older than [`STALE_THRESHOLD`],
//!    initiates a graceful close → restart cycle.
//! 4. Honors `signals/shutdown.signal` (final stop) and
//!    `signals/drain.signal` (stop accepting new turns; let inflight
//!    finish). The signal files are user-writable via `@ccteam pause`
//!    / `@ccteam stop` admin commands.
//!
//! Three-layer safety:
//! - **Layer A**: never kill before per-bot heartbeat stale-window
//!   expires.
//! - **Layer B**: graceful `close_thread()` first; only force-kill on
//!   subsequent failed restart.
//! - **Layer C**: max-restart budget per session (`MAX_RESTARTS_PER_HOUR`)
//!   stops a flap loop from burning IM API quota.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Context, Result};
use ccteam_core::execution::turns_mirror::{self, TurnRecord};
use ccteam_core::harness::{
    AgentSpecBrief, HarnessAdapter, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItemDetails, TurnId,
    TurnInput,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use crate::bot_mpsc::OutboundItem;
use crate::latency::now_unix_ms;
use crate::{imd_heartbeat_path, BotRegistration};

/// V0.6.1 F136 — interval between per-bot heartbeat writes. Picked so
/// 6 cycles fit comfortably inside [`STALE_THRESHOLD`] (60s) — one
/// missed tick from a busy task scheduler still leaves five healthy
/// writes inside the stale window.
pub const HEARTBEAT_TICK: Duration = Duration::from_secs(5);

/// Per-bot heartbeat older than this triggers restart.
pub const STALE_THRESHOLD: Duration = Duration::from_secs(60);

/// Restart budget per rolling hour.
pub const MAX_RESTARTS_PER_HOUR: usize = 6;

/// Name of the user-requested shutdown signal file (`shutdown.signal`).
pub const SHUTDOWN_SIGNAL: &str = "shutdown.signal";
/// Name of the drain-mode signal file (`drain.signal`).
pub const DRAIN_SIGNAL: &str = "drain.signal";

/// Per-bot runtime state held by the supervisor.
#[derive(Debug, Clone, Default)]
pub struct BotState {
    /// Current tmux/thread handle if running.
    pub handle: Option<ThreadHandle>,
    /// Restart history (Instant of each restart in the last hour).
    pub restarts: Vec<Instant>,
    /// True once `shutdown.signal` has been observed (terminal).
    pub shutting_down: bool,
    /// True once `drain.signal` has been observed (no new turns).
    pub draining: bool,
}

/// Decision the supervisor makes for one bot on a single tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupervisorAction {
    /// Nothing to do (bot is healthy).
    NoOp,
    /// User-requested shutdown — close the session and stop watching.
    Shutdown,
    /// Drain mode — stop accepting inbound, let inflight finish.
    Drain,
    /// Heartbeat stale → schedule a restart.
    Restart,
    /// Restart budget exhausted; escalate (log + skip).
    Quarantine,
    /// Initial spawn (no handle yet).
    Spawn,
}

/// Refresh the daemon-global heartbeat file. Creates parent dir.
pub fn refresh_global_heartbeat() -> Result<()> {
    let path = imd_heartbeat_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let now = chrono::Utc::now().to_rfc3339();
    fs::write(&path, now)?;
    Ok(())
}

/// Decide what to do with one bot on a single tick.
///
/// Pure decision function — no IO besides reading existing signal
/// files + heartbeat file. The daemon's main loop applies the action.
pub fn decide(
    projects_root: &Path,
    reg: &BotRegistration,
    state: &BotState,
    now: SystemTime,
) -> SupervisorAction {
    let bot_dir = bot_dir(projects_root, reg);

    // Layer A — shutdown beats everything else (terminal).
    if signal_present(&bot_dir, SHUTDOWN_SIGNAL) || state.shutting_down {
        return SupervisorAction::Shutdown;
    }

    // Layer B — drain mode.
    if signal_present(&bot_dir, DRAIN_SIGNAL) {
        return SupervisorAction::Drain;
    }

    // Layer C — restart budget exhausted → quarantine.
    let recent = state
        .restarts
        .iter()
        .filter(|t| t.elapsed() < Duration::from_secs(3600))
        .count();
    if recent >= MAX_RESTARTS_PER_HOUR {
        return SupervisorAction::Quarantine;
    }

    // No handle yet → initial spawn.
    if state.handle.is_none() {
        return SupervisorAction::Spawn;
    }

    // Heartbeat check.
    let hb = bot_dir.join("heartbeat");
    let stale = match fs::metadata(&hb).and_then(|m| m.modified()) {
        Ok(mtime) => match now.duration_since(mtime) {
            Ok(age) => age > STALE_THRESHOLD,
            Err(_) => false, // mtime in future — clock skew, treat as fresh
        },
        Err(_) => true, // heartbeat missing
    };
    if stale {
        SupervisorAction::Restart
    } else {
        SupervisorAction::NoOp
    }
}

/// Per-bot dir: `<projects_root>/<slug>/.ccteam/chat/<role>/`.
pub fn bot_dir(projects_root: &Path, reg: &BotRegistration) -> PathBuf {
    projects_root
        .join(&reg.workflow_slug)
        .join(".ccteam")
        .join("chat")
        .join(&reg.role)
}

fn signal_present(bot_dir: &Path, name: &str) -> bool {
    bot_dir.join("signals").join(name).exists()
}

/// Aggregate snapshot the daemon exposes via `status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    /// `(slug, role) -> action`.
    pub actions: HashMap<String, SupervisorAction>,
}

/// V0.6.0 Wave 3 — per-bot supervisor that owns one
/// [`HarnessAdapter`] thread (tmux session) end to end.
///
/// One [`BotSupervisor`] per [`BotRegistration`]. The daemon builds
/// these on boot, wires each one's outbound tail task, and routes
/// inbound mailbox envelopes through [`BotSupervisor::handle_inbound`].
///
/// Lifecycle:
///   1. `ensure_started()` (or `start()` directly) — calls
///      `adapter.start_thread` once and stashes the `ThreadHandle`.
///   2. `handle_inbound(payload)` — calls `adapter.submit_turn` with the
///      mailbox content as `TurnInput::UserText`.
///   3. `shutdown()` — calls `adapter.close_thread` and clears state.
///   4. `restart()` — close + start (used when heartbeat goes stale).
///
/// All adapter calls go through the [`HarnessAdapter`] trait — no
/// `ccteam_core::execution::*` import lives in this crate (red line
/// enforced by `tests/dep_graph_test.rs`). Tests inject a stub adapter.
pub struct BotSupervisor {
    /// Registration this supervisor binds to.
    pub reg: BotRegistration,
    /// Projects root (`<projects_root>/<slug>/.ccteam/chat/<role>/`).
    pub projects_root: PathBuf,
    /// Wave 3 — the adapter the daemon picked for this vendor/mode
    /// pair. Owned via `Arc` so the supervisor can hand a clone to
    /// the outbound tail task.
    pub adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    /// Active runtime state (handle + restart history + flags).
    state: Mutex<BotState>,
    /// V0.6.1 F136 — periodic heartbeat-writer task. Touches
    /// `<bot_dir>/heartbeat` every [`HEARTBEAT_TICK`] so the
    /// supervisor's `decide()` doesn't see the file as stale and
    /// trigger a needless restart loop. Lifetime tracks the underlying
    /// thread: spawned in `ensure_started`, aborted in `shutdown` /
    /// `restart`.
    heartbeat_task: Mutex<Option<JoinHandle<()>>>,
    /// V0.6.1 F137 — `events()` → `turns_mirror::append_turn` bridge.
    /// Without this task the ccteam-owned `turns.jsonl` mirror never
    /// gets populated, which leaves the F134 outbound forwarder with
    /// no source rows to dispatch. Lifetime mirrors the heartbeat
    /// writer.
    events_task: Mutex<Option<JoinHandle<()>>>,
    /// V0.6.1 fast-path — direct mpsc to the per-bot outbound dispatcher
    /// (see [`crate::bot_mpsc`]). The daemon's main loop populates this
    /// via [`set_outbound_tx`] right after the supervisor starts; the
    /// events consumer must therefore read the current value **per
    /// event** (not just at task-spawn time) to pick up the late
    /// wiring. Held as `Arc<Mutex<_>>` so the spawned events task can
    /// clone the Arc once and lock-read fresh values on every
    /// `ItemCompleted`.
    ///
    /// When `None`, the events consumer just appends `turns.jsonl` (the
    /// safety-net `drain_outboxes` pass will pick rows up later); when
    /// `Some`, the events consumer additionally sends each assistant
    /// row through the channel for ~immediate dispatch.
    outbound_tx: Arc<Mutex<Option<mpsc::Sender<OutboundItem>>>>,
}

impl std::fmt::Debug for BotSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotSupervisor")
            .field("slug", &self.reg.workflow_slug)
            .field("role", &self.reg.role)
            .field("vendor", &self.reg.vendor)
            .field("adapter", &self.adapter.name())
            .finish_non_exhaustive()
    }
}

impl BotSupervisor {
    /// Build a fresh supervisor for `reg`, using `adapter` for every
    /// HarnessAdapter call. Initial state is empty (no handle).
    pub fn new(
        reg: BotRegistration,
        projects_root: impl Into<PathBuf>,
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    ) -> Self {
        Self {
            reg,
            projects_root: projects_root.into(),
            adapter,
            state: Mutex::new(BotState::default()),
            heartbeat_task: Mutex::new(None),
            events_task: Mutex::new(None),
            outbound_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// V0.6.1 fast-path — daemon main loop calls this once per bot
    /// right after `ensure_started` succeeds. From the next event onward
    /// the events consumer enqueues each assistant row into `tx` so the
    /// per-bot outbound dispatcher can fire `channel.send` immediately
    /// (skipping the safety-net 60s `drain_outboxes` scan).
    pub async fn set_outbound_tx(&self, tx: mpsc::Sender<OutboundItem>) {
        *self.outbound_tx.lock().await = Some(tx);
    }

    /// `<projects_root>/<slug>/.ccteam/chat/<role>/`. Helper so the
    /// background tasks share one resolution path with `decide`.
    fn bot_dir(&self) -> PathBuf {
        bot_dir(&self.projects_root, &self.reg)
    }

    /// `<projects_root>/<slug>/`.
    pub fn project_dir(&self) -> PathBuf {
        self.projects_root.join(&self.reg.workflow_slug)
    }

    /// True iff `start_thread` has been called and `close_thread` has
    /// not since.
    pub async fn is_started(&self) -> bool {
        self.state.lock().await.handle.is_some()
    }

    /// Snapshot of the current [`ThreadHandle`] (clone of the internal
    /// state). `None` when the bot isn't running.
    pub async fn current_handle(&self) -> Option<ThreadHandle> {
        self.state.lock().await.handle.clone()
    }

    /// Snapshot the live per-bot state — used by the daemon tick when
    /// calling [`decide`] (pure function; needs a current view of
    /// `handle` + `restarts` + flags to make the right call).
    pub async fn state_snapshot(&self) -> BotState {
        self.state.lock().await.clone()
    }

    /// Idempotent: start the underlying tmux session via
    /// `adapter.start_thread` if not already running.
    pub async fn ensure_started(&self) -> Result<()> {
        {
            let st = self.state.lock().await;
            if st.handle.is_some() {
                return Ok(());
            }
        }
        let spec = AgentSpecBrief {
            role: self.reg.role.clone(),
        };
        let project_dir = self.project_dir();
        let ctx = SpawnCtx {
            slug: self.reg.workflow_slug.clone(),
            sid: format!("{}-{}", self.reg.workflow_slug, self.reg.role),
            cwd: project_dir.clone(),
            project_dir,
            extra_args: Vec::new(),
            // Wave 4 D14 — chat supervisor has no spec.model on hand;
            // adapter falls back to vendor default. The orchestrator
            // path (try_spawn_with_prompt) plumbs through workflow.yaml.
            model_id: None,
        };
        let handle = self
            .adapter
            .start_thread(&spec, &ctx)
            .await
            .with_context(|| {
                format!(
                    "start_thread for {}/{} via {}",
                    self.reg.workflow_slug,
                    self.reg.role,
                    self.adapter.name()
                )
            })?;
        let mut st = self.state.lock().await;
        st.handle = Some(handle.clone());
        drop(st);
        tracing::info!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            adapter = self.adapter.name(),
            "bot supervisor started thread"
        );

        // V0.6.1 F136 — start the heartbeat writer if it isn't already
        // running. Aborted on shutdown / restart so a stale heartbeat
        // from a closed session can't keep the supervisor pinned.
        self.spawn_heartbeat_writer().await;
        // V0.6.1 F137 — start the events → turns_mirror consumer so
        // assistant replies land in `<project>/.ccteam/chat/<role>/turns.jsonl`,
        // which is the source-of-truth the F134 outbound forwarder
        // tails.
        self.spawn_events_consumer(handle).await;
        Ok(())
    }

    /// V0.6.1 F136 — idempotent spawn of the per-bot heartbeat writer.
    /// Writes a fresh UTC timestamp to `<bot_dir>/heartbeat` every
    /// [`HEARTBEAT_TICK`]. Survives transient `std::fs::write` failures
    /// (warn-logged) — the next tick will retry.
    async fn spawn_heartbeat_writer(&self) {
        let mut guard = self.heartbeat_task.lock().await;
        if guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let bot_dir = self.bot_dir();
        let slug = self.reg.workflow_slug.clone();
        let role = self.reg.role.clone();
        let handle = tokio::spawn(async move {
            // Best-effort `mkdir -p` once at task start — the heartbeat
            // file's parent dir may not exist yet on first spawn (the
            // bot_dir is otherwise created lazily by inbox / outbound
            // writers).
            if let Err(err) = std::fs::create_dir_all(&bot_dir) {
                tracing::warn!(
                    slug = %slug,
                    role = %role,
                    path = %bot_dir.display(),
                    error = %err,
                    "imd: heartbeat writer mkdir failed"
                );
            }
            let path = bot_dir.join("heartbeat");
            let mut ticker = tokio::time::interval(HEARTBEAT_TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let now = chrono::Utc::now().to_rfc3339();
                if let Err(err) = std::fs::write(&path, &now) {
                    tracing::warn!(
                        slug = %slug,
                        role = %role,
                        path = %path.display(),
                        error = %err,
                        "imd: heartbeat write failed"
                    );
                }
                // V0.6.5 F146 — also touch the sidecar registry
                // heartbeat under `~/.ccteam/imd/registry/<slug>/<role>.heartbeat`
                // so a separate MCP-tool process can read `running`
                // status off disk without RPCing the daemon. Best-effort
                // — failure here doesn't kill the per-bot heartbeat
                // above (which the supervisor's stale-restart logic
                // relies on).
                if let Err(err) = crate::touch_bot_heartbeat(&slug, &role) {
                    tracing::warn!(
                        slug = %slug,
                        role = %role,
                        error = %err,
                        "imd: registry heartbeat write failed"
                    );
                }
            }
        });
        *guard = Some(handle);
        tracing::info!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            tick_secs = HEARTBEAT_TICK.as_secs(),
            "imd: bot heartbeat writer spawned"
        );
    }

    /// V0.6.1 F137 — idempotent spawn of the
    /// `adapter.events()` → `turns_mirror::append_turn` bridge.
    ///
    /// For every `ItemCompleted` event carrying `AgentMessage(text)`,
    /// build a [`TurnRecord`] and append it to the bot's
    /// `turns.jsonl`. User-side text never flows through the
    /// transcript event stream (the user input goes via `submit_turn`
    /// → tmux send-keys, never replayed back through `events()`), so
    /// only assistant rows land here; that's exactly what
    /// `forward_new_rows` filters on downstream.
    async fn spawn_events_consumer(&self, handle: ThreadHandle) {
        let mut guard = self.events_task.lock().await;
        if guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let adapter = self.adapter.clone();
        let project_dir = self.project_dir();
        let slug = self.reg.workflow_slug.clone();
        let role = self.reg.role.clone();
        let vendor = match self.reg.vendor {
            ccteam_core::harness::AgentVendor::Claude => "claude",
            ccteam_core::harness::AgentVendor::Codex => "codex",
        }
        .to_string();
        // V0.6.1 fast-path — clone the **Arc** so the task can lock-read
        // the latest sender per event. The daemon's main loop sets the
        // inner `Some(tx)` via `set_outbound_tx` AFTER `ensure_started`
        // (and therefore after this task is already spawned); cloning
        // the value at spawn time would freeze it at `None` and silently
        // disable the fast path. Locking per event is fine — this is a
        // tokio Mutex held only for a `clone()`, single-task contention.
        let outbound_tx_arc = self.outbound_tx.clone();
        let task = tokio::spawn(async move {
            let mut stream = adapter.events(&handle);
            while let Some(evt) = stream.next().await {
                let item = match evt {
                    ThreadEvent::ItemCompleted { item } => item,
                    _ => continue,
                };
                let text = match item.details {
                    ThreadItemDetails::AgentMessage(s) => s,
                    _ => continue,
                };
                if text.is_empty() {
                    continue;
                }
                let assistant_len = text.len();
                let turn_id_log = item.id.clone();
                let record = TurnRecord {
                    turn_id: item.id,
                    ts: chrono::Utc::now(),
                    vendor: vendor.clone(),
                    role: role.clone(),
                    user: String::new(),
                    assistant: text,
                    usage: Value::Null,
                    tool_calls: Vec::new(),
                };
                let append_t0 = std::time::Instant::now();
                match turns_mirror::append_turn(&project_dir, &role, &record) {
                    Ok(path) => {
                        // Capture post-append file size for the outbound
                        // cursor — the dispatcher persists this on
                        // successful TG ack so a daemon restart re-sends
                        // only un-acked rows.
                        let cursor_after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        tracing::info!(
                            event = "latency",
                            stage = "turn.done",
                            turn_id = %turn_id_log,
                            slug = %slug,
                            role = %role,
                            vendor = %vendor,
                            assistant_len,
                            append_ms = append_t0.elapsed().as_millis() as u64,
                            cursor_after,
                            path = %path.display(),
                            "latency turn.done"
                        );
                        // V0.6.1 fast-path — fan out to the per-bot
                        // outbound dispatcher if it's wired. Read the
                        // current `outbound_tx` per event (NOT once at
                        // spawn time) — the daemon may set it after
                        // this task already started. Safety-net
                        // `drain_outboxes` still covers the `None` case
                        // (e.g. supervisor restart mid-turn).
                        let tx_now = outbound_tx_arc.lock().await.clone();
                        if let Some(tx) = tx_now {
                            let item = OutboundItem {
                                turn_id: turn_id_log.clone(),
                                role: "assistant".into(),
                                content: record.assistant.clone(),
                                cursor_after,
                                enqueue_unix_ms: now_unix_ms(),
                            };
                            // `try_send` because the dispatcher is
                            // bounded; if it's backlogged the safety-net
                            // drain_outboxes pass will still pick this
                            // row up from disk later.
                            if let Err(err) = tx.try_send(item) {
                                tracing::warn!(
                                    event = "latency",
                                    stage = "turn.done.mpsc_full",
                                    turn_id = %turn_id_log,
                                    error = %err,
                                    "latency turn.done (outbound mpsc full; safety-net drain will retry)"
                                );
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            event = "latency",
                            stage = "turn.done.err",
                            turn_id = %turn_id_log,
                            slug = %slug,
                            role = %role,
                            error = %err,
                            "latency turn.done (append failed)"
                        );
                    }
                }
            }
            tracing::debug!(
                slug = %slug,
                role = %role,
                "imd: turns mirror consumer stream ended"
            );
        });
        *guard = Some(task);
        tracing::info!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            "imd: turns mirror consumer spawned"
        );
    }

    /// Abort the heartbeat + events background tasks, if any are
    /// running. Used by `shutdown` and `restart` to keep stale tasks
    /// from racing the fresh thread.
    async fn abort_background_tasks(&self) {
        if let Some(h) = self.heartbeat_task.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.events_task.lock().await.take() {
            h.abort();
        }
    }

    /// Submit one mailbox payload to the bot via
    /// `adapter.submit_turn(TurnInput::UserText(payload))`.
    ///
    /// Returns an error when the thread isn't started yet — callers
    /// typically `ensure_started().await?` first.
    pub async fn handle_inbound(&self, payload: String) -> Result<TurnId> {
        let handle = {
            let st = self.state.lock().await;
            st.handle.clone().ok_or_else(|| {
                anyhow!(
                    "bot {}/{} not started",
                    self.reg.workflow_slug,
                    self.reg.role
                )
            })?
        };
        let id = self
            .adapter
            .submit_turn(&handle, TurnInput::UserText(payload))
            .await
            .with_context(|| {
                format!(
                    "submit_turn for {}/{}",
                    self.reg.workflow_slug, self.reg.role
                )
            })?;
        tracing::debug!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            turn = %id.0,
            "submitted user turn"
        );
        Ok(id)
    }

    /// Gracefully close the underlying thread (`adapter.close_thread`)
    /// and clear local state. Idempotent — calling on a stopped
    /// supervisor is a no-op.
    pub async fn shutdown(&self) -> Result<()> {
        // V0.6.1 F136 / F137 — kill background tasks BEFORE close so
        // a final heartbeat write doesn't race the tmux teardown.
        self.abort_background_tasks().await;
        let handle = {
            let mut st = self.state.lock().await;
            st.shutting_down = true;
            st.handle.take()
        };
        if let Some(h) = handle {
            self.adapter.close_thread(&h).await.with_context(|| {
                format!(
                    "close_thread for {}/{}",
                    self.reg.workflow_slug, self.reg.role
                )
            })?;
            tracing::info!(
                slug = %self.reg.workflow_slug,
                role = %self.reg.role,
                "bot supervisor closed thread"
            );
        }
        Ok(())
    }

    /// Close-then-start cycle for stale-heartbeat recovery. Records the
    /// restart in the rolling-hour budget.
    pub async fn restart(&self) -> Result<()> {
        // V0.6.1 F136 / F137 — abort the previous thread's background
        // tasks before tearing it down; `ensure_started` will respawn
        // them against the fresh handle.
        self.abort_background_tasks().await;
        // Close first.
        let handle = self.state.lock().await.handle.take();
        if let Some(h) = handle {
            // Best-effort close; we proceed to start even if close fails
            // (the tmux session may already be dead, which is exactly
            // why we're restarting).
            if let Err(err) = self.adapter.close_thread(&h).await {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    error = %err,
                    "close_thread during restart failed; proceeding to start"
                );
            }
        }
        // Record this restart for budget tracking.
        {
            let mut st = self.state.lock().await;
            st.restarts.push(Instant::now());
            // Trim to last hour to keep the vec bounded.
            st.restarts
                .retain(|t| t.elapsed() < Duration::from_secs(3600));
        }
        // Start.
        self.ensure_started().await
    }

    /// Apply one supervisor decision (called per supervisor tick by
    /// the daemon). Returns the action that was applied for logging.
    pub async fn apply_action(&self, action: SupervisorAction) -> Result<SupervisorAction> {
        match action {
            SupervisorAction::Spawn => {
                self.ensure_started().await?;
            }
            SupervisorAction::Restart => {
                self.restart().await?;
            }
            SupervisorAction::Shutdown => {
                self.shutdown().await?;
            }
            SupervisorAction::Drain => {
                // No close — just flag so future handle_inbound calls
                // refuse new turns. (V0.6 Wave 3: enforcement landed in
                // handle_inbound is intentionally minimal — Wave 4
                // policy hook decides the drain UX.)
                let mut st = self.state.lock().await;
                st.draining = true;
            }
            SupervisorAction::Quarantine | SupervisorAction::NoOp => {}
        }
        Ok(action)
    }
}

#[cfg(test)]
mod bot_supervisor_tests {
    use super::*;
    use async_trait::async_trait;
    use ccteam_core::harness::{
        AgentVendor, ExecutionMode, HarnessError, ThreadEvent, ThreadHandle,
    };
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// Stub HarnessAdapter that records every call. The supervisor
    /// can't tell it apart from the real `ClaudeTuiAdapter` — exactly
    /// the integration contract the e2e mock test exercises.
    #[derive(Debug, Default)]
    pub struct StubAdapter {
        pub starts: AtomicUsize,
        pub submits: AtomicUsize,
        pub closes: AtomicUsize,
        pub fail_start: bool,
    }

    #[async_trait]
    impl HarnessAdapter for StubAdapter {
        fn name(&self) -> &'static str {
            "stub"
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
            if self.fail_start {
                return Err(HarnessError::SpawnFailed("stub-fail".into()));
            }
            Ok(ThreadHandle {
                vendor: AgentVendor::Claude,
                mode: ExecutionMode::Chat,
                identity: format!("stub-{}-{}", ctx.slug, spec.role),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({"slug": ctx.slug, "role": spec.role}),
            })
        }
        async fn submit_turn(
            &self,
            _h: &ThreadHandle,
            _input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Ok(TurnId::new("stub-turn"))
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

    fn reg() -> BotRegistration {
        BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "mock".into(),
            im_chat_id: "1".into(),
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn ensure_started_is_idempotent() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        sup.ensure_started().await.unwrap();
        sup.ensure_started().await.unwrap();
        assert_eq!(stub.starts.load(Ordering::SeqCst), 1);
        assert!(sup.is_started().await);
    }

    #[tokio::test]
    async fn handle_inbound_calls_submit_turn() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        sup.ensure_started().await.unwrap();
        let id = sup.handle_inbound("hello".into()).await.unwrap();
        assert_eq!(id.0, "stub-turn");
        assert_eq!(stub.submits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn handle_inbound_errors_when_not_started() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        assert!(sup.handle_inbound("x".into()).await.is_err());
        assert_eq!(stub.submits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn shutdown_closes_then_idempotent() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        sup.ensure_started().await.unwrap();
        sup.shutdown().await.unwrap();
        sup.shutdown().await.unwrap();
        assert_eq!(stub.closes.load(Ordering::SeqCst), 1);
        assert!(!sup.is_started().await);
    }

    #[tokio::test]
    async fn restart_closes_then_starts() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        sup.ensure_started().await.unwrap();
        sup.restart().await.unwrap();
        assert_eq!(stub.starts.load(Ordering::SeqCst), 2);
        assert_eq!(stub.closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn apply_action_dispatches() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        sup.apply_action(SupervisorAction::Spawn).await.unwrap();
        assert_eq!(stub.starts.load(Ordering::SeqCst), 1);
        sup.apply_action(SupervisorAction::Shutdown).await.unwrap();
        assert_eq!(stub.closes.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::harness::{AgentVendor, ExecutionMode, ThreadHandle};
    use chrono::Utc;
    use tempfile::TempDir;

    fn fake_reg() -> BotRegistration {
        BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "mock".into(),
            im_chat_id: "1".into(),
            created_at: Utc::now(),
        }
    }

    fn fake_handle() -> ThreadHandle {
        ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: "ccteam-chat-dev-foo-lead".into(),
            started_at: Utc::now(),
            raw_extras: serde_json::json!({}),
        }
    }

    #[test]
    fn shutdown_signal_wins() {
        let tmp = TempDir::new().unwrap();
        let reg = fake_reg();
        let dir = bot_dir(tmp.path(), &reg).join("signals");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SHUTDOWN_SIGNAL), "stop").unwrap();
        let st = BotState {
            handle: Some(fake_handle()),
            ..Default::default()
        };
        assert_eq!(
            decide(tmp.path(), &reg, &st, SystemTime::now()),
            SupervisorAction::Shutdown
        );
    }

    #[test]
    fn drain_signal_observed() {
        let tmp = TempDir::new().unwrap();
        let reg = fake_reg();
        let dir = bot_dir(tmp.path(), &reg).join("signals");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(DRAIN_SIGNAL), "drain").unwrap();
        // Provide a fresh heartbeat so it's not stale.
        let bot = bot_dir(tmp.path(), &reg);
        fs::create_dir_all(&bot).unwrap();
        fs::write(bot.join("heartbeat"), "x").unwrap();
        let st = BotState {
            handle: Some(fake_handle()),
            ..Default::default()
        };
        assert_eq!(
            decide(tmp.path(), &reg, &st, SystemTime::now()),
            SupervisorAction::Drain
        );
    }

    #[test]
    fn no_handle_triggers_spawn() {
        let tmp = TempDir::new().unwrap();
        let reg = fake_reg();
        assert_eq!(
            decide(tmp.path(), &reg, &BotState::default(), SystemTime::now()),
            SupervisorAction::Spawn
        );
    }

    #[test]
    fn missing_heartbeat_triggers_restart() {
        let tmp = TempDir::new().unwrap();
        let reg = fake_reg();
        let st = BotState {
            handle: Some(fake_handle()),
            ..Default::default()
        };
        assert_eq!(
            decide(tmp.path(), &reg, &st, SystemTime::now()),
            SupervisorAction::Restart
        );
    }

    #[test]
    fn fresh_heartbeat_is_noop() {
        let tmp = TempDir::new().unwrap();
        let reg = fake_reg();
        let dir = bot_dir(tmp.path(), &reg);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("heartbeat"), "x").unwrap();
        let st = BotState {
            handle: Some(fake_handle()),
            ..Default::default()
        };
        assert_eq!(
            decide(tmp.path(), &reg, &st, SystemTime::now()),
            SupervisorAction::NoOp
        );
    }

    #[test]
    fn restart_budget_quarantines() {
        let tmp = TempDir::new().unwrap();
        let reg = fake_reg();
        // Heartbeat present but doesn't matter — restart budget check
        // runs before the heartbeat check.
        let dir = bot_dir(tmp.path(), &reg);
        fs::create_dir_all(&dir).unwrap();
        let st = BotState {
            handle: Some(fake_handle()),
            restarts: (0..MAX_RESTARTS_PER_HOUR).map(|_| Instant::now()).collect(),
            ..Default::default()
        };
        assert_eq!(
            decide(tmp.path(), &reg, &st, SystemTime::now()),
            SupervisorAction::Quarantine
        );
    }

    #[test]
    fn refresh_global_heartbeat_writes_file() {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        refresh_global_heartbeat().unwrap();
        let p = imd_heartbeat_path();
        assert!(p.exists());
        std::env::remove_var("HOME");
    }
}
