//! V0.6.1 F139 — `hook.sh` script execution test.
//!
//! Spawns the materialized `~/.ccteam/hooks/hook.sh` and verifies the
//! fallback behaviour: with the daemon down and no token file the
//! script must `exec` the bare `ccteam internal hook ...` path. We
//! shim `ccteam` with a tiny stub on `PATH` so the test doesn't need
//! the real daemon; the stub records its argv + stdin into a log file
//! we then read back.
//!
//! This is the contract the script's `# fallback` branch protects: when
//! a user's daemon isn't running, hooks must still fire correctly.

use ccteam_core::HOOK_DISPATCHER_SH;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use tempfile::TempDir;

const STUB_CCTEAM: &str = r#"#!/bin/sh
# Test stub. Records argv (excluding $0) + stdin into $CCTEAM_STUB_LOG so
# the integration test can assert on what `hook.sh` ended up dispatching.
echo "ARGS: $*" >> "$CCTEAM_STUB_LOG"
echo "STDIN-BEGIN" >> "$CCTEAM_STUB_LOG"
cat >> "$CCTEAM_STUB_LOG"
echo "" >> "$CCTEAM_STUB_LOG"
echo "STDIN-END" >> "$CCTEAM_STUB_LOG"
exit 0
"#;

fn write_stub_ccteam(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("ccteam");
    std::fs::write(&path, STUB_CCTEAM).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn write_hook_sh(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("hook.sh");
    std::fs::write(&path, HOOK_DISPATCHER_SH).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

#[test]
fn hook_sh_falls_back_to_ccteam_internal_hook_when_no_token() {
    let tmp = TempDir::new().unwrap();
    let stub_dir = tmp.path().join("bin");
    write_stub_ccteam(&stub_dir);
    let log = tmp.path().join("stub.log");
    let hooks_dir = tmp.path().join(".ccteam/hooks");
    let hook_sh = write_hook_sh(&hooks_dir);

    // No token file ⇒ script must take the "no token" branch and exec
    // `ccteam internal hook ...` directly.
    let mut child = Command::new(&hook_sh)
        .arg("progress-append")
        .arg("Stop")
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", tmp.path().join(".ccteam"))
        .env("CCTEAM_STUB_LOG", &log)
        // Strip proxy + put the stub on PATH ahead of anything else.
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env("PATH", format!("{}:/usr/bin:/bin", stub_dir.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook.sh");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"cwd": "/tmp/demo", "tool_name": "Bash"}"#)
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().expect("wait hook.sh");
    assert!(status.success(), "hook.sh exited non-zero: {status}");

    let log_body = std::fs::read_to_string(&log).expect("stub log written");
    assert!(
        log_body.contains("ARGS: internal hook progress-append Stop"),
        "stub must see CLI argv from fallback path, log: {log_body}",
    );
    assert!(
        log_body.contains(r#""cwd": "/tmp/demo""#),
        "stub must receive the Claude Code hook stdin payload, log: {log_body}",
    );
}

#[test]
fn hook_sh_with_action_routes_kind_and_action_to_cli() {
    // Same as above but verifies the 2-arg path (kind + action) lands
    // intact in the fallback argv.
    let tmp = TempDir::new().unwrap();
    let stub_dir = tmp.path().join("bin");
    write_stub_ccteam(&stub_dir);
    let log = tmp.path().join("stub.log");
    let hook_sh = write_hook_sh(&tmp.path().join(".ccteam/hooks"));

    let mut child = Command::new(&hook_sh)
        .arg("chat-progress")
        .arg("user-prompt")
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", tmp.path().join(".ccteam"))
        .env("CCTEAM_STUB_LOG", &log)
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env("PATH", format!("{}:/usr/bin:/bin", stub_dir.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"cwd": "/tmp/demo", "prompt": "hi"}"#)
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());

    let log_body = std::fs::read_to_string(&log).unwrap();
    assert!(
        log_body.contains("ARGS: internal hook chat-progress user-prompt"),
        "log: {log_body}",
    );
}
