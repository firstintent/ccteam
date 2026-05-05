//! ccteam orchestrator main loop. M0 wires the bare-bones daemon:
//!
//! - load + M0-validate every phase template under `~/.ccteam/phases/`
//!   on startup (fail-fast on `parallelism != solo` per development-plan
//!   §2.1 M0.6 acceptance);
//! - 30s tick (configurable for tests);
//! - notify-rs watcher on `~/.ccteam/progress/` so the loop wakes when
//!   a hook appends an event;
//! - cancellable run via a caller-supplied shutdown future.
//!
//! Per-tick + per-event handling are stubs; M0.7 fills in tmux session
//! lifecycle, M0.8 the idle-aware injection, M0.9 the state machine,
//! M0.10 the context-reset bridge, M0.13/M0.14 stall + cost.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::paths::CcteamPaths;
use crate::phases::PhaseTemplate;

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// How often the main loop ticks in the absence of progress events.
    pub tick_interval: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub struct Orchestrator {
    paths: CcteamPaths,
    config: OrchestratorConfig,
    templates: Vec<PhaseTemplate>,
}

impl Orchestrator {
    /// Construct + validate. Returns `Err` if any shipped phase template
    /// fails M0 validation, so the daemon refuses to start in a known-bad
    /// state instead of silently routing through agent_team / multi_session
    /// paths that aren't implemented yet.
    pub fn new(paths: CcteamPaths, config: OrchestratorConfig) -> Result<Self> {
        let templates = load_phase_templates(&paths.phases_dir())?;
        for t in &templates {
            t.validate_m0()
                .with_context(|| format!("phase template `{}` failed M0 validation", t.name))?;
        }
        tracing::info!(
            templates = templates.len(),
            phases_dir = %paths.phases_dir().display(),
            "orchestrator initialized",
        );
        Ok(Self {
            paths,
            config,
            templates,
        })
    }

    pub fn templates(&self) -> &[PhaseTemplate] {
        &self.templates
    }

    pub fn paths(&self) -> &CcteamPaths {
        &self.paths
    }

    /// Run until `shutdown` resolves. Each tick + each progress event
    /// currently logs at debug level; later M0 tasks fill in the body.
    pub async fn run<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        let progress_dir = self.paths.root.join("progress");
        std::fs::create_dir_all(&progress_dir)
            .with_context(|| format!("create {}", progress_dir.display()))?;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();

        // Keep the watcher alive for the duration of the loop.
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    let _ = tx.send(event);
                }
                Err(err) => tracing::warn!(?err, "progress watcher error"),
            },
        )?;
        watcher
            .watch(&progress_dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", progress_dir.display()))?;

        let mut tick = tokio::time::interval(self.config.tick_interval);
        // First tick fires immediately by default; skip it so observers can
        // assert "tick fired" without tolerating a 0-delay tick.
        tick.tick().await;

        let mut tick_count: u64 = 0;
        let mut event_count: u64 = 0;

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!(
                        tick_count,
                        event_count,
                        "orchestrator shutdown signal received",
                    );
                    return Ok(());
                }
                _ = tick.tick() => {
                    tick_count += 1;
                    self.poll_tick(tick_count).await?;
                }
                Some(event) = rx.recv() => {
                    event_count += 1;
                    self.handle_progress_event(event).await?;
                }
            }
        }
    }

    async fn poll_tick(&self, tick_count: u64) -> Result<()> {
        // M0.7 will: scan `~/projects/<slug>/.ccteam/state.json` files,
        // ensure each project's tmux session is alive, dispatch the next
        // phase via idle-aware send-keys (M0.8), reset on 60% context
        // (M0.10), warn on stall (M0.13), accumulate cost (M0.14).
        tracing::debug!(
            tick = tick_count,
            templates = self.templates.len(),
            "orchestrator tick",
        );
        Ok(())
    }

    async fn handle_progress_event(&self, event: notify::Event) -> Result<()> {
        // M0.8/M0.9 will tail the changed JSONL file and run the state
        // machine. For now just log so we can confirm the watcher is
        // wired up.
        tracing::debug!(?event, "progress event");
        Ok(())
    }
}

fn load_phase_templates(dir: &Path) -> Result<Vec<PhaseTemplate>> {
    if !dir.exists() {
        tracing::warn!(
            phases_dir = %dir.display(),
            "phases directory absent — orchestrator running with no templates",
        );
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("md"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for e in entries {
        let path = e.path();
        let template = PhaseTemplate::load(&path)
            .with_context(|| format!("phase template {}", path.display()))?;
        out.push(template);
    }
    Ok(out)
}
