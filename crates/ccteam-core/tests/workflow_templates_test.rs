//! V0.6.0 Wave 2 F114 — integration tests for the five `workflow.yaml`
//! template presets. Each test renders with the default context and
//! asserts the key invariants the skill relies on (mode line, name,
//! mode-specific schema fields).

use ccteam_core::{default_workflow_ctx, render_workflow_template, WorkflowPreset};

#[test]
fn inproc_solo_renders() {
    let out = render_workflow_template(
        WorkflowPreset::InprocSolo,
        &default_workflow_ctx(WorkflowPreset::InprocSolo),
    )
    .unwrap();
    assert!(out.contains("mode: agent-team"));
    assert!(out.contains("teammate_mode: in-process"));
    assert!(!out.contains("{{"), "unsubstituted placeholder leaked:\n{out}");
}

#[test]
fn inproc_team_renders() {
    let out = render_workflow_template(
        WorkflowPreset::InprocTeam,
        &default_workflow_ctx(WorkflowPreset::InprocTeam),
    )
    .unwrap();
    assert!(out.contains("mode: agent-team"));
    assert!(out.contains("role: executor"));
    assert!(out.contains("role: critic"));
    assert!(!out.contains("{{"));
}

#[test]
fn bg_overnight_renders() {
    let out = render_workflow_template(
        WorkflowPreset::BgOvernight,
        &default_workflow_ctx(WorkflowPreset::BgOvernight),
    )
    .unwrap();
    assert!(out.contains("mode: artifact-driven"));
    assert!(out.contains("max_cost_usd_per_24h"));
    assert!(out.contains("watch:.ccteam/inbox/executor"));
    assert!(!out.contains("{{"));
}

#[test]
fn chat_pocket_renders() {
    let out = render_workflow_template(
        WorkflowPreset::ChatPocket,
        &default_workflow_ctx(WorkflowPreset::ChatPocket),
    )
    .unwrap();
    assert!(out.contains("mode: chat"));
    assert!(out.contains("bot_name: @demo_bot"));
    assert!(out.contains("compact_every_turns: 20"));
    assert!(out.contains("hop_limit: 1"));
    assert!(out.contains("recover_last_n_turns: 8"));
    assert!(!out.contains("{{"));
}

#[test]
fn chat_squad_renders() {
    let out = render_workflow_template(
        WorkflowPreset::ChatSquad,
        &default_workflow_ctx(WorkflowPreset::ChatSquad),
    )
    .unwrap();
    assert!(out.contains("mode: chat"));
    assert!(out.contains("hop_limit: 3"));
    assert!(out.contains("@lead_bot"));
    assert!(out.contains("chat_acl:"));
    assert!(!out.contains("{{"));
}

#[test]
fn all_presets_have_unique_yaml_names() {
    let mut seen = std::collections::HashSet::new();
    for &p in WorkflowPreset::all() {
        assert!(seen.insert(p.as_str()), "duplicate preset name: {}", p.as_str());
    }
}
