//! V0.5.0 F95 — parser + differ for `~/.claude/teams/<team>/config.json`.
//!
//! Anthropic writes the team's full topology as a single JSON file
//! (sample shapes pinned in `crates/ccteam-core/tests/fixtures/agent_teams/`).
//! This module is **pure** — it consumes raw bytes plus the previous
//! snapshot and returns the list of `team_member_joined` /
//! `team_member_left` events the watcher should emit. No IO, no
//! filesystem side effects.
//!
//! ## Red lines (PRD V0.5.0 F95 §需求)
//!
//! 1. **Read-only** — Anthropic owns the file; ccteam never writes it.
//! 2. **Schema-failure tolerance** — `parse_config` returns
//!    `Result<TeamConfigSnapshot>`. The caller WARNs once and degrades
//!    the team to mtime-only watch (still in discovery list, but
//!    events suppressed) instead of panicking.
//! 3. **`definition_backed` rule** — `agentType ∈ {"general-purpose",
//!    "team-lead"}` ⇒ `false` (ad-hoc / lead). Anything else ⇒ `true`
//!    (definition-backed subagent). PRD F95 §需求 .4.
//!
//! ## V0.5.0 SoT
//!
//! These events land in `~/.ccteam/teams-progress.jsonl` (the **global**
//! team-progress file documented in [`crate::paths::teams_progress_path`]).
//! Project-scoped `progress.jsonl` is workflow-bound and stays
//! unchanged; F95 events are not tied to any ccteam workflow.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

/// One member entry inside `config.json::members[]`. Field names mirror
/// the Anthropic schema (camelCase). Optional fields (`color`,
/// `agentType`, `model`, `cwd`, `backendType`) default to empty string
/// when absent so `definition_backed` + emit always have something to
/// hand to the event payload.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    /// Unique within the team (typically `<name>@<team>`).
    pub agent_id: String,
    /// Display name (the part before `@<team>`).
    pub name: String,
    /// Anthropic's subagent type marker. The empty default keeps
    /// `definition_backed` deterministic for malformed entries.
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub backend_type: String,
    /// Epoch milliseconds; converted to RFC3339 on emit.
    #[serde(default)]
    pub joined_at: i64,
}

/// Parsed snapshot of one team's `config.json`. We keep `members` in a
/// `BTreeMap` keyed by `agent_id` so diffs are deterministic and
/// O(member_count) joinable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamConfigSnapshot {
    pub name: String,
    pub members: BTreeMap<String, TeamMember>,
}

/// Wire-format deserialiser. Mirrors `config.json` exactly; conversion
/// to `TeamConfigSnapshot` happens in [`parse_config`] so we control
/// the diff index (BTreeMap) without polluting serde's shape.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawConfig {
    name: String,
    #[serde(default)]
    members: Vec<TeamMember>,
}

/// Parse a `config.json` byte buffer into a [`TeamConfigSnapshot`].
///
/// On schema break (truncation, type mismatch, missing `name`) this
/// returns `Err`; the watcher caller is expected to log once and skip
/// further events until the file recovers. Pure — no IO.
pub fn parse_config(bytes: &[u8]) -> Result<TeamConfigSnapshot> {
    let raw: RawConfig =
        serde_json::from_slice(bytes).context("teams_config_parser: deserialize config.json")?;
    let members = raw
        .members
        .into_iter()
        .map(|m| (m.agent_id.clone(), m))
        .collect();
    Ok(TeamConfigSnapshot {
        name: raw.name,
        members,
    })
}

/// True when `agentType` is **not** one of the ad-hoc / lead markers
/// (`general-purpose`, `team-lead`). PRD F95 §需求 .4 — F96 Web
/// Topology uses this to decide whether to show "↗ definition link" or
/// "📝 ad-hoc badge".
pub fn definition_backed_for(agent_type: &str) -> bool {
    !matches!(agent_type, "general-purpose" | "team-lead")
}

/// Diff `prev` against `next` and produce the list of events the
/// watcher should append to `~/.ccteam/teams-progress.jsonl`. Order
/// inside the returned `Vec`:
///
/// 1. `team_member_joined` for every member in `next` not in `prev`,
///    sorted by `agent_id` for determinism.
/// 2. `team_member_left` for every member in `prev` not in `next`,
///    same sort key.
///
/// `team_name` is taken from `next.name`. The caller is responsible
/// for time-of-discovery handling: when the watcher is first seeing
/// the team (cold start), pass `prev = Default::default()` to emit
/// `team_member_joined` for every current member (PRD F95 §验收 .2).
pub fn diff_snapshots(prev: &TeamConfigSnapshot, next: &TeamConfigSnapshot) -> Vec<Value> {
    let mut events = Vec::new();
    // Joined: in next, not in prev. BTreeMap iteration is sorted by key.
    for (id, member) in &next.members {
        if !prev.members.contains_key(id) {
            events.push(member_joined_event(&next.name, member));
        }
    }
    // Left: in prev, not in next.
    for (id, member) in &prev.members {
        if !next.members.contains_key(id) {
            events.push(member_left_event(&next.name, &member.name));
        }
    }
    events
}

fn member_joined_event(team_name: &str, m: &TeamMember) -> Value {
    let started_at = format_epoch_ms(m.joined_at);
    let now_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    json!({
        "event": "team_member_joined",
        "ts": now_ts,
        "team_name": team_name,
        "teammate_name": m.name,
        "agent_id": m.agent_id,
        "agent_type": m.agent_type,
        "model": m.model,
        "color": m.color,
        "cwd": m.cwd,
        "backend_type": m.backend_type,
        "definition_backed": definition_backed_for(&m.agent_type),
        "started_at": started_at,
    })
}

fn member_left_event(team_name: &str, teammate_name: &str) -> Value {
    let now_ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    json!({
        "event": "team_member_left",
        "ts": now_ts,
        "team_name": team_name,
        "teammate_name": teammate_name,
    })
}

/// Convert Anthropic's epoch-milliseconds timestamp to RFC3339 (UTC,
/// seconds precision, `Z` suffix). Falls back to "now" for 0 / negative
/// values which signal "field absent" in the wire format.
fn format_epoch_ms(epoch_ms: i64) -> String {
    if epoch_ms <= 0 {
        return Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    }
    let secs = epoch_ms / 1000;
    let nsec = ((epoch_ms % 1000).abs() * 1_000_000) as u32;
    match Utc.timestamp_opt(secs, nsec) {
        chrono::LocalResult::Single(dt) => dt.to_rfc3339_opts(SecondsFormat::Secs, true),
        _ => Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    }
}

/// Convert an RFC3339 string (for tests that need to round-trip a
/// timestamp value).
#[allow(dead_code)]
fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, name: &str, agent_type: &str) -> TeamMember {
        TeamMember {
            agent_id: id.to_string(),
            name: name.to_string(),
            agent_type: agent_type.to_string(),
            model: "sonnet".into(),
            color: Some("blue".into()),
            cwd: "/tmp".into(),
            backend_type: "in-process".into(),
            joined_at: 1_700_000_000_000,
        }
    }

    fn snap(name: &str, members: Vec<TeamMember>) -> TeamConfigSnapshot {
        TeamConfigSnapshot {
            name: name.into(),
            members: members
                .into_iter()
                .map(|m| (m.agent_id.clone(), m))
                .collect(),
        }
    }

    #[test]
    fn definition_backed_rule_excludes_general_purpose_and_team_lead() {
        assert!(!definition_backed_for("general-purpose"));
        assert!(!definition_backed_for("team-lead"));
        assert!(definition_backed_for("code-reviewer"));
        assert!(definition_backed_for("security-reviewer"));
        // Empty string (missing field default) is treated as "unknown
        // subagent type" → definition-backed = true. This is the safer
        // default; F96 still needs to render a badge.
        assert!(definition_backed_for(""));
    }

    #[test]
    fn diff_cold_start_emits_joined_for_all_members() {
        let prev = TeamConfigSnapshot::default();
        let next = snap(
            "roblog",
            vec![
                member("a@roblog", "a", "general-purpose"),
                member("b@roblog", "b", "general-purpose"),
            ],
        );
        let events = diff_snapshots(&prev, &next);
        assert_eq!(events.len(), 2);
        for e in &events {
            assert_eq!(e["event"], "team_member_joined");
            assert_eq!(e["team_name"], "roblog");
            assert_eq!(e["definition_backed"], false);
        }
    }

    #[test]
    fn diff_removed_member_emits_left() {
        let prev = snap(
            "roblog",
            vec![
                member("a@roblog", "a", "general-purpose"),
                member("b@roblog", "b", "general-purpose"),
            ],
        );
        let next = snap("roblog", vec![member("a@roblog", "a", "general-purpose")]);
        let events = diff_snapshots(&prev, &next);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "team_member_left");
        assert_eq!(events[0]["teammate_name"], "b");
    }

    #[test]
    fn diff_no_changes_returns_empty() {
        let s = snap("roblog", vec![member("a@roblog", "a", "general-purpose")]);
        assert!(diff_snapshots(&s, &s).is_empty());
    }

    #[test]
    fn parse_minimal_config() {
        let bytes = br#"{"name": "demo", "members": []}"#;
        let snap = parse_config(bytes).unwrap();
        assert_eq!(snap.name, "demo");
        assert!(snap.members.is_empty());
    }

    #[test]
    fn parse_broken_json_returns_err() {
        let bytes = b"{not json";
        assert!(parse_config(bytes).is_err());
    }

    #[test]
    fn epoch_ms_zero_falls_back_to_now() {
        // Should produce a valid RFC3339 string (now), not panic.
        let s = format_epoch_ms(0);
        assert!(s.contains('T'));
        assert!(s.ends_with('Z'));
    }

    #[test]
    fn epoch_ms_known_value_round_trip() {
        // 1_700_000_000_000 ms = 2023-11-14 22:13:20 UTC.
        let s = format_epoch_ms(1_700_000_000_000);
        assert_eq!(s, "2023-11-14T22:13:20Z");
    }

    #[test]
    fn roblog_fixture_all_members_ad_hoc() {
        // PRD F95 §验收 .2 — host roblog has 5 members, all
        // general-purpose / team-lead → all `definition_backed=false`.
        let bytes = include_bytes!("../tests/fixtures/agent_teams/config-roblog.json");
        let snap = parse_config(bytes).unwrap();
        assert_eq!(snap.name, "roblog");
        assert_eq!(snap.members.len(), 5);
        for member in snap.members.values() {
            assert!(
                !definition_backed_for(&member.agent_type),
                "fixture member {} has unexpected agent_type {}",
                member.name,
                member.agent_type,
            );
        }
        // Diff against empty prev → 5 joined events.
        let events = diff_snapshots(&TeamConfigSnapshot::default(), &snap);
        assert_eq!(events.len(), 5);
        for e in events {
            assert_eq!(e["event"], "team_member_joined");
            assert_eq!(e["team_name"], "roblog");
            assert_eq!(e["definition_backed"], false);
        }
    }
}
