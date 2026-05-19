//! V0.3 M5.0 dep-graph red line: `ccteam-web` MUST NOT depend on
//! `ccteam-cli` (binary-as-library is a dep-graph anti-pattern; the
//! two crates are siblings — both fan in to `ccteam-core`).
//!
//! `docs/versions/v0-3/prd.md` §3.2.3 + `docs/dev-coupling-audit.md` F45 lock
//! the rule. This test runs `cargo tree -p ccteam-web` and asserts no
//! `ccteam-cli` line appears anywhere in the output.
//!
//! If a future PR adds `ccteam-cli` to `ccteam-web`'s deps (directly
//! or transitively via, say, sharing CLI helpers), this test fails
//! loud — surfacing the coupling regression at PR-review time.

use std::process::Command;

#[test]
fn ccteam_web_does_not_depend_on_ccteam_cli() {
    // `cargo` is on PATH for tests (cargo invoked us). Use that — no
    // need to root through CARGO_HOME.
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", "ccteam-web", "--prefix=none"])
        .output()
        .expect("spawn cargo tree -p ccteam-web");
    assert!(
        out.status.success(),
        "cargo tree failed: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        // Match `ccteam-cli vX.Y.Z` exactly so a hypothetical future
        // crate `ccteam-cli-something` wouldn't false-positive.
        let head = line.split_whitespace().next().unwrap_or("");
        assert_ne!(
            head, "ccteam-cli",
            "ccteam-web must not depend on ccteam-cli (tech-design red line); \
             cargo tree output:\n{stdout}",
        );
    }
}
