//! ccteam-im — V0.6.0 Wave 2 F109 + F116
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
//! - Runtime routing talks to long-running chat sessions through
//!   [`ccteam_harness::HarnessAdapter`]. The default adapter factory
//!   still imports concrete core adapters until the next P1 slice moves
//!   those implementations into `ccteam-harness`.
//! - The daemon **never kills tmux sessions** outside the F116
//!   supervisor crash-restart codepath. User-initiated stop goes
//!   through `<project>/.ccteam/chat/<bot>/signals/shutdown.signal`.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod acl;
pub mod bot_mpsc;
pub mod credentials;
pub mod daemon;
pub mod gateway;
pub mod inbound;
pub mod latency;
pub mod nl_admin;
// v0.8.6 Item 4 — Telegram bot-token onboarding (token validation +
// owner chat_id capture). Wrapped by `ccteam config` (the IM-token menu
// item); the former `ccteam-im-setup` skill's job moves into the CLI.
pub mod onboarding;
pub mod outbound;
pub mod outbound_format;
pub mod pending;
pub mod progress;
pub mod rate_limit;
// v0.8.7 W3 DC.2 — network import of an agency-agents role into a project's
// `.claude/agents/`. The offline catalog (browse/search) lives in
// `ccteam-core`; the HTTP fetch lives here (next to `onboarding`) so the
// primitives leaf stays free of an async HTTP dependency.
pub mod role_import;
pub mod router;
pub mod sanitize;
pub mod supervisor;
pub mod three_layer_sec;
pub mod transport;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use ccteam_harness::AgentVendor;
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
    /// `"discord"`, `"ws"` (local browser/e2e), or `"mock"` (tests).
    pub im_platform: String,
    /// Platform-specific chat identifier (Telegram chat_id, Slack
    /// channel id, Discord channel id). Stored as a string for
    /// platform-agnostic round-tripping.
    pub im_chat_id: String,
    /// Optional IM handle this bot answers to in chat-mode messages.
    /// `None` (legacy / pre-`chat_handle` registrations) falls back to
    /// `role` as the routing handle. The daemon's `build_handle_map`
    /// resolves cross-slug collisions by suffixing the second claimant
    /// with `__<slug>` (double underscore — see
    /// [`crate::router::collision_suffix`] for the rationale; `@` would
    /// be eaten by the mention parser). `chat_register_bot` auto-mints
    /// a scientist nickname from `ccteam_core::agent_naming::SCIENTIST_NAMES`
    /// when the caller omits this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_handle: Option<String>,
    /// Absolute path to the project directory hosting
    /// `.ccteam/workflow.yaml`. When present, daemon resolves bot
    /// working dirs as `<project_dir>/.ccteam/chat/<role>/` directly.
    /// When `None` (legacy registrations pre-`project_dir`), daemon
    /// falls back to the historical
    /// `<projects_root>/<workflow_slug>/.ccteam/chat/<role>/` layout.
    /// The field is additive — `chat_register_bot` MCP accepts it as
    /// an optional input and the dispatcher defaults to
    /// `std::env::current_dir()` (canonicalized) so projects living
    /// outside `~/projects/<slug>/` (NAS shares, dir basename ≠
    /// workflow slug) resolve correctly. Pre-v1.0 — no migration: the
    /// `Option<PathBuf>` branch handles old registrations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<PathBuf>,
    /// RFC3339 timestamp.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Default ccteam root used by the home-derived path helpers
/// (`<HOME>/.ccteam`). The MCP tools and tests reach for the `_in`
/// variants below so they can isolate against a tempdir; daemon /
/// supervisor code stays on the home-derived path.
fn default_ccteam_root() -> PathBuf {
    if let Some(root) = std::env::var_os("CCTEAM_HOME") {
        return PathBuf::from(root);
    }
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

/// Canonical on-disk path of the gateway route-table snapshot
/// (`<ccteam_root>/imd/gateway-state.json`). The daemon persists its tracked
/// sessions here; the read-only `ccteam sessions` CLI view loads them back via
/// [`gateway::tracked_chat_session_names`]. Both sides resolve the path through
/// this one helper so they never drift.
pub fn default_gateway_state_path() -> PathBuf {
    default_ccteam_root().join("imd").join("gateway-state.json")
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

/// V0.6.5 F146 — per-bot heartbeat sidecar under the registry, so a
/// separate MCP tool process can read `running` status off disk. Sibling
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
/// doesn't exist yet (bot registered but no turn taken). Honors
/// `reg.project_dir` (F185) — when set, reads under the absolute
/// project path; otherwise falls back to
/// `<projects_root>/<workflow_slug>/.ccteam/chat/<role>/turns.jsonl`.
pub fn last_turn_at(
    projects_root: &Path,
    reg: &BotRegistration,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let meta = fs::metadata(turns_jsonl_path(projects_root, reg)).ok()?;
    let mtime = meta.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(mtime))
}

/// V0.6.5 F147 — resolve `<project>/.ccteam/chat/<role>/inbox/` for
/// this bot. Legacy mailbox path consumed by supervisor-oriented chat
/// helpers; v8.2 daemon IM ingress routes through the gateway directly.
///
/// F185 — prefers `reg.project_dir` (absolute path) over the historic
/// `<projects_root>/<workflow_slug>/` fallback so projects living
/// outside `~/projects/<slug>/` (NAS shares, dir basename ≠ slug)
/// resolve correctly.
pub fn chat_inbox_dir(projects_root: &Path, reg: &BotRegistration) -> PathBuf {
    reg.chat_dir(projects_root).join("inbox")
}

/// V0.6.5 F147 — resolve `<project>/.ccteam/chat/<role>/turns.jsonl`
/// for this bot. Source-of-truth file `chat_history` tails. Re-exports
/// [`outbound::turns_jsonl_path`] under a top-level name so MCP /
/// integration callers don't have to reach into the outbound module.
/// Honors `reg.project_dir` (F185).
pub fn turns_jsonl_path(projects_root: &Path, reg: &BotRegistration) -> PathBuf {
    outbound::turns_jsonl_path(projects_root, reg)
}

impl BotRegistration {
    /// Effective IM handle this bot answers to. Falls back to `role`
    /// when `chat_handle` is unset (legacy / pre-`chat_handle`
    /// registrations) so older registries route the same way they did
    /// before the schema field landed.
    pub fn effective_handle(&self) -> &str {
        self.chat_handle.as_deref().unwrap_or(&self.role)
    }

    /// Resolve the project root for this bot — the directory holding
    /// `.ccteam/workflow.yaml`. Prefers the registration's explicit
    /// `project_dir` (absolute path written by `chat_register_bot` at
    /// registration time). Falls back to the historical
    /// `<projects_root>/<workflow_slug>/` layout for legacy
    /// registrations that have `project_dir = None`.
    ///
    /// F190 — consult [`project_root_with_config`](Self::project_root_with_config)
    /// when the caller has access to `~/.ccteam/config.yaml::projects[]`
    /// (the slug → path SoT). This base form is kept for callers (MCP
    /// path helpers, unit tests) that don't have the config map on hand.
    pub fn project_root(&self, projects_root: &Path) -> PathBuf {
        match self.project_dir.as_deref() {
            Some(p) => p.to_path_buf(),
            None => projects_root.join(&self.workflow_slug),
        }
    }

    /// V0.6.8 F190 — three-tier project-root resolver. Priority chain:
    ///
    /// 1. `reg.project_dir` (F185 — written by `chat_register_bot`).
    /// 2. `config_projects[slug]` (F190 — `~/.ccteam/config.yaml::projects[]`
    ///    slug → path SoT introduced in V0.4.2 F73).
    /// 3. `<projects_root>/<workflow_slug>/` (historical layout).
    ///
    /// The daemon hands a populated `config_projects` to
    /// [`DefaultMailboxResolver::with_config_projects`] and
    /// [`BotSupervisor::new_with_config`] so legacy registrations (no
    /// `project_dir`) whose project lives outside the home tree
    /// (NAS shares, dir basename ≠ workflow slug) still route correctly
    /// without re-registering every bot.
    pub fn project_root_with_config(
        &self,
        projects_root: &Path,
        config_projects: &HashMap<String, PathBuf>,
    ) -> PathBuf {
        resolve_project_dir(self, projects_root, config_projects)
    }

    /// Resolve `<project>/.ccteam/chat/<role>/` for this bot. The
    /// per-bot working dir used by the supervisor, mailbox writer,
    /// transcript tail, outbound cursor, and all `chat_*` MCP paths.
    pub fn chat_dir(&self, projects_root: &Path) -> PathBuf {
        self.project_root(projects_root)
            .join(".ccteam")
            .join("chat")
            .join(&self.role)
    }

    /// V0.6.8 F190 — config-yaml-aware companion to [`chat_dir`](Self::chat_dir).
    /// Same `.ccteam/chat/<role>/` layout, but the project-root tier-2
    /// lookup picks the path out of `config_projects` when
    /// `reg.project_dir = None`.
    pub fn chat_dir_with_config(
        &self,
        projects_root: &Path,
        config_projects: &HashMap<String, PathBuf>,
    ) -> PathBuf {
        self.project_root_with_config(projects_root, config_projects)
            .join(".ccteam")
            .join("chat")
            .join(&self.role)
    }
}

/// V0.6.8 F190 — three-tier project-root resolver.
///
/// Returns the absolute path of the project this bot lives in. Priority
/// chain (first hit wins):
///
/// 1. `reg.project_dir` — F185 explicit field written by
///    `chat_register_bot` at registration time.
/// 2. `config_projects[reg.workflow_slug]` — F190 lookup against
///    `~/.ccteam/config.yaml::projects[]` (the slug → path SoT V0.4.2
///    F73 introduced). The daemon loads `CcteamConfig` once at startup
///    and builds this map; pass an empty map when the caller has no
///    config available (MCP helpers, unit tests).
/// 3. `projects_root.join(reg.workflow_slug)` — historical
///    `<projects_root>/<slug>/` layout used by pre-F185 registrations.
///
/// MailboxResolver, BotSupervisor, and gateway template registration
/// funnel through this helper so the same priority chain applies
/// everywhere; resolvers don't re-implement the logic.
pub fn resolve_project_dir(
    reg: &BotRegistration,
    projects_root: &Path,
    config_projects: &HashMap<String, PathBuf>,
) -> PathBuf {
    if let Some(p) = reg.project_dir.as_deref() {
        return p.to_path_buf();
    }
    if let Some(p) = config_projects.get(&reg.workflow_slug) {
        return p.clone();
    }
    projects_root.join(&reg.workflow_slug)
}

/// V0.6.8 F190 — load `~/.ccteam/config.yaml::projects[]` into a
/// `slug -> absolute path` map suitable for [`resolve_project_dir`].
///
/// Errors load loud (corrupt yaml is fail-fast — same semantics as
/// `ccteam_core::config::load`). A missing / empty file returns an
/// empty map (no config-yaml tier applies, resolvers fall through to
/// the projects_root tier).
pub fn load_config_projects_map(ccteam_root: &Path) -> Result<HashMap<String, PathBuf>> {
    let cfg = ccteam_core::config::load(ccteam_root)
        .with_context(|| format!("load ccteam config from {}", ccteam_root.display()))?;
    Ok(cfg.projects.into_iter().map(|p| (p.slug, p.path)).collect())
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
///
/// `chat_handle` is the optional IM mention this bot answers to. When
/// `None`, the router falls back to `role` as the handle. MCP callers
/// that pass `None` rely on `dispatch_register_bot` to auto-mint a
/// scientist nickname (`ccteam_core::agent_naming::SCIENTIST_NAMES`)
/// upstream of this call.
#[allow(clippy::too_many_arguments)]
pub fn register_bot_checked_in(
    ccteam_root: &Path,
    workflow_slug: &str,
    role: &str,
    vendor: AgentVendor,
    im_platform: &str,
    im_chat_id: &str,
    persona_id: Option<&str>,
    chat_handle: Option<&str>,
    project_dir: Option<&Path>,
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
        chat_handle: chat_handle.map(String::from),
        project_dir: project_dir.map(PathBuf::from),
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
/// non-clobbering MCP variant. `chat_handle = None` so the bot falls
/// back to `role` as its IM handle until the MCP path is used to mint
/// one.
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
        chat_handle: None,
        project_dir: None,
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

/// V0.8.4 P2b — resolve a bot's "home chat" `(im_platform, im_chat_id)`
/// from the registry, for zero-addressing outbound (`chat_send_file`).
/// The agent knows only its slug/role (ambient `CCTEAM_CHAT_{SLUG,ROLE}`
/// env); the daemon owns chat addressing. Covers the one-bot-one-chat
/// majority; in-turn precision (reply_to) is deferred.
pub fn resolve_home_chat(
    slug: &str,
    role: &str,
    bots: &[BotRegistration],
) -> Option<(String, String)> {
    bots.iter()
        .find(|b| b.workflow_slug == slug && b.role == role)
        .map(|b| (b.im_platform.clone(), b.im_chat_id.clone()))
}

/// Re-export the daemon entry points. `run_daemon_with_shutdown` is the
/// V0.6.1 F130 form `ccteam start` consumes (caller-supplied shutdown
/// future); `run_daemon` is the SIGINT-only convenience wrapper kept
/// for the existing integration-test surface.
pub use daemon::{build_gateway_for_daemon, run_daemon, run_daemon_with_shutdown, DaemonArgs};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_path_layout() {
        let p = registration_path("dev-foo", "lead");
        assert!(p.ends_with(".ccteam/imd/registry/dev-foo/lead.json"));
    }

    fn reg(slug: &str, role: &str, platform: &str, chat: &str) -> BotRegistration {
        BotRegistration {
            workflow_slug: slug.into(),
            role: role.into(),
            vendor: ccteam_harness::AgentVendor::Claude,
            persona_id: None,
            im_platform: platform.into(),
            im_chat_id: chat.into(),
            chat_handle: None,
            project_dir: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn resolve_home_chat_matches_slug_and_role() {
        let bots = vec![
            reg("dev-foo", "lead", "telegram", "chat-1"),
            reg("dev-foo", "reviewer", "telegram", "chat-2"),
            reg("dev-bar", "lead", "web", "web-9"),
        ];
        assert_eq!(
            resolve_home_chat("dev-foo", "reviewer", &bots),
            Some(("telegram".to_string(), "chat-2".to_string()))
        );
        assert_eq!(
            resolve_home_chat("dev-bar", "lead", &bots),
            Some(("web".to_string(), "web-9".to_string()))
        );
        // Unregistered slug/role → None (caller surfaces a failure).
        assert_eq!(resolve_home_chat("dev-foo", "ghost", &bots), None);
        assert_eq!(resolve_home_chat("nope", "lead", &bots), None);
    }
}
