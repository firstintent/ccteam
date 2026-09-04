//! The outbound half of a session's attachment: its ccteam TOOL FACE (the
//! vendor's own MCP client aimed at daemon `POST /mcp`).
//!
//! Every managed vendor has to answer [`HarnessAdapter::rebuild_tool_surface`]
//! explicitly. The point of the contract is that "I cannot rebuild it" must be
//! a stated, actionable answer rather than a silent success — a session whose
//! tool face died is fully alive and looks fine, so a no-op that reports
//! success is indistinguishable from a working rebuild until an agent tries to
//! call `agent` and finds nothing there.
//!
//! No vendor CAN rebuild in place (claude stream-json's `mcp_reconnect` was the
//! last claimant and was withdrawn once it was measured swapping the session's
//! principal for the machine credential — see `claude_stream_json_test.rs`), so
//! the surviving contract is the reason: this file pins that every vendor states
//! one, and `ToolSurfaceRebuild` no longer has a variant for the other answer.

use std::sync::Arc;

use ccteam_harness::execution::CodexAppServerAdapter;
use ccteam_harness::{
    AgentVendor, ExecutionMode, GrokAcpAdapter, HarnessAdapter, KimiAcpAdapter, OpencodeAcpAdapter,
    PiRpcAdapter, ThreadHandle, ToolSurfaceRebuild,
};

fn handle(vendor: AgentVendor) -> ThreadHandle {
    ThreadHandle {
        vendor,
        mode: ExecutionMode::Chat,
        identity: "not-live".to_string(),
        started_at: chrono::Utc::now(),
        raw_extras: serde_json::json!({}),
    }
}

/// Not "returns an error", not "returns Ok(())" — a REASON. The IM `/mcp`
/// receipt prints it verbatim, so an empty or vague one is a user staring at a
/// session that cannot use ccteam with no idea what to do next.
///
/// "Claims a rebuild it cannot perform" used to be an arm of this match that
/// panicked; it is now unrepresentable — the destructure below is irrefutable
/// because `RespawnRequired` is the only outcome the type admits.
fn assert_states_a_reason(outcome: ToolSurfaceRebuild, vendor: &str) {
    let ToolSurfaceRebuild::RespawnRequired { reason } = outcome;
    assert!(
        reason.len() > 20,
        "{vendor}: the reason is what the user acts on, got {reason:?}"
    );
    assert!(
        reason.contains("/new"),
        "{vendor}: say what restores the tool face, got {reason:?}"
    );
}

#[tokio::test]
async fn codex_states_that_only_a_respawn_reapplies_its_mcp_config() {
    let adapter = CodexAppServerAdapter::new();
    let outcome = adapter
        .rebuild_tool_surface(&handle(AgentVendor::Codex))
        .await
        .expect("declaring a limitation is an answer, not an error");
    assert_states_a_reason(outcome, "codex");
}

#[tokio::test]
async fn acp_vendors_state_that_mcp_servers_only_travel_on_session_creation() {
    for (name, adapter) in [
        (
            "grok",
            Box::new(GrokAcpAdapter::new()) as Box<dyn HarnessAdapter + Send + Sync>,
        ),
        ("opencode", Box::new(OpencodeAcpAdapter::new())),
        ("kimi", Box::new(KimiAcpAdapter::new())),
    ] {
        let outcome = adapter
            .rebuild_tool_surface(&handle(AgentVendor::Grok))
            .await
            .expect("declaring a limitation is an answer, not an error");
        assert_states_a_reason(outcome, name);
    }
}

#[tokio::test]
async fn pi_states_that_its_bridge_env_is_fixed_at_spawn() {
    let adapter = PiRpcAdapter::new(Arc::new(|_: &std::path::Path, _: &str| Ok(None)));
    let outcome = adapter
        .rebuild_tool_surface(&handle(AgentVendor::Pi))
        .await
        .expect("declaring a limitation is an answer, not an error");
    assert_states_a_reason(outcome, "pi");
}
