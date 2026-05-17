//! V0.5.0 F93b + F94 — Schema + scaffold tests for `workflow.yaml
//! mode: agent-team`. The full E2E spawn flow (claude --bg + plan-first
//! verification + real `claude attach` exec) needs a live Claude Code
//! binary and is deferred to Wave 4 host-level E2E. This file covers
//! the pure-data + filesystem assertions from PRD F93b §验收 1-12 +
//! F94 §验收 1-7 that can be validated in-process.
//!
//! Coverage matrix:
//!   1. `mode` field default + agent-team round-trip
//!   2. Parsing the shipped workflow.agent-team.yaml template
//!   3. Validation of agent-team mode (team_name + lead_seed required)
//!   4. Ad-hoc teammate requires adhoc_model
//!   5. Mode missing → defaults to artifact-driven (V0.4.6 compat)
//!   6. Agent team mode accepts empty agents map
//!   7. Artifact-driven mode rejects agent_team block
//!   8. Settings template for agent-team has 3 new hooks
//!   9. PROJECT_SETTINGS_AGENT_TEAM_JSON contains all 3 hook entries
//!   10. F94 hook progress-append accepts the 3 new event_type strings
//!       (delegated to progress_append unit; tested in ccteam-hooks)

use std::path::Path;

use ccteam_core::workflow::{
    AgentTeamSpec, SuggestedTeammate, SuggestedTeammateKind, WorkflowMode, WorkflowSpec,
};

#[test]
fn mode_field_defaults_to_artifact_driven_when_omitted() {
    // V0.4.6 backwards-compat: an existing workflow.yaml without
    // `mode:` parses as ArtifactDriven.
    let yaml = r#"
name: legacy-workflow
agents:
  explorer:
    trigger: manual
    executor: claude
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("legacy yaml should parse");
    assert_eq!(spec.mode, WorkflowMode::ArtifactDriven);
    assert!(spec.agent_team.is_none());
}

#[test]
fn explicit_artifact_driven_mode_parses() {
    let yaml = r#"
name: explicit
mode: artifact-driven
agents:
  explorer:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("artifact-driven yaml should parse");
    assert_eq!(spec.mode, WorkflowMode::ArtifactDriven);
}

#[test]
fn agent_team_mode_parses_with_full_spec() {
    let yaml = r#"
name: flaky-debate
mode: agent-team
agent_team:
  team_name: flaky-debate
  lead_seed: |
    Investigate why integration tests in src/auth/ flake.
    Suggested teammates below.
  teammate_mode: in-process
  cleanup_on_stop: force-kill
  snapshot_path: .ccteam/team-snapshot.json
  auto_spawn_teammates: false
  suggested_teammates:
    - role: code-reviewer
      kind: definition
      spawn_brief: |
        Review the auth changes specifically.
    - role: db-expert
      kind: ad-hoc
      spawn_brief: |
        You are a PostgreSQL expert. Report findings via SendMessage.
      adhoc_model: sonnet
      adhoc_color: purple
      adhoc_tools: [Read, Grep, Bash]
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("agent-team yaml should parse");
    assert_eq!(spec.mode, WorkflowMode::AgentTeam);
    let team = spec.agent_team.as_ref().expect("agent_team block present");
    assert_eq!(team.team_name, "flaky-debate");
    assert!(team.lead_seed.contains("Investigate why"));
    assert_eq!(team.teammate_mode.as_deref(), Some("in-process"));
    assert_eq!(team.cleanup_on_stop.as_deref(), Some("force-kill"));
    assert!(!team.auto_spawn_teammates);
    assert_eq!(team.suggested_teammates.len(), 2);
    assert_eq!(team.suggested_teammates[0].role, "code-reviewer");
    assert_eq!(
        team.suggested_teammates[0].kind,
        SuggestedTeammateKind::Definition,
    );
    assert_eq!(team.suggested_teammates[1].role, "db-expert");
    assert_eq!(
        team.suggested_teammates[1].kind,
        SuggestedTeammateKind::AdHoc,
    );
    assert_eq!(
        team.suggested_teammates[1].adhoc_model.as_deref(),
        Some("sonnet"),
    );
}

#[test]
fn agent_team_mode_rejects_missing_team_name() {
    let yaml = r#"
name: bad
mode: agent-team
agent_team:
  team_name: ""
  lead_seed: do something
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).unwrap_err();
    assert!(format!("{err}").contains("team_name"));
}

#[test]
fn agent_team_mode_rejects_empty_lead_seed() {
    let yaml = r#"
name: bad
mode: agent-team
agent_team:
  team_name: ok-name
  lead_seed: ""
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).unwrap_err();
    assert!(format!("{err}").contains("lead_seed"));
}

#[test]
fn agent_team_mode_accepts_empty_agents_map() {
    let yaml = r#"
name: just-team
mode: agent-team
agent_team:
  team_name: just-team
  lead_seed: do something useful
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("agent-team allows empty agents");
    assert_eq!(spec.agents.len(), 0);
}

#[test]
fn artifact_driven_mode_rejects_agent_team_block() {
    // mode unspecified → artifact-driven; agent_team block is a config
    // error.
    let yaml = r#"
name: mixed
agent_team:
  team_name: x
  lead_seed: y
agents:
  explorer:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).unwrap_err();
    assert!(format!("{err}").contains("agent_team block is only valid"));
}

#[test]
fn agent_team_mode_rejects_missing_agent_team_block() {
    let yaml = r#"
name: x
mode: agent-team
agents:
  explorer:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).unwrap_err();
    assert!(format!("{err}").contains("requires an `agent_team` block"));
}

#[test]
fn agent_team_adhoc_teammate_requires_model() {
    let yaml = r#"
name: x
mode: agent-team
agent_team:
  team_name: x
  lead_seed: do
  suggested_teammates:
    - role: adhoc-no-model
      kind: ad-hoc
      spawn_brief: |
        You are stuff.
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).unwrap_err();
    assert!(format!("{err}").contains("adhoc_model"));
}

#[test]
fn agent_team_workflow_yaml_template_parses() {
    // The bundled template (ccteam-core/src/templates/
    // workflow.agent-team.yaml) must round-trip through the schema.
    // This is the file `ccteam init --mode agent-team` writes.
    let template_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/templates/workflow.agent-team.yaml");
    let spec =
        WorkflowSpec::load(&template_path).expect("shipped workflow.agent-team.yaml must parse");
    assert_eq!(spec.mode, WorkflowMode::AgentTeam);
    let team = spec.agent_team.as_ref().unwrap();
    assert_eq!(team.team_name, "my-agent-team");
    assert!(!team.auto_spawn_teammates);
    // At least the one shown (uncommented) definition example.
    assert!(team
        .suggested_teammates
        .iter()
        .any(|t| t.role == "code-reviewer"));
}

#[test]
fn settings_agent_team_template_has_three_new_hooks() {
    // The shipped settings.agent-team.json must include all three F94
    // hook entries (`TeammateIdle`, `TaskCreated`, `TaskCompleted`).
    let body = ccteam_core::PROJECT_SETTINGS_AGENT_TEAM_JSON;
    let v: serde_json::Value =
        serde_json::from_str(body).expect("settings.agent-team.json must parse");
    let hooks = v
        .get("hooks")
        .and_then(|h| h.as_object())
        .expect("hooks object");
    for required in [
        "TeammateIdle",
        "TaskCreated",
        "TaskCompleted",
        // F94 red line: existing 7 hooks still present (no regression
        // vs settings.json baseline).
        "SessionStart",
        "Stop",
        "Notification",
        "PreToolUse",
        "PostToolUse",
        "SubagentStop",
        "SessionEnd",
    ] {
        assert!(
            hooks.contains_key(required),
            "settings.agent-team.json template missing `{required}` hook",
        );
    }
}

#[test]
fn render_project_settings_agent_team_substitutes_bin_path() {
    let body = ccteam_core::render_project_settings_agent_team(
        Path::new("/usr/local/bin/ccteam"),
        &ccteam_core::SettingsEnv::default(),
        &ccteam_core::EnabledPluginsSetting::default(),
    )
    .expect("render must succeed");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let teammate_idle = v["hooks"]["TeammateIdle"][0]["hooks"][0]["command"]
        .as_str()
        .expect("TeammateIdle command");
    assert_eq!(
        teammate_idle,
        "/usr/local/bin/ccteam internal hook progress-append team_teammate_idle",
    );
    let task_created = v["hooks"]["TaskCreated"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(
        task_created,
        "/usr/local/bin/ccteam internal hook progress-append team_task_created",
    );
    let task_completed = v["hooks"]["TaskCompleted"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(
        task_completed,
        "/usr/local/bin/ccteam internal hook progress-append team_task_completed",
    );
}

#[test]
fn write_project_settings_agent_team_lands_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    ccteam_core::write_project_settings_agent_team(
        project,
        &ccteam_core::EnabledPluginsSetting::default(),
    )
    .expect("write must succeed");
    let path = project.join(".claude").join("settings.json");
    assert!(path.exists(), "settings.json must be written");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("TeammateIdle"));
    assert!(body.contains("TaskCreated"));
    assert!(body.contains("TaskCompleted"));
}

#[test]
fn agent_team_spec_round_trips_through_serde() {
    let team = AgentTeamSpec {
        team_name: "x".into(),
        lead_seed: "do thing".into(),
        teammate_mode: Some("in-process".into()),
        cleanup_on_stop: Some("force-kill".into()),
        snapshot_path: Some(std::path::PathBuf::from(".ccteam/team-snapshot.json")),
        suggested_teammates: vec![SuggestedTeammate {
            role: "r1".into(),
            kind: SuggestedTeammateKind::Definition,
            spawn_brief: "review pls".into(),
            adhoc_model: None,
            adhoc_color: None,
            adhoc_tools: None,
        }],
        auto_spawn_teammates: false,
    };
    let yaml = serde_yaml::to_string(&team).expect("serialize");
    let back: AgentTeamSpec = serde_yaml::from_str(&yaml).expect("deserialize");
    assert_eq!(back, team);
}
