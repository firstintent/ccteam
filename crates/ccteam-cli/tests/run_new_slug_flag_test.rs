//! V0.2.2 F34 — CLI surface for the new slug-decision flags. Lives in
//! integration tests rather than the lib-internal mod so env-mutating
//! tests (`CCTEAM_AUTO_SLUG{,_BIN}` / `CCTEAM_HOME` / `HOME`) don't
//! race other tests in the same process.

use std::path::PathBuf;
use std::process::Command;

fn cct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

#[test]
fn cct_new_help_advertises_slug_flags() {
    let out = Command::new(cct_bin())
        .args(["new", "--help"])
        .output()
        .expect("spawn ccteam new --help");
    assert!(out.status.success(), "ccteam new --help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for flag in ["--slug", "--no-auto-slug", "--auto-slug-model"] {
        assert!(
            stdout.contains(flag),
            "ccteam new --help should advertise {flag}; got:\n{stdout}",
        );
    }
}

#[test]
fn cct_new_with_explicit_slug_creates_project_dir() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let projects_root = tmp.path().join("projects");
    let global = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(&global).unwrap();

    let out = Command::new(cct_bin())
        .args([
            "new",
            "--slug",
            "ccteam-ui",
            "--team",
            "dev",
            "--no-auto-slug",
            "Build the ccteam ui shell",
        ])
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", &global)
        .env("CCTEAM_PROJECTS_ROOT", &projects_root)
        // Belt-and-suspenders: the unit-side env knob also forces Tier 4.
        .env("CCTEAM_AUTO_SLUG", "off")
        .output()
        .expect("spawn ccteam new");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "ccteam new should succeed; stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        stdout.contains("dev-ccteam-ui"),
        "expected slug `dev-ccteam-ui` in stdout; got:\n{stdout}",
    );
    let project: PathBuf = projects_root.join("dev-ccteam-ui");
    assert!(
        project.join(".ccteam/spec.md").is_file(),
        "spec.md must exist under {}",
        project.display(),
    );
}

#[test]
fn cct_new_rejects_explicit_slug_with_illegal_chars() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let projects_root = tmp.path().join("projects");
    let global = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(&global).unwrap();

    let out = Command::new(cct_bin())
        .args([
            "new",
            "--slug",
            "Bad Slug!",
            "--team",
            "dev",
            "--no-auto-slug",
            "irrelevant brief",
        ])
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", &global)
        .env("CCTEAM_PROJECTS_ROOT", &projects_root)
        .env("CCTEAM_AUTO_SLUG", "off")
        .output()
        .expect("spawn ccteam new");

    assert!(!out.status.success(), "illegal slug must fail-loud");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[a-z0-9-]+"),
        "fail-loud should hint at the legal regex; got:\n{stderr}",
    );
}

#[test]
fn cct_new_tier3_reads_from_stub_claude_bin() {
    // Stub `claude -p` shell-out: a tiny shell script that ignores
    // its args, drains stdin, and prints a fixed slug. Verifies that
    // Tier 3 wires through `CCTEAM_AUTO_SLUG_BIN`, sanitizes stdout,
    // and (because stdin is the test harness's piped null, not a tty)
    // auto-accepts the suggestion.
    if cfg!(windows) {
        // Plain `sh` stub won't run on Windows; skip rather than ifdef
        // a parallel `.bat` script for one test.
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let projects_root = tmp.path().join("projects");
    let global = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(&global).unwrap();

    let stub = tmp.path().join("claude-stub.sh");
    std::fs::write(
        &stub,
        b"#!/usr/bin/env sh\ncat > /dev/null\nprintf '%s\\n' 'tier3-stub-slug'\n",
    )
    .expect("write stub");
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");

    let out = Command::new(cct_bin())
        .args([
            "new",
            "--team",
            "dev",
            "--auto-slug-model",
            "claude-haiku-4-5-20251001",
            "Build a tier-3 stub project",
        ])
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", &global)
        .env("CCTEAM_PROJECTS_ROOT", &projects_root)
        .env("CCTEAM_AUTO_SLUG_BIN", &stub)
        // CCTEAM_AUTO_SLUG is unset so Tier 3 is allowed to fire.
        .env_remove("CCTEAM_AUTO_SLUG")
        .output()
        .expect("spawn ccteam new");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "ccteam new with stub claude should succeed; stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        stdout.contains("dev-tier3-stub-slug"),
        "expected stubbed slug in output; got:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        projects_root
            .join("dev-tier3-stub-slug/.ccteam/spec.md")
            .is_file(),
        "spec.md must exist under projects_root for the stubbed slug",
    );
}

#[test]
fn cct_new_tier3_falls_back_to_tier4_when_stub_returns_garbage() {
    if cfg!(windows) {
        return;
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let projects_root = tmp.path().join("projects");
    let global = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(&global).unwrap();

    let stub = tmp.path().join("claude-garbage.sh");
    // Empty stdout → sanitizer rejects → Tier 4 fallback.
    std::fs::write(&stub, b"#!/usr/bin/env sh\ncat > /dev/null\nprintf ''\n").expect("write stub");
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");

    let out = Command::new(cct_bin())
        .args([
            "new",
            "--team",
            "dev",
            "--auto-slug-model",
            "claude-haiku-4-5-20251001",
            "Build the fallback widget",
        ])
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", &global)
        .env("CCTEAM_PROJECTS_ROOT", &projects_root)
        .env("CCTEAM_AUTO_SLUG_BIN", &stub)
        .env_remove("CCTEAM_AUTO_SLUG")
        .output()
        .expect("spawn ccteam new");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "Tier 4 fallback path must succeed even when stub LLM returns garbage",
    );
    // `slugify_brief("Build the fallback widget")` → tokens
    // `[build, fallback, widget]` (stop-word `the` dropped) → exactly
    // three tokens, joined.
    assert!(
        stdout.contains("dev-build-fallback-widget"),
        "expected Tier 4 deterministic slug; got:\n{stdout}",
    );
}
