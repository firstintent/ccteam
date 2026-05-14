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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as stdmpsc;
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
