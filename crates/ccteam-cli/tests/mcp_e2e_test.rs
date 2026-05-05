//! M2.5 end-to-end: spawn `ccteam mcp-serve` as a subprocess, drive it
//! via stdin/stdout JSON-RPC, and confirm the protocol shape.
//!
//! Confirms the wire contract that interfaces.md §12 promises:
//! - `initialize` returns `protocolVersion` + `tools` capability;
//! - `tools/list` enumerates exactly 9 tools, all `ccteam__*`;
//! - `tools/call ccteam__ls` returns a JSON-encoded projects list as
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
            .arg("mcp-serve")
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
fn mcp_serve_tools_list_returns_nine_ccteam_tools() {
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
    assert_eq!(tools.len(), 9, "M2.5 brief = exactly 9 tools");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for required in [
        "ccteam__ls",
        "ccteam__show",
        "ccteam__new",
        "ccteam__peek",
        "ccteam__progress",
        "ccteam__pause",
        "ccteam__resume",
        "ccteam__send_to_session",
        "ccteam__inject_decision",
    ] {
        assert!(names.contains(&required), "missing tool: {required}");
    }
    srv.shutdown();
}

#[test]
fn mcp_serve_tools_call_ls_returns_empty_projects_for_fresh_root() {
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
        "params": { "name": "ccteam__ls", "arguments": {} }
    }));
    let resp = srv.recv();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
    assert_eq!(resp["result"]["isError"], false);
    srv.shutdown();
}
