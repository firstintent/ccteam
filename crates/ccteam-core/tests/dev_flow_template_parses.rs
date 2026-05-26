//! Smoke test: `workflows/dev-flow/workflow.yaml` parses cleanly
//! through `WorkflowSpec::parse_str()` + `validate()`. The dev-flow
//! template lives outside `crates/`, so we read it via a workspace-
//! relative path. Keep this test cheap — it's the only ship gate that
//! catches yaml drift in dev-flow before users hit it.
//!
//! NOTE: This test is for the dev-flow template shipped at
//! `workflows/dev-flow/workflow.yaml` (the `holon-inspired 4-bot
//! chat-squad` demo). It is not gated to dev-flow exclusively — if
//! `workflows/` grows to host more templates, add one parses-clean test
//! per template (or refactor into a parameterized loop).

use std::path::PathBuf;

fn dev_flow_yaml_path() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/ccteam-core/ —— ../../workflows/dev-flow/.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // workspace root
    p.push("workflows");
    p.push("dev-flow");
    p.push("workflow.yaml");
    p
}

#[test]
fn dev_flow_workflow_yaml_parses_and_validates() {
    let path = dev_flow_yaml_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    // Parse via serde_yaml — same path as orchestrator load_workflow.
    let spec: ccteam_core::WorkflowSpec =
        serde_yaml::from_str(&body).unwrap_or_else(|e| panic!("serde_yaml parse failed: {e}"));

    // Run the full validator.
    spec.validate()
        .unwrap_or_else(|e| panic!("WorkflowSpec::validate failed: {e}"));

    // Sanity: chat mode + 4 known roles + each role has chat_handle.
    assert!(
        matches!(spec.mode, ccteam_core::WorkflowMode::Chat),
        "dev-flow must be mode: chat"
    );

    let expected_roles: Vec<&str> = vec!["pm", "dev", "reviewer", "ops"];
    for role in &expected_roles {
        assert!(
            spec.agents.contains_key(*role),
            "dev-flow workflow.yaml missing agents.{role}"
        );
    }

    // Each role declares a chat_handle (squad routing needs it).
    for (role, agent) in &spec.agents {
        assert!(
            agent.chat_handle.is_some(),
            "agents.{role} must declare chat_handle for squad routing"
        );
    }
}
