//! V0.6.0 Wave 1 (F111) — integration tests for `CCTEAM_DISABLE_TOOLS`
//! group-enum env filter.
//!
//! Each test spawns `ccteam mcp-serve` with a different
//! `CCTEAM_DISABLE_TOOLS` value and confirms `tools/list` shrinks /
//! grows as expected. Group enum (V0.6.0 README §A):
//! `admin` / `workflow` / `screenshot` / `chat` / `advise`. The
//! `workflow` token stays a valid enum value, but after the W5a EOL of
//! the `workflow_*` tools it gates an empty set, so only four groups
//! ever surface in `tools/list`.
//!
//! Unknown tokens are silently dropped (PRD F111 §B: the filter is
//! best-effort UX, not a security boundary). The acceptance script
//! at `docs/versions/v0-6-0/dev-plan.md` Wave 1 §F #6 invokes the matching
//! test name.

use std::collections::HashSet;
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
    fn spawn_with_disable(
        home: &std::path::Path,
        projects: &std::path::Path,
        disable: Option<&str>,
    ) -> Self {
        let bin = env!("CARGO_BIN_EXE_ccteam");
        let mut cmd = Command::new(bin);
        cmd.args(["internal", "mcp-serve"])
            .env("CCTEAM_HOME", home)
            .env("CCTEAM_PROJECTS_ROOT", projects)
            .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        match disable {
            Some(v) => {
                cmd.env("CCTEAM_DISABLE_TOOLS", v);
            }
            None => {
                cmd.env_remove("CCTEAM_DISABLE_TOOLS");
            }
        }
        let mut child = cmd.spawn().expect("spawn ccteam mcp-serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn list_names(&mut self) -> Vec<String> {
        let req = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "tools/list", "params": {}
        });
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).unwrap();
        let resp: Value = serde_json::from_str(buf.trim()).unwrap();
        resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
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

fn names_with_disable(disable: Option<&str>) -> Vec<String> {
    let (_tmp, home, projects) = tmp_paths();
    let mut srv = McpServer::spawn_with_disable(&home, &projects, disable);
    let names = srv.list_names();
    srv.shutdown();
    names
}

fn group_set(names: &[String]) -> HashSet<&'static str> {
    let mut out = HashSet::new();
    for n in names {
        let bare = n.strip_prefix("ccteam__").expect("server prefix");
        if bare == "screenshot" {
            out.insert("screenshot");
        } else if bare.starts_with("admin_") {
            out.insert("admin");
        } else if bare.starts_with("workflow_") {
            out.insert("workflow");
        } else if bare.starts_with("chat_") {
            out.insert("chat");
        } else if bare.starts_with("advise_") {
            out.insert("advise");
        } else if bare.starts_with("session_") {
            out.insert("session");
        }
    }
    out
}

#[test]
fn disable_unset_returns_all_visible_groups() {
    // W5a EOL — the 8 `workflow_*` tools were retired, so the `workflow`
    // group surfaces no tools and never appears in `tools/list` (its
    // enum token stays valid for the disable parser, but it gates an
    // empty set). The visible surface is the four remaining groups.
    let names = names_with_disable(None);
    let groups = group_set(&names);
    for g in ["admin", "screenshot", "chat", "advise", "session"] {
        assert!(
            groups.contains(g),
            "default tools/list should contain group `{g}`; got groups {:?}",
            groups
        );
    }
    assert!(
        !groups.contains("workflow"),
        "workflow group has no tools after W5a EOL; got groups {:?}",
        groups
    );
}

#[test]
fn disable_chat_hides_chat_keeps_others() {
    let names = names_with_disable(Some("chat"));
    let groups = group_set(&names);
    assert!(!groups.contains("chat"), "chat group should be hidden");
    for g in ["admin", "screenshot", "advise"] {
        assert!(groups.contains(g), "group `{g}` should still be present");
    }
    // Spot-check specific stubs.
    assert!(!names.iter().any(|n| n.starts_with("ccteam__chat_")));
    assert!(names.contains(&"ccteam__advise_vote".to_string()));
}

#[test]
fn disable_chat_and_screenshot_combines() {
    // Acceptance §F #6 — the headline value from team-lead prompt.
    let names = names_with_disable(Some("chat,screenshot"));
    let groups = group_set(&names);
    assert!(!groups.contains("chat"));
    assert!(!groups.contains("screenshot"));
    assert!(!names.contains(&"ccteam__screenshot".to_string()));
    assert!(groups.contains("admin"));
    assert!(groups.contains("advise"));
}

#[test]
fn disable_each_group_individually() {
    // One spawn per group; confirms the enum parser covers every
    // documented value. (Cheap — each spawn is ~200ms.)
    for g in [
        "admin",
        "workflow",
        "screenshot",
        "chat",
        "advise",
        "session",
    ] {
        let names = names_with_disable(Some(g));
        let groups = group_set(&names);
        assert!(
            !groups.contains(g),
            "disable `{g}` should hide that group; got groups {:?}",
            groups
        );
    }
}

#[test]
fn disable_unknown_token_is_silently_ignored() {
    // PRD F111 §B: unknown tokens dropped silently so a typo doesn't
    // crash the MCP server at startup.
    let baseline = names_with_disable(None);
    let with_typo = names_with_disable(Some("not-a-real-group,also-fake"));
    assert_eq!(
        baseline, with_typo,
        "unknown disable tokens must be a no-op",
    );
}

#[test]
fn disable_all_groups_returns_empty_list() {
    let names = names_with_disable(Some("admin,workflow,screenshot,chat,advise,session"));
    assert!(
        names.is_empty(),
        "disabling every group should hide the entire surface; got {:?}",
        names
    );
}

#[test]
fn disable_workflow_preserves_screenshot_group() {
    // Sanity: screenshot is its OWN group, not workflow's subset.
    // Disabling workflow must not collaterally hide screenshot.
    let names = names_with_disable(Some("workflow"));
    assert!(
        names.contains(&"ccteam__screenshot".to_string()),
        "screenshot must survive when only `workflow` is disabled",
    );
    let groups = group_set(&names);
    assert!(!groups.contains("workflow"));
    assert!(groups.contains("screenshot"));
}
