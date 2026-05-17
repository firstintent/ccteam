//! V0.5.0 F97 — Advanced path lifecycle completion tests.
//!
//! Covers `cleanup_on_stop` enum (3 strategies), the F82 hot-reload
//! classifier (hot vs cold), and the snapshot-driven `--restart-team`
//! recovery path. The full E2E flow (`claude --bg` spawn + real
//! `claude attach` exec) needs a live Claude Code binary and is
//! deferred to Wave 4 host-level E2E (same scope split as F93b).
//!
//! Coverage matrix (PRD F97 §验收):
//!   1. cleanup_on_stop force-kill / ask-lead / leave-running parse +
//!      round-trip
//!   2. AgentTeamSpec::classify_reload hot fields → None
//!   3. AgentTeamSpec::classify_reload cold field team_name → Some
//!   4. AgentTeamSpec::classify_reload cold field
//!      suggested_teammates[].role → Some
//!   5. AgentTeamSpec::classify_reload cold field
//!      suggested_teammates[].kind / spawn_brief → Some
//!   6. Default cleanup_on_stop is ForceKill (V0.4.6 compat)
//!
//! The CLI-side run_stop_slug + restart-team handlers are exercised
//! via integration tests at the CLI level (see
//! `crates/ccteam-cli/src/commands.rs::tests::cleanup_on_stop_*`).

use ccteam_core::workflow::{
    AgentTeamSpec, CleanupOnStop, SuggestedTeammate, SuggestedTeammateKind, WorkflowMode,
    WorkflowSpec,
};

fn base_team() -> AgentTeamSpec {
    AgentTeamSpec {
        team_name: "demo".into(),
        lead_seed: "investigate flaky tests".into(),
        teammate_mode: Some("in-process".into()),
        cleanup_on_stop: CleanupOnStop::ForceKill,
        snapshot_path: None,
        suggested_teammates: vec![
            SuggestedTeammate {
                role: "code-reviewer".into(),
                kind: SuggestedTeammateKind::Definition,
                spawn_brief: "review pls".into(),
                adhoc_model: None,
                adhoc_color: None,
                adhoc_tools: None,
            },
            SuggestedTeammate {
                role: "db-expert".into(),
                kind: SuggestedTeammateKind::AdHoc,
                spawn_brief: "you are a PG expert".into(),
                adhoc_model: Some("sonnet".into()),
                adhoc_color: Some("purple".into()),
                adhoc_tools: Some(vec!["Read".into(), "Bash".into()]),
            },
        ],
        auto_spawn_teammates: false,
    }
}

// ---- cleanup_on_stop parsing -------------------------------------------

#[test]
fn cleanup_on_stop_force_kill_parses() {
    let yaml = r#"
name: x
mode: agent-team
agent_team:
  team_name: x
  lead_seed: y
  cleanup_on_stop: force-kill
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("force-kill should parse");
    assert_eq!(spec.mode, WorkflowMode::AgentTeam);
    assert_eq!(
        spec.agent_team.unwrap().cleanup_on_stop,
        CleanupOnStop::ForceKill,
    );
}

#[test]
fn cleanup_on_stop_ask_lead_parses() {
    let yaml = r#"
name: x
mode: agent-team
agent_team:
  team_name: x
  lead_seed: y
  cleanup_on_stop: ask-lead
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("ask-lead should parse");
    assert_eq!(
        spec.agent_team.unwrap().cleanup_on_stop,
        CleanupOnStop::AskLead,
    );
}

#[test]
fn cleanup_on_stop_leave_running_parses() {
    let yaml = r#"
name: x
mode: agent-team
agent_team:
  team_name: x
  lead_seed: y
  cleanup_on_stop: leave-running
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("leave-running should parse");
    assert_eq!(
        spec.agent_team.unwrap().cleanup_on_stop,
        CleanupOnStop::LeaveRunning,
    );
}

#[test]
fn cleanup_on_stop_default_is_force_kill_when_omitted() {
    // V0.4.6 backwards-compat: workflow.yaml without cleanup_on_stop
    // must default to ForceKill (matches V0.4.6 + F93b MVP semantics).
    let yaml = r#"
name: x
mode: agent-team
agent_team:
  team_name: x
  lead_seed: y
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("default cleanup_on_stop should parse");
    assert_eq!(
        spec.agent_team.unwrap().cleanup_on_stop,
        CleanupOnStop::ForceKill,
    );
}

#[test]
fn cleanup_on_stop_unknown_value_errors() {
    let yaml = r#"
name: x
mode: agent-team
agent_team:
  team_name: x
  lead_seed: y
  cleanup_on_stop: wrecking-ball
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).expect_err("unknown strategy should fail");
    // serde error mentions the enum / unknown variant
    let msg = format!("{err}");
    assert!(
        msg.contains("wrecking-ball") || msg.contains("unknown") || msg.contains("CleanupOnStop"),
        "error must mention bad value, got: {msg}",
    );
}

#[test]
fn cleanup_on_stop_as_str_round_trips() {
    assert_eq!(CleanupOnStop::ForceKill.as_str(), "force-kill");
    assert_eq!(CleanupOnStop::AskLead.as_str(), "ask-lead");
    assert_eq!(CleanupOnStop::LeaveRunning.as_str(), "leave-running");
}

// ---- classify_reload hot/cold ------------------------------------------

#[test]
fn hot_reload_lead_seed_change_is_hot() {
    let old = base_team();
    let mut new = old.clone();
    new.lead_seed = "completely new mission".into();
    assert_eq!(
        old.classify_reload(&new),
        None,
        "lead_seed change must be hot (None) — applied via lead inbox",
    );
}

#[test]
fn hot_reload_teammate_mode_change_is_hot() {
    let old = base_team();
    let mut new = old.clone();
    new.teammate_mode = Some("tmux".into());
    assert_eq!(old.classify_reload(&new), None);
}

#[test]
fn hot_reload_adhoc_color_change_is_hot() {
    let old = base_team();
    let mut new = old.clone();
    new.suggested_teammates[1].adhoc_color = Some("teal".into());
    assert_eq!(
        old.classify_reload(&new),
        None,
        "cosmetic adhoc_color change must be hot",
    );
}

#[test]
fn hot_reload_cleanup_on_stop_change_is_hot() {
    // cleanup_on_stop is consulted at `ccteam stop` time, not at
    // spawn time — changing it doesn't require restarting the lead.
    let old = base_team();
    let mut new = old.clone();
    new.cleanup_on_stop = CleanupOnStop::AskLead;
    assert_eq!(old.classify_reload(&new), None);
}

#[test]
fn cold_reload_team_name_change_emits_cold_reload_required() {
    let old = base_team();
    let mut new = old.clone();
    new.team_name = "different-team".into();
    let reason = old.classify_reload(&new).expect("team_name change is cold");
    assert!(
        reason.contains("team_name"),
        "reason should mention team_name; got: {reason}",
    );
}

#[test]
fn cold_reload_topology_role_change_emits_cold_reload_required() {
    let old = base_team();
    let mut new = old.clone();
    new.suggested_teammates[0].role = "security-reviewer".into();
    let reason = old.classify_reload(&new).expect("role rename is cold");
    assert!(
        reason.contains("role"),
        "reason should mention role; got: {reason}",
    );
}

#[test]
fn cold_reload_topology_kind_change_emits_cold_reload_required() {
    let old = base_team();
    let mut new = old.clone();
    new.suggested_teammates[0].kind = SuggestedTeammateKind::AdHoc;
    let reason = old.classify_reload(&new).expect("kind flip is cold");
    assert!(
        reason.contains("kind"),
        "reason should mention kind; got: {reason}",
    );
}

#[test]
fn cold_reload_spawn_brief_change_emits_cold_reload_required() {
    let old = base_team();
    let mut new = old.clone();
    new.suggested_teammates[1].spawn_brief = "totally new brief".into();
    let reason = old
        .classify_reload(&new)
        .expect("spawn_brief rewrite is cold");
    assert!(
        reason.contains("spawn_brief"),
        "reason should mention spawn_brief; got: {reason}",
    );
}

#[test]
fn cold_reload_teammate_count_change_emits_cold_reload_required() {
    let old = base_team();
    let mut new = old.clone();
    new.suggested_teammates.pop();
    let reason = old
        .classify_reload(&new)
        .expect("teammate count change is cold");
    assert!(
        reason.contains("count") || reason.contains("suggested_teammates"),
        "reason should mention count change; got: {reason}",
    );
}

#[test]
fn hot_reload_no_change_returns_none() {
    let old = base_team();
    let new = old.clone();
    assert_eq!(old.classify_reload(&new), None);
}
