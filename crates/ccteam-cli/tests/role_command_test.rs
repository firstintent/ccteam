//! v0.8.9 Phase 2 — `ccteam role <search|list>` CLI surface tests.
//!
//! `search` reads the curated ccteam-hub marketplace `index.json` over HTTP;
//! `list` reads a project's `.claude/agents/`. Both run via the real binary
//! against a `TempDir` `CCTEAM_HOME` / cwd so nothing touches the real
//! `~/.ccteam` or `~/.claude.json`. For `search` we stand a tiny in-process
//! HTTP/1.1 responder in for `raw.githubusercontent.com` and point the spawned
//! binary at it via the `CCTEAM_HUB_BASE` env override (the documented hub
//! test seam) — so the round-trip is real HTTP but never touches the network.
//! (`role add` fetch + sha256 verify is covered by the deterministic
//! `ccteam-im` `hub_test.rs` mock-server tests, not here.)

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

use tempfile::TempDir;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

/// A fake ccteam-hub `index.json` with two agent plugins (one "backend"-tagged
/// so a `backend` query matches, one not). Bodies are never fetched by
/// `search`, so `content_sha` is a placeholder.
const FAKE_INDEX: &str = r#"{
  "version": 1,
  "name": "ccteam-hub",
  "description": "curated",
  "generated_at": "2026-01-01T00:00:00Z",
  "plugins": [
    {
      "id": "backend-architect",
      "type": "agent",
      "name": "Backend Architect",
      "description": "Designs backend services and APIs",
      "path": "agents/backend-architect.md",
      "content_sha": "0",
      "source": "agency-agents",
      "upstream": "https://github.com/example/x",
      "license": "MIT",
      "tags": ["backend", "architecture"]
    },
    {
      "id": "frontend-designer",
      "type": "agent",
      "name": "Frontend Designer",
      "description": "Builds polished UIs",
      "path": "agents/frontend-designer.md",
      "content_sha": "0",
      "source": "agency-agents",
      "upstream": "https://github.com/example/y",
      "license": "MIT",
      "tags": ["frontend", "ui"]
    }
  ]
}"#;

/// Spawn an in-process HTTP/1.1 responder that serves `FAKE_INDEX` at
/// `/index.json` (404 elsewhere) for up to `connections` requests, then exits.
/// Returns the `http://127.0.0.1:<port>` base to hand the binary as
/// `CCTEAM_HUB_BASE`. A couple of spare connections cover a stray probe.
fn spawn_fake_hub(connections: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let resp = if path == "/index.json" {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    FAKE_INDEX.len(),
                    FAKE_INDEX
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            };
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

#[test]
fn role_search_lists_marketplace() {
    let tmp = TempDir::new().unwrap();
    let base = spawn_fake_hub(4);
    let out = Command::new(ccteam_bin())
        .args(["role", "search", "backend"])
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .env("CCTEAM_HUB_BASE", &base)
        .output()
        .expect("spawn ccteam role search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "role search should exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The fake index has a backend-tagged plugin; the output should name it and
    // print the add hint.
    assert!(
        stdout.contains("backend-architect"),
        "search `backend` should list the backend plugin; got: {stdout}"
    );
    assert!(
        stdout.contains("ccteam role add"),
        "search output should hint at `ccteam role add`; got: {stdout}"
    );
}

#[test]
fn role_search_json_is_parseable_array() {
    let tmp = TempDir::new().unwrap();
    let base = spawn_fake_hub(4);
    let out = Command::new(ccteam_bin())
        .args(["role", "search", "backend", "--format", "json"])
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .env("CCTEAM_HUB_BASE", &base)
        .output()
        .expect("spawn ccteam role search --format json");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "role search --format json should exit 0.\nstdout: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("role search --format json must emit valid JSON");
    let arr = v.as_array().expect("search json is an array");
    assert!(!arr.is_empty(), "backend search should return entries");
    // Each entry carries the id used by `role add`.
    assert!(
        arr.iter()
            .all(|e| e.get("id").and_then(|i| i.as_str()).is_some()),
        "every plugin entry must carry an `id`"
    );
}

#[test]
fn role_search_no_match_is_clean() {
    let tmp = TempDir::new().unwrap();
    let base = spawn_fake_hub(4);
    let out = Command::new(ccteam_bin())
        .args(["role", "search", "zzz-no-such-plugin-zzz"])
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .env("CCTEAM_HUB_BASE", &base)
        .output()
        .expect("spawn ccteam role search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "an empty search result is not an error.\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("no plugins match"),
        "no-match should print a friendly message; got: {stdout}"
    );
}

#[test]
fn role_list_empty_project_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    // An uninitialized cwd: no .claude/agents/ dir at all.
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let out = Command::new(ccteam_bin())
        .args(["role", "list"])
        .current_dir(&repo)
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "role list on an uninitialized project must exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("no roles installed"),
        "empty project should print a friendly message; got: {stdout}"
    );
}

#[test]
fn role_list_reports_installed_roles() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let agents = repo.join(".claude").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Reviews diffs\n---\nYou review.\n",
    )
    .unwrap();

    let out = Command::new(ccteam_bin())
        .args(["role", "list"])
        .current_dir(&repo)
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role list");
    assert!(out.status.success(), "role list should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("reviewer"),
        "list should report the installed `reviewer` role; got: {stdout}"
    );
}
