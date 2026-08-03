//! F26 — `install_mcp()` honors `CLAUDE_CONFIG_HOME` so e2e harnesses
//! redirect the `.claude.json` mcpServers write away from the
//! developer's real `~/.claude.json`.
//!
//! v0.8.6 Item 4: the MCP-install CLI entrypoint moved from
//! `doctor --install-mcp` to the `config` setup hub. The headless
//! `ccteam config mcp` escape hatch drives the same `install_mcp()`
//! writer, so this redirect coverage now runs through `config mcp`.
//! Pre-V0.2.1 `install_mcp()` went straight through `dirs::home_dir()`
//! and ignored the env, which forced e2e suites to override `HOME`
//! instead.

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

    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["config", "mcp"])
        .env("CLAUDE_CONFIG_HOME", &claude_dir)
        .env("HOME", &fake_home)
        .env("CCTEAM_HOME", tmp.path().join("ccteam-home"))
        .output()
        .expect("spawn ccteam config mcp");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "config mcp should succeed; stdout={stdout}; stderr={stderr}",
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
    assert!(
        !fake_home.join(".pi").exists(),
        "config mcp must never invent a Pi config footprint"
    );
    assert!(
        stdout.contains(ccteam_core::host_registry::PI_MANAGED_BRIDGE_NOTICE),
        "config output must explain managed Pi versus plain shell Pi: {stdout}"
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
