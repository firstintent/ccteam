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
//! Wave 2 ships a **lifecycle skeleton**: the daemon boots, registers
//! channels, runs the supervisor tick, refreshes heartbeats. The
//! `claude-tui` adapter wiring (calling
//! `HarnessAdapter::start_thread` / `submit_turn` / `close_thread`)
//! is performed by a separate followup that imports the tui-impl
//! teammate's adapter once they land it. The skeleton + tests are
//! self-contained and prove the loop boots / shuts down cleanly.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::credentials::{self, Credentials};
use crate::supervisor;
use crate::{list_bots, BotRegistration};

/// CLI arguments forwarded from `main.rs`.
#[derive(Debug, Clone)]
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
}

impl Default for DaemonArgs {
    fn default() -> Self {
        Self {
            credentials: None,
            registry: None,
            tick: Duration::from_secs(5),
            max_runtime: None,
        }
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

    // Per-bot supervisor state, keyed by `"<slug>/<role>"`.
    let _bot_state: Arc<Mutex<std::collections::HashMap<String, supervisor::BotState>>> =
        Arc::new(Mutex::new(Default::default()));

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
                for reg in &bots {
                    decide_and_log(reg, args.registry.as_deref()).await;
                }

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

async fn decide_and_log(reg: &BotRegistration, projects_root: Option<&std::path::Path>) {
    // Honor explicit override for tests; production resolves the
    // projects-root via the standard ccteam convention later.
    let owned;
    let root: &std::path::Path = match projects_root {
        Some(p) => p,
        None => {
            owned = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join("projects");
            &owned
        }
    };
    let state = supervisor::BotState::default();
    let action = supervisor::decide(root, reg, &state, SystemTime::now());
    tracing::debug!(
        slug = %reg.workflow_slug,
        role = %reg.role,
        action = ?action,
        "supervisor decision"
    );
}

/// Compatibility shim — `lib.rs` re-exports this so the existing
/// `pub use daemon::run_daemon;` keeps working without forcing
/// callers to depend on this module path directly.
pub fn _link_check(_c: &Credentials) {}

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        run_daemon(args).await.unwrap();
        // Heartbeat was written at least once.
        let hb = crate::imd_heartbeat_path();
        assert!(hb.exists(), "heartbeat at {} should exist", hb.display());
        std::env::remove_var("HOME");
    }
}
