//! V0.6.5 F148 — end-to-end wire test for `/ccteam-creator` Phase 5.6.
//!
//! Background — F148 fixes the deepest root cause of the nas-box005
//! deploy fiasco: `skills/ccteam-creator/SKILL.md` Phase 5.6 used to
//! tell the LLM to "call `ccteam_imd::register_bot(...)`" — a Rust
//! function the LLM has no way to invoke. The result: workflow.yaml
//! landed, .claude/agents/<role>.md landed, .mcp.json landed — but
//! `~/.ccteam/imd/registry/<slug>/<role>.json` was never written,
//! so the daemon's `list_bots()` returned 0 → every Telegram message
//! got dropped without ever reaching a tmux session.
//!
//! V0.6.5 F146 made `mcp__ccteam__chat_register_bot` a real MCP tool;
//! F148 rewrites Phase 5.6 to tell the LLM to invoke that tool. The
//! skill body is markdown read by the LLM, not code we can execute
//! in a test — so these tests verify two things instead:
//!
//! 1. **Wire contract** — driving `ccteam mcp-serve` with the exact
//!    JSON args the rewritten Phase 5.6 documents lands the
//!    registration file at the path `list_bots()` reads. This catches
//!    regressions where the MCP tool's arg names / vendor casing /
//!    path layout drifts away from what the skill instructs.
//!
//! 2. **Skill text guard** — `skills/ccteam-creator/SKILL.md` Phase
//!    5.6 mentions `mcp__ccteam__chat_register_bot` (the F148 contract)
//!    and **not** the legacy `ccteam_imd::register_bot` Rust call.
//!    This prevents a future SKILL.md edit from reverting to the
//!    pre-F148 stub text.
//!
//! These tests stub the IM provider + the claude-tui adapter (neither
//! is exercised) — the **real** end-to-end "TG message → bot reply"
//! verification is Wave 4 host-probe on nas-box005.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

// ─────────────────────────── MCP test harness ───────────────────────────

struct McpServer {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl McpServer {
    fn spawn(home: &Path, projects: &Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_ccteam");
        let mut child = Command::new(bin)
            .arg("mcp-serve")
            .env("CCTEAM_HOME", home)
            .env("CCTEAM_PROJECTS_ROOT", projects)
            .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1")
            // V0.6.5 F165: tracing for `ccteam mcp-serve` now writes
            // to stderr (stdout is reserved for JSON-RPC frames), so
            // the V0.6.5 F147 `RUST_LOG=error` workaround is gone —
            // default tracing is fine here.
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

// ─────────────────────────── creator-flow fixture ───────────────────────────

/// Simulates what `/ccteam-creator` Phase 5.1 leaves on disk: a fresh
/// `HOME` with `.ccteam/im/credentials.json` containing a Telegram
/// block whose `allowed_chat_ids[0]` is the value Phase 5.6 passes
/// to `chat_register_bot` as `im_chat_id`.
struct CreatorFixture {
    _tmp: TempDir,
    ccteam_home: PathBuf,
    projects_root: PathBuf,
}

impl CreatorFixture {
    /// Stub IM credentials so the "creator just finished Phase 5.1" state
    /// is what the MCP server sees.
    fn fresh_with_telegram_chat(chat_id: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let ccteam_home = tmp.path().join("ccteam-home");
        let projects_root = tmp.path().join("projects");
        std::fs::create_dir_all(ccteam_home.join("im")).unwrap();
        std::fs::create_dir_all(&projects_root).unwrap();

        // Phase 5.1 output — Telegram credentials.json with one chat id.
        let creds = json!({
            "telegram": {
                "bot_token": "STUB-TG-TOKEN-12345",
                "allowed_chat_ids": [chat_id],
            }
        });
        std::fs::write(
            ccteam_home.join("im").join("credentials.json"),
            serde_json::to_string_pretty(&creds).unwrap(),
        )
        .unwrap();

        Self {
            _tmp: tmp,
            ccteam_home,
            projects_root,
        }
    }

    /// Mirror what Phase 5.3 (`render_workflow_template`) writes:
    /// `<project>/.ccteam/workflow.yaml`. Body is a minimal chat-mode
    /// stub so the daemon's loader is happy.
    fn seed_workflow_yaml(&self, slug: &str, role: &str, bot_handle: &str) -> PathBuf {
        let project_dir = self.projects_root.join(slug);
        let ccteam = project_dir.join(".ccteam");
        std::fs::create_dir_all(&ccteam).unwrap();
        let body = format!(
            r#"name: {slug}
agents:
  {role}:
    executor: claude
    mode: chat
    trigger: chat
chat:
  bot_name: "{bot_handle}"
"#
        );
        let path = ccteam.join("workflow.yaml");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// Phase 5.4 — drop persona body into `<project>/.claude/agents/<role>.md`.
    fn seed_persona_md(&self, slug: &str, role: &str) -> PathBuf {
        let dir = self.projects_root.join(slug).join(".claude").join("agents");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{role}.md"));
        let body = format!(
            "---\nname: {role}\ndescription: stub persona for F148 test\n---\n# {role}\n\nstub body\n",
        );
        std::fs::write(&path, body).unwrap();
        path
    }
}

// ───────────────────────────── tests ─────────────────────────────

/// F148 acceptance — driving the rewritten Phase 5.6 MCP call against
/// `ccteam mcp-serve` lands the registration JSON exactly where
/// `list_bots()` reads from, with the lowercase vendor canonical form
/// the daemon's `BotRegistration` deserialize requires.
///
/// This is the wire-contract proof that the SKILL.md text (which the
/// LLM follows) is reachable end-to-end. Stubs the IM provider + the
/// claude-tui adapter; real "TG → bot reply" verification is the
/// Wave 4 nas-box005 host-probe.
#[test]
fn t_creator_phase_5_6_mcp_call_lands_registration_with_lowercase_vendor() {
    let chat_id = "987654321";
    let slug = "tg-helper";
    let role = "tech-helper";
    let bot_handle = "@my_helper_bot";

    let fx = CreatorFixture::fresh_with_telegram_chat(chat_id);
    // Phases 5.3 + 5.4 prep — workflow.yaml + persona .md on disk so
    // the daemon's eventual roster pass has something to consume.
    let workflow_yaml = fx.seed_workflow_yaml(slug, role, bot_handle);
    let persona_md = fx.seed_persona_md(slug, role);

    // Sanity — Phase 5.1 stub left credentials.json behind.
    let creds_path = fx.ccteam_home.join("im").join("credentials.json");
    assert!(
        creds_path.exists(),
        "Phase 5.1 stub credentials.json missing"
    );

    // Phase 5.6 wire call — exact arg shape the SKILL.md docs.
    let mut srv = McpServer::spawn(&fx.ccteam_home, &fx.projects_root);
    let body = srv.call_tool(
        "ccteam__chat_register_bot",
        json!({
            "workflow_slug": slug,
            "role": role,
            "vendor": "claude",
            "im_platform": "telegram",
            "im_chat_id": chat_id,
            "persona_id": role,
        }),
    );

    // Response shape — { ok: true, path: "..." } per F146 contract.
    assert_eq!(
        body["ok"], true,
        "Phase 5.6 MCP call must succeed; body={body:?}"
    );
    assert_eq!(body["workflow_slug"], slug);
    assert_eq!(body["role"], role);
    let path_str = body["path"].as_str().expect("path field");
    let registration_path = PathBuf::from(path_str);

    // Acceptance gate — registration file lands where list_bots reads.
    assert!(
        registration_path.exists(),
        "registration JSON must land on disk at {}",
        registration_path.display(),
    );
    // Layout assertion — ccteam-imd::registration_path_in canonical form.
    let expected = ccteam_imd::registration_path_in(&fx.ccteam_home, slug, role);
    assert_eq!(
        registration_path.canonicalize().unwrap(),
        expected.canonicalize().unwrap(),
        "registration must land at <ccteam_root>/imd/registry/<slug>/<role>.json",
    );

    // On-disk content gate — lowercase vendor (Bug A防线 from nas-box005).
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&registration_path).unwrap()).unwrap();
    assert_eq!(
        on_disk["vendor"], "claude",
        "vendor on disk MUST be lowercase to keep daemon's BotRegistration deserialize happy",
    );
    assert_eq!(on_disk["workflow_slug"], slug);
    assert_eq!(on_disk["role"], role);
    assert_eq!(on_disk["im_platform"], "telegram");
    assert_eq!(on_disk["im_chat_id"], chat_id);
    assert_eq!(on_disk["persona_id"], role);

    // Round-trip via list_bots_in — this is the *daemon's* consumption
    // path; if this returns 1 row, the registry watcher will too.
    let bots = ccteam_imd::list_bots_in(&fx.ccteam_home, Some(slug)).unwrap();
    assert_eq!(
        bots.len(),
        1,
        "daemon-side list_bots must surface the registration exactly once",
    );
    assert_eq!(bots[0].workflow_slug, slug);
    assert_eq!(bots[0].role, role);
    assert_eq!(bots[0].im_platform, "telegram");
    assert_eq!(bots[0].im_chat_id, chat_id);

    // Earlier-phase artifacts must still be on disk (Phase 5.6 only
    // touches the registry; it doesn't disturb 5.3 / 5.4 outputs).
    assert!(
        workflow_yaml.exists(),
        "Phase 5.3 workflow.yaml should be intact"
    );
    assert!(
        persona_md.exists(),
        "Phase 5.4 persona .md should be intact"
    );

    srv.shutdown();
}

/// F148 SKILL.md text guard — `Phase 5.6` must mention the MCP tool by
/// name and must NOT still claim the LLM should call the Rust function
/// `ccteam_imd::register_bot` (the pre-F148 stub instruction).
///
/// This is a regression test against doc drift — the skill body is
/// LLM-consumed, so the only way to keep the wire contract honest is
/// to gate the text it surfaces.
#[test]
fn t_creator_skill_phase_5_6_documents_mcp_tool_not_rust_function() {
    // Locate the skill from the repo root (CARGO_MANIFEST_DIR is the
    // crate dir; the workspace root is two levels up).
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("ccteam-cli crate must sit two levels deep in workspace");
    let skill_path = workspace_root
        .join("skills")
        .join("ccteam-creator")
        .join("SKILL.md");
    let body = std::fs::read_to_string(&skill_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", skill_path.display()));

    // Locate the Phase 5.6 section.
    let phase_5_6 = body
        .split("## 5.6")
        .nth(1)
        .expect("SKILL.md must have a `## 5.6` Phase 5.6 section");
    // Section ends at the next `## ` heading.
    let phase_5_6_body = phase_5_6.split("\n## ").next().unwrap_or(phase_5_6);

    // F148 contract — must instruct invoking the MCP tool.
    assert!(
        phase_5_6_body.contains("mcp__ccteam__chat_register_bot"),
        "Phase 5.6 must instruct the LLM to invoke `mcp__ccteam__chat_register_bot`; got: {phase_5_6_body}",
    );
    // F148 contract — must NOT still tell the LLM to call the Rust fn
    // (the pre-F148 stub instruction the LLM physically cannot follow).
    assert!(
        !phase_5_6_body.contains("ccteam_imd::register_bot("),
        "Phase 5.6 must not still tell the LLM to call the Rust function `ccteam_imd::register_bot(...)`; that was the pre-F148 bug. Got:\n{phase_5_6_body}",
    );
    // F148 contract — lowercase vendor must be called out explicitly
    // (Bug A防线 — daemon BotRegistration deserialize trips on PascalCase).
    assert!(
        phase_5_6_body.contains("lowercase"),
        "Phase 5.6 must call out the lowercase vendor requirement (Bug A); got: {phase_5_6_body}",
    );
    // F148 contract — error handling must distinguish already_registered
    // (idempotent OK) from other errors (stop).
    assert!(
        phase_5_6_body.contains("already_registered"),
        "Phase 5.6 must document the `already_registered` idempotent-OK branch; got: {phase_5_6_body}",
    );

    // Phase 5.9 reply template must mention the registry path so the
    // user sees the bot actually landed somewhere.
    let phase_5_9 = body
        .split("## 5.9")
        .nth(1)
        .expect("SKILL.md must have a `## 5.9` Phase 5.9 section");
    let phase_5_9_body = phase_5_9.split("\n## ").next().unwrap_or(phase_5_9);
    assert!(
        phase_5_9_body.contains("imd/registry/"),
        "Phase 5.9 reply must mention `imd/registry/<slug>/<role>.json` so the user sees the bot was registered; got: {phase_5_9_body}",
    );
}
