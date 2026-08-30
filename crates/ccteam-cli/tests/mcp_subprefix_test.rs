//! Wire-name invariants for the MCP tool surface.
//!
//! Names are the part of the protocol an agent actually sees, so they are
//! locked here: bare names (never `ccteam__`-prefixed, which would render as
//! the double `mcp__ccteam__ccteam__agent`), the two prefix-less admin
//! singletons, and no culled or pre-rename name resurrected.
//!
//! These used to shell out to a stdio `mcp-serve` child for `tools/list`; that
//! transport is deleted, so the listing comes from the protocol core.

use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::CcteamPaths;

fn tmp_paths() -> (TempDir, CcteamPaths) {
    ccteam_core::tool_surface::disable_tool_surface_bootstrap_for_tests();
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(&paths.projects_root).unwrap();
    (tmp, paths)
}

async fn list_tool_names() -> Vec<String> {
    let (_tmp, paths) = tmp_paths();
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    let resp = ccteam_im::mcp::handle_request(&paths, &req, &ccteam_im::mcp::ToolFace::full())
        .await
        .expect("tools/list expects a response");
    resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn server_name_stays_ccteam_for_v05_muscle_memory() {
    // The SERVER identity in `initialize` stays `ccteam` so users'
    // `mcpServers.ccteam` entries keep working without a rename; the client
    // derives the model-visible namespace from it (`mcp__ccteam__<tool>`).
    let (_tmp, paths) = tmp_paths();
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    let resp = ccteam_im::mcp::handle_request(&paths, &req, &ccteam_im::mcp::ToolFace::full())
        .await
        .expect("initialize expects a response");
    assert_eq!(resp["result"]["serverInfo"]["name"], "ccteam");
}

#[tokio::test]
async fn every_tool_carries_group_subprefix_or_is_singleton() {
    let names = list_tool_names().await;
    assert!(!names.is_empty(), "tools/list returned empty");
    for n in &names {
        assert!(
            !n.starts_with("ccteam__"),
            "tool name {n:?} must not embed the server prefix (client namespaces by server key)"
        );
        let ok = n == "status"
            || n == "grok_claude_codex_kimi"
            || n == "agent"
            || n.starts_with("admin_")
            || n.starts_with("workflow_")
            || n.starts_with("chat_")
            || n.starts_with("agent_");
        assert!(
            ok,
            "tool {n:?} is missing a group sub-prefix (chat_/agent_/status)",
        );
    }
}

#[tokio::test]
async fn legacy_v05_unprefixed_names_are_gone() {
    // No compat shim. V0.5 names must NOT survive alongside the renamed ones.
    let names = list_tool_names().await;
    for legacy in [
        "ccteam__ls",
        "ccteam__show",
        "ccteam__peek",
        "ccteam__progress",
        "ccteam__new",
        "ccteam__pause",
        "ccteam__resume",
        "ccteam__send_to_session",
        "ccteam__inject_decision",
        "ccteam__spawn_agent",
        "ccteam__stop_agent",
        "ccteam__observe_agents",
        "ccteam__signal",
        "ccteam__set_parallelism",
        "ccteam__trigger_gate",
        "ccteam__get_artifact_summary",
        // v0.9 T1 culled names (no alias).
        "ccteam__admin_ls",
        "ccteam__admin_change_persona",
        "ccteam__admin_add_tool",
        "ccteam__advise_vote",
        "ccteam__advise_parallel",
        "ccteam__chat_register_bot",
        "ccteam__chat_unregister_bot",
        "ccteam__chat_list_bots",
        // 2026-07-26 cull: tmux-era pane screenshot (web route stays).
        "screenshot",
        // v0.9.1 rename: prefixed wire names dropped (the client namespaces by
        // server key, so the old form double-prefixed for the model).
        "ccteam__status",
        "ccteam__chat_send_file",
        // 2026-08-31 merge: spawn+dispatch → `agent`, list+collect →
        // `agent_read`. Pre-v1.0, so no alias survives.
        "session_spawn",
        "session_dispatch",
        "session_collect",
        "session_list",
        "session_stop",
        "ccteam__agent",
    ] {
        assert!(
            !names.contains(&legacy.to_string()),
            "legacy tool name {legacy:?} must not be listed"
        );
    }
}

#[tokio::test]
async fn status_keeps_singleton_name_in_listing() {
    // `status` (v0.9 T1 rename of `admin_ls`) and its bare-name beacon alias
    // are the prefix-less admin tools.
    let names = list_tool_names().await;
    assert!(
        names.contains(&"status".to_string()),
        "status must survive without sub-prefix"
    );
    assert!(
        names.contains(&"grok_claude_codex_kimi".to_string()),
        "the bare-name beacon alias must be listed"
    );
    assert!(
        !names.contains(&"screenshot".to_string()),
        "screenshot was culled 2026-07-26 and must not resurface"
    );
}

#[tokio::test]
async fn status_and_session_tools_dispatch_as_tool_results() {
    // Spot-check the remaining surface: the tools exist, and `status` lands as
    // a tools/call RESULT rather than a transport-shaped failure.
    let names = list_tool_names().await;
    assert!(names.contains(&"status".to_string()));
    assert!(names.contains(&"agent".to_string()));
    assert!(names.contains(&"agent_read".to_string()));
    assert!(names.contains(&"agent_stop".to_string()));
    assert!(names.contains(&"chat_send_file".to_string()));

    let (_tmp, paths) = tmp_paths();
    let req = json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "tools/call",
        "params": { "name": "status", "arguments": {} }
    });
    let resp: Value =
        ccteam_im::mcp::handle_request(&paths, &req, &ccteam_im::mcp::ToolFace::full())
            .await
            .expect("tools/call expects a response");
    assert_eq!(
        resp["result"]["isError"], false,
        "status should land as result, not isError"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text");
    assert!(
        text.contains("projects"),
        "status body should carry projects array; got: {text}"
    );
}
