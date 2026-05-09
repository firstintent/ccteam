//! F26 — `install_mcp()` honors `CLAUDE_CONFIG_HOME` so e2e harnesses
//! redirect the `.claude.json` mcpServers write away from the
//! developer's real `~/.claude.json`.
//!
//! Mirrors the redirection sibling installers
//! (`--install-skill`, `--install-memory-bridge`) get through
//! `user_claude_dir()`. Pre-V0.2.1 `install_mcp()` went straight
//! through `dirs::home_dir()` and ignored the env, which forced e2e
//! suites to override `HOME` instead.

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn install_mcp_writes_to_claude_config_home_when_set() {
    let tmp = TempDir::new().unwrap();
    // CLAUDE_CONFIG_HOME points at a `.claude/` dir. The `.claude.json`
    // file is its sibling — same convention as the trust-entry writer
    // (`projects::resolve_claude_json_path`).
    let claude_dir = tmp.path().join("isolated").join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let expected_json = tmp.path().join("isolated").join(".claude.json");
    assert!(
        !expected_json.exists(),
        "fixture: .claude.json should not exist yet",
    );

    // Sandbox HOME so even if the env-resolution logic regresses we
    // don't pollute the developer's real home dir.
    let fake_home = tmp.path().join("fake-home");
    std::fs::create_dir_all(&fake_home).unwrap();

    let bin = env!("CARGO_BIN_EXE_cct");
    let out = Command::new(bin)
        .args(["doctor", "--install-mcp"])
        .env("CLAUDE_CONFIG_HOME", &claude_dir)
        .env("HOME", &fake_home)
        .env("CCTEAM_HOME", tmp.path().join("ccteam-home"))
        .output()
        .expect("spawn ccteam doctor --install-mcp");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "doctor --install-mcp should succeed; stdout={stdout}; stderr={stderr}",
    );

    assert!(
        expected_json.exists(),
        "expected install_mcp to write {} (CLAUDE_CONFIG_HOME redirect); stdout={stdout}",
        expected_json.display(),
    );
    // Sanity: the real fake_home/.claude.json should NOT be touched.
    assert!(
        !fake_home.join(".claude.json").exists(),
        "install_mcp wrote to fallback HOME path despite CLAUDE_CONFIG_HOME being set",
    );

    // mcpServers.ccteam landed.
    let body = std::fs::read_to_string(&expected_json).unwrap();
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert!(
        parsed["mcpServers"]["ccteam"].is_object(),
        "expected mcpServers.ccteam in {}; got: {body}",
        expected_json.display(),
    );
}
