//! Integration tests for the MCP tool naming surface.
//!
//! Each test spawns a real `ccteam mcp-serve` subprocess and drives it
//! via stdin/stdout JSON-RPC:
//!
//! - server name still `ccteam` (V0.5 muscle memory preserved)
//! - every tool carries a group sub-prefix (`chat_` / `session_`) OR
//!   is the single-member exception (`status`)
//! - V0.5 unprefixed names (`ccteam__ls`, …) and culled v0.9 tools are
//!   GONE from `tools/list` — no compat alias preserved

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

struct McpServer {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl McpServer {
    fn spawn(home: &std::path::Path, projects: &std::path::Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_ccteam");
        let mut child = Command::new(bin)
            .args(["internal", "mcp-serve"])
            .env("CCTEAM_HOME", home)
            .env("CCTEAM_PROJECTS_ROOT", projects)
            .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1")
            // Make sure parent CCTEAM_DISABLE_TOOLS doesn't leak into
            // the "happy path" tests below.
            .env_remove("CCTEAM_DISABLE_TOOLS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ccteam mcp-serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read stdout");
        serde_json::from_str(line.trim()).expect("parse JSON-RPC response")
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
    }
}

fn tmp_paths() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    (tmp, home, projects)
}

fn list_tool_names() -> Vec<String> {
    let (_tmp, home, projects) = tmp_paths();
    let mut srv = McpServer::spawn(&home, &projects);
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/list", "params": {}
    }));
    let resp = srv.recv();
    let names = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    srv.shutdown();
    names
}

#[test]
fn server_name_stays_ccteam_for_v05_muscle_memory() {
    // The SERVER identity in `initialize` stays `ccteam` so users'
    // `~/.claude.json` `mcpServers.ccteam` entries continue to work
    // without rename; the client derives the model-visible namespace
    // from it (`mcp__ccteam__<tool>`).
    let (_tmp, home, projects) = tmp_paths();
    let mut srv = McpServer::spawn(&home, &projects);
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "initialize", "params": {}
    }));
    let resp = srv.recv();
    assert_eq!(resp["result"]["serverInfo"]["name"], "ccteam");
    srv.shutdown();
}

#[test]
fn every_tool_carries_group_subprefix_or_is_singleton() {
    let names = list_tool_names();
    assert!(!names.is_empty(), "tools/list returned empty");
    for n in &names {
        // Wire names are BARE `<group>_<rest>` (or the single-member
        // exception: `status`). The MCP client namespaces
        // by server key, so Claude Code shows `mcp__ccteam__session_spawn`
        // — a baked-in `ccteam__` prefix would render as the double
        // `mcp__ccteam__ccteam__session_spawn`.
        assert!(
            !n.starts_with("ccteam__"),
            "tool name {n:?} must not embed the server prefix (client namespaces by server key)"
        );
        let ok = n == "status"
            || n.starts_with("admin_")
            || n.starts_with("workflow_")
            || n.starts_with("chat_")
            || n.starts_with("session_");
        assert!(
            ok,
            "tool {n:?} is missing a group sub-prefix (chat_/session_/status)",
        );
    }
}

#[test]
fn legacy_v05_unprefixed_names_are_gone() {
    // No compat shim. V0.5 names must NOT survive alongside the
    // renamed V0.6 names.
    let names = list_tool_names();
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
        // v0.9.1 rename: prefixed wire names dropped (client namespaces
        // by server key; the old form double-prefixed for the model).
        "ccteam__status",
        "ccteam__screenshot",
        "ccteam__chat_send_file",
        "ccteam__session_spawn",
        "ccteam__session_dispatch",
        "ccteam__session_collect",
        "ccteam__session_list",
        "ccteam__session_stop",
    ] {
        assert!(
            !names.contains(&legacy.to_string()),
            "legacy/culled name {legacy:?} must NOT be in tools/list (no compat shim)",
        );
    }
}

#[test]
fn status_and_session_tools_dispatch_through_server() {
    // Spot-check remaining surface: status (local) + session_list
    // (forwards to daemon — may error without daemon, but must land
    // as a tools/call result shape rather than a transport failure).
    let names = list_tool_names();
    assert!(names.contains(&"status".to_string()));
    assert!(names.contains(&"session_list".to_string()));
    assert!(names.contains(&"chat_send_file".to_string()));

    let (_tmp, home, projects) = tmp_paths();
    let mut srv = McpServer::spawn(&home, &projects);
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "tools/call",
        "params": {
            "name": "status",
            "arguments": {}
        }
    }));
    let resp = srv.recv();
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
    srv.shutdown();
}

#[test]
fn status_keeps_singleton_name_in_listing() {
    // `status` is the one single-member group whose tool name does
    // NOT get a sub-prefix (v0.9 T1 rename of `admin_ls`).
    let names = list_tool_names();
    assert!(
        names.contains(&"status".to_string()),
        "status must survive without sub-prefix"
    );
    // Sanity: the culled screenshot singleton stays gone.
    assert!(
        !names.contains(&"screenshot".to_string()),
        "screenshot was culled 2026-07-26 and must not resurface"
    );
}
