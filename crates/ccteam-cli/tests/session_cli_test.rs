//! V0.3.1 F47 — `ccteam session` CLI surface.
//!
//! F47 PR ships the parser shape only; the master state.json::sessions
//! runtime path is F49 (V0.3.1 PR #4). This integration test pins the
//! shape three different consumers depend on:
//!
//! 1. `ccteam session --help` lists every subcommand (`add` / `ls` /
//!    `attach` / `rm`) — a typo in the parser quietly hides one without
//!    this canary.
//! 2. `ccteam session add <slug> --harness=codex` returns exit 1 with
//!    a stderr message citing V0.3.2 + the codex-integration research
//!    doc. This is the F47 verification target — exercising the
//!    [`ccteam_core::CodexAdapter::spawn_session`] stub error path
//!    end-to-end.
//! 3. The other stubs (`add --harness=claude` / `ls` / `attach` / `rm`)
//!    return exit 1 with a "see F49" pointer so anyone scripting
//!    against the final shape today gets immediate feedback that the
//!    runtime path is still pending.
//!
//! Lives in `tests/` rather than the lib-internal mod so it spawns
//! the real `ccteam` binary via `env!("CARGO_BIN_EXE_ccteam")` — the
//! shape only matters end-to-end.

use std::process::Command;

fn cct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

#[test]
fn session_help_advertises_every_subcommand() {
    let out = Command::new(cct_bin())
        .args(["session", "--help"])
        .output()
        .expect("spawn ccteam session --help");
    assert!(
        out.status.success(),
        "ccteam session --help should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for sub in ["add", "ls", "attach", "rm"] {
        assert!(
            stdout.contains(sub),
            "ccteam session --help should advertise subcommand `{sub}`; got:\n{stdout}",
        );
    }
}

#[test]
fn session_add_help_advertises_harness_flag() {
    let out = Command::new(cct_bin())
        .args(["session", "add", "--help"])
        .output()
        .expect("spawn ccteam session add --help");
    assert!(
        out.status.success(),
        "ccteam session add --help should exit 0"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--harness"),
        "ccteam session add --help should advertise --harness; got:\n{stdout}",
    );
    // ValueEnum derive surfaces the variant names in --help output.
    assert!(
        stdout.contains("claude") && stdout.contains("codex"),
        "--harness should list both `claude` and `codex` values; got:\n{stdout}",
    );
}

#[test]
fn session_add_codex_exits_with_v0_3_2_pointer() {
    // F47 verification target — exercises CodexAdapter::spawn_session's
    // NotImplemented error path through the full CLI stack.
    let out = Command::new(cct_bin())
        .args(["session", "add", "some-slug", "--harness=codex"])
        .output()
        .expect("spawn ccteam session add --harness=codex");
    assert!(
        !out.status.success(),
        "ccteam session add --harness=codex must exit non-zero (V0.3.1 stub); \
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "ccteam session add --harness=codex should exit code 1",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("V0.3.2"),
        "stderr should cite V0.3.2 deferral; got:\n{stderr}",
    );
    assert!(
        stderr.contains("docs/research/ccteam-codex-integration.md"),
        "stderr should cite codex-integration research doc; got:\n{stderr}",
    );
}

#[test]
fn session_add_claude_defers_to_f49() {
    // F47 doesn't ship the master state.json::sessions[] schema — the
    // claude branch must hard-fail with an F49 pointer rather than
    // creating an orphan tmux session via standalone spawn_session.
    let out = Command::new(cct_bin())
        .args(["session", "add", "some-slug", "--harness=claude"])
        .output()
        .expect("spawn ccteam session add --harness=claude");
    assert!(
        !out.status.success(),
        "F47 must defer claude branch to F49; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("F49"),
        "stderr should cite F49 (V0.3.1 PR #4); got:\n{stderr}",
    );
}

#[test]
fn session_ls_attach_rm_defer_to_f49() {
    for args in [
        vec!["session", "ls", "some-slug"],
        vec!["session", "attach", "some-slug", "claude-1"],
        vec!["session", "rm", "some-slug", "claude-1"],
    ] {
        let out = Command::new(cct_bin())
            .args(&args)
            .output()
            .unwrap_or_else(|_| panic!("spawn ccteam {args:?}"));
        assert!(
            !out.status.success(),
            "ccteam {args:?} must exit non-zero in F47; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("F49"),
            "ccteam {args:?} stderr should cite F49; got:\n{stderr}",
        );
    }
}
