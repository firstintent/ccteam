//! M2.5 end-to-end: spawn `ccteam mcp-serve` as a subprocess, drive it
//! via stdin/stdout JSON-RPC, and confirm the protocol shape.
//!
//! Confirms the wire contract that interfaces.md §12 promises:
//! - `initialize` returns `protocolVersion` + `tools` capability;
//! - `tools/list` enumerates exactly 26 tools, all `ccteam__*`
//!   (M2.5 shipped 9; V0.2.2 F38 added `ccteam__screenshot` → 10;
//!   V0.4.0 F65 added 7 workflow tools → 17; V0.6.0 Wave 1 F111
//!   added 5 chat stubs + 2 advise stubs → 24; V0.6.1 F128 adds
//!   2 admin mutators → 26);
//! - `tools/call ccteam__admin_ls` returns a JSON-encoded projects list as
//!   the first content[].text;
//! - V0.4.0 F65 `tools/call` smokes for the 7 new workflow tools
//!   exercise the marker-file side effects so a future refactor that
//!   skips the F66 hand-off surfaces here.

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
fn mcp_serve_tools_list_returns_full_tool_set() {
    // M2.5 shipped 9 tools; V0.2.2 F38 added `ccteam__screenshot` for
    // a total of 10. V0.4.0 F65 added 7 workflow-control tools for a
    // total of 17. V0.6.0 Wave 1 F111 added 5 chat stubs + 2 advise
    // stubs for a total of 24. V0.6.1 F128 adds 2 admin mutators
    // (`change_persona` + `add_tool`) for a total of 26. Bump this
    // when a new tool lands.
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
        26,
        "M2.5 9 + V0.2.2 F38 screenshot + V0.4.0 F65 7-tool workflow surface + V0.6.0 Wave 1 (5 chat + 2 advise stubs) + V0.6.1 F128 (2 admin mutators)"
    );
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for required in [
        // M2.5 / V0.2.2.
        "ccteam__admin_ls",
        "ccteam__workflow_show",
        "ccteam__workflow_new",
        "ccteam__workflow_peek",
        "ccteam__workflow_progress",
        "ccteam__workflow_pause",
        "ccteam__workflow_resume",
        "ccteam__workflow_send_to_session",
        "ccteam__workflow_inject_decision",
        "ccteam__screenshot",
        // V0.4.0 F65.
        "ccteam__workflow_spawn_agent",
        "ccteam__workflow_stop_agent",
        "ccteam__workflow_observe_agents",
        "ccteam__workflow_signal",
        "ccteam__workflow_set_parallelism",
        "ccteam__workflow_trigger_gate",
        "ccteam__workflow_get_artifact_summary",
        // V0.6.0 Wave 1 (F111) chat stubs.
        "ccteam__chat_send_input",
        "ccteam__chat_lifecycle",
        "ccteam__chat_session_reset",
        "ccteam__chat_list_bots",
        "ccteam__chat_show_turn_log",
        // V0.6.0 Wave 1 (F111) advise stubs.
        "ccteam__advise_vote",
        "ccteam__advise_parallel",
        // V0.6.1 F128 admin mutators.
        "ccteam__admin_change_persona",
        "ccteam__admin_add_tool",
    ] {
        assert!(names.contains(&required), "missing tool: {required}");
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
        "params": { "name": "ccteam__admin_ls", "arguments": {} }
    }));
    let resp = srv.recv();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
    assert_eq!(resp["result"]["isError"], false);
    srv.shutdown();
}

// =============== V0.4.0 F65 integration tests ===============

/// Prepare a project directory with a workflow.yaml and a fresh
/// heartbeat. Returns the `home` / `projects` paths so the caller can
/// hand them to `McpServer::spawn`.
fn bootstrap_workflow_project(
    home: &std::path::Path,
    projects: &std::path::Path,
    slug: &str,
    workflow_yaml: &str,
) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::create_dir_all(projects).unwrap();
    let project_dir = projects.join(slug);
    let ccteam = project_dir.join(".ccteam");
    std::fs::create_dir_all(&ccteam).unwrap();
    std::fs::write(ccteam.join("workflow.yaml"), workflow_yaml).unwrap();
    // The mutating F65 tools require a live daemon heartbeat. Write
    // one directly so we don't have to spin up `ccteam start`.
    let state_dir = home.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let body = format!("pid: {}\nat: {}\n", std::process::id(), now);
    std::fs::write(state_dir.join("orchestrator.heartbeat"), body).unwrap();
}

const FIXTURE_WF: &str = r#"
name: ui-quality-loop
agents:
  fixer:
    executor: claude
    trigger: watch:.ccteam/issues/
    parallelism: 3
    input: .ccteam/issues/
    output: .ccteam/fixes/
  shipper:
    executor: claude
    trigger: gate
    input: .ccteam/fixes/
"#;

/// Helper: invoke a tools/call over the wire and return the parsed
/// content[0].text JSON.
fn call_tool(srv: &mut McpServer, name: &str, args: Value) -> Value {
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    }));
    let resp = srv.recv();
    assert_eq!(
        resp["result"]["isError"], false,
        "tools/call {name} returned isError=true: {resp:?}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).expect("parse tool body as JSON")
}

#[test]
fn t_spawn_agent_returns_session_id() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap_workflow_project(&home, &projects, "demo", FIXTURE_WF);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_spawn_agent",
        json!({ "slug": "demo", "role": "fixer" }),
    );
    assert_eq!(body["ok"], true);
    let session_id = body["session_id"].as_str().unwrap();
    assert!(
        session_id.starts_with("fixer-"),
        "expected session_id to start with role prefix, got `{session_id}`",
    );
    // F66 hand-off: a marker file lives under .ccteam/spawn_requests/.
    let marker_dir = projects.join("demo/.ccteam/spawn_requests");
    let entries: Vec<_> = std::fs::read_dir(&marker_dir).unwrap().collect();
    assert_eq!(entries.len(), 1, "expected 1 spawn marker after one call");
    srv.shutdown();
}

#[test]
fn t_observe_agents_empty() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap_workflow_project(&home, &projects, "demo", FIXTURE_WF);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_observe_agents",
        json!({ "slug": "demo" }),
    );
    assert_eq!(
        body["agents"].as_array().unwrap().len(),
        0,
        "fresh project must report zero agents"
    );
    srv.shutdown();
}

#[test]
fn t_set_parallelism_writes_override_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap_workflow_project(&home, &projects, "demo", FIXTURE_WF);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_set_parallelism",
        json!({ "slug": "demo", "role": "fixer", "parallelism": 7 }),
    );
    assert_eq!(body["ok"], true);
    let override_path = projects.join("demo/.ccteam/workflow_overrides.json");
    assert!(
        override_path.exists(),
        "expected workflow_overrides.json at {}",
        override_path.display()
    );
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(&override_path).unwrap()).unwrap();
    assert_eq!(parsed["fixer"]["parallelism"], 7);
    srv.shutdown();
}

#[test]
fn t_get_artifact_summary_empty_dirs() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap_workflow_project(&home, &projects, "demo", FIXTURE_WF);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_get_artifact_summary",
        json!({ "slug": "demo" }),
    );
    let artifacts = body["artifacts"].as_object().unwrap();
    // Two distinct artifact dirs declared (input dirs share the fixer
    // output; dedup yields 2).
    assert!(artifacts.contains_key(".ccteam/issues/"));
    assert!(artifacts.contains_key(".ccteam/fixes/"));
    for (_, summary) in artifacts {
        assert_eq!(summary["count"], 0);
        assert_eq!(summary["exists"], false);
    }
    srv.shutdown();
}

#[test]
fn t_trigger_gate_writes_marker() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap_workflow_project(&home, &projects, "demo", FIXTURE_WF);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_trigger_gate",
        json!({ "slug": "demo", "role": "shipper", "force": true }),
    );
    assert_eq!(body["ok"], true);
    let marker = projects.join("demo/.ccteam/gate_override/shipper");
    assert!(marker.exists(), "expected marker at {}", marker.display());
    let marker_body: Value =
        serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
    assert_eq!(marker_body["force"], true);
    srv.shutdown();
}

#[test]
fn t_signal_btw_writes_inbox() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap_workflow_project(&home, &projects, "demo", FIXTURE_WF);
    // signal=btw routes through actions::send_to_session, which
    // requires the .ccteam/inbox/ dir; bootstrap_workflow_project
    // already laid down the .ccteam/ root, so send_to_session_with
    // will mkdir inbox on first write. No extra setup needed.

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_signal",
        json!({
            "slug": "demo",
            "role": "fixer",
            "signal": "btw",
            "message": "hello there",
        }),
    );
    assert_eq!(body["ok"], true);
    assert_eq!(body["signal"], "btw");
    let inbox_dir = projects.join("demo/.ccteam/inbox");
    let entries: Vec<_> = std::fs::read_dir(&inbox_dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", inbox_dir.display()))
        .collect();
    assert_eq!(entries.len(), 1, "expected one inbox file after btw");
    let inbox_body = std::fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
    assert!(
        inbox_body.contains("hello there"),
        "inbox file missing message body: {inbox_body}"
    );
    assert!(
        inbox_body.contains("fixer"),
        "inbox file should mention target role: {inbox_body}"
    );
    srv.shutdown();
}
