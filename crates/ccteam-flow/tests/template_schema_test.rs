//! Render-deserialize gate for every `workflow.yaml` preset template.
//!
//! Each preset must round-trip through `serde_yaml::from_str::<WorkflowSpec>`
//! after rendering with `default_workflow_ctx`. This catches schema drift
//! between the embedded template body and the real `WorkflowSpec` /
//! `ChatSpec` / `ChatAcl` struct fields — the failure mode that shipped
//! `chat-squad.yaml` with a `chat.im_platform` field ChatSpec doesn't
//! define and a `chat_acl: - <id>` list shape ChatAcl can't deserialize
//! (ChatAcl is a struct with `allow_users` + `allow_groups`).
//!
//! Deserialize-only: this gate does NOT call `WorkflowSpec::validate`.
//! Semantic validation is a separate axis (role-name charset, hop_limit
//! bounds, etc.) — those rules are exercised by `workflow_test.rs`.
//! Here we only care that the YAML the skill writes parses cleanly.

use ccteam_core::{
    default_workflow_ctx, render_workflow_agents_block, render_workflow_template,
    WorkflowAgentEntry, WorkflowPreset,
};
use ccteam_flow::WorkflowSpec;

#[test]
fn every_preset_renders_and_deserializes_into_workflow_spec() {
    for &preset in WorkflowPreset::all() {
        let ctx = default_workflow_ctx(preset);
        let yaml = render_workflow_template(preset, &ctx)
            .unwrap_or_else(|e| panic!("render({}) failed: {e}", preset.as_str()));
        assert!(
            !yaml.contains("{{"),
            "preset {} left an unsubstituted placeholder:\n{yaml}",
            preset.as_str()
        );
        let parsed: WorkflowSpec = serde_yaml::from_str(&yaml).unwrap_or_else(|e| {
            panic!(
                "preset {} rendered yaml does not deserialize into WorkflowSpec: {e}\n--- yaml ---\n{yaml}",
                preset.as_str()
            )
        });
        assert!(
            !parsed.name.is_empty(),
            "preset {} parsed WorkflowSpec.name is empty",
            preset.as_str()
        );
    }
}

#[test]
fn chat_squad_default_ctx_renders_a_chat_block_with_struct_shaped_acl() {
    let yaml = render_workflow_template(
        WorkflowPreset::ChatSquad,
        &default_workflow_ctx(WorkflowPreset::ChatSquad),
    )
    .expect("render chat-squad");
    let spec: WorkflowSpec = serde_yaml::from_str(&yaml).expect("deserialize chat-squad");
    let chat = spec
        .chat
        .as_ref()
        .expect("chat-squad preset must populate the `chat:` block");
    let acl = chat
        .chat_acl
        .as_ref()
        .expect("chat-squad default ctx must emit a `chat_acl` struct (not a list)");
    assert_eq!(
        acl.allow_groups.len(),
        1,
        "chat-squad default ctx maps `group_chat_id` to `chat_acl.allow_groups[0]`; got {acl:?}",
    );
    assert!(
        acl.allow_users.is_empty(),
        "chat-squad default ctx must not populate `allow_users`; got {acl:?}",
    );
}

#[test]
fn chat_pocket_default_ctx_renders_chat_acl_with_user_allow_list() {
    let yaml = render_workflow_template(
        WorkflowPreset::ChatPocket,
        &default_workflow_ctx(WorkflowPreset::ChatPocket),
    )
    .expect("render chat-pocket");
    let spec: WorkflowSpec = serde_yaml::from_str(&yaml).expect("deserialize chat-pocket");
    let chat = spec
        .chat
        .as_ref()
        .expect("chat-pocket must populate `chat:`");
    let acl = chat
        .chat_acl
        .as_ref()
        .expect("chat-pocket default ctx must emit a `chat_acl` struct");
    assert_eq!(
        acl.allow_users.len(),
        1,
        "chat-pocket binds owner via `allow_users`; got {acl:?}"
    );
    assert!(acl.allow_groups.is_empty());
}

#[test]
fn chat_squad_template_supports_arbitrary_agent_count() {
    // Stamp a 5-agent block — F189 acceptance: the chat-squad template
    // must no longer be capped at the legacy `role_a` / `role_b` /
    // `role_c` triple.
    let roles = ["dev", "lead", "pm", "cc", "arch"];
    let agents: Vec<WorkflowAgentEntry> =
        roles.iter().map(|r| WorkflowAgentEntry::new(*r)).collect();
    let block = render_workflow_agents_block(&agents);

    let mut ctx = default_workflow_ctx(WorkflowPreset::ChatSquad);
    ctx.vars.insert("agents_block".into(), block);

    let yaml = render_workflow_template(WorkflowPreset::ChatSquad, &ctx).expect("render");
    let spec: WorkflowSpec = serde_yaml::from_str(&yaml).expect("deserialize 5-agent chat-squad");
    assert_eq!(
        spec.agents.len(),
        roles.len(),
        "expected 5 agents in deserialized spec; got {} ({:?})",
        spec.agents.len(),
        spec.agents.keys().collect::<Vec<_>>(),
    );
    for role in roles {
        assert!(
            spec.agents.contains_key(role),
            "missing role `{role}` in {:?}",
            spec.agents.keys().collect::<Vec<_>>(),
        );
    }
}
