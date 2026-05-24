//! V0.6.0 Wave 3 F112 — `ccteam doctor --check-codex-version` /
//! `--check-codex-auth` end-to-end tests via the binary surface. We
//! manipulate `PATH` to point at a fake `codex` script so the doctor
//! report is deterministic regardless of whether the real codex CLI
//! is installed on the test host.

use std::io::Write;
use std::process::Command;

/// Build a tempdir + fake `codex` script that emits the supplied
/// stdout for `codex --version` and `codex login status`. Returns the
/// tempdir guard (must outlive callers — PATH points inside it).
fn fake_codex_dir(version_line: &str, login_line: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let codex_path = dir.path().join("codex");
    let mut f = std::fs::File::create(&codex_path).unwrap();
    let script = format!(
        "#!/usr/bin/env bash\n\
        case \"$1\" in\n  \
          --version) echo {version_line:?} ;;\n  \
          login)\n    \
            if [ \"$2\" = \"status\" ]; then echo {login_line:?}; fi\n    \
            ;;\n  \
          *) echo \"fake codex: unknown\" >&2; exit 1 ;;\n\
        esac\n",
    );
    f.write_all(script.as_bytes()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&codex_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&codex_path, perms).unwrap();
    dir
}

fn run_doctor_with_path(extra_path: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let mut path = extra_path.to_string_lossy().to_string();
    if let Some(orig) = std::env::var_os("PATH") {
        path.push(':');
        path.push_str(&orig.to_string_lossy());
    }
    let out = Command::new(bin)
        .env("PATH", path)
        .arg("doctor")
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn check_codex_version_reports_ok_for_supported_release() {
    let dir = fake_codex_dir("codex 0.131.0", "Not logged in");
    let (stdout, _stderr, code) = run_doctor_with_path(dir.path(), &["--check-codex-version"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("[OK]"), "stdout: {stdout}");
    assert!(stdout.contains("0.131"), "stdout: {stdout}");
}

#[test]
fn check_codex_version_warns_on_old_release() {
    let dir = fake_codex_dir("codex 0.120.0", "Not logged in");
    let (stdout, _stderr, code) = run_doctor_with_path(dir.path(), &["--check-codex-version"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("[WARN]"), "stdout: {stdout}");
}

#[test]
fn check_codex_auth_recognises_chatgpt_login() {
    let dir = fake_codex_dir("codex 0.131.0", "Logged in using ChatGPT");
    let (stdout, _stderr, code) = run_doctor_with_path(dir.path(), &["--check-codex-auth"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("[OK]"), "stdout: {stdout}");
    assert!(stdout.contains("ChatGPT"), "stdout: {stdout}");
}

#[test]
fn check_codex_auth_warns_when_not_logged_in() {
    let dir = fake_codex_dir("codex 0.131.0", "Not logged in. Please run `codex login`");
    let (stdout, _stderr, code) = run_doctor_with_path(dir.path(), &["--check-codex-auth"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("[WARN]"), "stdout: {stdout}");
    assert!(stdout.contains("codex login"), "stdout: {stdout}");
}
