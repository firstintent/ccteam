//! V0.6.1 Wave 1 F121 — `ccteam doctor --check-pricing-version` per-vendor
//! staleness classification. Drives the binary surface and pins the three
//! states (`ok` / `warn` @ 200 d / `error` @ 400 d) by stamping
//! `CCTEAM_TEST_NOW=YYYY-MM-DD` so "today" is deterministic regardless
//! of when CI runs (or how aged the embedded TOML tables drift).
//!
//! Per-vendor coverage (both tables bumped to 2026-06-24 — Opus 4.8 +
//! gpt-5.x rows):
//! - `pricing.anthropic` `schema_version` = 2026-06-24 (see
//!   `crates/ccteam-cost/pricing/anthropic.toml`)
//! - `pricing.openai`    `schema_version` = 2026-06-24 (see
//!   `crates/ccteam-cost/pricing/openai.toml`)
//!
//! The `CCTEAM_TEST_NOW` override is consumed by
//! `commands::doctor_today` (F121). Without it the report would compare
//! against the live UTC date — useful for the host probe, useless for
//! deterministic CI.

use std::process::Command;

/// Run `ccteam doctor --check-pricing-version` with `CCTEAM_TEST_NOW`
/// pinned. Returns `(stdout, stderr, exit_code)`.
fn run_doctor_with_now(now_ymd: &str) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .env("CCTEAM_TEST_NOW", now_ymd)
        .arg("doctor")
        .arg("--check-pricing-version")
        .output()
        .expect("spawn ccteam doctor");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// Both vendor lines must appear in every run — F121 acceptance gate #1
/// ("输出 2 行 anthropic + openai").
fn assert_both_vendor_lines(stdout: &str) {
    assert!(
        stdout.contains("[pricing.anthropic]"),
        "missing anthropic line. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("[pricing.openai]"),
        "missing openai line. stdout:\n{stdout}",
    );
}

#[test]
fn check_pricing_version_ok_when_both_tables_fresh() {
    // Pin "today" to the tables' authored date — both vendors sit at
    // age 0, within the 180-day window.
    let (stdout, _stderr, code) = run_doctor_with_now("2026-06-24");
    assert_eq!(code, 0, "doctor must exit 0. stdout:\n{stdout}");
    assert_both_vendor_lines(&stdout);
    // Both lines should land in the OK bucket — the WARN / ERROR
    // labels must be absent.
    assert!(
        stdout.contains("OK)"),
        "expected OK classifier. stdout:\n{stdout}",
    );
    assert!(
        !stdout.contains("warn pricing aging"),
        "must not warn when both fresh. stdout:\n{stdout}",
    );
    assert!(
        !stdout.contains("ERROR ship needs re-pull"),
        "must not error when both fresh. stdout:\n{stdout}",
    );
    // Summary line for the all-fresh case.
    assert!(
        stdout.contains("pricing tables fresh"),
        "expected all-fresh summary. stdout:\n{stdout}",
    );
}

#[test]
fn check_pricing_version_warns_when_table_is_200_days_old() {
    // Both tables schema_version = 2026-06-24 → 200 days later is
    // 2027-01-10. Both vendors land in (180, 365] → warn (not error).
    let (stdout, _stderr, code) = run_doctor_with_now("2027-01-10");
    assert_eq!(code, 0);
    assert_both_vendor_lines(&stdout);
    assert!(
        stdout.contains("warn pricing aging"),
        "expected warn classifier @ 200d. stdout:\n{stdout}",
    );
    assert!(
        !stdout.contains("ERROR ship needs re-pull"),
        "must not escalate to ERROR @ 200d. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("[WARN] one or more pricing tables older than"),
        "expected WARN summary line. stdout:\n{stdout}",
    );
}

#[test]
fn check_pricing_version_errors_when_table_is_400_days_old() {
    // Both tables schema_version = 2026-06-24 → 400 days later is
    // 2027-07-29. Both vendors land in (365, ∞) → error.
    let (stdout, _stderr, code) = run_doctor_with_now("2027-07-29");
    assert_eq!(code, 0);
    assert_both_vendor_lines(&stdout);
    assert!(
        stdout.contains("ERROR ship needs re-pull"),
        "expected ERROR classifier @ 400d. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("[ERROR] one or more pricing tables older than"),
        "expected ERROR summary line. stdout:\n{stdout}",
    );
}

#[test]
fn doctor_without_flag_runs_full_readiness_checkup_including_pricing() {
    // v0.8.22 — F121 acceptance gate #3 originally required the bare
    // `ccteam doctor` (no mode flag) to surface pricing staleness
    // without the operator remembering the explicit flag. Bare doctor
    // now runs a *full* readiness checkup (claude/codex/tmux/MCP/
    // daemon/pricing/home-layout — one line per check); this asserts
    // the pricing checklist line is still one of them. The detailed
    // per-vendor `[pricing.anthropic]` / `[pricing.openai]` breakdown
    // (and the `CCTEAM_TEST_NOW` staleness-threshold coverage above)
    // remains exercised via the explicit, still-functional (if now
    // hidden from `--help`) `--check-pricing-version` flag.
    //
    // Full env sandbox (isolated CCTEAM_HOME / HOME / CLAUDE_CONFIG_HOME)
    // so this never reads the developer's real `~/.claude.json` /
    // `~/.ccteam` — the bare checkup also probes MCP registration and
    // home-layout drift now, not just pricing.
    let tmp = tempfile::TempDir::new().unwrap();
    let claude_config_home = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_config_home).unwrap();
    let fake_home = tmp.path().join("fake-home");
    std::fs::create_dir_all(&fake_home).unwrap();

    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .env("CCTEAM_TEST_NOW", "2026-06-24")
        .env("CLAUDE_CONFIG_HOME", &claude_config_home)
        .env("CCTEAM_HOME", tmp.path().join("ccteam-home"))
        .env("HOME", &fake_home)
        .env("CODEX_HOME", tmp.path().join("codex-home"))
        .arg("doctor")
        .output()
        .expect("spawn ccteam doctor");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("ccteam doctor: readiness checkup"),
        "bare doctor should run the full readiness checkup. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("pricing tables"),
        "pricing checklist line missing from bare doctor. stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("summary:"),
        "bare doctor should end with a summary line. stdout:\n{stdout}",
    );
}
