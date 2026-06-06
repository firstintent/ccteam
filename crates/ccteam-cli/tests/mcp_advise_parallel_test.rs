//! V0.6.5 F153 — integration tests for `mcp__ccteam__advise_parallel`
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

/// F153 acceptance #1 — N=4 vendors=["claude","codex"] → 4 slots,
/// round-robin to 2 claude + 2 codex; every slot returns Ok.
#[test]
fn advise_parallel_n4_round_robin_returns_four_answers() {
    let (_tmp, home, projects) = fresh_dirs();
    let (_claude_dir, claude_bin) = fake_bin("Claude says use Foobar because it is the simplest.");
    let (_codex_dir, codex_bin) = fake_bin(
        r#"{"type":"thread.started","thread_id":"t1"}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Codex says use Foobar for the same reason."}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
    );

    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[(CLAUDE_BIN_ENV, &claude_bin), (CODEX_BIN_ENV, &codex_bin)],
    );
    let resp = srv.call_tool(
        2,
        "ccteam__advise_parallel",
        json!({
            "question": "Should I pick Foobar?",
            "n": 4,
            "vendors": ["claude", "codex"],
        }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], true, "got body: {parsed}");
    let answers = parsed["answers"].as_array().expect("answers is array");
    assert_eq!(answers.len(), 4, "expected 4 answers, got {answers:?}");

    // Round-robin order: [claude, codex, claude, codex].
    let vendors: Vec<&str> = answers
        .iter()
        .map(|a| a["vendor"].as_str().unwrap())
        .collect();
    assert_eq!(vendors, vec!["claude", "codex", "claude", "codex"]);
    for a in answers {
        assert_eq!(a["status"]["status"], "ok", "all slots must be ok; got {a}");
        assert!(!a["answer"].as_str().unwrap().is_empty());
    }

    // Budget ledger: one sample per slot (4 total).
    let ledger_path = home.join("cost-budget.json");
    assert!(ledger_path.is_file(), "ledger file must be written");
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(&ledger_path).unwrap()).unwrap();
    let samples = ledger["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 4, "expected 4 cost samples");
    let claude_n = samples.iter().filter(|s| s["vendor"] == "claude").count();
    let codex_n = samples.iter().filter(|s| s["vendor"] == "codex").count();
    assert_eq!(claude_n, 2);
    assert_eq!(codex_n, 2);

    srv.shutdown();
}

/// F153 acceptance #2 — N=2 vendors=["claude"] → both slots claude.
#[test]
fn advise_parallel_n2_claude_only_returns_two_claude_answers() {
    let (_tmp, home, projects) = fresh_dirs();
    let (_claude_dir, claude_bin) = fake_bin("Claude advice text.");

    let mut srv = McpServer::spawn(&home, &projects, &[(CLAUDE_BIN_ENV, &claude_bin)]);
    let resp = srv.call_tool(
        2,
        "ccteam__advise_parallel",
        json!({
            "question": "Q?",
            "n": 2,
            "vendors": ["claude"],
        }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], true, "got body: {parsed}");
    let answers = parsed["answers"].as_array().unwrap();
    assert_eq!(answers.len(), 2);
    for a in answers {
        assert_eq!(a["vendor"], "claude");
        assert_eq!(a["status"]["status"], "ok");
    }

    // 2 claude samples, no codex.
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("cost-budget.json")).unwrap())
            .unwrap();
    let samples = ledger["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 2);
    assert!(samples.iter().all(|s| s["vendor"] == "claude"));

    srv.shutdown();
}

/// F153 — Codex unavailable: a mixed N=2 slot with [claude, codex]
/// where codex binary is non-exec → the codex slot's status flips to
/// `unavailable` but the array still has 2 rows and the call returns
/// `ok:true`.
#[test]
fn advise_parallel_codex_unavailable_slot_marks_status_but_array_still_full() {
    let (tmp, home, projects) = fresh_dirs();
    let (_claude_dir, claude_bin) = fake_bin("Claude says A.");
    let nonexec = tmp.path().join("notexec");
    std::fs::write(&nonexec, b"plain text").unwrap();

    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[(CLAUDE_BIN_ENV, &claude_bin), (CODEX_BIN_ENV, &nonexec)],
    );
    let resp = srv.call_tool(
        2,
        "ccteam__advise_parallel",
        json!({
            "question": "Q?",
            "n": 2,
            "vendors": ["claude", "codex"],
            "timeout_secs": 5,
        }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], true, "got body: {parsed}");

    let answers = parsed["answers"].as_array().unwrap();
    assert_eq!(
        answers.len(),
        2,
        "N slots preserved regardless of vendor status"
    );
    assert_eq!(answers[0]["vendor"], "claude");
    assert_eq!(answers[0]["status"]["status"], "ok");
    assert_eq!(answers[1]["vendor"], "codex");
    assert_eq!(answers[1]["status"]["status"], "unavailable");
    let reason = answers[1]["status"]["detail"]["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("detail.reason missing in {}", answers[1]));
    assert!(
        reason.contains("not on PATH or not executable"),
        "unavailable reason must call out binary problem; got: {reason}"
    );
    // The unavailable slot returns an empty answer body so callers can
    // render a placeholder.
    assert_eq!(answers[1]["answer"], "");

    // Budget ledger: only the ok slot charged.
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("cost-budget.json")).unwrap())
            .unwrap();
    let samples = ledger["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0]["vendor"], "claude");

    srv.shutdown();
}

/// F153 acceptance #3 — pre-call budget check refuses before spawning
/// any advisor subprocess.
#[test]
fn advise_parallel_budget_exceeded_does_not_spawn_advisors() {
    let (_tmp, home, projects) = fresh_dirs();
    // Pre-seed the ledger past the cap.
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

    // Explode script proves we never reached the spawn path.
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
        "ccteam__advise_parallel",
        json!({
            "question": "Q?",
            "n": 2,
            "vendors": ["claude"],
            "max_cost_usd": 0.5,
        }),
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

/// F153 — `n` out of range is caught upstream of any spawn / budget.
#[test]
fn advise_parallel_n_out_of_range_returns_invalid_input() {
    let (_tmp, home, projects) = fresh_dirs();
    let mut srv = McpServer::spawn(
        &home,
        &projects,
        &[(CLAUDE_BIN_ENV, std::path::Path::new("/tmp/does-not-exist"))],
    );
    let resp = srv.call_tool(
        2,
        "ccteam__advise_parallel",
        json!({ "question": "Q?", "n": 9 }),
    );
    let parsed = parse_tool_response(&resp);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"], "invalid_input");
    assert!(parsed["detail"]
        .as_str()
        .unwrap()
        .contains("`n` must be in 2..=8"));
    srv.shutdown();
}
