//! V0.5.0 F93a + F100 — `ccteam doctor --install-skill [NAME]` test harness.
//!
//! V0.4.6 (`--install-skill` flag was bool) installed three skills:
//! `ccteam-control`, `ccteam-team-author`, `ccteam-project-creator`.
//! V0.5.0 F93a added `ccteam-team` as a fourth shipped skill; F100 then
//! merged `ccteam-team-author` + `ccteam-project-creator` into
//! `ccteam-creator`, landing the V0.5.0 final set at **three** entries:
//!   `ccteam-control` / `ccteam-creator` / `ccteam-team`
//! The flag stayed `Option<String>` so users can install one at a time
//! (`--install-skill ccteam-team`) or all (`--install-skill all` /
//! plain `--install-skill`).
//!
//! All tests are env-mutating (`CLAUDE_CONFIG_HOME`, `HOME`,
//! `CCTEAM_HOME`) and live in `tests/` so they run in independent
//! processes (per CLAUDE.md §六).

use std::process::Command;

use tempfile::TempDir;

/// Helper: run the ccteam binary with the env redirected into a tempdir.
/// Mirrors `install_mcp_claude_config_home_test.rs::install_mcp_writes_to_claude_config_home_when_set`.
fn run_ccteam(args: &[&str], tmp: &TempDir) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let claude_dir = tmp.path().join("isolated").join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let fake_home = tmp.path().join("fake-home");
    std::fs::create_dir_all(&fake_home).unwrap();
    Command::new(bin)
        .args(args)
        .env("CLAUDE_CONFIG_HOME", &claude_dir)
        .env("HOME", &fake_home)
        .env("CCTEAM_HOME", tmp.path().join("ccteam-home"))
        .output()
        .expect("spawn ccteam binary")
}

#[test]
fn install_skill_default_lays_down_all_three_shipped_skills() {
    // V0.5.0 F100: `--install-skill` with no value (or `all`) writes
    // every shipped skill into `~/.claude/skills/<name>/SKILL.md`. The
    // V0.5.0 set is 3 entries:
    //   ccteam-control / ccteam-creator / ccteam-team
    //
    // Regression guard: if a future patch drops one from
    // `render_install_skill_report`, this test fires.
    let tmp = TempDir::new().unwrap();
    let out = run_ccteam(&["doctor", "--install-skill"], &tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "doctor --install-skill should succeed; stdout={stdout}; stderr={stderr}",
    );
    let claude = tmp.path().join("isolated").join(".claude");
    for skill in ["ccteam-control", "ccteam-creator", "ccteam-team"] {
        let target = claude.join("skills").join(skill).join("SKILL.md");
        assert!(
            target.is_file(),
            "expected shipped skill at {} (skill={skill}); stdout={stdout}",
            target.display(),
        );
    }
}

#[test]
fn install_skill_all_explicit_value_matches_default() {
    // `--install-skill all` is the explicit form of the bare flag.
    // Same 3-skill default set must land.
    let tmp = TempDir::new().unwrap();
    let out = run_ccteam(&["doctor", "--install-skill", "all"], &tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "doctor failed: {stdout}");
    let claude = tmp.path().join("isolated").join(".claude");
    for skill in ["ccteam-control", "ccteam-creator", "ccteam-team"] {
        assert!(claude.join("skills").join(skill).join("SKILL.md").is_file());
    }
}

#[test]
fn install_skill_single_name_writes_just_that_skill() {
    // V0.5.0 F93a: `--install-skill ccteam-team` is single-skill mode —
    // only the named skill lands; the other shipped skills are NOT
    // written.
    let tmp = TempDir::new().unwrap();
    let out = run_ccteam(&["doctor", "--install-skill", "ccteam-team"], &tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "doctor --install-skill ccteam-team should succeed; stdout={stdout}; stderr={stderr}",
    );
    let claude = tmp.path().join("isolated").join(".claude");
    assert!(
        claude
            .join("skills")
            .join("ccteam-team")
            .join("SKILL.md")
            .is_file(),
        "ccteam-team SKILL.md should have been written; stdout={stdout}",
    );
    // Other shipped skills should NOT be present in single-skill mode.
    for other in ["ccteam-control", "ccteam-creator"] {
        assert!(
            !claude.join("skills").join(other).join("SKILL.md").exists(),
            "expected single-skill mode to skip {other} but it landed; stdout={stdout}",
        );
    }
}

#[test]
fn install_skill_is_idempotent_when_rerun() {
    // V0.5.0 F93a verifies the underlying `install_skill_body_into` is
    // re-entrant for the V0.5.0 ccteam-team entry (file already
    // present → no-op without `--force`). Mirrors the in-lib idempotent
    // test (`ccteam_team_skill_install_is_idempotent`) but goes
    // through the full CLI path so we catch e.g. a dispatch regression.
    let tmp = TempDir::new().unwrap();
    let first = run_ccteam(&["doctor", "--install-skill"], &tmp);
    assert!(first.status.success());
    let claude = tmp.path().join("isolated").join(".claude");
    let team_skill = claude.join("skills").join("ccteam-team").join("SKILL.md");
    let mtime_first = std::fs::metadata(&team_skill).unwrap().modified().unwrap();
    // Sleep a hair so a mtime delta would be observable if a write
    // happened. 30ms is plenty for ext4 / btrfs / apfs nanosecond
    // resolution.
    std::thread::sleep(std::time::Duration::from_millis(30));
    let second = run_ccteam(&["doctor", "--install-skill"], &tmp);
    let stdout2 = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success());
    // Idempotent: report must mention "already-present" (label
    // rendered by `skill_install_label`) for at least one shipped
    // skill on the second run.
    assert!(
        stdout2.contains("already-present"),
        "expected idempotent run to report 'already-present' for at least one skill; got: {stdout2}",
    );
    let mtime_second = std::fs::metadata(&team_skill).unwrap().modified().unwrap();
    assert_eq!(
        mtime_first, mtime_second,
        "ccteam-team SKILL.md mtime changed on idempotent re-run",
    );
}

#[test]
fn install_skill_unknown_name_errors_with_friendly_message() {
    // V0.5.0 F100: a bogus skill name (e.g. typo) should bail with a
    // helpful message listing the three canonical names. Don't bury the
    // user in a stack trace.
    let tmp = TempDir::new().unwrap();
    let out = run_ccteam(&["doctor", "--install-skill", "ccteam-bogus"], &tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "unknown skill name should fail; stdout={stdout}; stderr={stderr}",
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("ccteam-bogus") && combined.contains("unknown skill name"),
        "error message should name the bad arg + explain; got: stdout={stdout}; stderr={stderr}",
    );
    // Verify nothing got written despite the failure.
    let claude = tmp.path().join("isolated").join(".claude");
    assert!(
        !claude.join("skills").exists()
            || std::fs::read_dir(claude.join("skills"))
                .map(|d| d.count())
                .unwrap_or(0)
                == 0,
        "no skills should land when the selector is invalid",
    );
}

#[test]
fn install_skill_default_scope_is_user_level_claude_dir() {
    // V0.5.0 F93a: skills install under `~/.claude/skills/`
    // (user-scope), NOT into any project / cwd. Verify the harness's
    // `CLAUDE_CONFIG_HOME` redirect lands the files at
    // `<redirect>/skills/<name>/SKILL.md`, never at `$HOME/.claude/`.
    let tmp = TempDir::new().unwrap();
    let out = run_ccteam(&["doctor", "--install-skill", "ccteam-team"], &tmp);
    assert!(out.status.success());
    let claude = tmp.path().join("isolated").join(".claude");
    assert!(
        claude
            .join("skills")
            .join("ccteam-team")
            .join("SKILL.md")
            .is_file(),
        "ccteam-team SKILL.md should land under the redirect target",
    );
    // The fake $HOME should be untouched (no .claude/ created there).
    let fake_home = tmp.path().join("fake-home");
    assert!(
        !fake_home.join(".claude").join("skills").exists(),
        "user-scope install must not leak into fallback $HOME/.claude/",
    );
}
