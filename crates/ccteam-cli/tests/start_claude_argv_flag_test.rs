//! F29 — `ccteam start --claude-argv "<line>"` parses and the help
//! text advertises the flag. The flag plumbs through to
//! `OrchestratorConfig.claude_argv` (verified at the lib level in
//! `crates/ccteam-core/tests/orchestrator_claude_argv_env_test.rs`);
//! this test only confirms the CLI surface so a typo doesn't silently
//! drop the flag.

use std::process::Command;

#[test]
fn ccteam_start_help_advertises_claude_argv_flag() {
    let bin = env!("CARGO_BIN_EXE_cct");
    let out = Command::new(bin)
        .args(["start", "--help"])
        .output()
        .expect("spawn ccteam start --help");
    assert!(out.status.success(), "ccteam start --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--claude-argv"),
        "help text should mention --claude-argv; got: {stdout}",
    );
}
