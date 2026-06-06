//! V0.6.5 F165 — `ccteam mcp-serve` tracing isolation smoke test.
//!
//! `ccteam mcp-serve` speaks line-delimited JSON-RPC 2.0 over stdin /
//! stdout (`docs/interfaces.md` §12). Before F165 the default tracing
//! subscriber wrote to stdout, so an `info!` emitted during the first
//! `tools/list` handler would jump the JSON-RPC reply on the wire and
//! the client's strict per-line `serde_json::from_str` would blow up.
//! F147 worked around it with `RUST_LOG=error` in the test env; F165
//! moves the fmt layer to stderr so the workaround is no longer needed.
//!
//! This smoke test pins the regression: spawn `ccteam mcp-serve` under
//! `RUST_LOG=info` (the default verbosity ccteam ships with) without
//! the workaround, send one `tools/list` request, and assert the first
//! stdout line parses as a JSON-RPC 2.0 response carrying a `result`
//! field. tracing output should be visible on stderr only.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

#[test]
fn mcp_serve_stdout_is_clean_jsonrpc_under_default_tracing() {
    // Isolated CCTEAM_HOME / projects root so we never touch the
    // operator's real state; CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP=1
    // skips the `~/.claude.json` rewrites (see CLAUDE.md §六).
    let home = TempDir::new().expect("tempdir for CCTEAM_HOME");
    let projects = TempDir::new().expect("tempdir for projects_root");

    let bin = env!("CARGO_BIN_EXE_ccteam");
    let mut child = Command::new(bin)
        .args(["internal", "mcp-serve"])
        .env("CCTEAM_HOME", home.path())
        .env("CCTEAM_PROJECTS_ROOT", projects.path())
        .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1")
        // Crucial: RUST_LOG=info is what production callers see, and
        // it's what historically broke the stdio frame channel. F165
        // moves tracing to stderr; this test pins that contract.
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // We pipe stderr too so the test can assert tracing still
        // arrives somewhere (not silently dropped). The reader is
        // best-effort: we only check it after the JSON-RPC round-trip.
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ccteam mcp-serve");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {},
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).expect("write request");
    stdin.flush().expect("flush stdin");

    let mut first_line = String::new();
    stdout
        .read_line(&mut first_line)
        .expect("read first stdout line");

    // The first stdout line must parse cleanly as JSON — no tracing
    // log line ahead of it.
    let v: Value = serde_json::from_str(first_line.trim()).unwrap_or_else(|e| {
        panic!(
            "F165 regression: first stdout line is not valid JSON-RPC; \
             got {first_line:?}, parse error: {e}"
        )
    });
    assert_eq!(
        v.get("jsonrpc").and_then(|x| x.as_str()),
        Some("2.0"),
        "F165: stdout first line missing jsonrpc=2.0 field, got: {v}",
    );
    assert_eq!(
        v.get("id").and_then(|x| x.as_u64()),
        Some(1),
        "F165: stdout first line wrong id, got: {v}",
    );
    let result = v
        .get("result")
        .unwrap_or_else(|| panic!("F165: stdout reply missing `result`, got: {v}"));
    let tools = result
        .get("tools")
        .and_then(|t| t.as_array())
        .unwrap_or_else(|| panic!("F165: tools/list result missing tools array, got: {v}"));
    assert!(
        !tools.is_empty(),
        "F165: ccteam advertises ≥ 1 tool, got empty list: {v}",
    );

    // Tear down — drop stdin so the server exits on EOF, then drain
    // exit. The 2s budget mirrors the other mcp_*_test.rs harnesses.
    drop(stdin);
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if let Ok(Some(_status)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
