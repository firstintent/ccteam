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
//! Architectural red lines (see `docs/versions/v0-6-0/wave-2-decisions.md`):
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
pub mod bot_mpsc;
pub mod credentials;
pub mod daemon;
pub mod inbound;
pub mod latency;
pub mod nl_admin;
pub mod outbound;
pub mod rate_limit;
pub mod router;
pub mod sanitize;
pub mod supervisor;
pub mod three_layer_sec;
pub mod transport;

use std::fs;
use std::path::{Path, PathBuf};
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

/// Default ccteam root used by the home-derived path helpers
/// (`<HOME>/.ccteam`). The MCP tools and tests reach for the `_in`
/// variants below so they can isolate against a tempdir; daemon /
/// supervisor code stays on the home-derived path.
fn default_ccteam_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".ccteam")
}

/// V0.6.6 F169 — pub wrapper around [`default_ccteam_root`] so the
/// `nl_admin::AdminExecutor` (and other library callers) can resolve
/// the production ccteam-root without re-implementing the home-derived
/// path. Tests inject a tempdir via `AdminExecutor::with_ccteam_root`.
pub fn default_ccteam_root_public() -> PathBuf {
    default_ccteam_root()
}

/// `<ccteam_root>/imd/registry/` — base registry dir given an explicit
/// root (V0.6.5 F146).
pub fn registry_root_in(ccteam_root: &Path) -> PathBuf {
    ccteam_root.join("imd").join("registry")
}

/// Resolve the registry directory for the current user
/// (`~/.ccteam/imd/registry/`).
pub fn registry_root() -> PathBuf {
    registry_root_in(&default_ccteam_root())
}

/// Per-(slug, role) registration file path under an explicit root
/// (V0.6.5 F146).
pub fn registration_path_in(ccteam_root: &Path, slug: &str, role: &str) -> PathBuf {
    registry_root_in(ccteam_root)
        .join(slug)
        .join(format!("{role}.json"))
}

/// Per-(slug, role) registration file path.
pub fn registration_path(slug: &str, role: &str) -> PathBuf {
    registration_path_in(&default_ccteam_root(), slug, role)
}

/// V0.6.5 F146 — per-bot heartbeat sidecar under the registry, so the
/// MCP tool process (which has no access to the daemon's in-memory
/// `SupervisorRegistry`) can read `running` status off disk. Sibling
/// of the registration JSON: `<ccteam_root>/imd/registry/<slug>/<role>.heartbeat`.
pub fn bot_heartbeat_path_in(ccteam_root: &Path, slug: &str, role: &str) -> PathBuf {
    registry_root_in(ccteam_root)
        .join(slug)
        .join(format!("{role}.heartbeat"))
}

/// Home-derived form of [`bot_heartbeat_path_in`].
pub fn bot_heartbeat_path(slug: &str, role: &str) -> PathBuf {
    bot_heartbeat_path_in(&default_ccteam_root(), slug, role)
}

/// V0.6.5 F146 — heartbeat freshness window. Daemon's per-bot
/// supervisor refreshes the heartbeat every 5s (see
/// `HEARTBEAT_TICK`); anything fresher than 30s means the daemon is
/// alive **and** the bot's supervisor task is ticking.
pub const REGISTRY_HEARTBEAT_FRESH: Duration = Duration::from_secs(30);

/// Touch the per-bot registry heartbeat (V0.6.5 F146). Idempotent —
/// creates parent dir if missing. Called from the supervisor's
/// heartbeat-writer task so a separate MCP process can see running
/// status without RPCing the daemon.
pub fn touch_bot_heartbeat_in(ccteam_root: &Path, slug: &str, role: &str) -> Result<()> {
    let path = bot_heartbeat_path_in(ccteam_root, slug, role);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create heartbeat dir {}", parent.display()))?;
    }
    let now = chrono::Utc::now().to_rfc3339();
    fs::write(&path, now).with_context(|| format!("write heartbeat {}", path.display()))?;
    Ok(())
}

/// Home-derived form of [`touch_bot_heartbeat_in`].
pub fn touch_bot_heartbeat(slug: &str, role: &str) -> Result<()> {
    touch_bot_heartbeat_in(&default_ccteam_root(), slug, role)
}

/// V0.6.5 F146 — `true` when the heartbeat file exists and its mtime
/// is within [`REGISTRY_HEARTBEAT_FRESH`] of `now`.
pub fn bot_running_status_in(ccteam_root: &Path, slug: &str, role: &str) -> bool {
    let path = bot_heartbeat_path_in(ccteam_root, slug, role);
    let Ok(meta) = fs::metadata(&path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(age) => age <= REGISTRY_HEARTBEAT_FRESH,
        // mtime is in the future → clock skew; treat as fresh.
        Err(_) => true,
    }
}

/// Home-derived form of [`bot_running_status_in`].
pub fn bot_running_status(slug: &str, role: &str) -> bool {
    bot_running_status_in(&default_ccteam_root(), slug, role)
}

/// V0.6.5 F146 — read `last_turn_at` (mtime of the ccteam-owned
/// `turns.jsonl`) from the project tree. Returns `None` if the file
/// doesn't exist yet (bot registered but no turn taken).
pub fn last_turn_at(
    projects_root: &Path,
    slug: &str,
    role: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let meta = fs::metadata(turns_jsonl_path(projects_root, slug, role)).ok()?;
    let mtime = meta.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(mtime))
}

/// V0.6.5 F147 — resolve `<projects_root>/<slug>/.ccteam/chat/<role>/inbox/`.
/// The mailbox path the daemon's `drain_inboxes` / per-bot mpsc
/// fast-path consume. MCP `chat_send_input` writes a router-style
/// envelope here; daemon picks it up either via the fast-path (when
/// the per-bot mpsc is wired) or via the safety-net drain tick.
pub fn chat_inbox_dir(projects_root: &Path, slug: &str, role: &str) -> PathBuf {
    projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("inbox")
}

/// V0.6.5 F147 — resolve `<projects_root>/<slug>/.ccteam/chat/<role>/signals/reset.signal`.
/// MCP `chat_reset` writes this file; the supervisor's next tick reads
/// it via `signal_present(.., RESET_SIGNAL)` and applies the
/// ResetSession action (archive + close + start + cursor wipe).
pub fn chat_reset_signal_path(projects_root: &Path, slug: &str, role: &str) -> PathBuf {
    projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("signals")
        .join("reset.signal")
}

/// V0.6.5 F147 — resolve `<projects_root>/<slug>/.ccteam/chat/<role>/turns.jsonl`.
/// Source-of-truth file `chat_history` tails. Re-exports
/// [`outbound::turns_jsonl_path`] under a top-level name so MCP /
/// integration callers don't have to reach into the outbound module.
pub fn turns_jsonl_path(projects_root: &Path, slug: &str, role: &str) -> PathBuf {
    outbound::turns_jsonl_path(projects_root, slug, role)
}

/// V0.6.5 F146 — outcome of [`register_bot_checked_in`].
#[derive(Debug)]
pub enum RegisterOutcome {
    /// Wrote a fresh registration; on-disk path returned.
    Registered(PathBuf),
    /// `(slug, role)` already had a registration. The file is **not**
    /// clobbered. Caller should surface an `already_registered`
    /// error so the user explicitly unregisters first.
    AlreadyRegistered(PathBuf),
}

/// V0.6.5 F146 — non-clobbering registration used by the MCP tool.
/// Returns [`RegisterOutcome::AlreadyRegistered`] when a registration
/// for `(workflow_slug, role)` already exists on disk. Use
/// [`register_bot_in`] / [`register_bot`] for the idempotent overwrite
/// path the daemon uses.
pub fn register_bot_checked_in(
    ccteam_root: &Path,
    workflow_slug: &str,
    role: &str,
    vendor: AgentVendor,
    im_platform: &str,
    im_chat_id: &str,
    persona_id: Option<&str>,
) -> Result<RegisterOutcome> {
    let path = registration_path_in(ccteam_root, workflow_slug, role);
    if path.exists() {
        return Ok(RegisterOutcome::AlreadyRegistered(path));
    }
    let registration = BotRegistration {
        workflow_slug: workflow_slug.to_string(),
        role: role.to_string(),
        vendor,
        persona_id: persona_id.map(String::from),
        im_platform: im_platform.to_string(),
        im_chat_id: im_chat_id.to_string(),
        created_at: chrono::Utc::now(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create registry dir {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&registration).context("serialize BotRegistration")?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    tracing::info!(slug = workflow_slug, role, "registered bot (checked)");
    Ok(RegisterOutcome::Registered(path))
}

/// Register one bot under an explicit ccteam root (V0.6.5 F146).
/// Idempotent overwrite — see [`register_bot_checked_in`] for the
/// non-clobbering MCP variant.
pub fn register_bot_in(
    ccteam_root: &Path,
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
    let path = registration_path_in(ccteam_root, workflow_slug, role);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create registry dir {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&registration).context("serialize BotRegistration")?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    tracing::info!(slug = workflow_slug, role, "registered bot");
    Ok(path)
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
    register_bot_in(
        &default_ccteam_root(),
        workflow_slug,
        role,
        vendor,
        im_platform,
        im_chat_id,
    )
}

/// V0.6.5 F146 — return `(removed, path)` where `removed=false`
/// means the file was already absent (idempotent miss).
pub fn unregister_bot_in(
    ccteam_root: &Path,
    workflow_slug: &str,
    role: &str,
) -> Result<(bool, PathBuf)> {
    let path = registration_path_in(ccteam_root, workflow_slug, role);
    // V0.6.5 F146 — also remove the sidecar heartbeat so a stale
    // `running: true` doesn't survive an unregister/re-register cycle.
    let hb = bot_heartbeat_path_in(ccteam_root, workflow_slug, role);
    let removed = if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        let _ = fs::remove_file(&hb);
        tracing::info!(slug = workflow_slug, role, "unregistered bot");
        true
    } else {
        false
    };
    Ok((removed, path))
}

/// Unregister one bot. Daemon registry watcher tears down the
/// corresponding tmux session (graceful — writes
/// `signals/shutdown.signal`, lets `close_thread` run idempotently).
pub fn unregister_bot(workflow_slug: &str, role: &str) -> Result<()> {
    unregister_bot_in(&default_ccteam_root(), workflow_slug, role).map(|_| ())
}

/// V0.6.5 F146 — list bots under an explicit root, with optional
/// `workflow_slug` filter.
pub fn list_bots_in(ccteam_root: &Path, filter_slug: Option<&str>) -> Result<Vec<BotRegistration>> {
    let root = registry_root_in(ccteam_root);
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for slug_entry in fs::read_dir(&root)? {
        let slug_entry = slug_entry?;
        if !slug_entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(filter) = filter_slug {
            if slug_entry.file_name().to_string_lossy() != filter {
                continue;
            }
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

/// List every registered bot across all slugs.
pub fn list_bots() -> Result<Vec<BotRegistration>> {
    list_bots_in(&default_ccteam_root(), None)
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

/// Re-export the daemon entry points. `run_daemon_with_shutdown` is the
/// V0.6.1 F130 form `ccteam start` consumes (caller-supplied shutdown
/// future); `run_daemon` is the SIGINT-only convenience wrapper kept
/// for the existing integration-test surface.
pub use daemon::{run_daemon, run_daemon_with_shutdown, DaemonArgs};

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
