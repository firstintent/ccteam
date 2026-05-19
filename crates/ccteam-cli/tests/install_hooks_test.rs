//! V0.6.1 F139 — `install_hooks` end-to-end test, via the `ccteam doctor
//! --install-hooks` CLI surface.
//!
//! `ccteam_core::hooks_dispatcher::tests` already covers the pure unit
//! contract (idempotency, hand-edit recovery, mode 0755). This file
//! verifies the user-visible CLI plumbing — namely that:
//!
//! 1. `ccteam doctor --install-hooks` returns 0 and reports the action.
//! 2. The hook.sh file lands at the expected `~/.ccteam/hooks/hook.sh`
//!    path with chmod 0755 + the embedded body.
//! 3. The script body wires the daemon HTTP fast path + CLI fallback —
//!    a smoke test on the literal contents so a future template churn
//!    that drops one of the two paths fails loud here.

use ccteam_core::HOOK_DISPATCHER_SH;
use std::process::Command;
use tempfile::TempDir;

fn ccteam_bin() -> std::path::PathBuf {
    // `cargo test` puts the dev-built `ccteam` binary next to the
    // integration test executable (the convention used by every other
    // CLI integration test in this crate).
    let exe = std::env::current_exe().unwrap();
    let mut dir = exe.parent().unwrap().to_path_buf();
    while dir.parent().is_some() && !dir.join("ccteam").exists() {
        dir = dir.parent().unwrap().to_path_buf();
    }
    dir.join("ccteam")
}

#[test]
fn ccteam_doctor_install_hooks_materializes_hook_sh() {
    let bin = ccteam_bin();
    if !bin.exists() {
        // Same defensive skip pattern other CLI tests use when run in
        // sandboxes that didn't build the binary.
        eprintln!("skipping: {} not built", bin.display());
        return;
    }
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join(".ccteam");

    let output = Command::new(&bin)
        .arg("doctor")
        .arg("--install-hooks")
        .env("CCTEAM_HOME", &home)
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .output()
        .expect("spawn ccteam doctor --install-hooks");
    assert!(
        output.status.success(),
        "doctor exited non-zero: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--install-hooks (V0.6.1 F139)"),
        "expected F139 header in stdout, got: {stdout}",
    );

    let hook_sh = home.join("hooks/hook.sh");
    assert!(hook_sh.exists(), "hook.sh missing at {}", hook_sh.display());

    let body = std::fs::read_to_string(&hook_sh).unwrap();
    assert_eq!(body, HOOK_DISPATCHER_SH);
    assert!(
        body.contains("/internal/hook/"),
        "dispatcher must POST to /internal/hook/* (daemon fast path)",
    );
    assert!(
        body.contains("ccteam internal hook"),
        "dispatcher must fall back to `ccteam internal hook ...` when daemon is down",
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&hook_sh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "hook.sh must be chmod 0755");
    }
}

#[test]
fn ccteam_doctor_install_hooks_is_idempotent() {
    let bin = ccteam_bin();
    if !bin.exists() {
        eprintln!("skipping: {} not built", bin.display());
        return;
    }
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join(".ccteam");

    for _ in 0..3 {
        let status = Command::new(&bin)
            .arg("doctor")
            .arg("--install-hooks")
            .env("CCTEAM_HOME", &home)
            .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .status()
            .unwrap();
        assert!(status.success(), "re-run must stay successful: {status}");
    }

    // After three runs the script body still matches the embedded body.
    let hook_sh = home.join("hooks/hook.sh");
    let body = std::fs::read_to_string(&hook_sh).unwrap();
    assert_eq!(body, HOOK_DISPATCHER_SH);
}
