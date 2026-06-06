//! F150 — `mcp__ccteam__admin_*` MCP smoke tests.
//!
//! Verifies that `/ccteam-control` admin operations route correctly
//! through the MCP wire layer.  Each test drives the real `ccteam
//! mcp-serve` binary via stdin/stdout JSON-RPC (same harness as
//! `mcp_e2e_test.rs`) and confirms the expected JSON response shapes
//! and, where applicable, file-system side-effects.
//!
//! Operations covered (one test each):
//!  1. `admin_workflow_pause`    — `ccteam__workflow_pause` returns ok=true
//!  2. `admin_workflow_resume`   — `ccteam__workflow_resume` returns ok=true
//!  3. `admin_list_workflows`    — `ccteam__admin_ls` returns project list
//!  4. `admin_cost_today`        — `ccteam__admin_ls` response carries cost fields
//!  5. `admin_change_persona`    — `ccteam__admin_change_persona` rewrites agent .md

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

/// Minimal state.json required by `pause` / `resume` (ProjectState
/// serde-defaults handle all optional fields).
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

/// Write the minimal project layout the MCP admin tools need.
///
/// Creates:
/// - `projects/<slug>/.ccteam/workflow.yaml`
/// - `projects/<slug>/.ccteam/state.json`       (for pause/resume)
/// - `projects/<slug>/.claude/agents/<bot>.md`  (for change-persona test)
fn bootstrap(home: &std::path::Path, projects: &std::path::Path, slug: &str, bot: Option<&str>) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::create_dir_all(projects).unwrap();
    let ccteam_dir = projects.join(slug).join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir).unwrap();
    std::fs::write(ccteam_dir.join("workflow.yaml"), FIXTURE_WF).unwrap();
    std::fs::write(ccteam_dir.join("state.json"), minimal_state_json(slug)).unwrap();

    // Persona file for the change-persona smoke test.
    if let Some(b) = bot {
        let agents_dir = projects.join(slug).join(".claude").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join(format!("{b}.md")),
            format!("---\nname: {b}\ntools: Read\n---\nOriginal persona body.\n"),
        )
        .unwrap();
    }
}

fn fake_daemon(home: &std::path::Path) -> std::os::unix::net::UnixListener {
    let socket = home.join("run").join("mcp.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::os::unix::net::UnixListener::bind(socket).unwrap()
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

// ─────────────────────────── 1. pause ──────────────────────────────

/// F150 smoke 1 — `admin_workflow_pause`:
/// `ccteam__workflow_pause` must return `ok=true` and set
/// `user_pause_pending=true` in the response envelope.
#[test]
fn admin_workflow_pause() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-alpha", None);
    let _daemon = fake_daemon(&home);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__workflow_pause",
        json!({ "slug": "dev-alpha" }),
    );
    assert_eq!(body["ok"], true, "pause must return ok=true");
    assert_eq!(body["slug"], "dev-alpha", "pause must echo the slug back");
    assert_eq!(
        body["user_pause_pending"], true,
        "pause must set user_pause_pending=true in response"
    );
    srv.shutdown();
}

// ─────────────────────────── 2. resume ─────────────────────────────

/// F150 smoke 2 — `admin_workflow_resume`:
/// `ccteam__workflow_resume` must return `ok=true`.  We start from an
/// already-paused state (pause first) so the full round-trip exercises
/// both directions.
#[test]
fn admin_workflow_resume() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-beta", None);
    let _daemon = fake_daemon(&home);

    let mut srv = McpServer::spawn(&home, &projects);

    // Pause first, then resume — exercise the full round-trip.
    call_tool(
        &mut srv,
        "ccteam__workflow_pause",
        json!({ "slug": "dev-beta" }),
    );

    let body = call_tool(
        &mut srv,
        "ccteam__workflow_resume",
        json!({ "slug": "dev-beta" }),
    );
    assert_eq!(body["ok"], true, "resume must return ok=true");
    assert_eq!(body["slug"], "dev-beta", "resume must echo the slug");
    srv.shutdown();
}

// ─────────────────────────── 3. list workflows ─────────────────────

/// F150 smoke 3 — `admin_list_workflows`:
/// `ccteam__admin_ls` must enumerate existing projects and report them
/// in a `projects` array with the correct slugs.
#[test]
fn admin_list_workflows() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-gamma", None);
    bootstrap(&home, &projects, "dev-delta", None);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(&mut srv, "ccteam__admin_ls", json!({}));
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

// ─────────────────────────── 4. cost today ─────────────────────────

/// F150 smoke 4 — `admin_cost_today`:
/// `ccteam__admin_ls` response must carry `cost_24h_usd` on every
/// project entry — that is the data backing `/ccteam-control show-cost`
/// and `@ccteam cost today`.
#[test]
fn admin_cost_today() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-epsilon", None);

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(&mut srv, "ccteam__admin_ls", json!({}));
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
    // cost_used_usd (alias, also present for backwards compat).
    assert!(
        proj.get("cost_used_usd").is_some(),
        "project entry must carry cost_used_usd; got: {proj:?}"
    );
    // Sanity: fresh project has zero cost.
    let cost = proj["cost_24h_usd"].as_f64().unwrap_or(-1.0);
    assert!(
        cost >= 0.0,
        "cost_24h_usd must be non-negative for a fresh project; got {cost}"
    );
    srv.shutdown();
}

// ─────────────────────────── 5. change persona ─────────────────────

/// F150 smoke 6 — `admin_change_persona`:
/// `ccteam__admin_change_persona` must rewrite the bot's agent .md
/// with the provided content and return `ok=true` + `bytes_written`.
#[test]
fn admin_change_persona() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    bootstrap(&home, &projects, "dev-eta", Some("worker"));
    let _daemon = fake_daemon(&home);

    let new_persona =
        "---\nname: worker\ntools: Read, WebFetch\n---\nRevised persona: 专注于 web 研究。\n";

    let mut srv = McpServer::spawn(&home, &projects);
    let body = call_tool(
        &mut srv,
        "ccteam__admin_change_persona",
        json!({
            "slug":           "dev-eta",
            "bot":            "worker",
            "new_persona_md": new_persona,
        }),
    );
    assert_eq!(body["ok"], true, "change_persona must return ok=true");
    assert_eq!(body["slug"], "dev-eta");
    assert_eq!(body["bot"], "worker");
    let bytes = body["bytes_written"].as_u64().unwrap_or(0);
    assert!(bytes > 0, "bytes_written must be positive; got {bytes}");

    // Verify the agent .md was actually rewritten on disk.
    let agent_md = projects
        .join("dev-eta")
        .join(".claude")
        .join("agents")
        .join("worker.md");
    assert!(
        agent_md.exists(),
        "agent .md must exist after change_persona"
    );
    let on_disk = std::fs::read_to_string(&agent_md).unwrap();
    assert!(
        on_disk.contains("WebFetch"),
        "agent .md must contain the new tools list; got: {on_disk}"
    );
    assert!(
        on_disk.contains("Revised persona"),
        "agent .md must contain the new body; got: {on_disk}"
    );
    assert!(
        !on_disk.contains("Original persona"),
        "old persona body must be gone; got: {on_disk}"
    );
    srv.shutdown();
}
