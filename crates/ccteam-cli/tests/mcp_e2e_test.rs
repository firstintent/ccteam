//! M2.5 end-to-end: spawn `ccteam mcp-serve` as a subprocess, drive it
//! via stdin/stdout JSON-RPC, and confirm the protocol shape.
//!
//! Confirms the wire contract:
//! - `initialize` returns `protocolVersion` + `tools` capability;
//! - `tools/list` enumerates exactly 8 bare-named tools — the client adds
//!   the `mcp__ccteam__` namespace (status 1 + beacon alias 1 + chat 1 +
//!   session 5);
//! - `tools/call status` returns a JSON-encoded projects list as
//!   the first content[].text.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

/// Spawn `ccteam mcp-serve` with `CCTEAM_HOME` and `CCTEAM_PROJECTS_ROOT`
/// pointed at a tempdir so the test doesn't depend on the developer's
/// global state.
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
        // Closing stdin makes the server exit cleanly.
        drop(self.stdin);
        // Give the child a moment to drain.
        let _ = self.child.wait_timeout_or_kill(Duration::from_secs(2));
    }
}

trait WaitTimeoutOrKill {
    fn wait_timeout_or_kill(&mut self, timeout: Duration) -> Option<std::process::ExitStatus>;
}

impl WaitTimeoutOrKill for std::process::Child {
    fn wait_timeout_or_kill(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        // Crude poll loop — fine for tests.
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(Some(status)) = self.try_wait() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.kill();
        self.wait().ok()
    }
}

#[test]
fn mcp_serve_initialize_returns_protocol_version_and_tools_cap() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&projects).unwrap();

    let mut srv = McpServer::spawn(&home, &projects);
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }));
    let resp = srv.recv();
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    assert_eq!(resp["result"]["serverInfo"]["name"], "ccteam");
    srv.shutdown();
}

#[test]
fn mcp_serve_tools_list_returns_full_tool_set() {
    // status 1 + beacon alias 1 + chat 1 + session 5 = 8.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&projects).unwrap();

    let mut srv = McpServer::spawn(&home, &projects);
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    }));
    let resp = srv.recv();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        8,
        "status 1 + beacon alias 1 + chat 1 + session 5 = 8"
    );
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    let mut expected = vec![
        "chat_send_file",
        "claude_codex_grok_kimi_opencode_status",
        "session_collect",
        "session_dispatch",
        "session_list",
        "session_spawn",
        "session_stop",
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
        "ccteam__chat_reset",
        "ccteam__chat_lifecycle",
        "ccteam__workflow_show",
    ] {
        assert!(
            !names.contains(&gone),
            "culled/retired tool must be gone: {gone}"
        );
    }
    // Schema sanity: every tool must declare `inputSchema.type=object`.
    for tool in tools {
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "tool `{}` must declare object inputSchema",
            tool["name"]
        );
    }
    srv.shutdown();
}

#[test]
fn mcp_serve_tools_call_status_returns_empty_projects_for_fresh_root() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&projects).unwrap();

    let mut srv = McpServer::spawn(&home, &projects);
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "status", "arguments": {} }
    }));
    let resp = srv.recv();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
    assert_eq!(resp["result"]["isError"], false);
    srv.shutdown();
}
