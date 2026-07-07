//! v0.8.22 — bare `ccteam doctor` readiness checkup end-to-end tests.
//!
//! Before this version, a bare `ccteam doctor` (no flags) only printed
//! the implicit pricing-staleness check; the genuinely useful readiness
//! checks (claude/codex binaries, tmux, MCP registration, daemon health)
//! all hid behind opt-in flags. `crates/ccteam-cli/src/doctor.rs` now
//! renders one `[PASS]/[WARN]/[FAIL]/[SKIP]` line per check plus a
//! summary line, exiting 1 iff any check FAILs.
//!
//! Every test here fully sandboxes the environment (`CCTEAM_CLAUDE_BIN`,
//! `CLAUDE_CONFIG_HOME`, `CODEX_HOME`, `CCTEAM_HOME`, `HOME`) so the
//! binary never reads or writes the developer's real `~/.claude.json` /
//! `~/.codex/config.toml` / `~/.ccteam` (CLAUDE.md: polluting the real
//! `~/.claude.json` breaks the owner's login).

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;

/// Write a fake `claude`-shaped executable that prints a version line
/// and exits 0, so the "claude binary" check PASSes without depending
/// on a real Claude Code install being present on the test host.
fn write_fake_claude_bin(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("fake-claude.sh");
    std::fs::write(&path, "#!/bin/sh\necho \"claude 9.9.9 (fake)\"\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

/// Common sandbox: isolated `CCTEAM_HOME` / `HOME` / `CODEX_HOME` and a
/// `CLAUDE_CONFIG_HOME` pointing at `<tmp>/.claude` (so the resolved
/// `~/.claude.json` sibling is `<tmp>/.claude.json` — same convention
/// `ccteam_core::projects::resolve_claude_json_path` uses).
struct Sandbox {
    _tmp: TempDir,
    claude_config_home: std::path::PathBuf,
    claude_json: std::path::PathBuf,
    ccteam_home: std::path::PathBuf,
    fake_home: std::path::PathBuf,
    codex_home: std::path::PathBuf,
    claude_bin: std::path::PathBuf,
}

fn sandbox() -> Sandbox {
    let tmp = TempDir::new().unwrap();
    let claude_config_home = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_config_home).unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let ccteam_home = tmp.path().join("ccteam-home");
    let fake_home = tmp.path().join("fake-home");
    std::fs::create_dir_all(&fake_home).unwrap();
    let codex_home = tmp.path().join("codex-home");
    let claude_bin = write_fake_claude_bin(tmp.path());
    Sandbox {
        _tmp: tmp,
        claude_config_home,
        claude_json,
        ccteam_home,
        fake_home,
        codex_home,
        claude_bin,
    }
}

fn run_bare_doctor(sb: &Sandbox) -> (String, i32) {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .arg("doctor")
        .env("CCTEAM_CLAUDE_BIN", &sb.claude_bin)
        .env("CLAUDE_CONFIG_HOME", &sb.claude_config_home)
        .env("CCTEAM_HOME", &sb.ccteam_home)
        .env("HOME", &sb.fake_home)
        .env("CODEX_HOME", &sb.codex_home)
        .output()
        .expect("spawn ccteam doctor");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn bare_doctor_renders_every_check_line_and_a_summary() {
    let sb = sandbox();
    let (stdout, _code) = run_bare_doctor(&sb);
    for expected in [
        "ccteam doctor: readiness checkup",
        "claude binary",
        "codex binary",
        "tmux",
        "MCP (claude)",
        "MCP (codex)",
        "daemon",
        "pricing tables",
        "home layout",
        "summary:",
    ] {
        assert!(
            stdout.contains(expected),
            "bare doctor output missing {expected:?}. stdout:\n{stdout}",
        );
    }
}

#[test]
fn bare_doctor_fails_when_mcp_not_registered() {
    // Fresh `.claude.json` (no `mcpServers.ccteam`) → the MCP (claude)
    // check is a critical FAIL → non-zero exit.
    let sb = sandbox();
    assert!(
        !sb.claude_json.exists(),
        "fixture: .claude.json should not exist yet",
    );
    let (stdout, code) = run_bare_doctor(&sb);
    assert!(
        stdout.contains("[FAIL] MCP (claude)"),
        "expected a FAIL MCP (claude) line. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("ccteam config mcp"),
        "FAIL line should name the fix command. stdout:\n{stdout}",
    );
    assert_eq!(code, 1, "critical FAIL must exit 1. stdout:\n{stdout}");
    assert!(
        stdout.contains("NOT READY"),
        "summary should say NOT READY. stdout:\n{stdout}",
    );
}

#[test]
fn bare_doctor_fails_when_claude_binary_is_not_resolvable() {
    let sb = sandbox();
    // Pre-register MCP so the only FAIL is the claude binary itself.
    std::fs::write(
        &sb.claude_json,
        json!({"mcpServers": {"ccteam": {"command": "/usr/bin/true", "args": [], "env": {}}}})
            .to_string(),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .arg("doctor")
        .env("CCTEAM_CLAUDE_BIN", sb._tmp.path().join("does-not-exist"))
        .env("CLAUDE_CONFIG_HOME", &sb.claude_config_home)
        .env("CCTEAM_HOME", &sb.ccteam_home)
        .env("HOME", &sb.fake_home)
        .env("CODEX_HOME", &sb.codex_home)
        .output()
        .expect("spawn ccteam doctor");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("[FAIL] claude binary"),
        "expected a FAIL claude binary line. stdout:\n{stdout}",
    );
    assert_eq!(
        out.status.code().unwrap_or(-1),
        1,
        "missing claude binary must exit 1. stdout:\n{stdout}",
    );
}

#[test]
fn bare_doctor_exits_zero_when_claude_binary_and_mcp_are_both_ok() {
    let sb = sandbox();
    // Pre-register the ccteam MCP server so MCP (claude) PASSes; the fake
    // claude binary makes "claude binary" PASS too. Every other check
    // (codex/tmux/daemon/pricing/home-layout) is WARN/SKIP-only by
    // design, so the overall checkup must report READY / exit 0.
    std::fs::write(
        &sb.claude_json,
        json!({"mcpServers": {"ccteam": {"command": "/usr/bin/true", "args": [], "env": {}}}})
            .to_string(),
    )
    .unwrap();

    let (stdout, code) = run_bare_doctor(&sb);
    assert!(
        stdout.contains("[PASS] claude binary"),
        "expected a PASS claude binary line. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("[PASS] MCP (claude)"),
        "expected a PASS MCP (claude) line. stdout:\n{stdout}",
    );
    assert_eq!(code, 0, "no critical check should FAIL. stdout:\n{stdout}");
    assert!(
        stdout.contains("READY"),
        "summary should say READY. stdout:\n{stdout}",
    );
}

#[test]
fn doctor_help_hides_migration_flags_but_keeps_verify_mcp_visible() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["doctor", "--help"])
        .output()
        .expect("spawn ccteam doctor --help");
    assert!(out.status.success(), "ccteam doctor --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Historical one-shot migration / repair flags must no longer clutter
    // bare `--help` (they still work when passed explicitly — see
    // doctor_codex_test.rs / doctor_cost_orphan_test.rs / etc.).
    for hidden in [
        "--tool-surface",
        "--install-memory-bridge",
        "--reset-shipped-teams",
        "--validate-team",
        "--migrate-recommended-agents",
        "--screenshot-smoke",
        "--migrate-v041-to-v042",
        "--migrate-workflow-to-ccteam-dir",
        "--gc-claude-jobs",
        "--update-hooks",
        "--check-pricing-version",
        "--check-codex-version",
        "--check-codex-auth",
        "--check-codex-auto-critic",
        "--check-cost-orphan",
        "--install-hooks",
        "--migrate-hook-commands",
    ] {
        assert!(
            !stdout.contains(hidden),
            "ccteam doctor --help should hide {hidden:?} now; got:\n{stdout}",
        );
    }

    // `--verify-mcp` (+ its `--json` pair) is the one flag CLAUDE.md
    // calls out by name as needing to keep working — it must stay
    // visible.
    assert!(
        stdout.contains("--verify-mcp"),
        "ccteam doctor --help must keep advertising --verify-mcp; got:\n{stdout}",
    );
    assert!(
        stdout.contains("--json"),
        "ccteam doctor --help must keep advertising --json; got:\n{stdout}",
    );
}
