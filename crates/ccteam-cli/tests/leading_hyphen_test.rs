//! F87 — `ccteam send` / `ccteam spawn` accept leading-hyphen message
//! bodies as literal text. Without `#[arg(allow_hyphen_values = true)]`
//! clap parses `--help` (and friends) as ccteam's own flag and exits
//! before the orchestrator ever sees the string. The two tests below
//! only confirm clap's parse layer; the daemon side-effects are not
//! exercised here (the daemon isn't running in `cargo test`).
//!
//! Failure surface we care about: if a future refactor drops the
//! `allow_hyphen_values` attribute, `ccteam send <slug> "--help"`
//! silently prints ccteam's top-level help and exits 0, which is
//! indistinguishable from "message sent" from the user's POV. Both
//! tests assert on stderr / exit code shape that only the
//! orchestrator-rejection path produces.
//!
//! - `t01_send_accepts_leading_hyphen`:
//!   `ccteam send some-slug "--help"` must reach the daemon-connect
//!   phase (which then fails because no daemon is running). We assert
//!   `stdout` does NOT contain ccteam's top-level help banner.
//! - `t02_send_dash_dash_separator_still_works`:
//!   `ccteam send some-slug -- "--help"` (the V0.4.5 workaround) is
//!   equivalent to the bare form. Same expectation.

use std::process::Command;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

/// Stable substring from `ccteam --help` that should NOT appear when
/// the leading-hyphen arg is forwarded as a literal body. If clap
/// re-interprets `--help` as the global help flag, the help banner
/// lists subcommands; this substring is a stable anchor.
const HELP_BANNER_ANCHOR: &str = "Usage: ccteam";

#[test]
fn t01_send_accepts_leading_hyphen() {
    let out = Command::new(ccteam_bin())
        .args(["send", "ccteam-cli-test-no-such-slug", "--help"])
        .output()
        .expect("spawn ccteam send");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The literal `--help` must NOT have triggered ccteam's own help.
    assert!(
        !stdout.contains(HELP_BANNER_ANCHOR),
        "leading `--help` should be treated as message body, but ccteam printed its own help.\nstdout: {stdout}\nstderr: {stderr}",
    );
    // Daemon isn't running in tests; expect a non-zero exit + an
    // error path (daemon connect / project lookup / etc).
    assert!(
        !out.status.success(),
        "without a running daemon the send command should fail; got success.\nstdout: {stdout}\nstderr: {stderr}",
    );
}

#[test]
fn t02_send_dash_dash_separator_still_works() {
    let out = Command::new(ccteam_bin())
        .args(["send", "ccteam-cli-test-no-such-slug", "--", "--help"])
        .output()
        .expect("spawn ccteam send -- --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(HELP_BANNER_ANCHOR),
        "`--` separator should still suppress help-flag interpretation.\nstdout: {stdout}\nstderr: {stderr}",
    );
    assert!(
        !out.status.success(),
        "without a running daemon the send command should fail; got success.\nstdout: {stdout}\nstderr: {stderr}",
    );
}
