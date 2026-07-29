//! Smoke tests for `status` (renamed from `admin_ls` in v0.9 T1).
//!
//! Verifies the status tool routes correctly through the MCP wire layer.
//! Each test drives the real `ccteam mcp-serve` binary via stdin/stdout
//! JSON-RPC (same harness as `mcp_e2e_test.rs`) and confirms the expected
//! JSON response shapes.
//!
//! Operations covered (one test each):
//!  1. `status` list projects — `status` returns project list
//!  2. `status` cost fields  — `status` response carries cost fields
//!
//! `admin_change_persona` / `admin_add_tool` were culled in v0.9 T1.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

// ─────────────────────────── test harness ───────────────────────────

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
        self.stdout
            .read_line(&mut line)
            .expect("read mcp-serve stdout");
        serde_json::from_str(line.trim()).expect("parse mcp-serve JSON-RPC response")
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait_timeout_or_kill(Duration::from_secs(2));
    }
}

trait WaitTimeoutOrKill {
    fn wait_timeout_or_kill(&mut self, timeout: Duration) -> Option<std::process::ExitStatus>;
}

impl WaitTimeoutOrKill for std::process::Child {
    fn wait_timeout_or_kill(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
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

// ──────────────────────── project bootstrap ───────────────────────────

const FIXTURE_WF: &str = r#"
name: test-workflow
agents:
  worker:
    executor: claude
    trigger: watch:.ccteam/inbox/
    input: .ccteam/inbox/
    output: .ccteam/output/
"#;

/// Minimal state.json so `status` can load each bootstrapped project
/// (ProjectState serde-defaults handle all optional fields).
fn minimal_state_json(slug: &str) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    format!(
        r#"{{
  "slug": "{slug}",
  "team": "dev",
  "created_at": "{now}",
  "tmux_session": "ccteam-{slug}",
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 0,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": null,
  "last_user_interaction_at": "{now}",
  "user_attached": false,
  "user_pause_pending": false
}}"#
    )
}

/// Write the minimal project layout the status tool needs.
///
/// Creates:
/// - `projects/<slug>/.ccteam/workflow.yaml`
/// - `projects/<slug>/.ccteam/state.json`       (so status can load it)
fn bootstrap(home: &std::path::Path, projects: &std::path::Path, slug: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::create_dir_all(projects).unwrap();
    let ccteam_dir = projects.join(slug).join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir).unwrap();
    std::fs::write(ccteam_dir.join("workflow.yaml"), FIXTURE_WF).unwrap();
    std::fs::write(ccteam_dir.join("state.json"), minimal_state_json(slug)).unwrap();
}

/// Call a tool and assert `isError=false`; return the parsed body JSON.
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

// ─────────────────────────── 1. list workflows ─────────────────────

/// `status` must enumerate existing projects and report them
/// in a `projects` array with the correct slugs.
#[test]
fn status_list_projects() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-gamma");
    bootstrap(&home, &projects, "dev-delta");

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(&mut srv, "status", json!({}));
    let arr = body["projects"]
        .as_array()
        .expect("projects must be an array");
    assert!(
        arr.len() >= 2,
        "must list at least the 2 bootstrapped projects; got {}",
        arr.len()
    );
    let slugs: Vec<&str> = arr.iter().filter_map(|p| p["slug"].as_str()).collect();
    assert!(
        slugs.contains(&"dev-gamma"),
        "dev-gamma must appear in project list; got {slugs:?}"
    );
    assert!(
        slugs.contains(&"dev-delta"),
        "dev-delta must appear in project list; got {slugs:?}"
    );
    srv.shutdown();
}

// ─────────────────────────── 2. cost today ─────────────────────────

/// `status` response must carry `cost_24h_usd` on every
/// project entry — that is the data backing the per-project cost
/// surfaces (web cost pill, `/status`).
#[test]
fn status_cost_today() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-epsilon");

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(&mut srv, "status", json!({}));
    let arr = body["projects"]
        .as_array()
        .expect("projects must be an array");
    // Find the project we bootstrapped.
    let proj = arr
        .iter()
        .find(|p| p["slug"] == "dev-epsilon")
        .expect("dev-epsilon must appear in listing");
    // cost_24h_usd must be present (may be 0.0 for a fresh project).
    assert!(
        proj.get("cost_24h_usd").is_some(),
        "project entry must carry cost_24h_usd; got: {proj:?}"
    );
    // The retired `cost_used_usd` alias must be GONE: v0.9.10's STATUS-SLIM-1
    // cut the status wire to `{slug, cost_24h_usd}` (an owner-signed change —
    // the dead load was burning the caller's context every call). This
    // integration test sits outside the `--lib --bins` baseline, so it kept
    // asserting the alias unnoticed; the exact key set is locked by the MCP
    // renderer's own lib test.
    assert!(
        proj.get("cost_used_usd").is_none(),
        "the retired cost_used_usd alias must not come back; got: {proj:?}"
    );
    // Sanity: fresh project has zero cost.
    let cost = proj["cost_24h_usd"].as_f64().unwrap_or(-1.0);
    assert!(
        cost >= 0.0,
        "cost_24h_usd must be non-negative for a fresh project; got {cost}"
    );
    srv.shutdown();
}
