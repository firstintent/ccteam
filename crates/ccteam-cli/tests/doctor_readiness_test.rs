//! Bare `ccteam doctor` readiness checkup end-to-end tests.
//!
//! `crates/ccteam-cli/src/doctor.rs` renders one consolidated vendor row,
//! grouped ccteam/project advisories, and a summary, exiting 1 only when a
//! required check FAILs.
//!
//! Every test here fully sandboxes the environment (`CCTEAM_CLAUDE_BIN`,
//! `CLAUDE_CONFIG_HOME`, `CODEX_HOME`, `KIMI_CODE_HOME`, `XDG_CONFIG_HOME`,
//! `CCTEAM_HOME`, `HOME`, and all vendor binary overrides) so the
//! binary never reads or writes the developer's real `~/.claude.json` /
//! `~/.codex/config.toml` / `~/.ccteam` (CLAUDE.md: polluting the real
//! `~/.claude.json` breaks the owner's login).

use std::path::Path;
use std::process::Command;

use ccteam_core::{ProjectEntry, LOCAL_HOST};
use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;

/// Write a fake `claude`-shaped executable that prints a version line
/// and exits 0, so the "claude binary" check PASSes without depending
/// on a real Claude Code install being present on the test host.
fn write_fake_claude_bin(dir: &Path) -> std::path::PathBuf {
    write_fake_vendor_bin(dir, "fake-claude.sh", "claude 9.9.9 (fake)")
}

fn write_fake_vendor_bin(dir: &Path, filename: &str, version: &str) -> std::path::PathBuf {
    let path = dir.join(filename);
    std::fs::write(&path, format!("#!/bin/sh\necho \"{version}\"\nexit 0\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

fn write_failing_vendor_bin(dir: &Path, filename: &str) -> std::path::PathBuf {
    let path = write_fake_vendor_bin(dir, filename, "broken");
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
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
    kimi_home: std::path::PathBuf,
    xdg_config_home: std::path::PathBuf,
    claude_bin: std::path::PathBuf,
    pi_bin: std::path::PathBuf,
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
    let kimi_home = tmp.path().join("kimi-home");
    let xdg_config_home = tmp.path().join("xdg-config");
    let claude_bin = write_fake_claude_bin(tmp.path());
    let pi_bin = write_fake_vendor_bin(tmp.path(), "fake-pi.sh", "0.83.0");
    Sandbox {
        _tmp: tmp,
        claude_config_home,
        claude_json,
        ccteam_home,
        fake_home,
        codex_home,
        kimi_home,
        xdg_config_home,
        claude_bin,
        pi_bin,
    }
}

fn run_bare_doctor(sb: &Sandbox) -> (String, i32) {
    let out = doctor_command(sb).output().expect("spawn ccteam doctor");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn doctor_command(sb: &Sandbox) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ccteam"));
    command
        .arg("doctor")
        .env("CCTEAM_CLAUDE_BIN", &sb.claude_bin)
        .env("CLAUDE_CONFIG_HOME", &sb.claude_config_home)
        .env("CCTEAM_HOME", &sb.ccteam_home)
        .env("HOME", &sb.fake_home)
        .env("CODEX_HOME", &sb.codex_home)
        .env("KIMI_CODE_HOME", &sb.kimi_home)
        .env("XDG_CONFIG_HOME", &sb.xdg_config_home)
        .env("CCTEAM_CODEX_BIN", sb._tmp.path().join("missing-codex"))
        .env("CCTEAM_GROK_BIN", sb._tmp.path().join("missing-grok"))
        .env(
            "CCTEAM_OPENCODE_BIN",
            sb._tmp.path().join("missing-opencode"),
        )
        .env("CCTEAM_KIMI_BIN", sb._tmp.path().join("missing-kimi"))
        .env("CCTEAM_PI_BIN", &sb.pi_bin)
        .env("NO_COLOR", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("MOONSHOT_API_KEY");
    command
}

fn agent_row<'a>(stdout: &'a str, vendor: &str) -> &'a str {
    let marker = format!("] {vendor}");
    stdout
        .lines()
        .find(|line| line.contains(&marker))
        .unwrap_or_else(|| panic!("missing {vendor} row in:\n{stdout}"))
}

#[test]
fn bare_doctor_renders_the_readiness_contract() {
    let sb = sandbox();
    let (stdout, _code) = run_bare_doctor(&sb);
    for expected in [
        "ccteam doctor — readiness checkup",
        "claude",
        "codex",
        "grok",
        "opencode",
        "kimi",
        "pi",
        "daemon",
        "version",
        "pricing",
        "home",
        "summary:",
    ] {
        assert!(
            stdout.contains(expected),
            "bare doctor output missing {expected:?}. stdout:\n{stdout}",
        );
    }
    let pi_notice = ccteam_core::host_registry::AgentProbeSpec::by_vendor("pi")
        .and_then(ccteam_core::host_registry::AgentProbeSpec::tool_surface_notice)
        .unwrap();
    assert!(
        stdout.contains(&pi_notice),
        "doctor must explain managed Pi versus plain shell Pi: {stdout}"
    );
    assert!(
        !stdout.contains("tmux"),
        "tmux is not a readiness dependency. stdout:\n{stdout}"
    );
}

#[test]
fn doctor_warns_for_legacy_project_skill_entity_without_failing() {
    let sb = sandbox();
    std::fs::write(
        &sb.claude_json,
        json!({"mcpServers": {"ccteam": {"command": "/usr/bin/true", "args": [], "env": {}}}})
            .to_string(),
    )
    .unwrap();
    let project = sb._tmp.path().join("legacy-project");
    std::fs::create_dir_all(project.join(".claude/skills/old-skill")).unwrap();
    ccteam_core::config::upsert_project(
        &sb.ccteam_home,
        ProjectEntry {
            slug: "legacy-project".to_string(),
            path: project,
            host: LOCAL_HOST.to_string(),
            remote_slug: None,
            remote_path: None,
            team: "dev".to_string(),
            installed_at: Utc::now(),
        },
    )
    .unwrap();

    let (stdout, code) = run_bare_doctor(&sb);
    assert!(
        stdout.contains("projects\n  [WARN] skills")
            && stdout.contains("legacy-project")
            && stdout.contains("ccteam skill migrate-project"),
        "doctor must emit one migration advisory line. stdout:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("[WARN] skills").count(),
        1,
        "doctor must aggregate the advisory into one line. stdout:\n{stdout}"
    );
    assert_eq!(code, 0, "project-skill advisory must never fail doctor");
}

#[test]
fn bare_doctor_warns_when_claude_mcp_is_not_registered() {
    // Fresh `.claude.json` is self-healed by daemon start, so it is advisory.
    let sb = sandbox();
    assert!(
        !sb.claude_json.exists(),
        "fixture: .claude.json should not exist yet",
    );
    let (stdout, code) = run_bare_doctor(&sb);
    assert!(
        stdout.contains("[WARN] claude"),
        "expected one WARN claude row. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("auto-registers at `ccteam daemon start` (or `ccteam config mcp`)"),
        "WARN row should explain daemon-start self-healing. stdout:\n{stdout}",
    );
    assert_eq!(code, 0, "MCP advisory must exit 0. stdout:\n{stdout}");
    assert!(
        stdout.contains("READY (WARN is informational, not blocking)"),
        "summary should stay READY. stdout:\n{stdout}",
    );
}

#[test]
fn codex_config_created_by_mcp_registration_does_not_impersonate_login() {
    let sb = sandbox();
    std::fs::create_dir_all(&sb.codex_home).unwrap();
    std::fs::write(sb.codex_home.join("config.toml"), "[mcp_servers]\n").unwrap();
    let codex_bin =
        write_fake_vendor_bin(sb._tmp.path(), "fake-codex.sh", "codex-cli 0.0.0 (fake)");

    let first = doctor_command(&sb)
        .env("CCTEAM_CODEX_BIN", &codex_bin)
        .output()
        .expect("doctor with config-only codex");
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first.status.success());
    assert!(agent_row(&first_stdout, "codex")
        .contains("auth missing — run `codex login` or set OPENAI_API_KEY"));

    std::fs::write(sb.codex_home.join("auth.json"), "{}").unwrap();
    let second = doctor_command(&sb)
        .env("CCTEAM_CODEX_BIN", &codex_bin)
        .output()
        .expect("doctor with authenticated codex");
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success());
    assert!(agent_row(&second_stdout, "codex").contains("auth ok"));
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

    let out = doctor_command(&sb)
        .env("CCTEAM_CLAUDE_BIN", sb._tmp.path().join("does-not-exist"))
        .output()
        .expect("spawn ccteam doctor");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("[FAIL] claude"),
        "expected a FAIL claude row. stdout:\n{stdout}",
    );
    assert!(stdout.contains("NOT READY (fix the FAIL lines above)"));
    assert_eq!(
        out.status.code().unwrap_or(-1),
        1,
        "missing claude binary must exit 1. stdout:\n{stdout}",
    );
}

#[test]
fn bare_doctor_fails_when_claude_version_probe_exits_nonzero() {
    let sb = sandbox();
    let claude_bin = write_failing_vendor_bin(sb._tmp.path(), "broken-claude.sh");
    let output = doctor_command(&sb)
        .env("CCTEAM_CLAUDE_BIN", claude_bin)
        .output()
        .expect("doctor with broken claude binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1));
    let row = agent_row(&stdout, "claude");
    assert!(row.contains("[FAIL]"));
    assert!(row.contains("--version` failed (exit 1)"));
}

#[test]
fn bare_doctor_exits_zero_when_claude_binary_and_mcp_are_both_ok() {
    let sb = sandbox();
    // Pre-register MCP and provide auth so the consolidated Claude row PASSes.
    // Readiness requires the CURRENT (HTTP + admin bearer) shape — a legacy
    // stdio `mcp-serve` entry reads as not-registered so it gets repaired.
    std::fs::write(
        &sb.claude_json,
        json!({"mcpServers": {"ccteam": {
            "type": "http",
            "url": "http://127.0.0.1:7331/mcp",
            "headers": {"Authorization": "Bearer ccteam:tok"},
        }}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(sb.claude_config_home.join("credentials.json"), "{}").unwrap();

    let (stdout, code) = run_bare_doctor(&sb);
    assert!(
        stdout.contains("[PASS] claude") && stdout.contains("auth ok · MCP registered"),
        "expected one consolidated PASS claude row. stdout:\n{stdout}",
    );
    assert_eq!(code, 0, "no critical check should FAIL. stdout:\n{stdout}");
    assert!(
        stdout.contains("READY"),
        "summary should say READY. stdout:\n{stdout}",
    );
}

#[test]
fn daemon_down_start_hint_is_the_last_non_empty_line() {
    let sb = sandbox();
    let (stdout, _) = run_bare_doctor(&sb);
    assert_eq!(
        stdout.lines().rfind(|line| !line.is_empty()),
        Some("daemon not running — start it:  ccteam daemon start")
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

    // Historical one-shot migration / repair flags were removed outright
    // (pre-v1.0 = no back-compat shims), so `--help` must not advertise
    // any of them.
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
