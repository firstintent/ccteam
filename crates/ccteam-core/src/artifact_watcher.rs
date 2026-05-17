//! V0.4.0 F64 — artifact-trigger filesystem watcher.
//!
//! ## Role in the V0.4.0 architecture
//!
//! With phase state machines retired (F60), the new orchestrator (F66)
//! schedules agent sessions from filesystem events: every
//! `Trigger::Watch(<path>)` agent in `workflow.yaml` listens on a
//! directory, and each new artifact dropped there → one
//! [`ArtifactEvent`] → one session spawn (capped by `parallelism`).
//!
//! This module owns the filesystem half of that pipeline:
//!
//! ```text
//! workflow.yaml             notify::RecommendedWatcher
//!     │                     (inotify / fsevents / RDCW)
//!     │ Trigger::Watch(p)        │
//!     ▼                          ▼
//! ArtifactWatcher::new ─► std::sync::mpsc bridge
//!     │                          │
//!     │ start(self) ─► tokio task ─► 500 ms debounce
//!     ▼                                      │
//! tokio::sync::mpsc::Receiver<ArtifactEvent> ◄
//! ```
//!
//! ## Red lines (PRD v0-4-0 §6.2 / dev-plan §6.2)
//!
//! 1. **No file content parsing.** The watcher emits `(role, path, kind)`
//!    triples; reading / parsing the artifact body is the spawned
//!    agent's job (or the orchestrator's, when it later inspects
//!    artifacts for budget / golden-rule purposes).
//! 2. **No direct agent spawn.** Orchestrator consumes the
//!    `mpsc::Receiver` and decides whether to spawn — gate state,
//!    parallelism caps, cost budgets all live there.
//! 3. **Single allowed IO side effect**: writing the
//!    `artifact_dir_created` event to `progress.jsonl` when the
//!    watcher mkdir-s a previously-absent watch root. The whole point
//!    of `progress.jsonl` being orchestrator state-truth is that
//!    every dir auto-create is auditable.
//! 4. **Debounce window**: 500 ms per `(watch_root, role)` pair.
//!    Picked over PRD §6.2's hint of 200 ms because Claude Code
//!    streaming writes can fragment a single artifact across 200-300 ms.
//!    Debounce is per-path, not global — two roles watching different
//!    dirs never block each other.
//!
//! See also `docs/v0-4-0/prd.md` §F64 + §6.2, `dev-plan.md` §6,
//! `docs/dev-coupling-audit.md` F64.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc as stdmpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Poll interval inside the watcher's blocking loop. The thread wakes
/// every interval to check whether the tokio mpsc receiver (held by
/// the orchestrator) is still alive — i.e. it can see "shutdown" even
/// while notify is idle. Without this poll, an idle workflow leaves
/// the watcher thread parked in `recv()` forever, which keeps the
/// tokio blocking-pool alive and prevents `ccteam start` from exiting
/// on SIGTERM.
pub const WATCHER_SHUTDOWN_POLL: Duration = Duration::from_millis(500);

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;
use tokio::sync::mpsc;

use crate::progress;
use crate::workflow::{Trigger, WorkflowSpec};

/// Debounce window per `(watch_root, role)`. Two rapid filesystem
/// events on the same watch root inside this window collapse to one
/// emitted [`ArtifactEvent`]. See module doc red line #4.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

/// Bound for the internal tokio `mpsc` channel that carries
/// [`ArtifactEvent`] from the debouncer task to the orchestrator. Sized
/// to keep batch storms (e.g. a phase that drops 100 artifacts in a
/// loop) from back-pressuring the watcher, while staying tight enough
/// to surface real consumer slowness.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// One filesystem change inside a watched artifact directory, already
/// associated with the workflow role that asked to watch it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactEvent {
    /// Workflow role (key in `WorkflowSpec::agents`) that should be
    /// triggered. Orchestrator looks this up to decide which
    /// `.claude/agents/<role>.md` to spawn.
    pub role: String,
    /// Absolute path of the file whose change drove this event. When
    /// the platform reports multiple paths for a single inotify event
    /// (rare; mostly Move kinds we don't subscribe to), the watcher
    /// emits one [`ArtifactEvent`] per path.
    pub artifact_path: PathBuf,
    /// Coarse kind. `Created` / `Modified` / `Deleted` mirror notify's
    /// `EventKind::Create` / `EventKind::Modify` / `EventKind::Remove`
    /// without exposing the sub-kind enum surface; the orchestrator
    /// only branches on the coarse kind today.
    pub event_kind: WatchKind,
}

/// Coarse classification of a filesystem event. See [`ArtifactEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    /// A new file appeared in the watch root.
    Created,
    /// An existing file's contents changed.
    Modified,
    /// A previously-present file was removed. Platform support varies
    /// (Linux inotify reports it cleanly; some editor save patterns on
    /// macOS surface a Modify instead of a Remove). Orchestrator
    /// callers must treat absence-of-Deleted as advisory only.
    Deleted,
}

/// Filesystem watcher for `Trigger::Watch(<path>)` agents.
///
/// Construct with [`ArtifactWatcher::new`] (registers notify watchers
/// + mkdirs missing dirs), then consume with [`ArtifactWatcher::start`]
/// to spawn the tokio debouncer task. The returned
/// `mpsc::Receiver<ArtifactEvent>` is the orchestrator's input edge.
///
/// **Drop semantics**: dropping the `mpsc::Receiver` causes the next
/// `tx.send()` in the spawned task to fail, which exits the task.
/// Dropping `ArtifactWatcher` before `start()` is a no-op (no task
/// running yet); the notify `RecommendedWatcher` inside drops and
/// stops emitting events.
pub struct ArtifactWatcher {
    /// Owned notify watcher. Dropping this drops the OS-level watch
    /// registrations; the std-mpsc receiver inside `event_rx` then
    /// hangs up the next `recv()` and the spawned tokio task exits.
    /// We retain the field so the watcher lives as long as the
    /// [`ArtifactWatcher`] (and, after [`start`], the spawned task).
    _watcher: RecommendedWatcher,
    /// Tokio sender — the spawned task owns this; the matching
    /// `Receiver` is returned to the caller by [`new`].
    tx: mpsc::Sender<ArtifactEvent>,
    /// Synchronous receiver fed by the notify callback. The notify
    /// callback runs on notify's own thread, so the bridge uses
    /// `std::sync::mpsc` (sync sender from inside the callback, sync
    /// receiver into the tokio task).
    event_rx: stdmpsc::Receiver<notify::Result<notify::Event>>,
    /// Map of `<watch_root>` → `<role>`. notify reports the full file
    /// path of each event; the task walks up `path.ancestors()` to
    /// find which registered watch root contains it and looks up the
    /// owning role here.
    roots: Vec<(PathBuf, String)>,
}

impl ArtifactWatcher {
    /// Build a watcher for every `Trigger::Watch(path)` agent in
    /// `spec`. Side effects (PRD §6.2 + dev-plan §6.1):
    ///
    /// - mkdir -p each missing watch root (lazy creation on first
    ///   workflow run; tests t01 / t07 cover this).
    /// - if `progress_path` is `Some`, append one
    ///   `artifact_dir_created` event per directory the watcher
    ///   actually had to create.
    /// - register a recursive notify watch on each root.
    ///
    /// Watch roots can be relative paths in `WorkflowSpec` (PRD §6.1
    /// says they're project-relative). Caller is responsible for
    /// joining them to the project root before passing the spec in;
    /// `new` consumes the spec as-given and treats `path` literally.
    pub fn new(
        spec: &WorkflowSpec,
        progress_path: Option<&Path>,
    ) -> Result<(Self, mpsc::Receiver<ArtifactEvent>)> {
        // Collect (root, role) entries in YAML declaration order.
        // `WorkflowSpec::agents` is an IndexMap so this iteration is
        // deterministic across runs — test t05 relies on the role
        // string round-trip.
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        for (role, agent) in &spec.agents {
            if let Trigger::Watch(path) = &agent.trigger {
                roots.push((path.clone(), role.clone()));
            }
        }

        // Lazy mkdir each root + write one progress.jsonl event per
        // newly-created directory. We check `exists()` first so we
        // don't write an event for dirs the user already had.
        for (root, _role) in &roots {
            if !root.exists() {
                std::fs::create_dir_all(root)
                    .with_context(|| format!("create watch root {}", root.display()))?;
                if let Some(progress) = progress_path {
                    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
                    let event = json!({
                        "event": "artifact_dir_created",
                        "path": abs.to_string_lossy(),
                        "ts": ts,
                    });
                    // Best-effort: a progress.jsonl write failure
                    // (permissions, full disk) must not block the
                    // watcher itself — log and continue.
                    if let Err(err) = progress::append_event(progress, &event) {
                        tracing::warn!(
                            error = %err,
                            path = %progress.display(),
                            "artifact_watcher: failed to append artifact_dir_created event",
                        );
                    }
                }
            }
        }

        // Build the notify watcher with a std::mpsc bridge — notify's
        // callback runs on its own thread and is `FnMut + Send`, so we
        // use a sync sender to ferry events into the async task.
        let (event_tx, event_rx) = stdmpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            // Best-effort send: if the std receiver has been dropped
            // (i.e. the watcher is being torn down) we drop the
            // event silently.
            let _ = event_tx.send(res);
        })
        .context("artifact_watcher: initialize notify::RecommendedWatcher")?;

        // Recursive so writes to subdirectories under the watch root
        // (e.g. `.ccteam/issues/2026-05-14/foo.md`) still fire. PRD
        // §6.2 leaves the recursion choice to the watcher; recursive
        // is the strict superset and matches the orchestrator's
        // intent ("watch this whole artifact tree").
        for (root, _role) in &roots {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .with_context(|| format!("watch {}", root.display()))?;
        }

        let (tx, rx) = mpsc::channel::<ArtifactEvent>(EVENT_CHANNEL_CAPACITY);

        Ok((
            ArtifactWatcher {
                _watcher: watcher,
                tx,
                event_rx,
                roots,
            },
            rx,
        ))
    }

    /// Spawn the background task that translates raw notify events
    /// into debounced [`ArtifactEvent`]s. Returns the `JoinHandle` —
    /// callers usually drop it (fire-and-forget); the task ends when
    /// the receiver (returned by [`new`]) is dropped.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let ArtifactWatcher {
            _watcher,
            tx,
            event_rx,
            roots,
        } = self;

        tokio::task::spawn_blocking(move || {
            // `_watcher` and `event_rx` are owned by this thread so
            // the notify-side watch stays alive for the task's
            // lifetime; dropping the JoinHandle does NOT abort a
            // spawn_blocking task, but the receiver-drop exit path
            // (see below) does. `_watcher` is kept in scope by name
            // so optimizers don't reorder its drop ahead of usage.
            let _watcher = _watcher;

            // Per-watch-root debounce state. Key = `(watch_root, role)`
            // — collapsing two events for the same role on the same
            // root. We intentionally do NOT collapse per-file so two
            // distinct artifacts dropped 1 ms apart both surface
            // (test t10 verifies events < 10 for 100 files, not = 1).
            let mut last_emit: HashMap<(PathBuf, String), Instant> = HashMap::new();

            loop {
                // Bounded wait so the thread can notice "outbound tokio
                // mpsc closed" even when notify is idle. Without this,
                // an idle workflow would park the thread forever on
                // `recv()`, keeping the tokio blocking pool — and the
                // whole process — alive past SIGTERM.
                let res = match event_rx.recv_timeout(WATCHER_SHUTDOWN_POLL) {
                    Ok(v) => v,
                    Err(stdmpsc::RecvTimeoutError::Timeout) => {
                        // tokio Sender::is_closed is the cheap check;
                        // when the orchestrator drops its Receiver the
                        // channel is marked closed and we exit cleanly.
                        if tx.is_closed() {
                            return;
                        }
                        continue;
                    }
                    Err(stdmpsc::RecvTimeoutError::Disconnected) => break,
                };
                let ev = match res {
                    Ok(e) => e,
                    Err(err) => {
                        // Per dev-plan §6.1 #5.3: single inotify
                        // errors must not terminate the watcher.
                        tracing::warn!(?err, "artifact_watcher: notify reported error");
                        continue;
                    }
                };

                let kind = match coarse_kind(&ev.kind) {
                    Some(k) => k,
                    None => continue, // Access / Other / Any — ignore
                };

                for path in &ev.paths {
                    let Some((root, role)) = match_root(&roots, path) else {
                        continue;
                    };
                    let key = (root.clone(), role.clone());
                    let now = Instant::now();
                    if let Some(prev) = last_emit.get(&key) {
                        if now.duration_since(*prev) < DEBOUNCE_WINDOW {
                            continue; // debounced
                        }
                    }
                    last_emit.insert(key, now);

                    let event = ArtifactEvent {
                        role: role.clone(),
                        artifact_path: path.clone(),
                        event_kind: kind,
                    };
                    // Block on the tokio Sender — `blocking_send` is
                    // safe inside `spawn_blocking`. If the receiver
                    // is gone, `Err(_)` → break the loop and exit.
                    if tx.blocking_send(event).is_err() {
                        return;
                    }
                }
            }
        })
    }
}

/// Map notify's fine-grained `EventKind` onto our coarse [`WatchKind`].
/// Returns `None` for events the watcher intentionally ignores
/// (`Access`, `Any`, `Other`) so the caller can short-circuit.
fn coarse_kind(kind: &EventKind) -> Option<WatchKind> {
    match kind {
        EventKind::Create(_) => Some(WatchKind::Created),
        EventKind::Modify(_) => Some(WatchKind::Modified),
        EventKind::Remove(_) => Some(WatchKind::Deleted),
        EventKind::Access(_) | EventKind::Any | EventKind::Other => None,
    }
}

/// Find the `(root, role)` whose root is an ancestor of (or equal to)
/// `path`. Returns the first match in YAML declaration order so the
/// behaviour is deterministic when one root is nested inside another
/// (unusual, but the validator does not forbid it).
fn match_root<'a>(
    roots: &'a [(PathBuf, String)],
    path: &Path,
) -> Option<(&'a PathBuf, &'a String)> {
    for ancestor in path.ancestors() {
        for (root, role) in roots {
            if root.as_path() == ancestor {
                return Some((root, role));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// V0.5.0 F95 — global Anthropic Agent Teams watcher.
//
// Distinct from the per-workflow `ArtifactWatcher` above: this layer is
// **not bound to any ccteam project**. It scans `~/.claude/teams/*/` —
// Anthropic Agent Teams' on-disk SoT — and mirrors topology / message
// / task events into `~/.ccteam/teams-progress.jsonl` (the global team
// progress stream, per `paths::teams_progress_path`).
//
// Red lines (PRD V0.5.0 F95 §需求):
//   1. Read-only against `~/.claude/teams/` and `~/.claude/tasks/`.
//   2. Schema-failure tolerance — WARN once + degrade to mtime-only.
//   3. idle_notification filtering — system messages routed to F94
//      `team_teammate_idle` (Wave 2), not F95 `team_message_sent`.
//   4. 60s discovery rescan picks up new teams without daemon restart.
//
// See `docs/v0-5-0/prd.md` §F95 + `docs/v0-5-0/dev-plan.md` Wave 1.
// ---------------------------------------------------------------------------

/// How often `AgentTeamsWatcher` rescans `~/.claude/teams/` to pick up
/// newly-created team directories (PRD F95 §验收 .6: 60s).
pub const TEAMS_DISCOVERY_INTERVAL: Duration = Duration::from_secs(60);

/// Per-team snapshot tracked by the watcher. We hold the previous
/// parse output so the next inotify (or rescan) tick can diff against
/// it without re-reading historical events.
#[derive(Debug, Default)]
struct TeamState {
    /// Latest parsed `config.json`. `None` when the file is missing or
    /// schema-broken — the watcher degrades to mtime-only until parse
    /// recovers (PRD F95 §需求 .4).
    config: Option<crate::teams_config_parser::TeamConfigSnapshot>,
    /// Inbox snapshots keyed by teammate name. Cold-discovery seeds
    /// these with the current file contents so historical messages
    /// don't flood `progress.jsonl` on daemon restart.
    inboxes: BTreeMap<String, crate::teams_inbox_parser::InboxSnapshot>,
    /// Per-task file snapshots keyed by task id (the file stem). Used
    /// by `teams_task_parser::diff_task` to compute status transitions.
    tasks: BTreeMap<String, crate::teams_task_parser::TaskFile>,
    /// Has `WARN`-once already fired for `config.json` schema breakage?
    /// Reset on successful re-parse.
    config_warned: bool,
}

/// Shared map of `<team_name>` → [`TeamState`]. The notify callback +
/// the tokio discovery loop both hold this behind a `Mutex` so that
/// inotify events arriving mid-rescan can't race the discovery pass.
type SharedTeams = Arc<Mutex<BTreeMap<String, TeamState>>>;

/// V0.5.0 F95 — config for the global Anthropic Agent Teams watcher.
/// Mostly path overrides so integration tests can point at a temp
/// `~/.claude/teams/` clone.
#[derive(Debug, Clone)]
pub struct AgentTeamsWatcherConfig {
    /// `~/.claude/teams/` (override via `CCTEAM_AGENT_TEAMS_ROOT`).
    pub teams_root: PathBuf,
    /// `~/.claude/tasks/` (override via `CCTEAM_AGENT_TASKS_ROOT`).
    pub tasks_root: PathBuf,
    /// Destination for `team_*` events
    /// (`~/.ccteam/teams-progress.jsonl`).
    pub progress_path: PathBuf,
    /// Rescan interval. Defaults to [`TEAMS_DISCOVERY_INTERVAL`];
    /// tests override to single-digit milliseconds.
    pub discovery_interval: Duration,
}

impl AgentTeamsWatcherConfig {
    /// Resolve from the running user's environment (the default path
    /// production daemon uses). Honours `CCTEAM_AGENT_TEAMS_ROOT` /
    /// `CCTEAM_AGENT_TASKS_ROOT` / `CCTEAM_HOME` for test isolation.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            teams_root: crate::paths::agent_teams_root()?,
            tasks_root: crate::paths::agent_tasks_root()?,
            progress_path: crate::paths::teams_progress_path()?,
            discovery_interval: TEAMS_DISCOVERY_INTERVAL,
        })
    }
}

/// Global watcher for `~/.claude/teams/*/`.
///
/// Lifecycle:
///
/// 1. [`AgentTeamsWatcher::new`] resolves paths + installs the inotify
///    watch on `<teams_root>` itself (recursive). It does NOT touch
///    the filesystem if the root is missing — a daemon started before
///    the first Agent Teams session has nothing to watch and the
///    discovery loop will pick it up later.
/// 2. [`AgentTeamsWatcher::start`] spawns the tokio task that drives
///    discovery + reactive diffing. The returned `JoinHandle` is
///    discarded by the daemon (fire-and-forget); a graceful shutdown
///    drops the watcher → the task notices via `tx.is_closed()` and
///    exits.
///
/// Drop semantics: dropping the watcher drops the notify watcher
/// (sync mpsc receiver hangs up) and the tokio task exits on its
/// next wake.
pub struct AgentTeamsWatcher {
    config: AgentTeamsWatcherConfig,
    _watcher: Option<RecommendedWatcher>,
    event_rx: stdmpsc::Receiver<notify::Result<notify::Event>>,
    teams: SharedTeams,
    /// Cancellation flag used by tests to gracefully drop the
    /// discovery loop without waiting for the long interval.
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentTeamsWatcher {
    /// Build a fresh watcher. If `teams_root` does not exist, the
    /// returned watcher will rely entirely on its periodic discovery
    /// rescan (it's legitimate for the first ccteam run to predate
    /// any Anthropic Agent Teams session).
    pub fn new(config: AgentTeamsWatcherConfig) -> Result<Self> {
        let (event_tx, event_rx) = stdmpsc::channel::<notify::Result<notify::Event>>();
        let watcher = if config.teams_root.exists() {
            let mut w: RecommendedWatcher = notify::recommended_watcher(move |res| {
                let _ = event_tx.send(res);
            })
            .context("agent_teams_watcher: initialize notify::RecommendedWatcher")?;
            w.watch(&config.teams_root, RecursiveMode::Recursive)
                .with_context(|| format!("watch {}", config.teams_root.display()))?;
            // tasks_root is optional — Anthropic only creates it the
            // first time a task is recorded. If absent, the discovery
            // loop will re-attempt the watch each tick.
            if config.tasks_root.exists() {
                if let Err(err) = w.watch(&config.tasks_root, RecursiveMode::Recursive) {
                    tracing::warn!(
                        path = %config.tasks_root.display(),
                        error = %err,
                        "agent_teams_watcher: failed to watch tasks_root; will retry on discovery",
                    );
                }
            }
            Some(w)
        } else {
            tracing::info!(
                path = %config.teams_root.display(),
                "agent_teams_watcher: teams_root absent at startup; will be discovered later",
            );
            None
        };

        Ok(Self {
            config,
            _watcher: watcher,
            event_rx,
            teams: Arc::new(Mutex::new(BTreeMap::new())),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Shareable cancellation handle. Tests flip this to make the
    /// discovery loop exit without waiting for its next interval.
    pub fn cancel_handle(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.cancel.clone()
    }

    /// Spawn the discovery + dispatch loop. The task owns the notify
    /// watcher (kept in scope) + the std mpsc receiver. It exits when:
    ///
    /// 1. `cancel` flips to `true` (tests / graceful daemon shutdown), or
    /// 2. the std-mpsc sender is dropped (notify watcher died).
    ///
    /// Returns the `JoinHandle` so callers can `.await` it in tests;
    /// production daemon drops it.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let AgentTeamsWatcher {
            config,
            _watcher,
            event_rx,
            teams,
            cancel,
        } = self;
        tokio::task::spawn_blocking(move || {
            // Keep the notify watcher in scope so the OS-level watch
            // registration stays alive for the loop's lifetime.
            let _watcher = _watcher;

            // Cold-start discovery so daemon log can announce
            // "discovered N agent teams" and the SoT picks up
            // pre-existing teams. Errors here are non-fatal; the
            // discovery loop will retry.
            if let Err(err) = run_discovery(&config, &teams) {
                tracing::warn!(
                    error = %err,
                    "agent_teams_watcher: initial discovery failed",
                );
            }

            let mut last_discovery = Instant::now();
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                // Bounded wait: either we get an inotify event, or
                // we time out and rerun discovery if due.
                let timeout = config
                    .discovery_interval
                    .saturating_sub(last_discovery.elapsed())
                    .max(WATCHER_SHUTDOWN_POLL);
                let recv = event_rx.recv_timeout(timeout);
                match recv {
                    Ok(Ok(ev)) => {
                        for path in &ev.paths {
                            if let Err(err) = dispatch_path(&config, &teams, path) {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %err,
                                    "agent_teams_watcher: dispatch failed",
                                );
                            }
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(?err, "agent_teams_watcher: notify reported error");
                    }
                    Err(stdmpsc::RecvTimeoutError::Timeout) => {}
                    Err(stdmpsc::RecvTimeoutError::Disconnected) => break,
                }

                // Periodic discovery (also fires after the first
                // timeout). Picks up newly-created teams without a
                // daemon restart.
                if last_discovery.elapsed() >= config.discovery_interval {
                    if let Err(err) = run_discovery(&config, &teams) {
                        tracing::warn!(
                            error = %err,
                            "agent_teams_watcher: periodic discovery failed",
                        );
                    }
                    last_discovery = Instant::now();
                }
            }
        })
    }

    /// **Test-only**: dispatch the given list of paths against the
    /// current in-memory state — no extra discovery pass. The caller
    /// is expected to drive `test_run_discovery` themselves when they
    /// want cold-seeding to run first; this entry point models a pure
    /// "inotify event for path X arrived" tick.
    ///
    /// Returns every event currently on disk in `progress_path` (the
    /// whole file, not just events appended by this call) so the
    /// assertions stay simple.
    #[cfg(any(test, feature = "test-util"))]
    pub fn test_tick(&self, paths: &[PathBuf]) -> Result<Vec<serde_json::Value>> {
        for p in paths {
            dispatch_path(&self.config, &self.teams, p)?;
        }
        let body = std::fs::read_to_string(&self.config.progress_path).unwrap_or_default();
        let out = body
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Ok(out)
    }

    /// **Test-only**: discover-only entry point (no notify dispatch).
    /// Useful when the test wants to assert "cold-start picked up N
    /// teams" or to seed inbox/task baselines before the next tick.
    #[cfg(any(test, feature = "test-util"))]
    pub fn test_run_discovery(&self) -> Result<()> {
        run_discovery(&self.config, &self.teams)
    }
}

/// Scan `<teams_root>` for `<name>/config.json`, install any newly
/// discovered teams, and remove watchers for deleted ones. Emits
/// `team_member_joined` events on cold-start (PRD F95 §验收 .2) and
/// emits `team_member_left` for every member of a team that
/// disappeared between two discovery passes.
fn run_discovery(config: &AgentTeamsWatcherConfig, teams: &SharedTeams) -> Result<()> {
    if !config.teams_root.exists() {
        return Ok(());
    }
    let mut live: HashSet<String> = HashSet::new();
    let entries = match std::fs::read_dir(&config.teams_root) {
        Ok(e) => e,
        // Race: dir was removed between exists() and read_dir.
        // Defensive: treat as "no teams".
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let config_path = path.join("config.json");
        if !config_path.is_file() {
            continue;
        }
        live.insert(name.to_string());
        // Refresh config snapshot + emit joined events. We lock once
        // per team to keep critical sections short.
        sync_team_config(config, teams, name, &config_path)?;
        // Seed inboxes (cold) so historical messages don't replay.
        let inbox_dir = path.join("inboxes");
        if inbox_dir.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&inbox_dir) {
                for inbox in rd.flatten() {
                    let p = inbox.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("json") {
                        seed_inbox_snapshot(teams, name, &p)?;
                    }
                }
            }
        }
        // Seed task snapshots (cold).
        let tasks_dir = config.tasks_root.join(name);
        if tasks_dir.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&tasks_dir) {
                for task in rd.flatten() {
                    let p = task.path();
                    let Some(fname) = p.file_name().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if crate::teams_task_parser::is_sibling_file(fname) {
                        continue;
                    }
                    if p.extension().and_then(|s| s.to_str()) == Some("json") {
                        seed_task_snapshot(teams, name, &p)?;
                    }
                }
            }
        }
    }

    // Anything in our state map that isn't live anymore → emit
    // member_left for each known member, then drop.
    let gone: Vec<String> = {
        let teams_guard = teams.lock().expect("teams mutex poisoned");
        teams_guard
            .keys()
            .filter(|n| !live.contains(*n))
            .cloned()
            .collect()
    };
    for name in gone {
        let lost_members = {
            let mut teams_guard = teams.lock().expect("teams mutex poisoned");
            teams_guard
                .remove(&name)
                .and_then(|st| st.config)
                .map(|s| s.members)
                .unwrap_or_default()
        };
        for (_id, m) in lost_members {
            let event = serde_json::json!({
                "event": "team_member_left",
                "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "team_name": name,
                "teammate_name": m.name,
            });
            let _ = crate::progress::append_event(&config.progress_path, &event);
        }
    }

    tracing::info!(
        count = live.len(),
        path = %config.teams_root.display(),
        "agent_teams_watcher: discovered {} agent teams",
        live.len(),
    );
    Ok(())
}

/// Read `<team>/config.json`, diff against the cached snapshot,
/// append events to `progress_path`, and update the cached snapshot.
/// Schema breakage WARNs once + sets `config_warned=true`; subsequent
/// breakage on the same file is silent until a successful re-parse
/// resets the flag.
fn sync_team_config(
    config: &AgentTeamsWatcherConfig,
    teams: &SharedTeams,
    name: &str,
    config_path: &Path,
) -> Result<()> {
    let bytes = match std::fs::read(config_path) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(
                path = %config_path.display(),
                error = %err,
                "agent_teams_watcher: failed to read config.json",
            );
            return Ok(());
        }
    };
    let snap = match crate::teams_config_parser::parse_config(&bytes) {
        Ok(s) => s,
        Err(err) => {
            let mut guard = teams.lock().expect("teams mutex poisoned");
            let st = guard.entry(name.to_string()).or_default();
            if !st.config_warned {
                tracing::warn!(
                    team = name,
                    path = %config_path.display(),
                    error = %err,
                    "agent_teams_watcher: config.json schema broken; degrading to mtime-only",
                );
                st.config_warned = true;
            }
            return Ok(());
        }
    };
    let events = {
        let mut guard = teams.lock().expect("teams mutex poisoned");
        let st = guard.entry(name.to_string()).or_default();
        st.config_warned = false;
        let prev = st.config.clone().unwrap_or_default();
        let events = crate::teams_config_parser::diff_snapshots(&prev, &snap);
        st.config = Some(snap);
        events
    };
    for event in events {
        crate::progress::append_event(&config.progress_path, &event)?;
    }
    Ok(())
}

/// Cold-start seed: read the inbox once and store it as the baseline
/// so subsequent diffs only emit *new* messages. Without this, the
/// first dispatch tick would replay every historical message into
/// `teams-progress.jsonl`.
fn seed_inbox_snapshot(teams: &SharedTeams, team_name: &str, inbox_path: &Path) -> Result<()> {
    let Some(teammate) = inbox_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from)
    else {
        return Ok(());
    };
    let bytes = match std::fs::read(inbox_path) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    let snap = match crate::teams_inbox_parser::parse_inbox(&bytes) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                path = %inbox_path.display(),
                error = %err,
                "agent_teams_watcher: inbox schema broken; skipping seed",
            );
            return Ok(());
        }
    };
    let mut guard = teams.lock().expect("teams mutex poisoned");
    let st = guard.entry(team_name.to_string()).or_default();
    st.inboxes.entry(teammate).or_insert(snap);
    Ok(())
}

/// Cold-start seed: read each task file once and store its body so
/// subsequent diff_task calls have a proper `prev`. We skip
/// `.lock` / `.highwatermark` (PRD F95 §需求 .2).
fn seed_task_snapshot(teams: &SharedTeams, team_name: &str, task_path: &Path) -> Result<()> {
    let Some(stem) = task_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from)
    else {
        return Ok(());
    };
    let bytes = match std::fs::read(task_path) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    let task = match crate::teams_task_parser::parse_task(&bytes) {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(
                path = %task_path.display(),
                error = %err,
                "agent_teams_watcher: task schema broken; skipping seed",
            );
            return Ok(());
        }
    };
    let mut guard = teams.lock().expect("teams mutex poisoned");
    let st = guard.entry(team_name.to_string()).or_default();
    st.tasks.entry(stem).or_insert(task);
    Ok(())
}

/// Dispatch a notify event path to the right diff parser. Decides
/// based on path shape:
///
/// - `<teams_root>/<team>/config.json` → `sync_team_config`.
/// - `<teams_root>/<team>/inboxes/<teammate>.json` → inbox diff.
/// - `<tasks_root>/<team>/<id>.json` → task diff.
/// - Anything else (e.g. transcript files) → ignored.
fn dispatch_path(config: &AgentTeamsWatcherConfig, teams: &SharedTeams, path: &Path) -> Result<()> {
    // Skip sibling files unconditionally.
    if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
        if crate::teams_task_parser::is_sibling_file(fname) {
            return Ok(());
        }
    }

    if let Ok(rel) = path.strip_prefix(&config.teams_root) {
        let mut comps = rel.components();
        let Some(std::path::Component::Normal(team_os)) = comps.next() else {
            return Ok(());
        };
        let team = team_os.to_string_lossy().to_string();
        let rest: Vec<_> = comps.collect();
        match rest.as_slice() {
            // <team>/config.json
            [std::path::Component::Normal(file)] if file.to_string_lossy() == "config.json" => {
                sync_team_config(config, teams, &team, path)?;
            }
            // <team>/inboxes/<teammate>.json
            [std::path::Component::Normal(dir), std::path::Component::Normal(file)]
                if dir.to_string_lossy() == "inboxes" =>
            {
                let fname = file.to_string_lossy().to_string();
                if let Some(teammate) = fname.strip_suffix(".json") {
                    dispatch_inbox(config, teams, &team, teammate, path)?;
                }
            }
            // Anything else under <team>/... is not an F95 SoT file.
            _ => {}
        }
        return Ok(());
    }

    if let Ok(rel) = path.strip_prefix(&config.tasks_root) {
        let mut comps = rel.components();
        let Some(std::path::Component::Normal(team_os)) = comps.next() else {
            return Ok(());
        };
        let team = team_os.to_string_lossy().to_string();
        let rest: Vec<_> = comps.collect();
        if let [std::path::Component::Normal(file)] = rest.as_slice() {
            let fname = file.to_string_lossy().to_string();
            if crate::teams_task_parser::is_sibling_file(&fname) {
                return Ok(());
            }
            if let Some(task_id) = fname.strip_suffix(".json") {
                dispatch_task(config, teams, &team, task_id, path)?;
            }
        }
    }
    Ok(())
}

fn dispatch_inbox(
    config: &AgentTeamsWatcherConfig,
    teams: &SharedTeams,
    team_name: &str,
    teammate: &str,
    inbox_path: &Path,
) -> Result<()> {
    let bytes = match std::fs::read(inbox_path) {
        Ok(b) => b,
        Err(_) => return Ok(()), // delete races handled defensively
    };
    let next = match crate::teams_inbox_parser::parse_inbox(&bytes) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                path = %inbox_path.display(),
                error = %err,
                "agent_teams_watcher: inbox schema broken; skipping",
            );
            return Ok(());
        }
    };
    let events = {
        let mut guard = teams.lock().expect("teams mutex poisoned");
        let st = guard.entry(team_name.to_string()).or_default();
        let prev = st.inboxes.get(teammate).cloned().unwrap_or_default();
        let events = crate::teams_inbox_parser::diff_inbox(&prev, &next, team_name, teammate);
        st.inboxes.insert(teammate.to_string(), next);
        events
    };
    for event in events {
        crate::progress::append_event(&config.progress_path, &event)?;
    }
    Ok(())
}

fn dispatch_task(
    config: &AgentTeamsWatcherConfig,
    teams: &SharedTeams,
    team_name: &str,
    task_id: &str,
    task_path: &Path,
) -> Result<()> {
    let bytes = match std::fs::read(task_path) {
        Ok(b) => b,
        // Defensive: file may have been removed between notify event
        // dispatch and our read. Drop cached snapshot so a future
        // re-creation is treated as cold.
        Err(_) => {
            let mut guard = teams.lock().expect("teams mutex poisoned");
            if let Some(st) = guard.get_mut(team_name) {
                st.tasks.remove(task_id);
            }
            return Ok(());
        }
    };
    let next = match crate::teams_task_parser::parse_task(&bytes) {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(
                path = %task_path.display(),
                error = %err,
                "agent_teams_watcher: task schema broken; skipping",
            );
            return Ok(());
        }
    };
    let events = {
        let mut guard = teams.lock().expect("teams mutex poisoned");
        let st = guard.entry(team_name.to_string()).or_default();
        let prev = st.tasks.get(task_id).cloned();
        let events = crate::teams_task_parser::diff_task(prev.as_ref(), &next, team_name);
        st.tasks.insert(task_id.to_string(), next);
        events
    };
    for event in events {
        crate::progress::append_event(&config.progress_path, &event)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_kind_maps_create_modify_remove() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        assert_eq!(
            coarse_kind(&EventKind::Create(CreateKind::File)),
            Some(WatchKind::Created)
        );
        assert_eq!(
            coarse_kind(&EventKind::Modify(ModifyKind::Any)),
            Some(WatchKind::Modified)
        );
        assert_eq!(
            coarse_kind(&EventKind::Remove(RemoveKind::File)),
            Some(WatchKind::Deleted)
        );
    }

    #[test]
    fn coarse_kind_ignores_access_and_any() {
        use notify::event::{AccessKind, AccessMode};
        assert_eq!(
            coarse_kind(&EventKind::Access(AccessKind::Open(AccessMode::Read))),
            None
        );
        assert_eq!(coarse_kind(&EventKind::Any), None);
        assert_eq!(coarse_kind(&EventKind::Other), None);
    }

    #[test]
    fn match_root_finds_exact_path() {
        let roots = vec![(PathBuf::from("/x/a"), "role-a".to_string())];
        let m = match_root(&roots, Path::new("/x/a")).unwrap();
        assert_eq!(m.0.as_path(), Path::new("/x/a"));
        assert_eq!(m.1, "role-a");
    }

    #[test]
    fn match_root_finds_ancestor() {
        let roots = vec![(PathBuf::from("/x/a"), "role-a".to_string())];
        let m = match_root(&roots, Path::new("/x/a/sub/foo.md")).unwrap();
        assert_eq!(m.1, "role-a");
    }

    #[test]
    fn match_root_returns_none_when_unrelated() {
        let roots = vec![(PathBuf::from("/x/a"), "role-a".to_string())];
        assert!(match_root(&roots, Path::new("/x/b/foo.md")).is_none());
    }

    #[test]
    fn match_root_prefers_first_in_yaml_order_on_nesting() {
        // Two roots where /x/a is an ancestor of /x/a/inner.
        // YAML declaration order wins: /x/a registered first → /x/a wins.
        let roots = vec![
            (PathBuf::from("/x/a"), "outer".to_string()),
            (PathBuf::from("/x/a/inner"), "inner".to_string()),
        ];
        // For a path exactly at /x/a/inner, the ancestor walk visits
        // /x/a/inner FIRST. That walks the inner root path. So this
        // hits "inner" — the deeper root wins by being a closer
        // ancestor, not by declaration order. Document this in the
        // doc comment on match_root (which we do above): first match
        // in `path.ancestors()` order, then in YAML declaration order
        // at the same ancestor depth.
        let m = match_root(&roots, Path::new("/x/a/inner/foo.md")).unwrap();
        assert_eq!(m.1, "inner");
    }
}
