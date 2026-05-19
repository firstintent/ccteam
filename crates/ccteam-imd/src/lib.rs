//! ccteam-imd — V0.6.0 Wave 2 F109 + F116
//!
//! Single per-host daemon that bridges IM platforms (Telegram / Slack /
//! Discord) to ccteam-managed long-running chat sessions, plus a tmux
//! supervisor (F116) that runs heartbeat + crash-restart for every
//! registered `mode: chat` bot.
//!
//! Public API (called from `ccteam-creator` skill / CLI):
//!
//! - [`register_bot`] / [`unregister_bot`] — manage the on-disk registry
//!   under `~/.ccteam/imd/registry/<slug>/<role>.json`.
//! - [`run_daemon`] — the main event loop (clap-driven from `main.rs`).
//!
//! Architectural red lines (see `docs/v0-6-0/wave-2-decisions.md`):
//!
//! - **`ccteam-core` stays openhuman-free.** The dependency graph
//!   integration test `tests/dep_graph_test.rs` enforces this.
//! - We talk to long-running chat sessions through
//!   [`ccteam_core::harness::HarnessAdapter`] only — never reach into
//!   `ccteam_core::execution::*` directly.
//! - The daemon **never kills tmux sessions** outside the F116
//!   supervisor crash-restart codepath. User-initiated stop goes
//!   through `<project>/.ccteam/chat/<bot>/signals/shutdown.signal`.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod acl;
pub mod credentials;
pub mod daemon;
pub mod inbound;
pub mod nl_admin;
pub mod outbound;
pub mod rate_limit;
pub mod router;
pub mod sanitize;
pub mod supervisor;
pub mod three_layer_sec;
pub mod transport;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use ccteam_core::harness::AgentVendor;
use serde::{Deserialize, Serialize};

/// One registered bot — the on-disk payload at
/// `~/.ccteam/imd/registry/<slug>/<role>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotRegistration {
    /// `workflow.yaml`'s `name` field — the per-project slug.
    pub workflow_slug: String,
    /// Role within the workflow (e.g. `"lead"`, `"reviewer"`).
    pub role: String,
    /// Which harness vendor runs the underlying tmux session.
    pub vendor: AgentVendor,
    /// Stable persona identity (used for IM display name / avatar
    /// mapping, never as a routing key — routing keys are
    /// `<slug>/<role>`).
    pub persona_id: Option<String>,
    /// Which IM platform this bot binds to: `"telegram"`, `"slack"`,
    /// `"discord"`, or `"mock"` (tests).
    pub im_platform: String,
    /// Platform-specific chat identifier (Telegram chat_id, Slack
    /// channel id, Discord channel id). Stored as a string for
    /// platform-agnostic round-tripping.
    pub im_chat_id: String,
    /// RFC3339 timestamp.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Resolve the registry directory for the current user
/// (`~/.ccteam/imd/registry/`).
pub fn registry_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".ccteam")
        .join("imd")
        .join("registry")
}

/// Per-(slug, role) registration file path.
pub fn registration_path(slug: &str, role: &str) -> PathBuf {
    registry_root().join(slug).join(format!("{role}.json"))
}

/// Register one bot. Creator skill calls this when scaffolding a new
/// `mode: chat` workflow; the daemon's registry watcher picks the file
/// up and spawns the tmux session via `HarnessAdapter::start_thread`.
///
/// Idempotent — re-registering with the same `(slug, role)` overwrites
/// the existing entry.
pub fn register_bot(
    workflow_slug: &str,
    role: &str,
    vendor: AgentVendor,
    im_platform: &str,
    im_chat_id: &str,
) -> Result<PathBuf> {
    let registration = BotRegistration {
        workflow_slug: workflow_slug.to_string(),
        role: role.to_string(),
        vendor,
        persona_id: None,
        im_platform: im_platform.to_string(),
        im_chat_id: im_chat_id.to_string(),
        created_at: chrono::Utc::now(),
    };
    let path = registration_path(workflow_slug, role);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create registry dir {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&registration)
        .context("serialize BotRegistration")?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    tracing::info!(slug = workflow_slug, role, "registered bot");
    Ok(path)
}

/// Unregister one bot. Daemon registry watcher tears down the
/// corresponding tmux session (graceful — writes
/// `signals/shutdown.signal`, lets `close_thread` run idempotently).
pub fn unregister_bot(workflow_slug: &str, role: &str) -> Result<()> {
    let path = registration_path(workflow_slug, role);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        tracing::info!(slug = workflow_slug, role, "unregistered bot");
    }
    Ok(())
}

/// List every registered bot across all slugs.
pub fn list_bots() -> Result<Vec<BotRegistration>> {
    let root = registry_root();
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for slug_entry in fs::read_dir(&root)? {
        let slug_entry = slug_entry?;
        if !slug_entry.file_type()?.is_dir() {
            continue;
        }
        for role_entry in fs::read_dir(slug_entry.path())? {
            let role_entry = role_entry?;
            let path = role_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let body = fs::read_to_string(&path)?;
            match serde_json::from_str::<BotRegistration>(&body) {
                Ok(reg) => out.push(reg),
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "skip malformed registration");
                }
            }
        }
    }
    Ok(out)
}

/// Heartbeat file the daemon refreshes every supervisor tick.
pub fn imd_heartbeat_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".ccteam")
        .join("state")
        .join("imd.heartbeat")
}

/// Result of [`wait_for_health`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthResult {
    /// A heartbeat written at or after `started_at` was observed before
    /// the timeout elapsed.
    Ready,
    /// The timeout elapsed without observing a fresh heartbeat.
    Timeout,
}

/// V0.6.1 F119 — block until the daemon publishes a heartbeat written
/// at or after `started_at`, or `timeout` elapses.
///
/// "Fresh" is defined as the heartbeat file's mtime being `>=
/// started_at`. Comparing against a caller-recorded `started_at` (not
/// just "exists") prevents a stale heartbeat from a previous daemon
/// invocation from spoofing readiness. Callers using `ccteam-imd
/// health` semantics capture `SystemTime::now()` *before* spawning
/// the daemon, then pass it here.
///
/// `poll` is the sleep between filesystem checks (200ms is a sane
/// default; tests pass `Duration::from_millis(5)` for hermetic
/// fast-loop checks).
pub fn wait_for_health(started_at: SystemTime, timeout: Duration, poll: Duration) -> HealthResult {
    let deadline = Instant::now() + timeout;
    let hb = imd_heartbeat_path();
    loop {
        if let Ok(meta) = fs::metadata(&hb) {
            if let Ok(mtime) = meta.modified() {
                if mtime >= started_at {
                    return HealthResult::Ready;
                }
            }
        }
        if Instant::now() >= deadline {
            return HealthResult::Timeout;
        }
        std::thread::sleep(poll);
    }
}

/// Re-export the main daemon entry so `main.rs` stays a thin shim.
pub use daemon::{run_daemon, DaemonArgs};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_path_layout() {
        let p = registration_path("dev-foo", "lead");
        assert!(p.ends_with(".ccteam/imd/registry/dev-foo/lead.json"));
    }

    #[test]
    fn health_result_round_trip_ready_eq_timeout_distinct() {
        // Sanity: HealthResult variants aren't accidentally collapsed.
        assert_eq!(HealthResult::Ready, HealthResult::Ready);
        assert_ne!(HealthResult::Ready, HealthResult::Timeout);
    }
}
