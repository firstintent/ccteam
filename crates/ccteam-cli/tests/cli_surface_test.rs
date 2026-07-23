//! CLI surface tests for the command-tree shape.
//!
//! Originally V0.4.6 F89 (the `internal` subcommand reorg); updated for
//! v0.8.6 W4a, which regrouped the top-level surface into the `project` /
//! `session` groups and deleted the six deprecated top-level aliases
//! outright (no back-compat shim — pre-v1.0).
//!
//! Tests:
//!
//! 1. `t01_help_user_facing_only` — `ccteam --help` lists the flat
//!    lifecycle commands plus the `project` / `session` groups; the
//!    `internal` umbrella is hidden, and the removed/re-homed verbs
//!    (`hook` / `spawn` / `attach` / `pause` / `new` / `web` / ...) do
//!    not appear at the top level.
//! 2. `t02_internal_help_lists_subcommands` — `ccteam internal --help`
//!    enumerates the internal subcommands (hook / mcp-serve / attach /
//!    peek / progress / resume / send / spawn) plus the W4a additions
//!    (mux / probe-project / web).
//! 3. `t03_deprecated_top_level_hook_alias_is_removed` — the six deleted
//!    top-level aliases (`hook` / `mcp-serve` / `spawn` / `send` /
//!    `peek` / `progress`) now fail clap parsing with a non-zero exit +
//!    "unrecognized subcommand" message.
//! 4. `t04_v03_legacy_commands_removed` — `ccteam phase show ...`,
//!    `ccteam decisions`, and `ccteam watchdog scan` all return
//!    non-zero with a clap "unrecognized subcommand" message; the V0.3
//!    legacy commands are wholly gone.

use std::process::{Command, Stdio};

/// `t01` — `ccteam --help` lists the flat lifecycle commands plus the
/// `project` / `session` groups. The `internal` group is hidden
/// (`#[command(hide = true)]`); the W4a-removed/re-homed verbs (`hook`,
/// `spawn`, `attach`, `pause`, `new`, `web`, …) are not top-level.
#[test]
fn t01_help_user_facing_only() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["--help"])
        .output()
        .expect("spawn ccteam --help");
    assert!(out.status.success(), "ccteam --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // v0.8.6 W4a — the top-level surface is the flat lifecycle commands
    // plus the `project` / `session` groups. `ls` / `show` / `new` now
    // live under `project`; `attach` / `pause` / `resume` (and the chat
    // bot register / persona / add-tool ops) under `session`; `web` and
    // `probe-project` under the hidden `internal` group.
    // v0.8.7 W3 added `role`; v0.9.9 adds the global `skill` library group.
    for required in [
        "init", "start", "stop", "status", "project", "session", "role", "skill", "host", "doctor",
        "config",
    ] {
        assert!(
            stdout.contains(required),
            "ccteam --help should advertise `{required}`; got: {stdout}",
        );
    }

    // The `internal` group is `#[command(hide = true)]`, so it must NOT
    // appear in the top-level command column.
    assert!(
        !stdout.contains("  internal "),
        "ccteam --help must not list the hidden `internal` group; got: {stdout}",
    );

    // v0.8.6 W4a deleted the six top-level aliases outright (no shim) and
    // re-homed the lifecycle verbs into the `project` / `session` groups.
    // None of these may appear as a top-level command anymore. The
    // matching deprecation-removal exit-code assertions are in t03.
    let no_longer_top_level = [
        "hook",
        "spawn",
        "send",
        "peek",
        "progress",
        "mcp-serve",
        "attach",
        "pause",
        "resume",
        "new",
        "web",
    ];
    for d in no_longer_top_level {
        // Word-boundary check: look for `  <name>  ` (the whitespace
        // clap uses in its command column) so we don't match e.g.
        // `progress` inside a description line.
        let pattern = format!("  {d} ");
        assert!(
            !stdout.contains(&pattern),
            "ccteam --help should not list `{d}` at top level (W4a removed/re-homed it); got: {stdout}",
        );
    }
}

/// `t02` — `ccteam internal --help` enumerates the surviving internal
/// subcommands (hook / mcp-serve / attach / peek / progress / mux / web /
/// experience). The de-legacy pass removed `resume` / `send` / `spawn` /
/// `probe-project` outright (pre-v1.0 = no back-compat shims).
#[test]
fn t02_internal_help_lists_subcommands() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["internal", "--help"])
        .output()
        .expect("spawn ccteam internal --help");
    assert!(
        out.status.success(),
        "ccteam internal --help should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    for required in [
        "hook",
        "mcp-serve",
        "attach",
        "peek",
        "progress",
        // v0.8.6 W4a folded `mux` / `web` into the hidden `internal` group.
        "mux",
        "web",
    ] {
        assert!(
            stdout.contains(required),
            "ccteam internal --help should advertise `{required}`; got: {stdout}",
        );
    }

    // Removed internal subcommands must NOT appear anymore.
    for gone in ["resume", "send", "spawn", "probe-project"] {
        let pattern = format!("  {gone} ");
        assert!(
            !stdout.contains(&pattern),
            "ccteam internal --help should not list `{gone}` (de-legacy pass removed it); \
             got: {stdout}",
        );
    }
}

/// `t03` — v0.8.6 W4a deleted the deprecated top-level `ccteam hook`
/// alias outright (pre-v1.0 = no back-compat shims). It now resolves to
/// nothing: clap rejects it with a non-zero exit and an "unrecognized
/// subcommand" error. The live handler moved to `ccteam internal hook`
/// (exercised elsewhere); this test only pins that the top-level alias is
/// gone.
#[test]
fn t03_deprecated_top_level_hook_alias_is_removed() {
    let bin = env!("CARGO_BIN_EXE_ccteam");

    // The six top-level aliases W4a removed. Each must now fail clap
    // parsing rather than dispatch (or print a deprecation WARN).
    for args in [
        vec!["hook", "progress-append", "test_event"],
        vec!["mcp-serve"],
        vec!["spawn", "some-slug", "some-role"],
        vec!["send", "some-slug", "body"],
        vec!["peek", "some-slug"],
        vec!["progress", "some-slug"],
    ] {
        let out = Command::new(bin)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("spawn ccteam {args:?}: {e}"));
        assert!(
            !out.status.success(),
            "ccteam {args:?} must fail now that the top-level alias is removed; got exit {:?}",
            out.status.code(),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let subcmd = args[0];
        let recognized_error = stderr.contains("unrecognized subcommand")
            || stderr.contains("invalid subcommand")
            || stderr.contains("unexpected argument")
            || stderr.contains(&format!("'{subcmd}'"));
        assert!(
            recognized_error,
            "ccteam {args:?} should print a clap unrecognized-subcommand error; \
             got stderr: {stderr}",
        );
    }
}

/// `t04` — `ccteam phase`, `ccteam decisions`, `ccteam watchdog` are
/// removed. clap rejects them with a non-zero exit and an
/// "unrecognized subcommand" / "invalid value" error message.
#[test]
fn t04_v03_legacy_commands_removed() {
    let bin = env!("CARGO_BIN_EXE_ccteam");

    for legacy_args in [
        vec!["phase", "show", "dev", "implement"],
        vec!["decisions"],
        vec!["watchdog", "scan"],
    ] {
        let out = Command::new(bin)
            .args(&legacy_args)
            .output()
            .unwrap_or_else(|e| panic!("spawn ccteam {legacy_args:?}: {e}"));
        assert!(
            !out.status.success(),
            "ccteam {legacy_args:?} should fail; got exit {:?}",
            out.status.code(),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        // clap's default unknown-subcommand message contains "unrecognized
        // subcommand" or "invalid subcommand" depending on version; we
        // accept either + the literal subcommand name.
        let subcmd = legacy_args[0];
        let recognized_error = stderr.contains("unrecognized subcommand")
            || stderr.contains("invalid subcommand")
            || stderr.contains("unexpected argument")
            || stderr.contains(&format!("'{subcmd}'"));
        assert!(
            recognized_error,
            "ccteam {legacy_args:?} should print a clap unknown-subcommand error; \
             got stderr: {stderr}",
        );
    }
}
