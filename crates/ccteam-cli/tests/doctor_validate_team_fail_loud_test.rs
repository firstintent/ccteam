//! F30 — `ccteam doctor --validate-team <team>` must be fail-loud:
//! any `[FAIL]` line (phase-section or plugin-manifest section) bubbles
//! a non-zero process exit. Run as integration tests so the
//! `XDG_CONFIG_HOME` env mutation runs in its own process and doesn't
//! race against unit tests in the same binary (CLAUDE.md §六 rule).

use std::process::Command;

use tempfile::TempDir;

/// Run `ccteam doctor --validate-team <team>` with `CCTEAM_HOME` and
/// `XDG_CONFIG_HOME` pointed at the tempdir so the test cannot pollute
/// the developer's real `~/.ccteam/` or `~/.config/ccteam/`.
fn run_doctor_validate(
    home: &std::path::Path,
    xdg: &std::path::Path,
    team: &str,
) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    Command::new(bin)
        .arg("doctor")
        .arg("--validate-team")
        .arg(team)
        .env("CCTEAM_HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        // Override HOME too so the staging-dir fallback (resolver's
        // last branch) cannot escape the tempdir.
        .env("HOME", xdg)
        .output()
        .expect("spawn ccteam doctor")
}

#[test]
fn validate_team_unknown_team_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("ccteam-home");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();

    let out = run_doctor_validate(&home, &xdg, "totally-unknown");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout={stdout}; stderr={stderr}",
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("[FAIL] team.yaml load"),
        "expected fail line; got stdout={stdout}; stderr={stderr}",
    );
}

#[test]
fn validate_team_corrupt_plugin_manifest_exits_nonzero() {
    // Stage a plugin tree with a deliberately-broken plugin.json
    // (missing required `author` field) at the user staging dir for a
    // synthetic team. Without resetting shipped teams the team.yaml
    // resolution will fail, but the plugin-section [FAIL] runs first
    // and is what we want to count — the assertion confirms the
    // plugin-section finding is in the report and the exit is non-zero.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("ccteam-home");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();

    // staging_dir_for default = `$XDG_CONFIG_HOME/ccteam/teams/<name>/`
    let staging = xdg.join("ccteam").join("teams").join("smoketeam");
    std::fs::create_dir_all(staging.join(".claude-plugin")).unwrap();
    std::fs::write(
        staging.join(".claude-plugin/plugin.json"),
        // Deliberately missing `author` → PluginManifest::validate fails.
        r#"{"name":"smoketeam","description":"corrupt fixture"}"#,
    )
    .unwrap();
    std::fs::write(staging.join("team.yaml"), "name: smoketeam\n").unwrap();
    std::fs::create_dir_all(staging.join("phases")).unwrap();

    let out = run_doctor_validate(&home, &xdg, "smoketeam");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout={stdout}; stderr={stderr}",
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("[FAIL] plugin.json"),
        "expected plugin-section fail in report; got stdout={stdout}; stderr={stderr}",
    );
}
