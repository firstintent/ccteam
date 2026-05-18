//! Main daemon event loop.
//!
//! Composes credentials → Channel listeners → inbound pipeline →
//! supervisor → outbound tailers. The loop is `tokio::select`-driven
//! across:
//!
//! - one inbound mpsc receiver (multiplexed across active Channels),
//! - a supervisor tick timer,
//! - a SIGTERM future for graceful shutdown,
//! - an optional max-runtime watchdog (test-only — production is `0`).
//!
//! Wave 3 follow-up to Wave 2 skeleton: the supervisor tick now
//! actually drives [`BotSupervisor::apply_action`] against the live
//! per-bot state, so `decide(...) = Spawn` reaches `start_thread`,
//! `Restart` reaches `close_thread` + `start_thread`, and `Shutdown`
//! reaches `close_thread`. One [`BotSupervisor`] is built per
//! `BotRegistration` on first sight via the daemon's
//! [`AdapterFactory`] (defaults to `ClaudeTuiAdapter` for
//! `AgentVendor::Claude`; tests inject a stub).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use ccteam_core::execution::ClaudeTuiAdapter;
use ccteam_core::harness::{AgentVendor, HarnessAdapter};
use tokio::sync::Mutex;

use crate::credentials::{self, Credentials};
use crate::supervisor::{self, BotSupervisor};
use crate::{list_bots, BotRegistration};

/// Builds the production [`HarnessAdapter`] for one bot's `vendor`.
///
/// Hidden behind a function pointer so integration tests can swap
/// the real `ClaudeTuiAdapter` for a stub. The default returned by
/// [`default_adapter_factory`] is what `main.rs` wires.
pub type AdapterFactory =
    Arc<dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync>;

/// V0.6.0 Wave 3 — pick the canonical production adapter for `vendor`.
///
/// - `Claude` → [`ClaudeTuiAdapter`] (the mode 3 chat adapter).
/// - `Codex` → also [`ClaudeTuiAdapter`] today; the codex-exec-impl
///   teammate's `CodexAppServerAdapter` will replace this arm in a
///   follow-up commit. Falling back to the Claude adapter (rather
///   than panicking) keeps any mis-registered Codex bot inert
///   (`start_thread` will fail noisily on the wrong vendor) instead
///   of taking down the daemon.
pub fn default_adapter_factory() -> AdapterFactory {
    Arc::new(|vendor: AgentVendor| match vendor {
        AgentVendor::Claude => {
            Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
        }
        AgentVendor::Codex => {
            // TODO(wave-3 codex-exec-impl): swap to CodexAppServerAdapter
            // once it lands.
            Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>
        }
    })
}

/// CLI arguments forwarded from `main.rs`.
#[derive(Clone)]
pub struct DaemonArgs {
    /// Override credentials path (`None` → default).
    pub credentials: Option<PathBuf>,
    /// Override registry root.
    pub registry: Option<PathBuf>,
    /// Supervisor tick interval.
    pub tick: Duration,
    /// Optional max-runtime watchdog (`None` → unbounded; tests
    /// pass `Some(_)` to keep the harness from hanging).
    pub max_runtime: Option<Duration>,
    /// Wave 3 — adapter factory the supervisor registry uses to
    /// instantiate one [`HarnessAdapter`] per registered bot.
    /// `None` → [`default_adapter_factory`].
    pub adapter_factory: Option<AdapterFactory>,
}

impl std::fmt::Debug for DaemonArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonArgs")
            .field("credentials", &self.credentials)
            .field("registry", &self.registry)
            .field("tick", &self.tick)
            .field("max_runtime", &self.max_runtime)
            .field("adapter_factory", &self.adapter_factory.is_some())
            .finish()
    }
}

impl Default for DaemonArgs {
    fn default() -> Self {
        Self {
            credentials: None,
            registry: None,
            tick: Duration::from_secs(5),
            max_runtime: None,
            adapter_factory: None,
        }
    }
}

/// V0.6.0 Wave 3 — keyed map of live [`BotSupervisor`]s.
///
/// Keyed by `"<slug>/<role>"` (same convention as `bot_dir`). New
/// registrations grow the map; the daemon never removes entries
/// during the loop (`unregister_bot` deletes the JSON on disk; the
/// supervisor stays Shutdown via the signal file).
#[derive(Default)]
pub struct SupervisorRegistry {
    inner: HashMap<String, Arc<BotSupervisor>>,
}

impl SupervisorRegistry {
    fn key(reg: &BotRegistration) -> String {
        format!("{}/{}", reg.workflow_slug, reg.role)
    }

    /// Add a supervisor for `reg` if one isn't already registered.
    /// Returns the live (existing-or-new) supervisor handle.
    pub fn ensure(
        &mut self,
        reg: &BotRegistration,
        projects_root: &Path,
        factory: &AdapterFactory,
    ) -> Arc<BotSupervisor> {
        let k = Self::key(reg);
        self.inner
            .entry(k)
            .or_insert_with(|| {
                let adapter = factory(reg.vendor);
                Arc::new(BotSupervisor::new(
                    reg.clone(),
                    projects_root.to_path_buf(),
                    adapter,
                ))
            })
            .clone()
    }

    /// Snapshot of every live supervisor for test introspection.
    pub fn all(&self) -> Vec<Arc<BotSupervisor>> {
        self.inner.values().cloned().collect()
    }
}

/// Run the daemon. Returns `Ok(())` on graceful shutdown.
pub async fn run_daemon(args: DaemonArgs) -> Result<()> {
    let creds = credentials::load(args.credentials.as_deref())?;
    let initial = list_bots()?;
    tracing::info!(
        bots = initial.len(),
        has_telegram = creds.telegram.is_some(),
        has_slack = creds.slack.is_some(),
        has_discord = creds.discord.is_some(),
        "ccteam-imd daemon starting"
    );

    let factory = args
        .adapter_factory
        .clone()
        .unwrap_or_else(default_adapter_factory);
    let registry: Arc<Mutex<SupervisorRegistry>> =
        Arc::new(Mutex::new(SupervisorRegistry::default()));

    let mut ticker = tokio::time::interval(args.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let started = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(err) = supervisor::refresh_global_heartbeat() {
                    tracing::warn!(error = %err, "heartbeat refresh failed");
                }
                let bots = match list_bots() {
                    Ok(b) => b,
                    Err(err) => {
                        tracing::warn!(error = %err, "list_bots failed");
                        Vec::new()
                    }
                };
                tick_supervisors(&bots, &registry, args.registry.as_deref(), &factory).await;

                if let Some(max) = args.max_runtime {
                    if started.elapsed() >= max {
                        tracing::info!("max_runtime reached; exiting");
                        return Ok(());
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT received; graceful shutdown");
                return Ok(());
            }
        }
    }
}

/// V0.6.0 Wave 3 — per-tick driver. Registers any new bots with the
/// supervisor registry, then for each known bot calls
/// `decide(...)` against its live state and applies the resulting
/// action through the supervisor (`apply_action`).
pub(crate) async fn tick_supervisors(
    bots: &[BotRegistration],
    registry: &Arc<Mutex<SupervisorRegistry>>,
    projects_root_override: Option<&Path>,
    factory: &AdapterFactory,
) {
    let owned;
    let projects_root: &Path = match projects_root_override {
        Some(p) => p,
        None => {
            owned = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join("projects");
            &owned
        }
    };

    // First pass: ensure a supervisor exists per bot.
    let supervisors: Vec<(BotRegistration, Arc<BotSupervisor>)> = {
        let mut reg = registry.lock().await;
        bots.iter()
            .map(|b| {
                let sup = reg.ensure(b, projects_root, factory);
                (b.clone(), sup)
            })
            .collect()
    };

    // Second pass: decide + apply per bot (drop the registry lock for
    // each adapter call so a slow start_thread doesn't stall other
    // bots' decisions).
    for (bot, sup) in supervisors {
        let state = sup.state_snapshot().await;
        let action = supervisor::decide(projects_root, &bot, &state, SystemTime::now());
        tracing::debug!(
            slug = %bot.workflow_slug,
            role = %bot.role,
            ?action,
            "supervisor decision"
        );
        if let Err(err) = sup.apply_action(action.clone()).await {
            tracing::warn!(
                slug = %bot.workflow_slug,
                role = %bot.role,
                ?action,
                error = %err,
                "apply_action failed"
            );
        }
    }
}

/// Compatibility shim — `lib.rs` re-exports this so the existing
/// `pub use daemon::run_daemon;` keeps working without forcing
/// callers to depend on this module path directly.
pub fn _link_check(_c: &Credentials) {}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ccteam_core::harness::{
        AgentSpecBrief, ExecutionMode, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, TurnId,
        TurnInput,
    };
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    #[tokio::test(flavor = "current_thread", start_paused = false)]
    async fn daemon_boots_and_exits_on_max_runtime() {
        // Point HOME at a tempdir so no real credentials are read.
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let args = DaemonArgs {
            credentials: None,
            registry: None,
            tick: Duration::from_millis(50),
            max_runtime: Some(Duration::from_millis(120)),
            adapter_factory: None,
        };
        run_daemon(args).await.unwrap();
        // Heartbeat was written at least once.
        let hb = crate::imd_heartbeat_path();
        assert!(hb.exists(), "heartbeat at {} should exist", hb.display());
        std::env::remove_var("HOME");
    }

    #[derive(Debug, Default)]
    struct RecordingAdapter {
        starts: AtomicUsize,
        closes: AtomicUsize,
    }
    #[async_trait]
    impl HarnessAdapter for RecordingAdapter {
        fn name(&self) -> &'static str {
            "recording"
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
            Ok(ThreadHandle {
                vendor: AgentVendor::Claude,
                mode: ExecutionMode::Chat,
                identity: format!("rec-{}-{}", ctx.slug, spec.role),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({}),
            })
        }
        async fn submit_turn(
            &self,
            _h: &ThreadHandle,
            _input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            Ok(TurnId::new("t"))
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

    #[tokio::test]
    async fn tick_supervisors_registers_and_spawns_bot() {
        let projects = TempDir::new().unwrap();
        let adapter = Arc::new(RecordingAdapter::default());
        let adapter_factory: AdapterFactory = {
            let cloned = adapter.clone();
            Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
        };
        let registry = Arc::new(Mutex::new(SupervisorRegistry::default()));
        let bot = BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "telegram".into(),
            im_chat_id: "1".into(),
            created_at: chrono::Utc::now(),
        };

        // Tick 1: decide() returns Spawn (no handle yet) → start_thread.
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

        // Tick 2: heartbeat missing → decide() returns Restart → close + start.
        tick_supervisors(
            std::slice::from_ref(&bot),
            &registry,
            Some(projects.path()),
            &adapter_factory,
        )
        .await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 2);
        assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);

        // Drop in a fresh heartbeat: next tick is a NoOp.
        let bot_dir = projects
            .path()
            .join("dev-foo/.ccteam/chat/lead");
        std::fs::create_dir_all(&bot_dir).unwrap();
        std::fs::write(bot_dir.join("heartbeat"), "x").unwrap();
        tick_supervisors(std::slice::from_ref(&bot), &registry, Some(projects.path()), &adapter_factory).await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 2, "no new start");

        // Verify the supervisor is registered + holds a live handle.
        let supervisors = registry.lock().await.all();
        assert_eq!(supervisors.len(), 1);
        assert!(supervisors[0].is_started().await);
    }

    #[tokio::test]
    async fn tick_supervisors_honors_shutdown_signal() {
        let projects = TempDir::new().unwrap();
        let adapter = Arc::new(RecordingAdapter::default());
        let adapter_factory: AdapterFactory = {
            let cloned = adapter.clone();
            Arc::new(move |_| cloned.clone() as Arc<dyn HarnessAdapter + Send + Sync>)
        };
        let registry = Arc::new(Mutex::new(SupervisorRegistry::default()));
        let bot = BotRegistration {
            workflow_slug: "dev-foo".into(),
            role: "lead".into(),
            vendor: AgentVendor::Claude,
            persona_id: None,
            im_platform: "telegram".into(),
            im_chat_id: "1".into(),
            created_at: chrono::Utc::now(),
        };
        // Tick 1: Spawn.
        tick_supervisors(std::slice::from_ref(&bot), &registry, Some(projects.path()), &adapter_factory).await;
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);

        // Drop shutdown.signal; next tick → Shutdown action.
        let sig_dir = projects
            .path()
            .join("dev-foo/.ccteam/chat/lead/signals");
        std::fs::create_dir_all(&sig_dir).unwrap();
        std::fs::write(sig_dir.join("shutdown.signal"), "").unwrap();
        tick_supervisors(std::slice::from_ref(&bot), &registry, Some(projects.path()), &adapter_factory).await;
        assert_eq!(adapter.closes.load(Ordering::SeqCst), 1);
        let supervisors = registry.lock().await.all();
        assert!(!supervisors[0].is_started().await);
    }
}
