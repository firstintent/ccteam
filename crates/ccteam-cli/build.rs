//! Capture the git commit the binary is built from and expose it as the
//! `CCTEAM_GIT_COMMIT` compile-time env var, so `ccteam --version` can
//! report `0.8.4 (<commit>)`. This makes a running daemon's exact build
//! identifiable (the operator was chasing "is the running binary the one
//! I just rebuilt?"). Falls back to "unknown" when git is unavailable
//! (e.g. building from a source tarball with no `.git`).

use std::process::Command;

fn main() {
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CCTEAM_GIT_COMMIT={commit}");

    // Rebuild when the checked-out commit moves so --version stays accurate.
    if let Some(git_dir) = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
}
