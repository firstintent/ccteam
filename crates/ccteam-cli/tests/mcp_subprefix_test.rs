//! V0.6.0 Wave 1 (F111) — integration tests for the MCP sub-prefix
//! rename + chat / advise stubs.
//!
//! Each test spawns a real `ccteam mcp-serve` subprocess and drives it
//! via stdin/stdout JSON-RPC to make sure the wire surface a V0.5
//! user's `~/.claude.json` sees is what we expect:
//!
//! - server name still `ccteam` (V0.5 muscle memory preserved)
//! - every tool carries a group sub-prefix (`admin_` / `workflow_` /
//!   `chat_` / `advise_`) except `ccteam__screenshot` which is the
//!   single-member screenshot group that keeps its V0.5 name
//! - `chat_*` + `advise_*` stubs return `NotImplemented` graceful
//!   bodies (so MCP clients don't see transport errors when poking
//!   the Wave 2 / Wave 3 surface ahead of time)
//! - V0.5 unprefixed names (`ccteam__ls`, `ccteam__show`, ...) are
//!   GONE from `tools/list` — no compat alias preserved
//!   (CLAUDE.md §五:pre-V1.0 no backwards-compat shims)

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
    // V0.6 commitment: only TOOL names get sub-prefixed; the SERVER
    // identity in `initialize` stays `ccteam` so V0.5 users'
    // `~/.claude.json` `mcpServers.ccteam` entries continue to work
    // without rename.
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
fn every_tool_carries_group_subprefix_or_is_screenshot() {
    let names = list_tool_names();
    assert!(!names.is_empty(), "tools/list returned empty");
    for n in &names {
        // Each tool name must start with `ccteam__` (server prefix),
        // followed by either `<group>_<rest>` OR the special-cased
        // single-member `screenshot` group whose member kept its
        // pre-V0.6 name.
        let bare = n
            .strip_prefix("ccteam__")
            .unwrap_or_else(|| panic!("tool name {n:?} missing required `ccteam__` server prefix"));
        let ok = bare == "screenshot"
            || bare.starts_with("admin_")
            || bare.starts_with("workflow_")
            || bare.starts_with("chat_")
            || bare.starts_with("advise_");
        assert!(
            ok,
            "tool {n:?} is missing a V0.6.0 group sub-prefix (admin_/workflow_/chat_/advise_/screenshot)",
        );
    }
}

#[test]
fn legacy_v05_unprefixed_names_are_gone() {
    // Acceptance §F #4 — no compat shim. V0.5 names must NOT survive
    // alongside the renamed V0.6 names.
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
    ] {
        assert!(
            !names.contains(&legacy.to_string()),
            "legacy unprefixed name {legacy:?} must NOT be in tools/list (no compat shim)",
        );
    }
}

#[test]
fn chat_and_advise_real_tools_dispatch_through_server() {
    // V0.6.5 F147 + F152 — both `chat_send_input` (F147) and
    // `advise_vote` (F152) are real tools backed by file-system
    // control plane (chat) / per-vendor advisor calls + budget ledger
    // (advise). Spot-check both group shapes:
    //
    // - Schema is listed.
    // - `tools/call advise_vote` with a pre-seeded budget-exceeded
    //   ledger returns a structured `ok:false, error:budget_exceeded`
    //   body without invoking any vendor subprocess (so the test is
    //   hermetic — no `claude` / `codex` binary required on PATH).
    // - `tools/call chat_send_input` exercises the happy path so we
    //   can be sure the dispatcher is still wired.
    let names = list_tool_names();
    assert!(names.contains(&"ccteam__chat_send_input".to_string()));
    assert!(names.contains(&"ccteam__advise_vote".to_string()));
    assert!(names.contains(&"ccteam__advise_parallel".to_string()));

    let (_tmp, home, projects) = tmp_paths();
    // Pre-seed the advise budget ledger so the vote call refuses
    // pre-spawn (no real `claude` / `codex` binary needed).
    std::fs::create_dir_all(&home).unwrap();
    for _ in 0..15 {
        ccteam_core::advise::append_budget_sample(&home, ccteam_core::AgentVendor::Claude, 0.10)
            .unwrap();
    }
    let mut srv = McpServer::spawn(&home, &projects);
    // advise_vote — budget gate triggers `ok:false` body.
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "tools/call",
        "params": {
            "name": "ccteam__advise_vote",
            "arguments": { "question": "Should I pick A or B?", "max_cost_usd": 0.50 }
        }
    }));
    let resp = srv.recv();
    assert_eq!(
        resp["result"]["isError"], false,
        "advise_vote with budget_exceeded should land as result, not isError"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text");
    assert!(
        text.contains("budget_exceeded"),
        "advise_vote body should report budget_exceeded; got: {text}"
    );
    assert!(
        text.contains("\"ok\": false"),
        "advise_vote body should carry ok:false; got: {text}"
    );

    // chat_send_input — F147 real tool. Exercise the happy path so
    // we can be sure the dispatcher is wired (no NotImplemented).
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 3,
        "method": "tools/call",
        "params": {
            "name": "ccteam__chat_send_input",
            "arguments": {
                "workflow_slug": "demo",
                "role": "helper",
                "content": "hello",
            }
        }
    }));
    let resp = srv.recv();
    assert_eq!(
        resp["result"]["isError"], false,
        "real chat_send_input should land as result not isError"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text");
    assert!(
        text.contains("\"ok\": true"),
        "chat_send_input body should report ok:true; got: {text}"
    );
    srv.shutdown();
}

#[test]
fn screenshot_keeps_v05_name_in_v06_listing() {
    // README §九 / F111 decision: `screenshot` is the one single-
    // member group whose tool name does NOT get a sub-prefix, to
    // preserve V0.5 muscle memory for the one tool the user is
    // statistically most likely to have wired up by name in a
    // dashboard / script.
    let names = list_tool_names();
    assert!(
        names.contains(&"ccteam__screenshot".to_string()),
        "ccteam__screenshot must survive the V0.6 rename without sub-prefix"
    );
    // Sanity: no `ccteam__screenshot_*` accidentally added.
    for n in &names {
        if n == "ccteam__screenshot" {
            continue;
        }
        assert!(
            !n.starts_with("ccteam__screenshot_"),
            "no other tool may live under the screenshot group: {n}"
        );
    }
}
