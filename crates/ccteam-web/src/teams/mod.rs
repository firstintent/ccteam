//! V0.5.0 F96 — Agent Teams read-only data layer.
//!
//! This module wraps Anthropic's `~/.claude/teams/<>/` SoT for the
//! ccteam-web Teams tab. **READ-ONLY**: every helper here parses and
//! emits Rust structs; nothing writes back to `~/.claude/teams/` or
//! `~/.claude/tasks/` (PRD V0.5.0 §整体红线 1 — the official docs warn
//! the directory is rewritten on state update).
//!
//! Layout under `<claude_home>/`:
//!
//! - `teams/<team_name>/config.json` — team metadata + `members[]`.
//! - `teams/<team_name>/inboxes/<teammate>.json` — JSON array of
//!   messages, schema `{from, text, timestamp, color, read}`. The
//!   `text` field may be a JSON-stringified `{type:"idle_notification",
//!   ...}` system message; F95 watcher splits those into a separate
//!   event stream — F96 panels just hide them from the Mailbox view.
//! - `tasks/<team_name>/*.json` — one file per task with `status`
//!   (pending / in_progress / completed) and an open schema. We pick a
//!   tolerant superset (`title|subject`, `owner|assignee`,
//!   `dependencies|blockedBy`).
//!
//! `claude_home()` is the single resolver: env `CCTEAM_CLAUDE_HOME`
//! (test seam) overrides `$HOME/.claude`. Tests stage a tempdir + set
//! the env so `~/.claude/teams/` access is fully sandboxed.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

pub mod discovery;
pub mod inbox;
pub mod subagent_resolver;
pub mod tasks;

#[cfg(test)]
mod tests;

pub use discovery::{discover_teams, load_team_config, MemberView, TeamConfig, TeamListEntry};
pub use inbox::{load_inbox, InboxMessage};
pub use subagent_resolver::{resolve_definition, AgentDefinition, ResolvedScope};
pub use tasks::{load_tasks, TaskCounts, TaskView};

/// Resolve Anthropic's `~/.claude/` root. Tests override with
/// `CCTEAM_CLAUDE_HOME`. This is **distinct** from `CCTEAM_HOME`
/// (`~/.ccteam/`) — Anthropic's directory is the SoT we mirror from,
/// not the ccteam state dir.
pub fn claude_home() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("CCTEAM_CLAUDE_HOME") {
        return Ok(PathBuf::from(s));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    Ok(home.join(".claude"))
}

/// `<claude_home>/teams/` — directory holding every Agent Team's
/// metadata. Missing-on-disk is a valid empty state (`/api/v1/teams`
/// returns `[]`).
///
/// Honours F95's `CCTEAM_AGENT_TEAMS_ROOT` env override so the
/// integration tests staged by the F95 watcher remain compatible
/// (they point this env at a tempdir + expect both watcher and web
/// API to converge on the same directory).
pub fn teams_root(claude_home: &std::path::Path) -> PathBuf {
    if let Ok(s) = std::env::var("CCTEAM_AGENT_TEAMS_ROOT") {
        return PathBuf::from(s);
    }
    claude_home.join("teams")
}

/// `<claude_home>/tasks/<team>/` — per-team Kanban task files. Missing
/// is normal (team with no tasks). Honours `CCTEAM_AGENT_TASKS_ROOT`
/// to match F95's watcher test contract.
pub fn tasks_root(claude_home: &std::path::Path, team: &str) -> PathBuf {
    if let Ok(s) = std::env::var("CCTEAM_AGENT_TASKS_ROOT") {
        return PathBuf::from(s).join(team);
    }
    claude_home.join("tasks").join(team)
}

/// Truncate a JSON-array message preview to N characters with an
/// ellipsis. Used by `/api/v1/teams/<name>` recent-messages snippet.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
