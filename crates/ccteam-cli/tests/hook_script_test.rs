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
//!
//! F186: also exercises the daemon-HTTP fast path by binding a one-shot
//! TCP listener (the same primitive `internal_hook_test.rs` uses) and
//! asserting the `X-Ccteam-Role` / `X-Ccteam-Slug` headers land on the
//! wire when the env vars are set.

use ccteam_core::HOOK_DISPATCHER_SH;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
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

/// Spawn a freshly-written executable, retrying the handful of kernel
/// races that show up only under heavy parallel test load. When many
/// test threads write + exec scripts at once we intermittently see
/// `ETXTBSY` (os error 26, "text file busy" — the file is still open
/// for write somewhere) or `EAGAIN` (os error 11, transient fork/exec
/// resource pressure). Both are retryable: a tiny sleep lets the writer
/// `close(2)` land or the resource free up. This is test-harness
/// hardening only — it does NOT change what the test asserts, only how
/// resilient the `spawn()` itself is to the parallel-load flake that
/// has truncated the gate before. Any other error fails immediately.
fn spawn_with_retry(cmd: &mut Command) -> std::process::Child {
    use std::io::ErrorKind;
    for attempt in 0..10 {
        match cmd.spawn() {
            Ok(child) => return child,
            Err(e) => {
                let raw = e.raw_os_error();
                let retryable =
                    e.kind() == ErrorKind::WouldBlock || matches!(raw, Some(26) | Some(11)); // ETXTBSY | EAGAIN
                if retryable && attempt < 9 {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                panic!("spawn hook.sh failed (attempt {attempt}): {e}");
            }
        }
    }
    unreachable!("spawn_with_retry exhausted loop without returning")
}

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
    let mut cmd = Command::new(&hook_sh);
    cmd.arg("progress-append")
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
        .stderr(Stdio::piped());
    let mut child = spawn_with_retry(&mut cmd);
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

    let mut cmd = Command::new(&hook_sh);
    cmd.arg("chat-progress")
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
        .stderr(Stdio::piped());
    let mut child = spawn_with_retry(&mut cmd);
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

/// F186 — when `$CCTEAM_CHAT_ROLE` / `$CCTEAM_CHAT_SLUG` are set in
/// the hook subprocess env (which mirrors the production tmux env
/// injection F175 set up in `claude_tui.rs::start_thread`), the script
/// must forward them as `X-Ccteam-Role` / `X-Ccteam-Slug` HTTP request
/// headers on the daemon fast path. Without this the daemon process
/// (running as its own process tree, NOT inheriting claude's env)
/// cannot derive the bot identity and chat-progress events land with
/// `role=""`.
///
/// Test scaffold: a one-shot TCP listener stands in for the ccteam
/// daemon, captures the raw HTTP request bytes the script sends,
/// returns a minimal `200 OK` so curl exits clean, and the assertions
/// run against the captured bytes. Same primitive used by
/// `crates/ccteam-web/tests/internal_hook_test.rs` (different layer).
#[test]
fn hook_sh_forwards_chat_role_and_slug_headers_on_http_path() {
    let tmp = TempDir::new().unwrap();
    let hooks_dir = tmp.path().join(".ccteam/hooks");
    let hook_sh = write_hook_sh(&hooks_dir);

    // A token file flips hook.sh into the curl fast path. Body is
    // opaque to the script (it just forwards it as `Bearer ccteam:<x>`).
    let ccteam_home = tmp.path().join(".ccteam");
    std::fs::create_dir_all(&ccteam_home).unwrap();
    let token_file = ccteam_home.join("web-token");
    std::fs::write(&token_file, b"f186tokenf186tokenf186tokenf186t").unwrap();

    // Bind 127.0.0.1:0 → get an ephemeral port the script will hit.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    // Background thread: accept one connection, read until headers end,
    // write a minimal HTTP/1.1 200 OK + `{}` body so curl exits 0.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let server = thread::spawn(move || {
        listener.set_nonblocking(false).expect("blocking listener");
        let (mut stream, _) = listener.accept().expect("accept");
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut buf = vec![0u8; 8192];
        let mut captured = Vec::<u8>::new();
        // Read until we see the end-of-headers marker. The body
        // (POSTed JSON) follows; we don't need to consume it for the
        // header assertion but we drain a couple of reads so the
        // client doesn't EPIPE on the response write.
        for _ in 0..8 {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    captured.extend_from_slice(&buf[..n]);
                    if captured.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        let _ = stream.flush();
        tx.send(captured).expect("send captured bytes");
    });

    let mut cmd = Command::new(&hook_sh);
    cmd.arg("chat-progress")
        .arg("session-start")
        .env("HOME", tmp.path())
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_WEB_PORT", port.to_string())
        .env("CCTEAM_CHAT_ROLE", "bob")
        .env("CCTEAM_CHAT_SLUG", "demo-slug")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_with_retry(&mut cmd);
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"cwd": "/tmp/demo", "session_id": "sid-f186"}"#)
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().expect("wait hook.sh");
    assert!(status.success(), "hook.sh exited non-zero: {status}");

    server.join().expect("listener thread join");
    let captured = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("captured request bytes within timeout");
    let req = String::from_utf8_lossy(&captured);

    // Match case-insensitively because curl emits header names in
    // canonical (mixed) case; the contract is the value.
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("x-ccteam-role:bob") || lower.contains("x-ccteam-role: bob"),
        "expected X-Ccteam-Role: bob in request, got:\n{req}",
    );
    assert!(
        lower.contains("x-ccteam-slug:demo-slug") || lower.contains("x-ccteam-slug: demo-slug"),
        "expected X-Ccteam-Slug: demo-slug in request, got:\n{req}",
    );
    // Sanity: kind/action made it onto the request line.
    assert!(
        req.starts_with("POST /internal/hook/chat-progress/session-start"),
        "expected POST to chat-progress/session-start, got:\n{req}",
    );
}
