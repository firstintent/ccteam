//! F197 / F198 — `mcp__ccteam__chat_register_bot` end-to-end test that
//! the MCP-side safety net writes `<project>/.ccteam/state.json` and
//! `<ccteam_home>/hooks/hook.sh`.
//!
//! Background — `/ccteam-creator` Phase 5 writes workflow.yaml, persona,
//! and registry, but historically skipped `bootstrap_project_at_dir`
//! and `install_hooks`, so users who ran only the creator flow ended up
//! with chat-mode bots whose SessionStart hook walk-up couldn't locate
//! the project root ("not under any ccteam project") and whose hook.sh
//! dispatcher script never landed at all. F197 and F198 lift both calls
//! into `dispatch_register_bot` so the daemon-side path always seeds
//! state.json and hook.sh even if a future skill maintainer drops the
//! steps.
//!
//! The bootstrap is gated on an explicit `project_dir` argument so
//! existing inline tests (which omit `project_dir` and fall back to
//! `std::env::current_dir()`, i.e. the ccteam source tree) don't
//! pollute the repo. The creator flow always passes `project_dir`, so
//! the safety net fires in the real-user scenario.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
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
    fn spawn(home: &Path, projects: &Path) -> Self {
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
        let n = self
            .stdout
            .read_line(&mut line)
            .expect("read mcp-serve stdout");
        assert!(
            n > 0,
            "mcp-serve closed stdout without responding (line={line:?})",
        );
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("parse JSON-RPC response: {e}; got line: {line:?}"))
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }));
        let resp = self.recv();
        assert_eq!(
            resp["result"]["isError"], false,
            "tools/call {name} reported isError=true; resp={resp:?}",
        );
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("content[0].text");
        serde_json::from_str(text).expect("parse tool body as JSON")
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

/// Skill-flow acceptance — driving `chat_register_bot` with an explicit
/// `project_dir` lands `.ccteam/state.json` AND `<root>/hooks/hook.sh`.
#[test]
fn t_register_bot_writes_state_json_and_installs_hook_sh() {
    let tmp = TempDir::new().unwrap();
    let ccteam_home = tmp.path().join("ccteam-home");
    let projects_root = tmp.path().join("projects");
    std::fs::create_dir_all(&ccteam_home).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();

    // Caller-supplied project_dir lives outside the projects_root —
    // mirrors the creator-flow case (NAS shares / repo basename ≠ slug)
    // F185 was originally written for.
    let project_dir = tmp.path().join("my-repo");
    std::fs::create_dir_all(&project_dir).unwrap();

    let slug = "research-squad";
    let role = "helper";

    let mut srv = McpServer::spawn(&ccteam_home, &projects_root);
    let body = srv.call_tool(
        "ccteam__chat_register_bot",
        json!({
            "workflow_slug": slug,
            "role": role,
            "vendor": "claude",
            "im_platform": "telegram",
            "im_chat_id": "42",
            "project_dir": project_dir.to_string_lossy(),
        }),
    );
    assert_eq!(body["ok"], true, "register must succeed; body={body:?}");

    // F197 — state.json laid down at <project>/.ccteam/state.json so
    // the SessionStart hook's `session_context_from_cwd` walk-up
    // resolves the project root.
    let state_path = project_dir.join(".ccteam").join("state.json");
    assert!(
        state_path.exists(),
        "F197: state.json must be created at {} after chat_register_bot",
        state_path.display(),
    );
    // Round-trip via serde — `session_context_from_cwd` calls
    // `ProjectState::load`; if our seed body parses, the walk-up
    // resolves cleanly.
    let state_body = std::fs::read_to_string(&state_path).unwrap();
    let parsed: Value = serde_json::from_str(&state_body).unwrap_or_else(|e| {
        panic!("state.json must be valid JSON (got `{state_body}`): {e}");
    });
    assert_eq!(
        parsed["slug"], slug,
        "state.json::slug must match the workflow_slug passed to chat_register_bot",
    );

    // F198 — hook.sh dispatcher materialized so Claude Code's hook
    // commands (which point at this absolute path via settings.json)
    // can actually exec.
    let hook_path = ccteam_home.join("hooks").join("hook.sh");
    assert!(
        hook_path.exists(),
        "F198: hook.sh must be installed at {}",
        hook_path.display(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&hook_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "F198: hook.sh must be chmod 0755 so Claude Code can exec it",
        );
    }
    // Body sanity — confirms the F139 dispatcher (not some other script
    // by accident).
    let hook_body = std::fs::read_to_string(&hook_path).unwrap();
    assert!(
        hook_body.contains("/internal/hook/"),
        "hook.sh must be the daemon-aware F139 dispatcher",
    );

    srv.shutdown();
}

/// Idempotency — re-registering (after a prior register) must not
/// clobber state.json. The skill flow's "user re-runs creator" scenario.
#[test]
fn t_register_bot_does_not_clobber_existing_state_json() {
    let tmp = TempDir::new().unwrap();
    let ccteam_home = tmp.path().join("ccteam-home");
    let projects_root = tmp.path().join("projects");
    std::fs::create_dir_all(&ccteam_home).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();

    let project_dir = tmp.path().join("my-repo");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Pre-seed state.json with a sentinel slug — the bootstrap gate
    // ("if state.json doesn't exist") must leave it untouched on the
    // second register. Build via `ProjectState::initial_for_team` so
    // the schema stays in lockstep with `state.rs`.
    let ccteam_dir = project_dir.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir).unwrap();
    let state_path = ccteam_dir.join("state.json");
    let pre_existing =
        ccteam_core::ProjectState::initial_for_team("pre-existing-slug".into(), "chat".into());
    pre_existing.save(&state_path).unwrap();
    let original_bytes = std::fs::read(&state_path).unwrap();

    let mut srv = McpServer::spawn(&ccteam_home, &projects_root);
    let body = srv.call_tool(
        "ccteam__chat_register_bot",
        json!({
            "workflow_slug": "demo",
            "role": "helper",
            "vendor": "claude",
            "im_platform": "telegram",
            "im_chat_id": "42",
            "project_dir": project_dir.to_string_lossy(),
        }),
    );
    assert_eq!(body["ok"], true, "register must succeed; body={body:?}");

    let after_bytes = std::fs::read(&state_path).unwrap();
    assert_eq!(
        original_bytes, after_bytes,
        "F197 bootstrap must skip when state.json already exists (no clobber on re-register)",
    );

    srv.shutdown();
}
