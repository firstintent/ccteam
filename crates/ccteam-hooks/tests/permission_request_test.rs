//! v0.8.8 F1 — `permission-request` HITL hook forwards the ccteam session
//! **sid** (`session_sid`) in the `permission/ask` daemon request.
//!
//! Post-dedup, `(slug, role)` is no longer a unique key, so the daemon needs
//! the firing session's exact sid to label the approval prompt and resolve its
//! gateway entry. The hook learns the sid from stdin `ccteam_sid` (the HTTP
//! fast-path folds `X-Ccteam-Sid` here) or the `CCTEAM_CHAT_SID` env (injected
//! into the pane by `chat_spawn_env_owned`).
//!
//! RED LINE: `session_sid` (ccteam `s<N>`) is distinct from Anthropic's native
//! `session_id` UUID — both are forwarded, but only `session_sid` is the
//! ccteam identity key.
//!
//! This is a REAL round-trip: we stand up a Unix-socket "daemon" at the
//! canonical socket path, capture the request line the hook sends, reply with
//! an `allow` decision, then assert the captured `params.session_sid`. Env
//! mutation + socket bind live here (integration / own-process) per the repo's
//! "env-mutating tests go in tests/" discipline.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;

use ccteam_core::CcteamPaths;
use ccteam_hooks::permission_request_decide;
use serde_json::{json, Value};
use serial_test::serial;
use tempfile::TempDir;

fn fake_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join(".ccteam"),
        projects_root: tmp.path().join("projects"),
    }
}

/// Bind a one-shot Unix-socket server at the daemon socket path. It reads ONE
/// request line, ships it back over `captured`, replies with `reply`, and
/// returns. Returns once it is actually listening (so the caller can connect
/// without a race).
fn spawn_capture_daemon(socket: std::path::PathBuf, reply: Value, captured: mpsc::Sender<Value>) {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind daemon socket");
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                if let Ok(req) = serde_json::from_str::<Value>(line.trim()) {
                    let _ = captured.send(req);
                }
            }
            // Reply on the same stream.
            let mut out = reply.to_string();
            out.push('\n');
            let _ = reader.get_mut().write_all(out.as_bytes());
            let _ = reader.get_mut().flush();
        }
    });
}

#[test]
#[serial]
fn permission_ask_request_carries_session_sid_from_env() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(&tmp);
    let socket = ccteam_core::daemon_socket_path(&paths);

    let (tx, rx) = mpsc::channel();
    // The daemon approves so the decider returns allow — and we capture the
    // exact request the hook sent.
    spawn_capture_daemon(socket, json!({ "result": { "behavior": "allow" } }), tx);
    // Give the listener a moment to be ready before the hook connects.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // The pane env carries slug/role/sid (as `chat_spawn_env_owned` injects).
    std::env::set_var("CCTEAM_CHAT_SLUG", "demo");
    std::env::set_var("CCTEAM_CHAT_ROLE", "reviewer");
    std::env::set_var("CCTEAM_CHAT_SID", "s7");

    let stdin = json!({
        "tool_name": "Bash",
        "tool_input": { "command": "ls" },
        "session_id": "anthropic-native-uuid", // NOT the ccteam identity key
        "cwd": "/tmp/demo",
    });
    let decision = permission_request_decide(&paths, &stdin);

    std::env::remove_var("CCTEAM_CHAT_SLUG");
    std::env::remove_var("CCTEAM_CHAT_ROLE");
    std::env::remove_var("CCTEAM_CHAT_SID");

    // The approval round-tripped → allow.
    assert_eq!(
        decision.pointer("/hookSpecificOutput/decision/behavior"),
        Some(&json!("allow")),
        "approve click → allow"
    );

    // The captured request carries the ccteam sid in params.session_sid.
    let req = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("daemon must have captured the permission/ask request");
    assert_eq!(req["method"], "permission/ask");
    assert_eq!(
        req.pointer("/params/session_sid"),
        Some(&json!("s7")),
        "permission/ask must forward the ccteam session sid; got: {req}"
    );
    // The Anthropic native session_id is forwarded too, but is a DISTINCT field
    // (red line: never confused with the ccteam sid).
    assert_eq!(
        req.pointer("/params/session_id"),
        Some(&json!("anthropic-native-uuid")),
    );
    assert_ne!(
        req.pointer("/params/session_sid"),
        req.pointer("/params/session_id"),
        "the two id layers must stay distinct"
    );
}

#[test]
#[serial]
fn permission_ask_prefers_stdin_ccteam_sid_over_env() {
    // The HTTP fast-path folds `X-Ccteam-Sid` into stdin `ccteam_sid`; that
    // explicit value must win over the ambient env (mirrors the slug/role
    // precedence + chat_progress::derive_sid_from_payload).
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(&tmp);
    let socket = ccteam_core::daemon_socket_path(&paths);

    let (tx, rx) = mpsc::channel();
    spawn_capture_daemon(socket, json!({ "result": { "behavior": "deny" } }), tx);
    std::thread::sleep(std::time::Duration::from_millis(50));

    std::env::set_var("CCTEAM_CHAT_SLUG", "demo");
    std::env::set_var("CCTEAM_CHAT_ROLE", "reviewer");
    std::env::set_var("CCTEAM_CHAT_SID", "s_env"); // should be overridden

    let stdin = json!({
        "tool_name": "Bash",
        "tool_input": {},
        "ccteam_sid": "s_stdin", // explicit (header-folded) wins
    });
    let _decision = permission_request_decide(&paths, &stdin);

    std::env::remove_var("CCTEAM_CHAT_SLUG");
    std::env::remove_var("CCTEAM_CHAT_ROLE");
    std::env::remove_var("CCTEAM_CHAT_SID");

    let req = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("daemon must have captured the request");
    assert_eq!(
        req.pointer("/params/session_sid"),
        Some(&json!("s_stdin")),
        "explicit stdin ccteam_sid must win over CCTEAM_CHAT_SID env; got: {req}"
    );
}
