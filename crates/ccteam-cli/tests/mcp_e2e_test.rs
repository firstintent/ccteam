//! End-to-end tests for the MCP protocol core (`ccteam_im::mcp`).
//!
//! These used to drive a `ccteam internal mcp-serve` child over stdin/stdout.
//! That transport is deleted — every caller now reaches ccteam over the
//! daemon's `POST /mcp` — so the same assertions run against the request
//! handler directly. The subject never was the pipe: it is that `initialize`
//! advertises the protocol + tool capability, that `tools/list` returns the
//! full surface with no culled name resurrected, and that `status` answers on
//! a fresh root.
//!
//! Operations covered (one test each):
//!  1. `initialize` — protocol version + tools capability + server identity
//!  2. `tools/list` — the exact 6-tool surface
//!  3. `tools/call status` — empty project list on a fresh root

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

async fn call(paths: &CcteamPaths, req: Value) -> Value {
    ccteam_im::mcp::handle_request(paths, &req, &ccteam_im::mcp::ToolFace::full())
        .await
        .expect("request expects a response")
}

#[tokio::test]
async fn mcp_initialize_returns_protocol_version_and_tools_cap() {
    let (_tmp, paths) = tmp_paths();
    let resp = call(
        &paths,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )
    .await;
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(
        resp["result"]["protocolVersion"],
        ccteam_im::mcp::MCP_PROTOCOL_VERSION
    );
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    // The server identity stays `ccteam`: clients derive the model-visible
    // namespace from it (`mcp__ccteam__<tool>`).
    assert_eq!(resp["result"]["serverInfo"]["name"], "ccteam");
}

#[tokio::test]
async fn mcp_tools_list_returns_full_tool_set() {
    // status 1 + beacon alias 1 + chat 1 + session 3 = 6.
    let (_tmp, paths) = tmp_paths();
    let resp = call(
        &paths,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
    )
    .await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        6,
        "status 1 + beacon alias 1 + chat 1 + session 3 = 6"
    );
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    let mut expected = vec![
        "agent",
        "agent_read",
        "agent_stop",
        "chat_send_file",
        "grok_claude_codex_kimi",
        "status",
    ];
    expected.sort();
    assert_eq!(names, expected);
    // Culled tools must stay gone (no deprecated alias).
    for gone in [
        "ccteam__admin_ls",
        "ccteam__admin_change_persona",
        "ccteam__admin_add_tool",
        "ccteam__advise_vote",
        "ccteam__advise_parallel",
        "ccteam__chat_register_bot",
        "ccteam__chat_unregister_bot",
        "ccteam__chat_list_bots",
        "ccteam__chat_lifecycle",
        "ccteam__workflow_show",
        "screenshot",
        "ccteam__status",
        "ccteam__chat_send_file",
        "session_spawn",
        "session_dispatch",
        "session_collect",
        "session_list",
        "session_stop",
    ] {
        assert!(!names.contains(&gone), "culled tool present: {gone}");
    }
}

#[tokio::test]
async fn mcp_tools_call_status_returns_empty_projects_for_fresh_root() {
    let (_tmp, paths) = tmp_paths();
    let resp = call(
        &paths,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "status", "arguments": {} }
        }),
    )
    .await;
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
}
