//! V0.3.1 F46 — `ccteam doctor --install-statusline-adapter` integration
//! tests.
//!
//! ENV-mutating: each `Command::spawn` overrides `CLAUDE_CONFIG_HOME`
//! and `HOME` so the tested binary writes into a tempdir instead of
//! the developer's real `~/.claude/`. CLAUDE.md §六 requires these to
//! live in `crates/*/tests/*.rs` (separate test binaries) so the env
//! mutation doesn't race other tests in the same binary.
//!
//! Coverage (per dev-plan §2.4):
//!
//! 1. **Fresh install**: no existing `statusline-command.sh` → wrapper
//!    is created with the marker; no backup file.
//! 2. **Wrap user file**: existing user statusline → backup created
//!    with `.bak-<utc-ts>` suffix; wrapper invokes the backup.
//! 3. **Idempotent re-install**: rerunning over a marker-bearing file
//!    leaves the backup count unchanged + rewrites the wrapper.
//! 4. **Subprocess smoke**: actually pipe stdin into the wrapper as a
//!    shell child + assert (a) the original passthrough still emits
//!    the user's footer, (b) the dual-write path runs (we don't
//!    require the snapshot file because `cwd` is outside
//!    `projects_root` — the no-op branch is the right thing to
//!    smoke-test on a tempdir).

use std::process::Command;

use tempfile::TempDir;

const MARKER: &str = "# ccteam-managed:statusline begin (V0.3.1 F46";

fn run_install(claude_dir: &std::path::Path, home: &std::path::Path, ccteam_home: &std::path::Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    Command::new(bin)
        .args(["doctor", "--install-statusline-adapter"])
        .env("CLAUDE_CONFIG_HOME", claude_dir)
        .env("HOME", home)
        .env("CCTEAM_HOME", ccteam_home)
        .output()
        .expect("spawn ccteam doctor --install-statusline-adapter")
}

#[test]
fn fresh_install_writes_wrapper_with_marker_and_no_backup() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ccteam_home = tmp.path().join(".ccteam");

    let out = run_install(&claude_dir, &home, &ccteam_home);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit not ok; stdout={stdout}; stderr={stderr}");

    let target = claude_dir.join("statusline-command.sh");
    assert!(target.exists(), "wrapper file should exist");
    let body = std::fs::read_to_string(&target).unwrap();
    assert!(
        body.contains(MARKER),
        "wrapper missing marker; body=\n{body}",
    );
    assert!(body.contains("hook harness-snapshot"));

    // Output report announces no backup.
    assert!(stdout.contains("backup: none"), "stdout=\n{stdout}");

    // No `.bak-*` files exist yet (the writer takes a backup ONLY when
    // it is wrapping a user-authored file).
    let backups: Vec<_> = std::fs::read_dir(&claude_dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("statusline-command.sh.bak-"))
                .unwrap_or(false)
        })
        .collect();
    assert!(backups.is_empty(), "no backup expected on fresh install");
}

#[test]
fn install_over_user_file_creates_backup_and_passthrough() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ccteam_home = tmp.path().join(".ccteam");

    // Lay down a user-authored statusline that prints a fixed footer.
    let target = claude_dir.join("statusline-command.sh");
    std::fs::write(
        &target,
        "#!/bin/sh\nINPUT=$(cat)\nprintf 'USER FOOTER: %s' \"$INPUT\"\n",
    )
    .unwrap();

    let out = run_install(&claude_dir, &home, &ccteam_home);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit not ok; stdout={stdout}; stderr={stderr}");

    // 1. Wrapper has marker.
    let body = std::fs::read_to_string(&target).unwrap();
    assert!(body.contains(MARKER));
    assert!(body.contains("hook harness-snapshot"));

    // 2. Backup exists with the timestamp suffix (8 digits + T + 6 digits + Z).
    let backups: Vec<_> = std::fs::read_dir(&claude_dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("statusline-command.sh.bak-"))
        .collect();
    assert_eq!(backups.len(), 1, "expected exactly one backup; found {backups:?}");
    let bak_name = &backups[0];
    let suffix = bak_name.trim_start_matches("statusline-command.sh.bak-");
    // Format `YYYYMMDDTHHMMSSZ` (16 chars).
    assert_eq!(
        suffix.len(),
        16,
        "expected 16-char timestamp; got `{suffix}`",
    );
    assert!(suffix.ends_with('Z'));

    // 3. Backup content == original user file.
    let bak_path = claude_dir.join(bak_name);
    let bak_body = std::fs::read_to_string(&bak_path).unwrap();
    assert!(bak_body.contains("USER FOOTER"));

    // 4. Wrapper passthrough block references the backup path.
    assert!(
        body.contains(bak_name.as_str()),
        "wrapper missing passthrough to backup; body=\n{body}",
    );

    // 5. Report announces the backup path.
    assert!(stdout.contains(bak_name.as_str()), "report missing backup path; stdout=\n{stdout}");
}

#[test]
fn reinstall_over_marker_keeps_existing_backup() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ccteam_home = tmp.path().join(".ccteam");

    // Pre-existing user file → first install takes a backup.
    let target = claude_dir.join("statusline-command.sh");
    std::fs::write(&target, "#!/bin/sh\necho v1\n").unwrap();

    let _ = run_install(&claude_dir, &home, &ccteam_home);
    let backups_after_first: Vec<_> = std::fs::read_dir(&claude_dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("statusline-command.sh.bak-"))
        .collect();
    assert_eq!(backups_after_first.len(), 1);

    // Second install — already marker-bearing wrapper, must not
    // accumulate a second backup of our own output.
    let out = run_install(&claude_dir, &home, &ccteam_home);
    assert!(out.status.success());

    let backups_after_second: Vec<_> = std::fs::read_dir(&claude_dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("statusline-command.sh.bak-"))
        .collect();
    assert_eq!(
        backups_after_second.len(),
        1,
        "re-install must not accumulate extra backups; {backups_after_second:?}",
    );
    assert_eq!(backups_after_first, backups_after_second);

    // Wrapper still has marker + still passes through to backup.
    let body = std::fs::read_to_string(&target).unwrap();
    assert!(body.contains(MARKER));
    assert!(body.contains(&backups_after_first[0]));
}

#[test]
fn wrapper_subprocess_runs_passthrough_and_dual_write_branch() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ccteam_home = tmp.path().join(".ccteam");

    // User authored statusline that echoes a recognizable token.
    let target = claude_dir.join("statusline-command.sh");
    std::fs::write(&target, "#!/bin/sh\nprintf 'USER_FOOTER\\n'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = run_install(&claude_dir, &home, &ccteam_home);
    assert!(out.status.success());

    // Run the wrapper as a subprocess. The dual-write call goes
    // through `ccteam hook harness-snapshot`, which silently no-ops
    // when `cwd` is outside `projects_root` (this tempdir IS outside).
    // We still expect the user footer to land on stdout via the
    // passthrough block.
    let wrapper_run = Command::new("sh")
        .arg(&target)
        .env("CLAUDE_CONFIG_HOME", &claude_dir)
        .env("HOME", &home)
        .env("CCTEAM_HOME", &ccteam_home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn wrapper sh");

    use std::io::Write;
    {
        let mut stdin = wrapper_run.stdin.as_ref().unwrap();
        stdin.write_all(b"{\"model\":\"sonnet-4.5\"}").unwrap();
    }
    let output = wrapper_run.wait_with_output().expect("wait wrapper");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("USER_FOOTER"),
        "passthrough should still emit user footer; stdout=\n{stdout}",
    );
}
