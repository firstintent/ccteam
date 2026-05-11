//! V0.3.1 — team factory staging trees must be directly usable by
//! `ccteam new --team <name>`.

use std::process::Command;

fn cct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

fn isolated_cmd(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::new(cct_bin());
    cmd.env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("xdg"))
        .env("CCTEAM_HOME", tmp.path().join("ccteam-home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .env("CLAUDE_CONFIG_HOME", tmp.path().join("claude"))
        .env("CCTEAM_AUTO_SLUG", "off");
    cmd
}

#[test]
fn cct_new_accepts_team_init_staging_team() {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    let init = isolated_cmd(&tmp)
        .args([
            "team",
            "init",
            "manual-flex",
            "--kind",
            "flex",
            "--description",
            "manual flex verification",
            "--author-name",
            "tester",
        ])
        .output()
        .expect("spawn ccteam team init");
    assert!(
        init.status.success(),
        "team init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    let new = isolated_cmd(&tmp)
        .args([
            "new",
            "Manual V0.3.1 verification",
            "--team",
            "manual-flex",
            "--slug",
            "manual-flex-check",
            "--no-auto-slug",
        ])
        .output()
        .expect("spawn ccteam new");
    assert!(
        new.status.success(),
        "ccteam new should resolve team init staging tree\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&new.stdout),
        String::from_utf8_lossy(&new.stderr)
    );

    let state_path = tmp
        .path()
        .join("projects")
        .join("manual-flex-check")
        .join(".ccteam")
        .join("state.json");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).expect("read state.json"))
            .expect("parse state.json");
    assert_eq!(state["team"], "manual-flex");
    assert_eq!(state["team_kind"], "flex");
}
