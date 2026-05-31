//! Red-line: `ccteam-core` must not transitively depend on any IM /
//! openhuman crate. Wave 2 split puts every IM transport dependency
//! inside `ccteam-im`. This test runs `cargo tree -p ccteam-core
//! --prefix none` (the canonical way to enumerate transitive deps
//! per the Cargo book §6.1) and asserts the forbidden names are
//! absent.
//!
//! Why a test and not a CI grep: the matrix of forbidden names
//! grows with each new provider (V0.7 lark/dingtalk/qq…). Keeping
//! the assertion next to the crate boundary makes it harder to
//! accidentally regress when adding a new dep that pulls in
//! openhuman via a different path.

use std::process::Command;

const FORBIDDEN: &[&str] = &[
    "openhuman",
    "openhuman-core",
    "teloxide",
    "slack_morphism",
    "serenity",
    "matrix-sdk",
    "whatsapp-rust",
];

#[test]
fn ccteam_core_has_no_im_deps() {
    let out = Command::new("cargo")
        .args(["tree", "-p", "ccteam-core", "--prefix", "none"])
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .output();
    let out = match out {
        Ok(o) => o,
        Err(err) => {
            // If `cargo tree` is unavailable for some reason (sandbox
            // / offline) skip rather than block CI; the more common
            // case is the user running `cargo test --workspace`
            // locally where the tool exists.
            eprintln!("cargo tree unavailable: {err}; skipping dep graph check");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in FORBIDDEN {
        assert!(
            !stdout.lines().any(|l| l.contains(needle)),
            "ccteam-core transitively depends on `{needle}`:\n{stdout}"
        );
    }
}

#[test]
fn ccteam_im_owns_reqwest_dep() {
    // Inverse: confirm ccteam-im does pull `reqwest` (so that
    // refactors that accidentally remove the HTTP client surface
    // are caught — none of the Channel providers would work without
    // it).
    let out = Command::new("cargo")
        .args(["tree", "-p", "ccteam-im", "--prefix", "none"])
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .output();
    let out = match out {
        Ok(o) => o,
        Err(err) => {
            eprintln!("cargo tree unavailable: {err}; skipping");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.contains("reqwest")),
        "ccteam-im should depend on reqwest:\n{stdout}"
    );
}
