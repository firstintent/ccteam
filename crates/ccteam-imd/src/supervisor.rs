//! Per-bot tmux session supervisor + heartbeat.
//!
//! V0.6.0 Wave 2 F116. The supervisor:
//!
//! 1. Refreshes the daemon-global heartbeat file each tick
//!    (`~/.ccteam/state/imd.heartbeat`).
//! 2. For every registered bot, checks the per-bot heartbeat
//!    (`<project>/.ccteam/chat/<bot>/heartbeat`) — written by the
//!    `claude-tui` adapter (tui-impl teammate's F108 work).
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
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ccteam_core::harness::ThreadHandle;
use serde::{Deserialize, Serialize};

use crate::{imd_heartbeat_path, BotRegistration};

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
        assert_eq!(decide(tmp.path(), &reg, &st, SystemTime::now()), SupervisorAction::Shutdown);
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
        assert_eq!(decide(tmp.path(), &reg, &st, SystemTime::now()), SupervisorAction::Drain);
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
        assert_eq!(decide(tmp.path(), &reg, &st, SystemTime::now()), SupervisorAction::Restart);
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
        assert_eq!(decide(tmp.path(), &reg, &st, SystemTime::now()), SupervisorAction::NoOp);
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
