//! V0.6.0 Wave 3 F112 — `CodexAppServerAdapter` tests over a UDS
//! socket served by a scripted in-process JSON-RPC peer. We dial the
//! peer via `CCTEAM_CODEX_APP_SERVER_SOCKET` env override so the
//! adapter's `client()` connects to the test socket rather than
//! `$CODEX_HOME/app-server-control/app-server-control.sock`.

use ccteam_core::execution::codex_app_server::{
    translate_notification, turn_input_to_items, CodexAppServerAdapter, APP_SERVER_SOCKET_ENV,
};
use ccteam_core::execution::codex_jsonrpc::Notification;
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, TurnInput,
};
use serde_json::{json, Value};
use serial_test::serial;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Bind a unique UDS socket under /tmp and return its path. Each test
/// gets its own so they can run in parallel without trampling on each
/// other's `APP_SERVER_SOCKET_ENV` override.
fn unique_socket_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "ccteam-wave3-codex-app-server-{tag}-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// Spawn a scripted peer that accepts ONE connection and serves the
/// supplied request → response map. Notifications can be pushed
/// out-of-band via the returned channel.
async fn spawn_scripted_peer(
    sock: PathBuf,
    handler: impl Fn(&Value) -> Value + Send + 'static,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Sender<Value>,
) {
    let listener = UnixListener::bind(&sock).unwrap();
    let (notif_tx, mut notif_rx) = tokio::sync::mpsc::channel::<Value>(16);
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);

        loop {
            let mut buf = String::new();
            tokio::select! {
                line = reader.read_line(&mut buf) => {
                    match line {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let req: Value = match serde_json::from_str(buf.trim()) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let id = req.get("id").cloned();
                            let mut resp = handler(&req);
                            if let Some(id) = id {
                                resp["id"] = id;
                            }
                            let mut bytes = serde_json::to_vec(&resp).unwrap();
                            bytes.push(b'\n');
                            let _ = w.write_all(&bytes).await;
                            let _ = w.flush().await;
                        }
                    }
                }
                notif = notif_rx.recv() => {
                    match notif {
                        Some(n) => {
                            let mut bytes = serde_json::to_vec(&n).unwrap();
                            bytes.push(b'\n');
                            let _ = w.write_all(&bytes).await;
                            let _ = w.flush().await;
                        }
                        None => continue,
                    }
                }
            }
        }
    });
    (task, notif_tx)
}

#[tokio::test(flavor = "current_thread")]
async fn turn_input_to_items_handles_all_variants() {
    let text = turn_input_to_items(TurnInput::UserText("hi".into())).unwrap();
    assert_eq!(text[0]["type"], "text");

    let img = turn_input_to_items(TurnInput::Image(PathBuf::from("/x.png"))).unwrap();
    assert_eq!(img[0]["type"], "localImage");

    let err = turn_input_to_items(TurnInput::SystemDirective("/compact".into())).unwrap_err();
    assert!(matches!(err, HarnessError::SubmitFailed(_)));

    let tr = turn_input_to_items(TurnInput::ToolResult {
        call_id: "c1".into(),
        content: json!({"ok": true}),
    })
    .unwrap();
    assert_eq!(tr[0]["type"], "text");
}

#[test]
fn translate_notification_thread_filtering() {
    // Matching thread_id → translated; mismatching → None.
    let ok = Notification {
        method: "thread/started".into(),
        params: json!({ "thread_id": "wanted" }),
    };
    let miss = Notification {
        method: "thread/started".into(),
        params: json!({ "thread_id": "other" }),
    };
    assert!(translate_notification(&ok, "wanted").is_some());
    assert!(translate_notification(&miss, "wanted").is_none());
}

#[test]
fn translate_notification_unknown_method_returns_none() {
    let n = Notification {
        method: "thread/status/changed".into(),
        params: json!({ "thread_id": "t-1" }),
    };
    // We deliberately don't propagate status-changed today (orchestrator
    // poll path owns state transitions); ensure it returns None.
    assert!(translate_notification(&n, "t-1").is_none());
}

#[test]
fn translate_item_completed_extracts_file_change_details() {
    let n = Notification {
        method: "item/completed".into(),
        params: json!({
            "thread_id": "t-1",
            "item": {
                "id": "i-1",
                "type": "file_change",
                "changes": [{ "path": "/x.rs", "kind": "update" }]
            }
        }),
    };
    let e = translate_notification(&n, "t-1").unwrap();
    match e {
        ThreadEvent::ItemCompleted { item } => {
            match item.details {
                ccteam_core::harness::ThreadItemDetails::FileChange { path, kind } => {
                    assert_eq!(path, PathBuf::from("/x.rs"));
                    assert_eq!(kind, "update");
                }
                other => panic!("expected FileChange, got {other:?}"),
            }
        }
        _ => panic!("expected ItemCompleted"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn adapter_returns_spawn_failed_when_socket_missing() {
    let bogus = std::env::temp_dir().join("ccteam-wave3-nonexistent.sock");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &bogus);
    let _ = std::fs::remove_file(&bogus);
    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief { role: "demo".into() };
    let ctx = SpawnCtx {
        slug: "test".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
    };
    let err = adapter.start_thread(&spec, &ctx).await.unwrap_err();
    assert!(matches!(err, HarnessError::SpawnFailed(_)));
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn adapter_start_thread_against_scripted_peer() {
    let sock = unique_socket_path("start-thread");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let (peer, _notif) = spawn_scripted_peer(sock.clone(), |req| {
        if req["method"] == "thread/start" {
            json!({ "result": { "thread": { "thread_id": "tid-42" } } })
        } else {
            json!({ "error": { "code": -32601, "message": "unexpected" } })
        }
    })
    .await;
    // Give the listener a moment.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief { role: "demo".into() };
    let ctx = SpawnCtx {
        slug: "test".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
    };
    let h = adapter.start_thread(&spec, &ctx).await.unwrap();
    assert_eq!(h.vendor, AgentVendor::Codex);
    assert_eq!(h.mode, ExecutionMode::Chat);
    assert_eq!(h.identity, "tid-42");

    drop(peer); // shutdown peer; adapter no-ops on subsequent calls
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}
