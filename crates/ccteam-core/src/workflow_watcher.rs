//! V0.4.6 F82 — `workflow.yaml` file watcher (hot-reload trigger).
//!
//! ## Role in the V0.4.6 architecture
//!
//! V0.4.5 required a daemon restart to pick up `workflow.yaml` edits:
//! changing `enabled`, swapping a `watch:` path, mutating `agents` —
//! all of them needed `ccteam stop && ccteam start`. F82 adds a thin
//! inotify watcher per rostered project that emits a
//! [`WorkflowFileEvent`] every time the project's workflow.yaml is
//! modified. The [`crate::orchestrator::Orchestrator::run`] loop picks
//! events off the channel and calls `reload_project(slug)` to apply
//! the new spec via the F82 cancellation-token machinery.
//!
//! ## Architectural notes
//!
//! ```text
//! per-project workflow.yaml      notify::RecommendedWatcher
//!   <project>/.ccteam/workflow.yaml          │
//!   <project>/workflow.yaml (fallback)       │
//!                                            ▼
//!                              std::sync::mpsc bridge
//!                                            │
//!                              tokio task ── 1s debounce
//!                                            ▼
//!                            tokio::sync::mpsc<WorkflowFileEvent>
//! ```
//!
//! ## Red lines
//!
//! 1. **Try both paths.** F83 moves workflow.yaml from `<root>/workflow.yaml`
//!    to `<root>/.ccteam/workflow.yaml`. Watch both so a project on
//!    either layout is covered without hard-failing on the missing path.
//! 2. **Debounce window**: 1 s per slug. Editors like vim write +
//!    rename; without debounce a single save fires multiple events.
//! 3. **Best-effort**: a path that does not exist yet (project still
//!    bootstrapping) is silently skipped — the watcher does NOT bubble
//!    errors so a single broken project can't take down the daemon.
//! 4. **No re-parsing**: this module only signals "the file changed".
//!    The orchestrator decides whether the new spec is loadable; YAML
//!    syntax errors are logged + the old loop is left running (F82 PRD
//!    §validation #4 fail-safe).
//!
//! See also `docs/versions/v0-4-6/prd.md` §F82 + dev-plan §阶段 1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as stdmpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// Per-slug debounce window. notify (and vim, and the kernel) may
/// fire multiple events for a single user "save"; this collapses them.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_secs(1);

/// Poll interval inside the watcher's blocking loop. Same role as
/// `artifact_watcher::WATCHER_SHUTDOWN_POLL` — lets the thread notice
/// "outbound tokio mpsc closed" while notify is idle.
pub const WATCHER_SHUTDOWN_POLL: Duration = Duration::from_millis(500);

/// Channel capacity. 64 is generous — workflow.yaml is rarely edited
/// faster than the orchestrator can process reloads.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// One reload signal — emitted whenever the watcher detects a change
/// in a watched workflow.yaml. Consumers (the orchestrator) take this
/// as "stop your current loop for `slug` and re-roster against the
/// new on-disk spec".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowFileEvent {
    /// Slug of the project whose workflow.yaml changed.
    pub slug: String,
    /// Coarse-grained event kind. The orchestrator currently treats
    /// every variant uniformly (re-read the file), but the field is
    /// preserved for future logging/diagnostics.
    pub kind: WorkflowFileEventKind,
}

/// Coarse classification of a workflow.yaml change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowFileEventKind {
    /// File contents changed (most common — vim save, `ccteam doctor`
    /// edit, manual `sed -i`, etc.).
    Modified,
    /// File deleted (rare — usually means project teardown).
    Deleted,
}

/// Filesystem watcher for `workflow.yaml` files across rostered
/// projects. Build with [`new`]; consume via the returned
/// `mpsc::Receiver<WorkflowFileEvent>`.
///
/// [`new`]: WorkflowFileWatcher::new
pub struct WorkflowFileWatcher {
    /// Owned notify watcher — drop drops the OS-level registrations.
    _watcher: RecommendedWatcher,
    /// Tokio sender; owned by the spawned task.
    tx: mpsc::Sender<WorkflowFileEvent>,
    /// Synchronous receiver fed by the notify callback (sync→async
    /// bridge; same pattern as `artifact_watcher`).
    event_rx: stdmpsc::Receiver<notify::Result<notify::Event>>,
    /// Map of watched file path → owning slug. notify reports the
    /// full file path; the task walks `path.ancestors()` to match a
    /// registered slug. We store full file paths (not just dirs) so
    /// events for sibling files in the same dir don't fire reloads.
    files: Vec<(PathBuf, String)>,
}

impl WorkflowFileWatcher {
    /// Build a watcher for every `(slug, project_dir)` pair. Side
    /// effects:
    ///
    /// - For each project, register notify watches on BOTH possible
    ///   workflow.yaml locations: `<project_dir>/.ccteam/workflow.yaml`
    ///   (F83 preferred) and `<project_dir>/workflow.yaml` (legacy
    ///   fallback). A path that doesn't exist is skipped silently —
    ///   the watcher does not auto-mkdir; if the user later writes the
    ///   file, they need to call this constructor again (or rely on
    ///   daemon restart). This matches the dev-plan §阶段 1
    ///   "best-effort" semantics.
    /// - The notify watcher runs in NON-recursive mode — we want
    ///   exactly the workflow.yaml file, not sibling artifacts.
    ///
    /// Returns the watcher + receiver. Caller must hold both alive
    /// until shutdown; dropping the receiver closes the channel which
    /// the spawned task then notices and exits.
    pub fn new(
        projects: &[(String, PathBuf)],
    ) -> Result<(Self, mpsc::Receiver<WorkflowFileEvent>)> {
        let (event_tx, event_rx) = stdmpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            let _ = event_tx.send(res);
        })
        .context("workflow_watcher: initialize notify::RecommendedWatcher")?;

        let mut files: Vec<(PathBuf, String)> = Vec::new();
        for (slug, project_dir) in projects {
            // F83 preferred location
            let nested = project_dir.join(".ccteam").join("workflow.yaml");
            // V0.4.5 legacy fallback
            let root = project_dir.join("workflow.yaml");
            for candidate in [&nested, &root] {
                if candidate.exists() {
                    if let Err(err) = watcher.watch(candidate, RecursiveMode::NonRecursive) {
                        tracing::warn!(
                            slug,
                            path = %candidate.display(),
                            ?err,
                            "workflow_watcher: failed to install notify watch; skipping",
                        );
                        continue;
                    }
                    files.push((candidate.clone(), slug.clone()));
                }
            }
        }

        let (tx, rx) = mpsc::channel::<WorkflowFileEvent>(EVENT_CHANNEL_CAPACITY);
        Ok((
            WorkflowFileWatcher {
                _watcher: watcher,
                tx,
                event_rx,
                files,
            },
            rx,
        ))
    }

    /// Add a project to an already-running watcher. Used when the
    /// rescan-loop picks up a new slug. Best-effort: missing
    /// workflow.yaml files are skipped silently.
    pub fn add_project(&mut self, slug: &str, project_dir: &Path) {
        let nested = project_dir.join(".ccteam").join("workflow.yaml");
        let root = project_dir.join("workflow.yaml");
        for candidate in [&nested, &root] {
            if candidate.exists() {
                if let Err(err) = self._watcher.watch(candidate, RecursiveMode::NonRecursive) {
                    tracing::warn!(
                        slug,
                        path = %candidate.display(),
                        ?err,
                        "workflow_watcher: add_project failed",
                    );
                    continue;
                }
                self.files.push((candidate.clone(), slug.to_string()));
            }
        }
    }

    /// Drop watches for a slug (used by F81 remove). Best-effort.
    pub fn remove_project(&mut self, slug: &str) {
        self.files.retain(|(path, owning_slug)| {
            if owning_slug == slug {
                let _ = self._watcher.unwatch(path);
                false
            } else {
                true
            }
        });
    }

    /// Spawn the background task that translates raw notify events
    /// into debounced [`WorkflowFileEvent`]s. The task exits when the
    /// returned receiver is dropped.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let WorkflowFileWatcher {
            _watcher,
            tx,
            event_rx,
            files,
        } = self;

        tokio::task::spawn_blocking(move || {
            let _watcher = _watcher; // keep alive

            // Debounce per slug — multiple paths owned by the same
            // slug (e.g. nested + root both exist transiently during
            // migration) still collapse to one event in the window.
            let mut last_emit: HashMap<String, Instant> = HashMap::new();

            loop {
                let res = match event_rx.recv_timeout(WATCHER_SHUTDOWN_POLL) {
                    Ok(v) => v,
                    Err(stdmpsc::RecvTimeoutError::Timeout) => {
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
                        tracing::warn!(?err, "workflow_watcher: notify reported error");
                        continue;
                    }
                };

                let kind = match coarse_kind(&ev.kind) {
                    Some(k) => k,
                    None => continue,
                };

                for path in &ev.paths {
                    let Some(slug) = match_file(&files, path) else {
                        continue;
                    };
                    let now = Instant::now();
                    if let Some(prev) = last_emit.get(slug) {
                        if now.duration_since(*prev) < DEBOUNCE_WINDOW {
                            continue;
                        }
                    }
                    last_emit.insert(slug.clone(), now);

                    let event = WorkflowFileEvent {
                        slug: slug.clone(),
                        kind,
                    };
                    if tx.blocking_send(event).is_err() {
                        return;
                    }
                }
            }
        })
    }
}

/// Map notify's fine-grained kind onto our coarse classification.
fn coarse_kind(kind: &EventKind) -> Option<WorkflowFileEventKind> {
    match kind {
        EventKind::Modify(_) | EventKind::Create(_) => Some(WorkflowFileEventKind::Modified),
        EventKind::Remove(_) => Some(WorkflowFileEventKind::Deleted),
        EventKind::Access(_) | EventKind::Any | EventKind::Other => None,
    }
}

/// Find the slug whose watched file matches `path`. Editors that do
/// "write to tmp + rename" sometimes deliver events on the directory
/// containing the file instead of the file itself; we accept any
/// ancestor match so those rename-style saves still trigger.
fn match_file<'a>(files: &'a [(PathBuf, String)], path: &Path) -> Option<&'a String> {
    for ancestor in path.ancestors() {
        for (watched, slug) in files {
            if watched.as_path() == ancestor {
                return Some(slug);
            }
            // Editor rename pattern: notify reports the parent dir.
            if let Some(parent) = watched.parent() {
                if parent == ancestor && watched.file_name() == path.file_name() {
                    return Some(slug);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_kind_modify_create_remove() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        assert_eq!(
            coarse_kind(&EventKind::Create(CreateKind::File)),
            Some(WorkflowFileEventKind::Modified)
        );
        assert_eq!(
            coarse_kind(&EventKind::Modify(ModifyKind::Any)),
            Some(WorkflowFileEventKind::Modified)
        );
        assert_eq!(
            coarse_kind(&EventKind::Remove(RemoveKind::File)),
            Some(WorkflowFileEventKind::Deleted)
        );
        assert_eq!(coarse_kind(&EventKind::Any), None);
    }

    #[test]
    fn match_file_exact() {
        let files = vec![(PathBuf::from("/x/a/workflow.yaml"), "alpha".to_string())];
        assert_eq!(
            match_file(&files, Path::new("/x/a/workflow.yaml")),
            Some(&"alpha".to_string())
        );
    }

    #[test]
    fn match_file_rejects_sibling() {
        let files = vec![(PathBuf::from("/x/a/workflow.yaml"), "alpha".to_string())];
        // sibling in same dir
        assert_eq!(match_file(&files, Path::new("/x/a/state.json")), None);
    }
}
