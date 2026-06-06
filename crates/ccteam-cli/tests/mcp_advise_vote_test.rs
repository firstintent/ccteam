//! V0.6.5 F152 — integration tests for `mcp__ccteam__advise_vote`
//! real implementation.
//!
//! These tests drive `ccteam mcp-serve` over stdio JSON-RPC, with
//! `CCTEAM_CLAUDE_BIN` / `CCTEAM_CODEX_BIN` env vars pointed at a
//! per-test fake script that emits a deterministic body so the
//! assertions are hermetic (no real claude / codex binary required).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_harness::{CLAUDE_BIN_ENV, CODEX_BIN_ENV};

/// Build a fake binary script that echoes the supplied stdout on
/// invocation and exits 0. Returns the (guard, path) so the caller
/// keeps the tempdir alive for the test's duration.
fn fake_bin(stdout: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("fake.sh");
    let mut body = String::from("#!/usr/bin/env bash\n");
    body.push_str("cat <<'EOF'\n");
    body.push_str(stdout);
    if !stdout.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("EOF\n");
    body.push_str("exit 0\n");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f.sync_all().unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    (dir, path)
}

struct McpServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpServer {
    fn spawn(
        home: &std::path::Path,
        projects: &std::path::Path,
        env: &[(&str, &std::path::Path)],
    ) -> Self {
        let bin = env!("CARGO_BIN_EXE_ccteam");
        let mut cmd = Command::new(bin);
        cmd.args(["internal", "mcp-serve"])
            .env("CCTEAM_HOME", home)
            .env("CCTEAM_PROJECTS_ROOT", projects)
            .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1")
            .env_remove("CCTEAM_DISABLE_TOOLS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
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

    fn call_tool(&mut self, id: u64, tool: &str, args: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }));
        self.recv()
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

/// Extract the JSON body from the MCP `content[0].text` envelope.
fn parse_tool_response(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("missing content[0].text in {resp}"));
    serde_json::from_str(text).expect("tool body parses as JSON")
}

/// Set up a fresh ccteam-root + projects dir.
fn fresh_dirs() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    (tmp, home, projects)
}

/// F152 acceptance #1 — happy path: Claude returns prose, Codex
/// returns a JSONL stream → verdict synthesised, agreement classified,
/// budget ledger advances.
#[test]
fn advise_vote_happy_path_real_claude_real_codex() {
    let (_tmp, home, projects) = fresh_dirs();
    let (_claude_dir, claude_bin) = fake_bin(
        "Use approach Foobar because it is fast secure maintainable performant cheap minimal.",
    );
    let (_codex_dir, codex_bin) = fake_bin(
        r#"{"type":"thread.started","thread_id":"t1"}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Pick approach Foobar because simple secure maintainable performant cheap minimal."}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
    );

    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[(CLAUDE_BIN_ENV, &claude_bin), (CODEX_BIN_ENV, &codex_bin)],
    );
    let resp = srv.call_tool(
        2,
        "ccteam__advise_vote",
        json!({ "question": "Should I pick Foobar or Barbaz?" }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], true, "got body: {parsed}");
    assert!(parsed["claude_answer"].as_str().unwrap().contains("Foobar"));
    assert!(parsed["codex_answer"].as_str().unwrap().contains("Foobar"));
    assert_eq!(parsed["codex_status"]["status"], "ok");
    let verdict = parsed["verdict"].as_str().unwrap();
    assert!(!verdict.is_empty(), "verdict must not be empty");
    assert!(
        !verdict.contains("Codex unavailable"),
        "happy path verdict must NOT mention Codex unavailable; got: {verdict}"
    );
    assert_eq!(parsed["agreement"], "agree");

    // Budget ledger: 3 cost samples (claude advisor + codex advisor +
    // verdict synth).
    let ledger_path = home.join("cost-budget.json");
    assert!(ledger_path.is_file(), "ledger file must be written");
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(&ledger_path).unwrap()).unwrap();
    let samples = ledger["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 3, "expected 3 cost samples");
    let claude_n = samples.iter().filter(|s| s["vendor"] == "claude").count();
    let codex_n = samples.iter().filter(|s| s["vendor"] == "codex").count();
    assert_eq!(claude_n, 2, "expected 2 claude samples (advisor + synth)");
    assert_eq!(codex_n, 1);

    srv.shutdown();
}

/// F152 acceptance #2 — Codex unavailable: verdict must explicitly say
/// "Codex unavailable: <reason>"; result still `ok:true`.
#[test]
fn advise_vote_codex_unavailable_marks_status_and_verdict() {
    let (tmp, home, projects) = fresh_dirs();
    let (_claude_dir, claude_bin) = fake_bin("Use approach Foobar.");
    // Non-exec text file → codex_binary_usable() returns false →
    // CodexStatus::Unavailable.
    let nonexec = tmp.path().join("notexec");
    std::fs::write(&nonexec, b"plain text").unwrap();

    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[(CLAUDE_BIN_ENV, &claude_bin), (CODEX_BIN_ENV, &nonexec)],
    );
    let resp = srv.call_tool(
        2,
        "ccteam__advise_vote",
        json!({ "question": "Should I pick Foobar?", "codex_timeout_secs": 5 }),
    );
    let parsed = parse_tool_response(&resp);

    assert_eq!(parsed["ok"], true);
    assert!(parsed["claude_answer"].as_str().unwrap().contains("Foobar"));
    assert!(parsed["codex_answer"].is_null());
    assert_eq!(parsed["codex_status"]["status"], "unavailable");
    let reason = parsed["codex_status"]["detail"]["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("detail.reason missing in {parsed}"));
    assert!(
        reason.contains("not on PATH or not executable"),
        "unavailable reason must call out the binary problem; got: {reason}"
    );

    let verdict = parsed["verdict"].as_str().unwrap();
    assert!(
        verdict.contains("Codex unavailable"),
        "verdict MUST explicitly state Codex unavailable; got: {verdict}"
    );
    assert_eq!(parsed["agreement"], "unknown");

    // Budget ledger: 2 Claude samples (advisor + synth), 0 codex.
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("cost-budget.json")).unwrap())
            .unwrap();
    let samples = ledger["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 2);
    assert!(samples.iter().all(|s| s["vendor"] == "claude"));

    srv.shutdown();
}

/// F152 acceptance #2b — Codex spawn error (binary exists + is
/// executable but exits non-zero) → status `error`, verdict mentions
/// unavailability.
#[test]
fn advise_vote_codex_error_status_when_binary_exits_nonzero() {
    let (_tmp, home, projects) = fresh_dirs();
    let (_claude_dir, claude_bin) = fake_bin("Pick Foobar.");
    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[
            (CLAUDE_BIN_ENV, &claude_bin),
            (CODEX_BIN_ENV, std::path::Path::new("/bin/false")),
        ],
    );
    let resp = srv.call_tool(
        2,
        "ccteam__advise_vote",
        json!({ "question": "Should I pick Foobar?", "codex_timeout_secs": 5 }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["codex_status"]["status"], "error");
    let verdict = parsed["verdict"].as_str().unwrap();
    assert!(
        verdict.contains("Codex unavailable"),
        "error path verdict must include Codex unavailable note; got: {verdict}"
    );
    assert_eq!(parsed["agreement"], "unknown");
    srv.shutdown();
}

/// F152 acceptance #3 — budget pre-check refuses before spawning any
/// advisor subprocess.
#[test]
fn advise_vote_budget_exceeded_does_not_spawn_advisors() {
    let (_tmp, home, projects) = fresh_dirs();
    // Pre-seed the ledger past the cap by writing the JSON directly so
    // the test doesn't depend on `ccteam_core::advise` re-export
    // visibility.
    let now = chrono::Utc::now().to_rfc3339();
    let mut samples = String::from("[");
    for i in 0..15 {
        if i > 0 {
            samples.push(',');
        }
        samples.push_str(&format!(r#"{{"vendor":"claude","usd":0.10,"ts":"{now}"}}"#));
    }
    samples.push(']');
    let body = format!(r#"{{"samples":{samples}}}"#);
    std::fs::write(home.join("cost-budget.json"), body).unwrap();

    // Explode script — proves we never reached the spawn path.
    let tmp_explode = TempDir::new().unwrap();
    let explode = tmp_explode.path().join("explode.sh");
    std::fs::write(
        &explode,
        "#!/usr/bin/env bash\necho 'spawned!' >&2\nexit 99\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&explode).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&explode, perms).unwrap();

    let mut srv = McpServer::spawn(&home, &projects, &[(CLAUDE_BIN_ENV, &explode)]);
    let resp = srv.call_tool(
        2,
        "ccteam__advise_vote",
        json!({ "question": "Q?", "max_cost_usd": 0.5 }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"], "budget_exceeded");

    // Budget ledger size unchanged (no advisor calls charged).
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("cost-budget.json")).unwrap())
            .unwrap();
    let samples = ledger["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 15, "budget refusal must not append");

    srv.shutdown();
}

/// F152 — empty question is caught upstream of any spawn / budget
/// check.
#[test]
fn advise_vote_empty_question_returns_invalid_input() {
    let (_tmp, home, projects) = fresh_dirs();
    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[(CLAUDE_BIN_ENV, std::path::Path::new("/tmp/does-not-exist"))],
    );
    let resp = srv.call_tool(2, "ccteam__advise_vote", json!({ "question": "   " }));
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"], "invalid_input");
    srv.shutdown();
}
