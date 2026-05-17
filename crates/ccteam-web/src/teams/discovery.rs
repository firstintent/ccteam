//! V0.5.0 F96 — Agent Teams discovery + config.json parser.
//!
//! Authoritative shape: `crates/ccteam-core/tests/fixtures/agent_teams/
//! config-roblog.json` (live-probed off `ssh rob@host` Aug 2026,
//! matches the host's `~/.claude/teams/roblog/config.json`). We use a
//! tolerant superset:
//!
//! - `name` / `description` / `createdAt` / `leadAgentId` /
//!   `leadSessionId` — all optional in spec; required-ish in practice.
//! - `members[]` — per-member fields `agentId`, `name`, `agentType`,
//!   `model`, `color` (lead has no color in the live fixture),
//!   `prompt` (only present on ad-hoc members), `cwd`, `joinedAt`,
//!   `subscriptions[]`, `tmuxPaneId`, `backendType`,
//!   `planModeRequired`.
//!
//! `definition_backed` is computed here (PRD §F95 — lives on the wire
//! schema even though the actual computation is host-side once F95
//! ships; until then F96 derives it inline for the API surface):
//!
//! - `agentType ∈ {"general-purpose", "team-lead"}` → `false` (ad-hoc;
//!   prompt is inline in `config.json::members[i].prompt`).
//! - Anything else (`"code-reviewer"`, `"security-reviewer"`, ...) →
//!   `true` (definition-backed; F96 SPA renders an
//!   `↗ definition` link).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::teams_root;

/// One row in `GET /api/v1/teams`. Compact summary; the detail
/// endpoint returns the full `TeamConfig`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamListEntry {
    pub name: String,
    pub description: Option<String>,
    pub member_count: usize,
    /// `joinedAt` of the most-recent member as RFC3339, or `None` if
    /// no members or no timestamps available. Sorted-on top of the
    /// list lets the SPA show "most-recently active" without an extra
    /// fetch.
    pub last_activity: Option<String>,
}

/// Parsed `<claude_home>/teams/<team>/config.json`. Shape mirrors the
/// host live-probe (see fixture). All fields are owned `String`s so
/// the struct is `Serialize` for the wire + reusable across requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JS `Date.now()` in ms. Optional so a malformed config still
    /// renders cards.
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<i64>,
    #[serde(default, rename = "leadAgentId")]
    pub lead_agent_id: Option<String>,
    #[serde(default, rename = "leadSessionId")]
    pub lead_session_id: Option<String>,
    #[serde(default)]
    pub members: Vec<MemberView>,
}

/// One `config.json::members[i]` entry, decorated with the
/// F96-derived `definition_backed` flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemberView {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub name: String,
    #[serde(default, rename = "agentType")]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    /// JS `Date.now()` in ms; lead's `joinedAt` doubles as
    /// `createdAt` for the team.
    #[serde(default, rename = "joinedAt")]
    pub joined_at: Option<i64>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Set on ad-hoc teammates (`agentType ∈ {"general-purpose",
    /// "team-lead"}`). Definition-backed members get their full
    /// prompt from `.claude/agents/<agentType>.md`, so this field
    /// is absent.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub subscriptions: Vec<String>,
    #[serde(default, rename = "tmuxPaneId")]
    pub tmux_pane_id: Option<String>,
    #[serde(default, rename = "backendType")]
    pub backend_type: Option<String>,
    #[serde(default, rename = "planModeRequired")]
    pub plan_mode_required: Option<bool>,
    /// Computed by `compute_definition_backed`. Wire-visible so the
    /// SPA Topology can branch on it directly.
    #[serde(default = "default_definition_backed")]
    pub definition_backed: bool,
}

fn default_definition_backed() -> bool {
    false
}

/// PRD §F95 `definition_backed` rule.
pub fn compute_definition_backed(agent_type: Option<&str>) -> bool {
    match agent_type {
        None => false,
        Some(t) => !matches!(t, "general-purpose" | "team-lead"),
    }
}

/// `<claude_home>/teams/` → enumerate every direct subdir whose
/// `config.json` parses. Malformed configs are surfaced as a
/// `TeamListEntry` with `member_count = 0` so the SPA shows an empty
/// card instead of swallowing the team. Missing teams root → `Ok([])`
/// (no-team host is valid).
pub fn discover_teams(claude_home: &Path) -> Result<Vec<TeamListEntry>> {
    let root = teams_root(claude_home);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)
        .with_context(|| format!("read_dir {}", root.display()))?
        .flatten()
    {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let team_name = match p.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let cfg_path = p.join("config.json");
        if !cfg_path.exists() {
            // Skip non-team dirs that happen to live alongside.
            continue;
        }
        match load_team_config_from(&cfg_path) {
            Ok(cfg) => {
                let last_activity = cfg
                    .members
                    .iter()
                    .filter_map(|m| m.joined_at)
                    .max()
                    .map(epoch_ms_to_rfc3339);
                out.push(TeamListEntry {
                    name: cfg.name.clone(),
                    description: cfg.description.clone(),
                    member_count: cfg.members.len(),
                    last_activity,
                });
            }
            Err(err) => {
                tracing::warn!(team = %team_name, error = %err, "config.json parse failed; degrading to mtime-only");
                // PRD §F95 acceptance #5 — schema fail must still
                // expose the team in the listing.
                let last_activity = fs::metadata(&cfg_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(system_time_to_rfc3339);
                out.push(TeamListEntry {
                    name: team_name,
                    description: None,
                    member_count: 0,
                    last_activity,
                });
            }
        }
    }
    // Stable-sort by name so the SPA list is deterministic.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// `<claude_home>/teams/<name>/config.json` → parsed `TeamConfig`
/// with `members[].definition_backed` filled in.
pub fn load_team_config(claude_home: &Path, name: &str) -> Result<TeamConfig> {
    let path = teams_root(claude_home).join(name).join("config.json");
    load_team_config_from(&path)
}

pub(crate) fn load_team_config_from(path: &Path) -> Result<TeamConfig> {
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut cfg: TeamConfig =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    // Decorate members with the derived `definition_backed` flag.
    for m in &mut cfg.members {
        m.definition_backed = compute_definition_backed(m.agent_type.as_deref());
    }
    Ok(cfg)
}

/// Path to a member's definition `.md` file *if* the member is
/// definition-backed. Returns `None` for ad-hoc members.
pub fn definition_md_target(member: &MemberView) -> Option<PathBuf> {
    if !member.definition_backed {
        return None;
    }
    let agent_type = member.agent_type.as_deref()?;
    Some(PathBuf::from(format!(".claude/agents/{agent_type}.md")))
}

fn epoch_ms_to_rfc3339(ms: i64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| ms.to_string())
}

fn system_time_to_rfc3339(t: SystemTime) -> String {
    use chrono::{DateTime, Utc};
    let dt: DateTime<Utc> = t.into();
    dt.to_rfc3339()
}
