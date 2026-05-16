//! V0.4.6 F89 — CLI surface tests for the `internal` subcommand
//! reorganization.
//!
//! Tests (from `docs/v0-4-6/dev-plan.md` §10):
//!
//! 1. `t01_help_user_facing_only` — `ccteam --help` lists only the
//!    user-facing surface plus the `internal` umbrella, hiding the
//!    deprecated top-level aliases (`hook` / `spawn` / `peek` / ...).
//! 2. `t02_internal_help_lists_subcommands` — `ccteam internal --help`
//!    enumerates all eight internal subcommands.
//! 3. `t03_legacy_top_level_still_works_with_warn` — `ccteam hook
//!    progress-append <ev>` still resolves (V0.4.6 one-release shim)
//!    but emits a stderr WARN flagging the deprecated path.
//! 4. `t04_v03_legacy_commands_removed` — `ccteam phase show ...`,
//!    `ccteam decisions`, and `ccteam watchdog scan` all return
//!    non-zero with a clap "unrecognized subcommand" message; the V0.3
//!    legacy commands are wholly gone.

use std::io::Write;
use std::process::{Command, Stdio};

/// `t01` — `ccteam --help` lists the user-facing surface and the
/// `internal` umbrella; deprecated top-level aliases (`hook`, `spawn`,
/// `peek`, etc.) are hidden via `#[command(hide = true)]`.
#[test]
fn t01_help_user_facing_only() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["--help"])
        .output()
        .expect("spawn ccteam --help");
    assert!(out.status.success(), "ccteam --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // User-facing commands must show up.
    for required in [
        "init", "start", "stop", "new", "ls", "status", "show", "doctor", "web", "internal",
    ] {
        assert!(
            stdout.contains(required),
            "ccteam --help should advertise `{required}`; got: {stdout}",
        );
    }

    // Deprecated top-level aliases are hidden — they still work but
    // are no longer listed in the top-level help to keep the user
    // surface clean (V0.4.6 F89).
    let deprecated = [
        "hook",
        "spawn",
        "send",
        "peek",
        "attach",
        "progress",
        "resume",
        "mcp-serve",
    ];
    for d in deprecated {
        // Use word-boundary check: look for `  <name>  ` (with the
        // surrounding whitespace clap uses in its commands column) so
        // we don't match e.g. `internal` substrings.
        let pattern = format!("  {d} ");
        assert!(
            !stdout.contains(&pattern),
            "ccteam --help should not list deprecated `{d}` at top level; got: {stdout}",
        );
    }
}

/// `t02` — `ccteam internal --help` enumerates the eight subcommands
/// that previously lived at the top level.
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
        "resume",
        "send",
        "spawn",
    ] {
        assert!(
            stdout.contains(required),
            "ccteam internal --help should advertise `{required}`; got: {stdout}",
        );
    }
}

/// `t03` — `ccteam hook progress-append <ev>` still works for one
/// release (V0.4.6 back-compat) but emits a stderr WARN naming the
/// new `ccteam internal hook` path so settings.json upgrades stop
/// silently looking healthy.
#[test]
fn t03_legacy_top_level_still_works_with_warn() {
    let bin = env!("CARGO_BIN_EXE_ccteam");

    // V0.4.6 F89: spawn with a stdin pipe so the `hook progress-append`
    // handler can finish without blocking on a missing stdin. We feed
    // it a minimal valid JSON payload (the handler reads stdin via
    // `parse_hook_stdin_json`); on success the handler writes a line
    // into the per-project progress log and returns Ok(()). The exact
    // stdout / stderr ordering of the WARN line is the contract we're
    // asserting.
    let mut child = Command::new(bin)
        .args(["hook", "progress-append", "test_event"])
        .env("CCTEAM_HOME", "/tmp/ccteam-f89-shim-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ccteam hook progress-append");

    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin
            .write_all(br#"{"slug": "ghost-slug-no-project"}"#)
            .expect("write hook stdin");
    }

    let out = child
        .wait_with_output()
        .expect("await ccteam hook progress-append");
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The deprecation WARN MUST appear regardless of whether the hook
    // body itself succeeded (an unknown slug is fine — the handler
    // bails on the missing project, but the WARN comes first).
    assert!(
        stderr.contains("ccteam hook")
            && stderr.contains("deprecated")
            && stderr.contains("ccteam internal hook"),
        "expected deprecation WARN mentioning `ccteam internal hook`; \
         got stderr: {stderr}",
    );
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
