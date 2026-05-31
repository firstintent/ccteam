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
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ccteam_harness::execution::turns_mirror::{self, TurnRecord};
use ccteam_harness::{
    AgentSpecBrief, HarnessAdapter, MarkerReporter, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadItemDetails, TurnId, TurnInput,
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
/// V0.6.5 F147 — name of the session-reset signal file
/// (`reset.signal`). When present in `<bot_dir>/signals/`, the next
/// supervisor tick archives the bot's `turns.jsonl`, closes the active
/// tmux session, force-resets the outbound + transcript cursors to 0
/// (V0.6.4 Bug B防线 — prevents the new session's first burst of
/// transcript bytes from getting deduped against the old cursor), and
/// starts a fresh thread. The signal file is unlinked after the reset
/// completes so the next tick doesn't loop.
pub const RESET_SIGNAL: &str = "reset.signal";

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
    /// V0.6.8 F192c — consecutive `HarnessAdapter::start_thread`
    /// failures. Incremented in `ensure_started` on adapter error,
    /// reset to 0 on a successful start. Cap is
    /// [`MAX_START_THREAD_ATTEMPTS`]; on hitting the cap the supervisor
    /// flips [`Self::permanent_failure`] and stops retrying.
    pub fail_count: u32,
    /// V0.6.8 F192c — once the supervisor has burned through
    /// [`MAX_START_THREAD_ATTEMPTS`] consecutive `start_thread`
    /// failures, this flag latches `true`. `decide_with_config`
    /// short-circuits to `Quarantine` on every subsequent tick so the
    /// daemon stops spamming identical WARN entries every 5 seconds.
    /// Recovery: `ccteam restart-bot <slug>/<role>` (which removes the
    /// registration and re-registers, building a fresh `BotState`) or
    /// a daemon restart.
    pub permanent_failure: bool,
    /// V0.6.8 F196 — consecutive "active-session-id marker missing"
    /// reports from the chat-mode tail loop. The SessionStart hook
    /// writes the F176 marker; when it fails (state.json missing, hook
    /// env propagation broke, hook subprocess errored), the marker
    /// never appears, the tail loop polls forever, and the bot is
    /// silently dead despite a healthy tmux pane. Counter resets to 0
    /// on every `report_marker_found` (the loop saw the marker again),
    /// and on every successful `reset_session`-driven self-heal start
    /// (the new session's tail loop re-arms from zero).
    pub marker_missing_count: u32,
    /// V0.6.8 F196 — number of consecutive self-heal session resets
    /// attempted in response to sustained marker-missing reports. Caps
    /// at [`MAX_MARKER_SELF_HEAL_ATTEMPTS`]; once the cap is reached
    /// the supervisor latches [`Self::marker_stuck`] and emits
    /// `chat_bot_marker_stuck`. Counter resets on `record_marker_found`
    /// so a single bad spawn followed by a clean recovery does not
    /// burn the budget down forever.
    pub marker_self_heal_attempts: u32,
    /// V0.6.8 F196 — once the supervisor has burned through
    /// [`MAX_MARKER_SELF_HEAL_ATTEMPTS`] consecutive self-heal session
    /// resets without the F176 marker ever appearing again, this flag
    /// latches `true`. Further `record_marker_missing` reports
    /// short-circuit to `MarkerHealAction::Quiet` so the supervisor
    /// stops cycling the session. Recovery requires the operator to
    /// restore the SessionStart hook prerequisite + write
    /// `signals/reset.signal` (which clears the latch as part of the
    /// `reset_session` flow) or restart the daemon.
    pub marker_stuck: bool,
}

/// V0.6.8 F192c — maximum consecutive `start_thread` attempts before
/// the supervisor latches `permanent_failure`. After this many
/// back-to-back failures the supervisor emits one
/// `chat_bot_permanent_failure` progress event and stops retrying.
/// The current tick interval (5s) means the give-up wall-clock is
/// roughly `MAX_START_THREAD_ATTEMPTS * tick = 15s` — enough to catch
/// a transient flake on the first or second tick while not letting a
/// permanently-broken bot flood logs with identical WARNs.
pub const MAX_START_THREAD_ATTEMPTS: u32 = 3;

/// V0.6.8 F196 — number of consecutive tail-loop "marker missing"
/// reports before the supervisor escalates to a self-heal session
/// reset. The chat-mode tail loop reports roughly once every 2s in
/// the inotify-driven path (safety-net cadence) and the polling
/// fallback's exponential backoff settles at the same ceiling, so
/// 30 reports ≈ 60s of silence — enough to cover a slow first-prompt
/// SessionStart grace period while still catching a stuck loop well
/// before the user reports "bot dead".
pub const MARKER_MISSING_RESET_THRESHOLD: u32 = 30;

/// V0.6.8 F196 — cap on consecutive self-heal session resets before
/// the supervisor latches the marker-stuck state and stops trying.
/// Same envelope as [`MAX_START_THREAD_ATTEMPTS`]: a single
/// SessionStart hook flake gets one cheap recovery, two flakes get
/// another, three consecutive failures mean the breakage is
/// structural (state.json deleted by ops, hook script unreadable,
/// etc.) and further auto-resets only churn the bot.
pub const MAX_MARKER_SELF_HEAL_ATTEMPTS: u32 = 3;

/// V0.6.8 F196 — outcome of one [`BotSupervisor::record_marker_missing`]
/// call, returned to the caller (the chat-mode tail loop's
/// [`MarkerReporter`] impl, internally — operators never see this
/// enum directly).
///
/// Frame: this is **escalate-after-sustained-stuck-state**, not
/// kill-mid-turn. R5 "永不主动 kill 长 session" honours the same
/// channel F84 (budget overflow) and F192c (spawn-failure) use:
/// the supervisor only escalates when the bot is functionally dead
/// (no marker means the tail loop will never see new content) and
/// the recovery path is a fresh `start_thread` against the same
/// `(slug, role)` identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerHealAction {
    /// Counter incremented, threshold not yet reached. No action.
    Quiet,
    /// Threshold reached this tick. Caller should run the heal: emit
    /// the `chat_marker_self_heal_attempt` progress event, call
    /// `reset_session`, and bump `marker_self_heal_attempts`.
    Heal,
    /// Self-heal budget exhausted. Caller should emit the
    /// `chat_bot_marker_stuck` event once and refuse further resets.
    /// Subsequent `record_marker_missing` calls remain `Quiet` until
    /// the operator clears the latch (write `signals/reset.signal`
    /// or restart the daemon).
    PermanentFailure,
}

/// V0.6.8 F195 — per-turn watchdog state held inside
/// [`BotSupervisor::active_turn`].
///
/// Set when [`BotSupervisor::handle_inbound`] successfully calls
/// `adapter.submit_turn`; cleared on `ItemCompleted/AgentMessage`
/// (assistant turn done from the harness's perspective), `shutdown`,
/// `reset_session`. Two latches — `long_emitted`, `timeout_emitted` —
/// stop the daemon's tick from re-firing the same notification on
/// every 5s pass.
#[derive(Debug, Clone)]
pub struct TurnDeadline {
    /// Adapter-assigned turn id, copied from the `submit_turn` return.
    pub turn_id: TurnId,
    /// When [`BotSupervisor::handle_inbound`] called `submit_turn`.
    /// `Instant` (monotonic) so wall-clock drift can't reset the timer.
    pub started_at: Instant,
    /// True once the `chat_turn_running_long` notification fired.
    pub long_emitted: bool,
    /// True once the `chat_turn_timeout` notification fired.
    pub timeout_emitted: bool,
}

/// V0.6.8 F195 — one notification the watchdog wants to surface for an
/// outstanding turn. Returned from
/// [`BotSupervisor::check_turn_watchdog`] so the daemon owns the IO
/// (progress.jsonl append + `channel.send`) — the supervisor stays a
/// pure decision-maker, mirroring `decide_with_config`'s split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnWatchdogNotice {
    /// First threshold (`1× turn_timeout_sec`). User-facing IM line:
    /// "Still working on your message (turn started <HH:MM:SS>).
    /// Continuing."
    RunningLong {
        /// Adapter turn id.
        turn_id: String,
        /// Seconds since `submit_turn`.
        elapsed_sec: u64,
    },
    /// Second threshold (`2× turn_timeout_sec`). User-facing IM line
    /// includes recovery instructions ("/clear and retry").
    Timeout {
        /// Adapter turn id.
        turn_id: String,
        /// Seconds since `submit_turn`.
        elapsed_sec: u64,
    },
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
    /// V0.6.5 F147 — user-requested session reset
    /// (`signals/reset.signal`). Archive `turns.jsonl`, close + start a
    /// fresh thread, and reset the outbound + transcript cursors to 0.
    /// Differs from `Restart`: doesn't count against the per-hour
    /// restart budget (this is an intentional user action, not a flap),
    /// and ALWAYS archives so the new session starts on a clean mirror.
    ResetSession,
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
/// Wrapper around [`decide_with_config`] that passes an empty config
/// map (the daemon's production path uses the config-aware form so
/// F190's `~/.ccteam/config.yaml::projects[]` tier applies).
pub fn decide(
    projects_root: &Path,
    reg: &BotRegistration,
    state: &BotState,
    now: SystemTime,
) -> SupervisorAction {
    decide_with_config(projects_root, reg, state, now, &HashMap::new())
}

/// V0.6.8 F190 — config-yaml-aware variant of [`decide`]. Legacy
/// supervisor-driven callers thread the slug → path map through this
/// entry so a bot (no `reg.project_dir`) whose project lives outside
/// the projects_root tree still hits the right bot_dir for signal /
/// heartbeat checks.
pub fn decide_with_config(
    projects_root: &Path,
    reg: &BotRegistration,
    state: &BotState,
    now: SystemTime,
    config_projects: &HashMap<String, PathBuf>,
) -> SupervisorAction {
    let bot_dir = bot_dir_with_config(projects_root, reg, config_projects);

    // Layer A — shutdown beats everything else (terminal).
    if signal_present(&bot_dir, SHUTDOWN_SIGNAL) || state.shutting_down {
        return SupervisorAction::Shutdown;
    }

    // Layer B — drain mode.
    if signal_present(&bot_dir, DRAIN_SIGNAL) {
        return SupervisorAction::Drain;
    }

    // V0.6.5 F147 — explicit user reset wins over the heartbeat-stale
    // restart loop, but loses to shutdown / drain (terminal states).
    // Independent of restart budget — a manual reset is an intentional
    // action, not a flap, and should always succeed even when the bot
    // has burned through its hourly Restart budget. Reset also clears
    // F192c `permanent_failure` (the supervisor's `reset_session`
    // wipes `fail_count` + `permanent_failure` so a recovered config
    // can retry).
    if signal_present(&bot_dir, RESET_SIGNAL) {
        return SupervisorAction::ResetSession;
    }

    // V0.6.8 F192c — `start_thread` retry budget exhausted. The
    // supervisor's `ensure_started` flips this latch after
    // `MAX_START_THREAD_ATTEMPTS` consecutive failures and emits one
    // `chat_bot_permanent_failure` progress event. Subsequent ticks
    // return `Quarantine` so the daemon stops spamming identical
    // WARNs every 5 seconds. User-issued reset / drain / shutdown
    // still cut through above; Quarantine maps to `NoOp` in
    // `apply_action`, no further adapter calls are made.
    if state.permanent_failure {
        return SupervisorAction::Quarantine;
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

/// Per-bot dir: `<project>/.ccteam/chat/<role>/`. F185 — prefers
/// `reg.project_dir` (absolute path written by `chat_register_bot` at
/// registration time). Falls back to the historical
/// `<projects_root>/<workflow_slug>/.ccteam/chat/<role>/` layout for
/// pre-F185 registrations that have `project_dir = None`.
///
/// F190 wrapper — see [`bot_dir_with_config`] for the config-yaml-aware
/// resolver the daemon uses; this base form keeps the legacy / test
/// signature stable (callers without a config map pass through here).
pub fn bot_dir(projects_root: &Path, reg: &BotRegistration) -> PathBuf {
    reg.chat_dir(projects_root)
}

/// V0.6.8 F190 — config-yaml-aware variant of [`bot_dir`]. Resolves
/// `<project>/.ccteam/chat/<role>/` honoring the full three-tier
/// priority chain (`reg.project_dir` → `config_projects[slug]` →
/// `<projects_root>/<slug>/`). Legacy supervisor-driven callers use
/// this via [`decide_with_config`] so bots whose project lives outside
/// the projects_root tree hit the right signal / heartbeat path.
pub fn bot_dir_with_config(
    projects_root: &Path,
    reg: &BotRegistration,
    config_projects: &HashMap<String, PathBuf>,
) -> PathBuf {
    reg.chat_dir_with_config(projects_root, config_projects)
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
/// concrete execution adapter import lives in this supervisor. Tests
/// inject a stub adapter.
pub struct BotSupervisor {
    /// Registration this supervisor binds to.
    pub reg: BotRegistration,
    /// Projects root (`<projects_root>/<slug>/.ccteam/chat/<role>/`).
    pub projects_root: PathBuf,
    /// V0.6.8 F190 — `~/.ccteam/config.yaml::projects[]` slug → path
    /// map. Daemon loads this once at startup and passes it through
    /// [`Self::new_with_config`] so legacy bots (no `reg.project_dir`)
    /// whose project lives outside the projects_root tree still
    /// resolve correctly via the F190 tier of
    /// [`crate::resolve_project_dir`]. Empty map for tests / callers
    /// that skip the config tier.
    pub config_projects: HashMap<String, PathBuf>,
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
    /// V0.6.8 F193 — hop counter of the most recent inbound turn the
    /// supervisor accepted. Stored atomically so the spawned events
    /// consumer (which builds the [`OutboundItem`] for each reply) can
    /// stamp every outbound row with the inbound's hop. The daemon's
    /// outbound dispatcher then computes `next_hop = item.hop + 1`
    /// when it detects an embedded `@<otherbot>` mention and
    /// synthesizes a cross-bot `InboxItem`. User-IM-sourced turns enter
    /// with `hop = 0`.
    current_hop: Arc<AtomicU8>,
    /// V0.6.8 F196 — self-Weak captured during
    /// `register_as_marker_reporter` so the [`MarkerReporter`] trait
    /// impl (which only has `&self`) can recover an `Arc<Self>` to
    /// drive the async heal task without holding the supervisor alive
    /// past its rightful drop. Set once; subsequent registrations
    /// (restart / reset) reuse the same Weak.
    self_weak: std::sync::OnceLock<Weak<Self>>,
    /// V0.6.8 F195 — outstanding-turn watchdog state. Set by
    /// `handle_inbound` after a successful `submit_turn`, cleared by
    /// the `events()` consumer on `ItemCompleted/AgentMessage` (the
    /// HarnessAdapter's local view of "turn completed"), and also
    /// cleared in `shutdown` / `reset_session`. The daemon's tick loop
    /// calls [`check_turn_watchdog`] each pass to drain
    /// [`TurnWatchdogNotice`]s that crossed a threshold.
    ///
    /// `Arc<Mutex<_>>` so the spawned events-consumer task can hold its
    /// own clone and clear the deadline without re-locking the
    /// supervisor's main `state`.
    active_turn: Arc<Mutex<Option<TurnDeadline>>>,
    /// V0.6.8 F195 — per-turn watchdog threshold (seconds). First
    /// crossing emits `chat_turn_running_long`; second crossing (at
    /// `2× turn_timeout_sec`) emits `chat_turn_timeout`. Defaults to
    /// [`ccteam_core::DEFAULT_TURN_TIMEOUT_SECS`] when
    /// callers use [`Self::new`] / [`Self::new_with_config`]; a future
    /// patch will plumb `workflow.yaml::chat.turn_timeout_sec` through
    /// here once the daemon learns to load workflow specs at startup.
    pub turn_timeout_sec: u32,
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
    /// HarnessAdapter call. Initial state is empty (no handle). The
    /// F190 config-yaml tier is empty — callers that want the daemon's
    /// `~/.ccteam/config.yaml::projects[]` lookup should use
    /// [`Self::new_with_config`].
    pub fn new(
        reg: BotRegistration,
        projects_root: impl Into<PathBuf>,
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    ) -> Self {
        Self::new_with_config(reg, projects_root, adapter, HashMap::new())
    }

    /// V0.6.8 F190 — full constructor wiring the
    /// `~/.ccteam/config.yaml::projects[]` slug → path map so
    /// `project_dir()` / `bot_dir()` resolution honors the F190 tier
    /// for legacy registrations whose project lives outside the
    /// projects_root tree.
    pub fn new_with_config(
        reg: BotRegistration,
        projects_root: impl Into<PathBuf>,
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
        config_projects: HashMap<String, PathBuf>,
    ) -> Self {
        Self {
            reg,
            projects_root: projects_root.into(),
            config_projects,
            adapter,
            state: Mutex::new(BotState::default()),
            heartbeat_task: Mutex::new(None),
            events_task: Mutex::new(None),
            outbound_tx: Arc::new(Mutex::new(None)),
            current_hop: Arc::new(AtomicU8::new(0)),
            self_weak: std::sync::OnceLock::new(),
            active_turn: Arc::new(Mutex::new(None)),
            turn_timeout_sec: ccteam_core::DEFAULT_TURN_TIMEOUT_SECS,
        }
    }

    /// V0.6.8 F195 — full constructor including the watchdog timeout
    /// override. Production callers stick with
    /// [`Self::new`] / [`Self::new_with_config`] (default 90s); tests
    /// pass a tight value (e.g. 1s) so threshold assertions fit under
    /// `max_runtime`. A future patch will thread
    /// `workflow.yaml::chat.turn_timeout_sec` through here once the
    /// daemon learns to load workflow specs at startup.
    pub fn new_with_turn_timeout(
        reg: BotRegistration,
        projects_root: impl Into<PathBuf>,
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
        config_projects: HashMap<String, PathBuf>,
        turn_timeout_sec: u32,
    ) -> Self {
        let mut sup = Self::new_with_config(reg, projects_root, adapter, config_projects);
        sup.turn_timeout_sec = turn_timeout_sec;
        sup
    }

    /// V0.6.1 fast-path — daemon main loop calls this once per bot
    /// right after `ensure_started` succeeds. From the next event onward
    /// the events consumer enqueues each assistant row into `tx` so the
    /// per-bot outbound dispatcher can fire `channel.send` immediately
    /// (skipping the safety-net 60s `drain_outboxes` scan).
    pub async fn set_outbound_tx(&self, tx: mpsc::Sender<OutboundItem>) {
        *self.outbound_tx.lock().await = Some(tx);
    }

    /// V0.6.8 F196 — register this supervisor as the chat-mode tail
    /// loop's marker reporter for `(slug, role)`. Called by the daemon
    /// right after `ensure_started` succeeds (mirrors the
    /// `set_outbound_tx` shape — both wire the supervisor as a
    /// downstream of the adapter's event stream).
    ///
    /// The registry holds a [`std::sync::Weak`] so the supervisor's
    /// drop semantics are unchanged: a stopped bot's reporter cleanly
    /// disappears, and the tail loop's lookup returns `None` after
    /// the Arc drops. Re-registration on restart / reset overwrites
    /// the previous entry without leaking.
    pub fn register_as_marker_reporter(self: &Arc<Self>) {
        // Stash the self-Weak the first time we register so the
        // MarkerReporter trait impl can recover Arc<Self> to drive
        // the heal task (`&self` alone can't spawn an `Arc<Self>`
        // tokio future). Subsequent registrations reuse the same
        // Weak — re-registering after restart is fine since we
        // always work off the same supervisor Arc.
        let _ = self.self_weak.set(Arc::downgrade(self));
        let weak: Weak<dyn MarkerReporter> = Arc::downgrade(self) as Weak<dyn MarkerReporter>;
        ccteam_harness::execution::marker_reporter::register(
            &self.reg.workflow_slug,
            &self.reg.role,
            weak,
        );
    }

    /// V0.6.8 F196 — companion to [`Self::register_as_marker_reporter`].
    /// Called from `shutdown` so a long-lived daemon doesn't accumulate
    /// dead entries. Idempotent on a missing key.
    pub fn unregister_marker_reporter(&self) {
        ccteam_harness::execution::marker_reporter::unregister(
            &self.reg.workflow_slug,
            &self.reg.role,
        );
    }

    /// `<projects_root>/<slug>/.ccteam/chat/<role>/`. Helper so the
    /// background tasks share one resolution path with `decide`. F190 —
    /// honors `config_projects` so the legacy registration tier picks
    /// the right path out of `~/.ccteam/config.yaml::projects[]`.
    fn bot_dir(&self) -> PathBuf {
        bot_dir_with_config(&self.projects_root, &self.reg, &self.config_projects)
    }

    /// Root dir for the project hosting this bot's
    /// `.ccteam/workflow.yaml`. F185 — prefers `reg.project_dir` when
    /// set; F190 — falls back to `config_projects[slug]` when
    /// `reg.project_dir = None`; final fallback is the historical
    /// `<projects_root>/<workflow_slug>/` layout.
    pub fn project_dir(&self) -> PathBuf {
        self.reg
            .project_root_with_config(&self.projects_root, &self.config_projects)
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

    /// V0.6.8 F192b/c — book-keep one `start_thread` failure. Logs a
    /// WARN line carrying the full anyhow chain (which includes the
    /// adapter's `SpawnFailed` body — for tmux-backed adapters that's
    /// `tmux new-session` stderr already embedded), increments
    /// `BotState::fail_count`, and on hitting
    /// [`MAX_START_THREAD_ATTEMPTS`] consecutive failures latches
    /// `BotState::permanent_failure` and emits one
    /// `chat_bot_permanent_failure` progress event.
    async fn record_start_failure(&self, err: &anyhow::Error) {
        let mut st = self.state.lock().await;
        st.fail_count = st.fail_count.saturating_add(1);
        let attempts = st.fail_count;
        let permanent = attempts >= MAX_START_THREAD_ATTEMPTS;
        if permanent {
            st.permanent_failure = true;
        }
        drop(st);

        // The full anyhow chain (`{err:#}`) includes the underlying
        // adapter SpawnFailed body, which for tmux-backed adapters
        // already embeds `tmux new-session` stderr. Truncate to 1 KB
        // so a perpetually-failing flap doesn't bloat the log file.
        let chain = format!("{err:#}");
        let chain_trunc: String = chain.chars().take(1024).collect();

        if permanent {
            tracing::warn!(
                slug = %self.reg.workflow_slug,
                role = %self.reg.role,
                adapter = self.adapter.name(),
                attempts,
                error_chain = %chain_trunc,
                "F192c: start_thread failed {} times in a row; latching permanent_failure (no more retries)",
                attempts,
            );
            // Best-effort emit `chat_bot_permanent_failure` so IM /
            // web surfaces show the user the bot is stuck. Path
            // resolution honors `CCTEAM_HOME` so tests land in their
            // tempdir layout. A missing CcteamPaths (no env) is the
            // normal `cargo test` posture — we silently skip the
            // append; the WARN above is the audit trail.
            if let Ok(paths) = ccteam_core::CcteamPaths::from_env() {
                let progress_path = paths.progress_jsonl(&self.reg.workflow_slug);
                let ev = ccteam_core::progress::build_chat_bot_permanent_failure_event(
                    &self.reg.role,
                    &chain_trunc,
                    attempts,
                );
                if let Err(append_err) = ccteam_core::progress::append_event(&progress_path, &ev) {
                    tracing::warn!(
                        slug = %self.reg.workflow_slug,
                        role = %self.reg.role,
                        error = %append_err,
                        "F192c: failed to append chat_bot_permanent_failure event to progress.jsonl"
                    );
                }
            }
        } else {
            tracing::warn!(
                slug = %self.reg.workflow_slug,
                role = %self.reg.role,
                adapter = self.adapter.name(),
                attempts,
                max = MAX_START_THREAD_ATTEMPTS,
                error_chain = %chain_trunc,
                "F192b: start_thread failed (attempt {}/{}); will retry next tick",
                attempts,
                MAX_START_THREAD_ATTEMPTS,
            );
        }
    }

    /// V0.6.8 F196 — record one tail-loop tick where the F176
    /// `active-session-id` marker was missing. Returns the
    /// [`MarkerHealAction`] the caller should apply.
    ///
    /// State machine:
    /// - Below [`MARKER_MISSING_RESET_THRESHOLD`]: increment counter,
    ///   return `Quiet`.
    /// - At threshold AND `marker_self_heal_attempts <
    ///   MAX_MARKER_SELF_HEAL_ATTEMPTS`: bump
    ///   `marker_self_heal_attempts`, reset `marker_missing_count` to
    ///   zero (the next reset arms a fresh window), return `Heal`.
    /// - At threshold AND `marker_self_heal_attempts ==
    ///   MAX_MARKER_SELF_HEAL_ATTEMPTS - 1` after this bump: same as
    ///   `Heal` for the current attempt, but the next miss-threshold
    ///   crossing returns `PermanentFailure`.
    /// - Once `marker_stuck` has latched: every call returns `Quiet`.
    ///   The supervisor is done; reset / restart-bot clears the latch.
    pub async fn record_marker_missing(&self) -> MarkerHealAction {
        let mut st = self.state.lock().await;
        if st.marker_stuck {
            return MarkerHealAction::Quiet;
        }
        st.marker_missing_count = st.marker_missing_count.saturating_add(1);
        if st.marker_missing_count < MARKER_MISSING_RESET_THRESHOLD {
            return MarkerHealAction::Quiet;
        }
        // Threshold crossed — escalate. Reset the per-window counter
        // so the *next* heal attempt also takes a fresh threshold's
        // worth of silence (avoiding back-to-back resets if the new
        // session happens to be slow on its first hook fire).
        st.marker_missing_count = 0;
        if st.marker_self_heal_attempts >= MAX_MARKER_SELF_HEAL_ATTEMPTS {
            st.marker_stuck = true;
            return MarkerHealAction::PermanentFailure;
        }
        st.marker_self_heal_attempts = st.marker_self_heal_attempts.saturating_add(1);
        // Note: we do NOT latch `marker_stuck` here even when this bump
        // hits the cap — the current Heal still gets to run. The latch
        // trips on the NEXT threshold crossing (where
        // `marker_self_heal_attempts >= MAX_MARKER_SELF_HEAL_ATTEMPTS`
        // returns `PermanentFailure` above). That branch is what
        // distinguishes "burning the last attempt" (Heal) from "having
        // burned the last attempt" (PermanentFailure).
        MarkerHealAction::Heal
    }

    /// V0.6.8 F196 — record one tail-loop tick where the F176 marker
    /// was present and resolvable. Resets both consecutive-miss + heal
    /// budget counters: a healthy bot starts every silence window
    /// from scratch, so a single sustained outage doesn't shorten
    /// the next legitimate flake's grace period. Does NOT clear
    /// `marker_stuck` — once we've declared permanent failure, only
    /// an operator-driven `reset_session` re-arms the supervisor.
    pub async fn record_marker_found(&self) {
        let mut st = self.state.lock().await;
        st.marker_missing_count = 0;
        st.marker_self_heal_attempts = 0;
    }

    /// V0.6.8 F196 — implementation of the heal sequence the daemon
    /// runs when `record_marker_missing` returned `Heal`. Emits the
    /// `chat_marker_self_heal_attempt` progress event for observability
    /// (operators see it in progress.jsonl + the web dashboard) and
    /// drives the existing F192c `reset_session` so the tmux pane is
    /// recycled and the new session fires a fresh SessionStart hook.
    /// Errors during reset are logged but not propagated — a failing
    /// heal counts toward the budget via the next threshold crossing
    /// (the marker will still be missing on the next tick).
    ///
    /// Returns the attempt number used in the emitted event (1-based).
    pub async fn attempt_marker_self_heal(self: &Arc<Self>) -> u32 {
        let attempt_n = self.state.lock().await.marker_self_heal_attempts;
        tracing::warn!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            attempt_n,
            max = MAX_MARKER_SELF_HEAL_ATTEMPTS,
            "F196: SessionStart marker missing past threshold; escalating to session reset"
        );
        if let Ok(paths) = ccteam_core::CcteamPaths::from_env() {
            let progress_path = paths.progress_jsonl(&self.reg.workflow_slug);
            let ev = ccteam_core::progress::build_chat_marker_self_heal_attempt_event(
                &self.reg.role,
                attempt_n,
            );
            if let Err(err) = ccteam_core::progress::append_event(&progress_path, &ev) {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    error = %err,
                    "F196: failed to append chat_marker_self_heal_attempt event to progress.jsonl"
                );
            }
        }
        if let Err(err) = self.reset_session().await {
            tracing::warn!(
                slug = %self.reg.workflow_slug,
                role = %self.reg.role,
                error = %err,
                "F196: reset_session during self-heal failed; next threshold crossing will retry or latch"
            );
        }
        attempt_n
    }

    /// V0.6.8 F196 — companion to [`Self::attempt_marker_self_heal`]
    /// for the `PermanentFailure` branch. Emits one
    /// `chat_bot_marker_stuck` event and logs a WARN; subsequent
    /// `record_marker_missing` calls return `Quiet` so the supervisor
    /// stops cycling the bot.
    pub async fn record_marker_stuck(&self) {
        let attempts = self.state.lock().await.marker_self_heal_attempts;
        tracing::warn!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            attempts,
            max = MAX_MARKER_SELF_HEAL_ATTEMPTS,
            "F196: SessionStart marker still missing after {} self-heal resets; latching marker_stuck (no more auto-recovery)",
            attempts,
        );
        if let Ok(paths) = ccteam_core::CcteamPaths::from_env() {
            let progress_path = paths.progress_jsonl(&self.reg.workflow_slug);
            let ev =
                ccteam_core::progress::build_chat_bot_marker_stuck_event(&self.reg.role, attempts);
            if let Err(err) = ccteam_core::progress::append_event(&progress_path, &ev) {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    error = %err,
                    "F196: failed to append chat_bot_marker_stuck event to progress.jsonl"
                );
            }
        }
    }

    /// Idempotent: start the underlying tmux session via
    /// `adapter.start_thread` if not already running.
    ///
    /// V0.6.8 F192b/c — on `start_thread` failure this method:
    /// 1. Logs a WARN line carrying the full anyhow error chain
    ///    (`{err:#}` includes the adapter's `SpawnFailed` body, which
    ///    for tmux-backed adapters already embeds `tmux new-session`
    ///    stderr — see `crates/ccteam-core/src/tmux.rs::start_with_env`).
    /// 2. Increments `BotState::fail_count`. On hitting
    ///    [`MAX_START_THREAD_ATTEMPTS`] consecutive failures, latches
    ///    `BotState::permanent_failure = true` and emits one
    ///    `chat_bot_permanent_failure` progress event so the daemon's
    ///    next `decide_with_config` short-circuits to `Quarantine` and
    ///    stops spamming identical WARNs every 5 seconds.
    /// 3. Returns the underlying error to the caller so legacy
    ///    supervisor-driven loops can log it and continue running other
    ///    bots.
    pub async fn ensure_started(&self) -> Result<()> {
        {
            let st = self.state.lock().await;
            if st.handle.is_some() {
                return Ok(());
            }
            // V0.6.8 F192c — once we've latched permanent failure,
            // refuse to retry until reset / restart-bot clears it.
            // The daemon's tick path returns `Quarantine` first so we
            // shouldn't normally get here, but `apply_action(Spawn)`
            // direct callers (e.g. tests) still hit this guard.
            if st.permanent_failure {
                return Err(anyhow!(
                    "bot {}/{} is in permanent_failure state \
                     (start_thread failed {} times in a row); \
                     run `ccteam restart-bot {}/{}` or restart the daemon",
                    self.reg.workflow_slug,
                    self.reg.role,
                    st.fail_count,
                    self.reg.workflow_slug,
                    self.reg.role,
                ));
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
        let start_result = self
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
            });
        let handle = match start_result {
            Ok(h) => h,
            Err(err) => {
                self.record_start_failure(&err).await;
                return Err(err);
            }
        };
        let mut st = self.state.lock().await;
        st.handle = Some(handle.clone());
        // V0.6.8 F192c — success path resets the consecutive-failure
        // counter. (`permanent_failure` is only cleared on reset /
        // restart-bot; once latched the supervisor refuses to retry
        // even after a successful manual restart, which is the
        // intended hard-stop semantic.)
        st.fail_count = 0;
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
            ccteam_harness::AgentVendor::Claude => "claude",
            ccteam_harness::AgentVendor::Codex => "codex",
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
        // V0.6.8 F193 — clone the current_hop Arc so each event reads
        // the hop value last stashed by `handle_inbound`. The atomic
        // load is wait-free; the per-event read picks up the most
        // recent value (across submit_turn await points + scheduler
        // hand-off).
        let current_hop_arc = self.current_hop.clone();
        // V0.6.8 F195 — clone the `active_turn` Arc into the spawned
        // task so it can clear the watchdog deadline the moment the
        // adapter emits the matching `ItemCompleted/AgentMessage`. The
        // task can't reach `self` (lifetime), so we hand the Arc in
        // explicitly. Mirrors the `outbound_tx` pattern above.
        let active_turn_arc = self.active_turn.clone();
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
                // V0.6.8 F195 — assistant text means the harness saw
                // the turn finish. Clear the watchdog so a subsequent
                // post-timeout reply doesn't keep firing the daemon's
                // tick notifications. We clear unconditionally (rather
                // than match on turn_id) because in steady-state the
                // bot serializes one turn at a time; the rare edge
                // case where a stale post-clear turn lands is benign
                // (next `handle_inbound` re-arms the deadline anyway).
                {
                    let mut guard = active_turn_arc.lock().await;
                    *guard = None;
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
                            let hop = current_hop_arc.load(Ordering::SeqCst);
                            let item = OutboundItem {
                                turn_id: turn_id_log.clone(),
                                role: "assistant".into(),
                                content: record.assistant.clone(),
                                cursor_after,
                                enqueue_unix_ms: now_unix_ms(),
                                hop,
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
    ///
    /// V0.6.8 F193 — `hop` is the bot-to-bot loop-guard counter of the
    /// inbound turn. The supervisor stores it on `current_hop` so the
    /// outbound events consumer can stamp every reply's
    /// `OutboundItem.hop` with it; the outbound dispatcher reads that
    /// value to compute `next_hop = hop + 1` when it detects an
    /// embedded `@<otherbot>` mention and synthesizes a cross-bot
    /// `InboxItem`. User-IM-sourced turns enter with `hop = 0`.
    ///
    /// V0.6.8 F195 — on a successful submit, arms the per-turn watchdog
    /// by storing a fresh [`TurnDeadline`] in `active_turn`. The
    /// daemon's tick loop polls [`check_turn_watchdog`] each pass; when
    /// the elapsed time crosses `1× turn_timeout_sec` (default 90s) the
    /// daemon emits `chat_turn_running_long` + a "still working" IM
    /// reply; at `2×` it emits `chat_turn_timeout` + a "stuck" IM
    /// reply. The deadline is cleared when the events consumer sees
    /// the matching `ItemCompleted/AgentMessage` (turn done) or when
    /// the supervisor is shut down / reset.
    pub async fn handle_inbound(&self, payload: String, hop: u8) -> Result<TurnId> {
        // V0.6.8 F193 — stash the inbound hop BEFORE submit_turn so the
        // events consumer (which fires asynchronously when the adapter
        // emits ItemCompleted) reads the right value when it builds
        // the OutboundItem.
        self.current_hop.store(hop, Ordering::SeqCst);
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
        // V0.6.8 F195 — arm the watchdog *after* submit_turn succeeds.
        // On submit failure we leave any previous deadline alone — a
        // pre-existing in-flight turn from an earlier `handle_inbound`
        // is still a legitimate stall to surface. (Practically the bot
        // serializes turns through tmux, so back-to-back submits land
        // sequentially; the rare overlap window is benign.)
        {
            let mut guard = self.active_turn.lock().await;
            *guard = Some(TurnDeadline {
                turn_id: id.clone(),
                started_at: Instant::now(),
                long_emitted: false,
                timeout_emitted: false,
            });
        }
        tracing::debug!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            turn = %id.0,
            hop,
            "submitted user turn"
        );
        Ok(id)
    }

    /// V0.6.8 F195 — snapshot the outstanding turn (test / observability).
    pub async fn active_turn_snapshot(&self) -> Option<TurnDeadline> {
        self.active_turn.lock().await.clone()
    }

    /// V0.6.8 F195 — poll the watchdog for one bot.
    ///
    /// Returns the notification (if any) the daemon should surface this
    /// tick AND latches `long_emitted` / `timeout_emitted` so the next
    /// tick doesn't re-fire the same notice. Pure decision-side: no
    /// progress.jsonl writes, no `channel.send` — those happen on the
    /// daemon side (which already owns the [`ChannelMap`] +
    /// [`CcteamPaths`] surface needed for the IO).
    ///
    /// Hard rule (CLAUDE.md §三 R5): this method never touches the
    /// underlying claude / tmux session. The watchdog only **observes**
    /// the silent stall and surfaces it; recovery is user-driven.
    ///
    /// Returns at most one notice per call (the earliest unlatched
    /// threshold the deadline has crossed). The daemon calls this once
    /// per 5s tick per bot; in steady-state a stalled turn emits
    /// `RunningLong` on the tick after the 90s mark, then `Timeout` on
    /// the tick after the 180s mark, then nothing.
    pub async fn check_turn_watchdog(&self) -> Option<TurnWatchdogNotice> {
        let mut guard = self.active_turn.lock().await;
        let deadline = guard.as_mut()?;
        let elapsed = deadline.started_at.elapsed();
        let timeout_sec = self.turn_timeout_sec.max(1) as u64;
        let elapsed_sec = elapsed.as_secs();
        // Second threshold (2x) wins over first when both are crossed
        // and `RunningLong` was already emitted — we always advance
        // forward through the thresholds, never re-emit.
        if !deadline.timeout_emitted && elapsed_sec >= timeout_sec.saturating_mul(2) {
            deadline.timeout_emitted = true;
            // Set `long_emitted` too so a daemon that skipped the
            // intermediate tick (e.g. busy tokio scheduler) doesn't
            // back-fire `RunningLong` next pass.
            deadline.long_emitted = true;
            return Some(TurnWatchdogNotice::Timeout {
                turn_id: deadline.turn_id.0.clone(),
                elapsed_sec,
            });
        }
        if !deadline.long_emitted && elapsed_sec >= timeout_sec {
            deadline.long_emitted = true;
            return Some(TurnWatchdogNotice::RunningLong {
                turn_id: deadline.turn_id.0.clone(),
                elapsed_sec,
            });
        }
        None
    }

    /// V0.6.8 F195 — clear the outstanding-turn watchdog. Called by the
    /// events consumer when `ItemCompleted/AgentMessage` fires (turn
    /// done from the harness's view) and by `shutdown` /
    /// `reset_session` so a stale deadline doesn't survive a session
    /// teardown. Idempotent — clearing an already-cleared slot is a
    /// no-op.
    pub async fn clear_active_turn(&self) {
        *self.active_turn.lock().await = None;
    }

    /// Gracefully close the underlying thread (`adapter.close_thread`)
    /// and clear local state. Idempotent — calling on a stopped
    /// supervisor is a no-op.
    pub async fn shutdown(&self) -> Result<()> {
        // V0.6.1 F136 / F137 — kill background tasks BEFORE close so
        // a final heartbeat write doesn't race the tmux teardown.
        self.abort_background_tasks().await;
        // V0.6.8 F196 — drop the marker reporter registration so the
        // tail loop (if still alive on a respawn) doesn't fire heal
        // attempts against a closing supervisor.
        self.unregister_marker_reporter();
        // V0.6.8 F195 — drop any outstanding watchdog deadline so a
        // post-shutdown daemon tick doesn't keep emitting timeout
        // notices for a bot whose session is gone.
        self.clear_active_turn().await;
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
        // V0.6.8 F195 — drop the watchdog deadline: the old session is
        // being torn down, any in-flight turn it owned is gone.
        self.clear_active_turn().await;
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

    /// V0.6.5 F147 — execute a user-requested session reset.
    ///
    /// Sequence:
    /// 1. Abort heartbeat + events background tasks against the old
    ///    handle (mirrors `restart` / `shutdown`).
    /// 2. Archive the current `turns.jsonl` into
    ///    `<bot_dir>/archive/turns-<unix-ms>.jsonl` so the new session
    ///    starts on an empty mirror (post-condition the F147 acceptance
    ///    spec asserts).
    /// 3. Clear the **on-disk** `transcript-cursor.json` (V0.6.4 Bug B
    ///    防线 — the new session writes a fresh `<sid>.jsonl` whose
    ///    byte offsets start at 0; leaving the old `prior_offsets`
    ///    around would make the tail loop dedup-skip new content as
    ///    "already seen" the moment Anthropic happens to re-pick a
    ///    historical sid). The **in-memory** OutboundCursor is reset by
    ///    the daemon's tick handler before calling this method (it
    ///    owns the `Arc<OutboundCursor>` via `bot_channels`).
    /// 4. Best-effort `close_thread` on the old handle. We proceed to
    ///    start even on close failure — the tmux session may already
    ///    be dead from a prior crash, which is one of the legitimate
    ///    reasons users hit reset.
    /// 5. Unlink the `signals/reset.signal` file so the next tick
    ///    doesn't loop.
    /// 6. `ensure_started` against a fresh handle.
    ///
    /// Returns the path of the archived `turns.jsonl` (or `None` when
    /// the file didn't exist — fresh bot that received a reset before
    /// any turn was taken).
    pub async fn reset_session(&self) -> Result<Option<PathBuf>> {
        self.abort_background_tasks().await;
        // V0.6.8 F195 — wipe the watchdog deadline. A reset means the
        // user explicitly asked for a fresh session; any pre-reset
        // turn's stall is no longer actionable.
        self.clear_active_turn().await;

        // 2. Archive the existing turns.jsonl.
        let bot_dir = self.bot_dir();
        let turns_path = bot_dir.join("turns.jsonl");
        let archived = if turns_path.exists() {
            let archive_dir = bot_dir.join("archive");
            fs::create_dir_all(&archive_dir)
                .with_context(|| format!("mkdir -p {}", archive_dir.display()))?;
            let unix_ms = chrono::Utc::now().timestamp_millis();
            let dest = archive_dir.join(format!("turns-{unix_ms}.jsonl"));
            fs::rename(&turns_path, &dest).with_context(|| {
                format!("rename {} -> {}", turns_path.display(), dest.display())
            })?;
            tracing::info!(
                slug = %self.reg.workflow_slug,
                role = %self.reg.role,
                from = %turns_path.display(),
                to = %dest.display(),
                "F147 reset: archived turns.jsonl"
            );
            Some(dest)
        } else {
            None
        };

        // 3. Clear the on-disk transcript cursor (V0.6.4 Bug B防线 — see
        // doc-comment above). Best-effort: a missing / corrupt cursor
        // file would have been treated as fresh anyway, so we don't
        // fail the reset for a `remove_file` error here.
        let cursor_file = bot_dir.join("transcript-cursor.json");
        if cursor_file.exists() {
            if let Err(err) = fs::remove_file(&cursor_file) {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    path = %cursor_file.display(),
                    error = %err,
                    "F147 reset: transcript cursor unlink failed (continuing)"
                );
            }
        }

        // Also clear the disk outbound.cursor so a daemon restart between
        // here and the next outbound dispatch starts from 0 too. The
        // in-memory cursor is reset by the daemon-side coordinator (sees
        // `ResetSession` returned from `apply_action` and calls
        // `force_set(0)` on the shared `OutboundCursor` Arc).
        let outbound_cursor_file = bot_dir.join("outbound.cursor");
        if outbound_cursor_file.exists() {
            if let Err(err) = fs::remove_file(&outbound_cursor_file) {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    path = %outbound_cursor_file.display(),
                    error = %err,
                    "F147 reset: outbound cursor unlink failed (continuing)"
                );
            }
        }

        // 4. Close the old handle (best-effort). V0.6.8 F192c — also
        //    clear `fail_count` + `permanent_failure` so reset is a
        //    legitimate recovery path: an operator who fixed the
        //    underlying breakage (config typo, missing binary, dead
        //    tmux session) can re-arm the supervisor by writing
        //    `signals/reset.signal` instead of restarting the daemon.
        //
        //    V0.6.8 F196 — the marker state machine fields
        //    (`marker_missing_count` / `marker_self_heal_attempts` /
        //    `marker_stuck`) are intentionally NOT cleared here. The
        //    F196 self-heal path calls `reset_session` directly, and
        //    clearing the budget mid-heal would prevent the supervisor
        //    from ever latching `marker_stuck` (every reset would
        //    reset the budget too, looping forever). The operator
        //    reset path clears them explicitly via `apply_action`'s
        //    `ResetSession` branch — that's the right place because
        //    only operator intent should re-arm a marker_stuck bot.
        let handle = {
            let mut st = self.state.lock().await;
            st.fail_count = 0;
            st.permanent_failure = false;
            st.handle.take()
        };
        if let Some(h) = handle {
            if let Err(err) = self.adapter.close_thread(&h).await {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    error = %err,
                    "F147 reset: close_thread failed; proceeding to start"
                );
            }
        }

        // 5. Unlink the signal so the next tick doesn't re-trigger.
        let sig = bot_dir.join("signals").join(RESET_SIGNAL);
        if sig.exists() {
            if let Err(err) = fs::remove_file(&sig) {
                tracing::warn!(
                    slug = %self.reg.workflow_slug,
                    role = %self.reg.role,
                    path = %sig.display(),
                    error = %err,
                    "F147 reset: signal unlink failed (next tick may re-trigger)"
                );
            }
        }

        // 6. Fresh start.
        self.ensure_started().await?;
        tracing::info!(
            slug = %self.reg.workflow_slug,
            role = %self.reg.role,
            archived = ?archived.as_ref().map(|p| p.display().to_string()),
            "F147 reset: session restart complete"
        );
        Ok(archived)
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
            SupervisorAction::ResetSession => {
                // V0.6.5 F147 — the daemon's tick handler is responsible
                // for resetting the in-memory `OutboundCursor` (it owns
                // the `Arc<OutboundCursor>` via `bot_channels`) before
                // we get here. This method just covers the bot-side
                // teardown + archive + transcript-cursor wipe.
                self.reset_session().await?;
                // V0.6.8 F196 — operator-driven reset (via
                // `signals/reset.signal`) is the right place to clear
                // the marker self-heal state. `reset_session` itself
                // can't do this because the F196 heal path calls
                // `reset_session` too — clearing there would prevent
                // the budget from ever draining. By scoping the clear
                // to this branch we ensure only an explicit operator
                // intent (or `restart-bot` / daemon restart) re-arms
                // a marker_stuck bot.
                let mut st = self.state.lock().await;
                st.marker_missing_count = 0;
                st.marker_self_heal_attempts = 0;
                st.marker_stuck = false;
            }
            SupervisorAction::Quarantine | SupervisorAction::NoOp => {}
        }
        Ok(action)
    }
}

/// V0.6.8 F196 — bridge the chat-mode adapter's per-tick marker
/// observations into the supervisor's heal state machine.
///
/// The trait impl is intentionally thin: it forwards to
/// [`BotSupervisor::record_marker_missing`] / `record_marker_found`,
/// and on a `Heal` / `PermanentFailure` outcome it recovers an
/// `Arc<Self>` from the stashed Weak (set by
/// `register_as_marker_reporter`) and spawns the async heal task so
/// the tail loop's `report_marker_missing` call returns immediately —
/// the loop must not block while the supervisor archives turns.jsonl,
/// kills tmux, and respawns the session.
///
/// R5 framing: the heal path calls `reset_session`, which is the same
/// code path operators trigger via `signals/reset.signal` and that
/// F192c uses to recover from spawn flakes. Frame is "escalate from
/// a stuck state" — the marker never appearing means the bot is
/// functionally dead and the tmux session, while alive, is silently
/// useless. Recycling it is recovery, not interruption.
#[async_trait]
impl MarkerReporter for BotSupervisor {
    async fn report_marker_missing(&self) {
        let action = self.record_marker_missing().await;
        match action {
            MarkerHealAction::Quiet => {}
            MarkerHealAction::Heal => {
                // Recover Arc<Self> via the stashed Weak so we can
                // tokio::spawn the heal task. If the upgrade fails the
                // supervisor is mid-drop — skip; the next loop tick
                // will see lookup() return None anyway.
                let Some(weak) = self.self_weak.get() else {
                    tracing::warn!(
                        slug = %self.reg.workflow_slug,
                        role = %self.reg.role,
                        "F196: marker reporter fired before register_as_marker_reporter; skipping heal"
                    );
                    return;
                };
                let Some(arc) = weak.upgrade() else {
                    return;
                };
                tokio::spawn(async move {
                    arc.attempt_marker_self_heal().await;
                });
            }
            MarkerHealAction::PermanentFailure => {
                self.record_marker_stuck().await;
            }
        }
    }

    async fn report_marker_found(&self) {
        self.record_marker_found().await;
    }
}

#[cfg(test)]
mod bot_supervisor_tests {
    use super::*;
    use async_trait::async_trait;
    use ccteam_harness::{AgentVendor, ExecutionMode, HarnessError, ThreadEvent, ThreadHandle};
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
            chat_handle: None,
            project_dir: None,
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
        let id = sup.handle_inbound("hello".into(), 0).await.unwrap();
        assert_eq!(id.0, "stub-turn");
        assert_eq!(stub.submits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn handle_inbound_errors_when_not_started() {
        let stub = Arc::new(StubAdapter::default());
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        assert!(sup.handle_inbound("x".into(), 0).await.is_err());
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

    // ---- V0.6.8 F192c — start_thread retry budget + permanent failure ----

    #[tokio::test]
    async fn ensure_started_increments_fail_count_on_spawn_error() {
        let stub = Arc::new(StubAdapter {
            fail_start: true,
            ..StubAdapter::default()
        });
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());

        // First failure: fail_count -> 1, not yet permanent.
        let res = sup.ensure_started().await;
        assert!(res.is_err());
        let st = sup.state_snapshot().await;
        assert_eq!(st.fail_count, 1);
        assert!(!st.permanent_failure);

        // Second failure: fail_count -> 2, still not permanent.
        let res = sup.ensure_started().await;
        assert!(res.is_err());
        let st = sup.state_snapshot().await;
        assert_eq!(st.fail_count, 2);
        assert!(!st.permanent_failure);

        // Third failure: fail_count -> 3, latches permanent_failure.
        let res = sup.ensure_started().await;
        assert!(res.is_err());
        let st = sup.state_snapshot().await;
        assert_eq!(st.fail_count, MAX_START_THREAD_ATTEMPTS);
        assert!(
            st.permanent_failure,
            "expected permanent_failure latch after {MAX_START_THREAD_ATTEMPTS} consecutive fails"
        );

        // After latching, further `ensure_started` calls refuse without
        // invoking the adapter. The stub's call counter should not have
        // ticked past 3 — proving "no more retries" once latched.
        let _ = sup.ensure_started().await;
        assert_eq!(
            stub.starts.load(Ordering::SeqCst),
            MAX_START_THREAD_ATTEMPTS as usize,
            "F192c: latched supervisor MUST NOT invoke adapter.start_thread again"
        );
    }

    #[tokio::test]
    async fn ensure_started_resets_fail_count_on_success() {
        let stub = Arc::new(StubAdapter::default()); // succeeds
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        // Pre-seed fail_count to simulate one earlier transient failure.
        {
            let mut st = sup.state.lock().await;
            st.fail_count = 2;
            assert!(!st.permanent_failure);
        }
        sup.ensure_started().await.unwrap();
        let st = sup.state_snapshot().await;
        assert_eq!(
            st.fail_count, 0,
            "successful start_thread must reset fail_count"
        );
        assert!(!st.permanent_failure);
    }

    #[tokio::test]
    async fn decide_returns_quarantine_when_permanent_failure_latched() {
        // Pure decide() check: state.permanent_failure short-circuits
        // to Quarantine regardless of handle/heartbeat presence, so
        // the daemon stops re-calling Spawn on a flap-locked bot.
        let tmp = TempDir::new().unwrap();
        let r = reg();
        let st = BotState {
            permanent_failure: true,
            fail_count: MAX_START_THREAD_ATTEMPTS,
            ..Default::default()
        };
        let action = decide(tmp.path(), &r, &st, SystemTime::now());
        assert_eq!(action, SupervisorAction::Quarantine);
    }

    #[tokio::test]
    async fn reset_session_clears_permanent_failure() {
        // F192c — `signals/reset.signal` is the operator's recovery
        // path. reset_session must wipe both fail_count and the
        // permanent_failure latch so the next tick can attempt
        // start_thread again (now that the operator presumably fixed
        // whatever broke the spawn).
        let stub = Arc::new(StubAdapter::default()); // will succeed on reset
        let tmp = TempDir::new().unwrap();
        let sup = BotSupervisor::new(reg(), tmp.path(), stub.clone());
        {
            let mut st = sup.state.lock().await;
            st.fail_count = MAX_START_THREAD_ATTEMPTS;
            st.permanent_failure = true;
        }
        sup.reset_session().await.unwrap();
        let st = sup.state_snapshot().await;
        assert_eq!(st.fail_count, 0);
        assert!(!st.permanent_failure);
        assert_eq!(
            stub.starts.load(Ordering::SeqCst),
            1,
            "reset_session ends with one ensure_started call"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::{AgentVendor, ExecutionMode, ThreadHandle};
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
            chat_handle: None,
            project_dir: None,
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
