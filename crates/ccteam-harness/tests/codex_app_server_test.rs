//! V0.6.0 Wave 3 F112 — `CodexAppServerAdapter` tests over a UDS
//! socket served by a scripted in-process JSON-RPC peer. We dial the
//! peer via `CCTEAM_CODEX_APP_SERVER_SOCKET` env override so the
//! adapter's `client()` connects to the test socket rather than
//! `$CODEX_HOME/app-server-control/app-server-control.sock`.

use ccteam_harness::execution::codex_app_server::{
    translate_notification, turn_input_to_items, CodexAppServerAdapter, APP_SERVER_SOCKET_ENV,
    APP_SERVER_TRANSPORT_ENV,
};
use ccteam_harness::execution::codex_jsonrpc::Notification;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, TurnInput,
};
use serde_json::{json, Value};
use serial_test::serial;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tempfile::TempDir;
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

#[tokio::test]
#[serial]
async fn real_codex_app_server_start_thread_smoke() {
    if std::env::var("CCTEAM_REAL_CODEX_APP_SERVER")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!("skip: set CCTEAM_REAL_CODEX_APP_SERVER=1 for real app-server smoke");
        return;
    }
    let transport = std::env::var(APP_SERVER_TRANSPORT_ENV).unwrap_or_else(|_| "uds".to_string());
    if transport != "stdio" {
        let socket = std::env::var(APP_SERVER_SOCKET_ENV)
            .expect("CCTEAM_CODEX_APP_SERVER_SOCKET must point to a real app-server UDS");
        assert!(
            std::path::Path::new(&socket).exists(),
            "real app-server socket must exist: {socket}"
        );
    }

    let tmp = TempDir::new().unwrap();
    let adapter = CodexAppServerAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(15),
        adapter.start_thread(
            &AgentSpecBrief {
                role: "real-smoke".to_string(),
            },
            &SpawnCtx {
                slug: "real-codex-smoke".to_string(),
                sid: "s-real".to_string(),
                cwd: tmp.path().to_path_buf(),
                project_dir: tmp.path().to_path_buf(),
                extra_args: vec![],
                model_id: None,
            },
        ),
    )
    .await
    .expect("real codex app-server thread/start timed out")
    .expect("real codex app-server thread/start should succeed");
    assert_eq!(handle.vendor, AgentVendor::Codex);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    assert!(!handle.identity.trim().is_empty());
    tokio::time::timeout(Duration::from_secs(10), adapter.close_thread(&handle))
        .await
        .expect("real codex app-server thread/archive timed out")
        .expect("real codex app-server thread/archive should succeed");
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
    // Real wire: `PatchChangeKind` is a tagged-enum object
    // (`{"type":"update"}`), NOT a flat string. (No-compat policy: the
    // fixture uses the real shape; the legacy flat-string read was the bug.)
    let n = Notification {
        method: "item/completed".into(),
        params: json!({
            "thread_id": "t-1",
            "item": {
                "id": "i-1",
                "type": "file_change",
                "changes": [{ "path": "/x.rs", "kind": { "type": "update" }, "diff": "" }]
            }
        }),
    };
    let e = translate_notification(&n, "t-1").unwrap();
    match e {
        ThreadEvent::ItemCompleted { item } => match item.details {
            ccteam_harness::ThreadItemDetails::FileChange { path, kind } => {
                assert_eq!(path, PathBuf::from("/x.rs"));
                assert_eq!(kind, "update");
            }
            other => panic!("expected FileChange, got {other:?}"),
        },
        _ => panic!("expected ItemCompleted"),
    }
}

// V0.8 rmux #20 — the three real-wire item-event bugs surfaced after the
// #18 camelCase sweep. Each test feeds the REAL `codex app-server` v2 wire
// shape (camelCase enum status / tagged-enum patch kind / nested
// thread.id) verified against references/codex (commit 76845d716b).

#[test]
fn translate_command_execution_status_camelcase_folds_to_snake() {
    // Bug 1: `CommandExecutionStatus` is a camelCase enum, so the live
    // binary sends `"inProgress"`; it must land in progress.jsonl as the
    // snake_case `in_progress`, not leak the raw camelCase token.
    let n = Notification {
        method: "item/started".into(),
        params: json!({
            "thread_id": "t-1",
            "item": {
                "id": "c-1",
                "type": "command_execution",
                "command": "cargo test",
                "status": "inProgress"
            }
        }),
    };
    let e = translate_notification(&n, "t-1").unwrap();
    match e {
        ThreadEvent::ItemStarted { item } => match item.details {
            ccteam_harness::ThreadItemDetails::CommandExecution { cmd, status } => {
                assert_eq!(cmd, "cargo test");
                assert_eq!(status, "in_progress", "camelCase status must fold to snake");
            }
            other => panic!("expected CommandExecution, got {other:?}"),
        },
        _ => panic!("expected ItemStarted"),
    }
}

#[test]
fn translate_file_change_tagged_kind_add() {
    // Bug 2: `PatchChangeKind` is internally tagged `{"type":"add"}`. The
    // prior `changes[0].kind` string read yielded None → defaulted every
    // patch to "update". With the object read, an add must surface as "add".
    let n = Notification {
        method: "item/completed".into(),
        params: json!({
            "thread_id": "t-1",
            "item": {
                "id": "f-1",
                "type": "file_change",
                "changes": [{ "path": "/new.rs", "kind": { "type": "add" }, "diff": "" }]
            }
        }),
    };
    let e = translate_notification(&n, "t-1").unwrap();
    match e {
        ThreadEvent::ItemCompleted { item } => match item.details {
            ccteam_harness::ThreadItemDetails::FileChange { path, kind } => {
                assert_eq!(path, PathBuf::from("/new.rs"));
                assert_eq!(
                    kind, "add",
                    "tagged-enum kind must be read from the object .type"
                );
            }
            other => panic!("expected FileChange, got {other:?}"),
        },
        _ => panic!("expected ItemCompleted"),
    }
}

#[test]
fn translate_file_change_tagged_kind_update_with_move_is_rename() {
    // Bug 2 (rename): the wire has no `rename` variant — a rename is an
    // `update` carrying a `movePath`. Surface the richer "rename" kind.
    let n = Notification {
        method: "item/completed".into(),
        params: json!({
            "thread_id": "t-1",
            "item": {
                "id": "f-2",
                "type": "file_change",
                "changes": [{
                    "path": "/old.rs",
                    "kind": { "type": "update", "movePath": "/renamed.rs" },
                    "diff": ""
                }]
            }
        }),
    };
    let e = translate_notification(&n, "t-1").unwrap();
    match e {
        ThreadEvent::ItemCompleted { item } => match item.details {
            ccteam_harness::ThreadItemDetails::FileChange { kind, .. } => {
                assert_eq!(kind, "rename", "update + movePath must surface as rename");
            }
            other => panic!("expected FileChange, got {other:?}"),
        },
        _ => panic!("expected ItemCompleted"),
    }
}

#[test]
fn translate_file_change_tagged_kind_delete() {
    let n = Notification {
        method: "item/completed".into(),
        params: json!({
            "thread_id": "t-1",
            "item": {
                "id": "f-3",
                "type": "file_change",
                "changes": [{ "path": "/gone.rs", "kind": { "type": "delete" }, "diff": "" }]
            }
        }),
    };
    let e = translate_notification(&n, "t-1").unwrap();
    match e {
        ThreadEvent::ItemCompleted { item } => match item.details {
            ccteam_harness::ThreadItemDetails::FileChange { kind, .. } => {
                assert_eq!(kind, "delete");
            }
            other => panic!("expected FileChange, got {other:?}"),
        },
        _ => panic!("expected ItemCompleted"),
    }
}

#[test]
fn translate_thread_started_real_wire_nested_id_filters_foreign() {
    // Bug 3: `thread/started`'s only id is nested at `params.thread.id`
    // (`ThreadStartedNotification { thread: Thread }`). A foreign thread's
    // started notification must be filtered out — previously it slipped the
    // top-level-only gate and laundered the foreign id into the wanted slot.
    let foreign = Notification {
        method: "thread/started".into(),
        params: json!({ "thread": { "id": "other", "sessionId": "s-1" } }),
    };
    assert!(
        translate_notification(&foreign, "ours").is_none(),
        "foreign thread/started (nested thread.id) must be filtered out"
    );

    // And the matching thread/started (nested id == wanted) still surfaces
    // with the real id, not a laundered fallback.
    let ours = Notification {
        method: "thread/started".into(),
        params: json!({ "thread": { "id": "ours", "sessionId": "s-1" } }),
    };
    match translate_notification(&ours, "ours").expect("matching thread/started must surface") {
        ThreadEvent::ThreadStarted { thread_id } => assert_eq!(thread_id, "ours"),
        other => panic!("expected ThreadStarted, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn adapter_returns_spawn_failed_when_socket_missing() {
    let bogus = std::env::temp_dir().join("ccteam-wave3-nonexistent.sock");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &bogus);
    let _ = std::fs::remove_file(&bogus);
    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief {
        role: "demo".into(),
    };
    let ctx = SpawnCtx {
        slug: "test".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
        model_id: None,
    };
    let err = adapter.start_thread(&spec, &ctx).await.unwrap_err();
    assert!(matches!(err, HarnessError::SpawnFailed(_)));
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn adapter_sends_initialize_handshake_before_thread_start() {
    // W3b catalog §7.2 defect fix: the adapter MUST send the `initialize`
    // request (with `capabilities.experimentalApi == true`) and the
    // one-way `initialized` notification BEFORE the first `thread/start`.
    // Without it the server keeps experimental_api=false and silently
    // filters turn/plan/updated etc. This test records the exact order of
    // methods the peer receives and asserts the handshake precedes
    // thread/start.
    let sock = unique_socket_path("handshake-order");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let listener = UnixListener::bind(&sock).unwrap();
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel::<Value>(16);
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        loop {
            let mut buf = String::new();
            match reader.read_line(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let req: Value = match serde_json::from_str(buf.trim()) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    // Record every inbound frame (requests + notifications).
                    let _ = seen_tx.send(req.clone()).await;
                    // Only requests (with id) get a reply.
                    if let Some(id) = req.get("id").cloned() {
                        let result = match req["method"].as_str() {
                            Some("initialize") => json!({
                                "user_agent": "codex-test/0.0.0",
                                "codex_home": "/tmp/.codex",
                                "platform_family": "unix",
                                "platform_os": "linux"
                            }),
                            Some("thread/start") => {
                                json!({ "thread": { "thread_id": "tid-77" } })
                            }
                            _ => json!({ "ok": true }),
                        };
                        let resp = json!({ "id": id, "result": result });
                        let mut bytes = serde_json::to_vec(&resp).unwrap();
                        bytes.push(b'\n');
                        let _ = w.write_all(&bytes).await;
                        let _ = w.flush().await;
                    }
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief {
        role: "demo".into(),
    };
    let ctx = SpawnCtx {
        slug: "test".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
        model_id: None,
    };
    let h = adapter.start_thread(&spec, &ctx).await.unwrap();
    assert_eq!(h.identity, "tid-77");

    // Collect the first three frames the peer saw and assert ordering.
    let mut methods: Vec<String> = Vec::new();
    let mut initialize_frame: Option<Value> = None;
    let mut thread_start_frame: Option<Value> = None;
    for _ in 0..3 {
        let frame = tokio::time::timeout(Duration::from_secs(1), seen_rx.recv())
            .await
            .expect("expected a frame from the adapter")
            .unwrap();
        let m = frame["method"].as_str().unwrap_or("").to_string();
        if m == "initialize" {
            initialize_frame = Some(frame.clone());
        } else if m == "thread/start" {
            thread_start_frame = Some(frame.clone());
        }
        methods.push(m);
    }

    assert_eq!(
        methods,
        vec![
            "initialize".to_string(),
            "initialized".to_string(),
            "thread/start".to_string()
        ],
        "handshake must precede thread/start; got {methods:?}"
    );
    let init = initialize_frame.expect("initialize frame must be present");
    assert_eq!(
        init["params"]["capabilities"]["experimentalApi"], true,
        "initialize must negotiate experimentalApi=true to unlock turn/plan/updated"
    );
    assert_eq!(init["params"]["clientInfo"]["name"], "ccteam");
    let thread_start = thread_start_frame.expect("thread/start frame must be present");
    assert_eq!(thread_start["params"]["threadSource"], "user");
    assert_eq!(thread_start["params"]["sessionStartSource"], "startup");
    assert_eq!(thread_start["params"]["serviceName"], "ccteam/test");
    assert_eq!(
        thread_start["params"]["developerInstructions"],
        "ccteam role: demo (slug=test, sid=codex-1)"
    );
    assert!(
        thread_start["params"].get("session_source").is_none(),
        "thread/start must use current Codex v2 camelCase fields"
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn adapter_start_thread_against_scripted_peer() {
    let sock = unique_socket_path("start-thread");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let (peer, _notif) = spawn_scripted_peer(sock.clone(), |req| {
        match req["method"].as_str() {
            // W3b: the adapter now completes the `initialize` handshake on
            // connect before `thread/start`, so the scripted peer must
            // answer it. `initialized` is a one-way notification (no id) —
            // the peer simply receives it and produces no reply (the empty
            // result here is dropped because there's no id to attach).
            Some("initialize") => json!({
                "result": {
                    "user_agent": "codex-test/0.0.0",
                    "codex_home": "/tmp/.codex",
                    "platform_family": "unix",
                    "platform_os": "linux"
                }
            }),
            Some("thread/start") => {
                json!({ "result": { "thread": { "thread_id": "tid-42" } } })
            }
            _ => json!({ "error": { "code": -32601, "message": "unexpected" } }),
        }
    })
    .await;
    // Give the listener a moment.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief {
        role: "demo".into(),
    };
    let ctx = SpawnCtx {
        slug: "test".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
        model_id: None,
    };
    let h = adapter.start_thread(&spec, &ctx).await.unwrap();
    assert_eq!(h.vendor, AgentVendor::Codex);
    assert_eq!(h.mode, ExecutionMode::Chat);
    assert_eq!(h.identity, "tid-42");

    drop(peer); // shutdown peer; adapter no-ops on subsequent calls
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn adapter_maps_system_directives_to_command_rpcs() {
    let sock = unique_socket_path("system-directive-rpc");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let seen = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let seen_for_peer = Arc::clone(&seen);
    let (peer, _notif) = spawn_scripted_peer(sock.clone(), move |req| {
        seen_for_peer.lock().unwrap().push(req.clone());
        match req["method"].as_str() {
            Some("initialize") => json!({
                "result": {
                    "user_agent": "codex-test/0.0.0",
                    "codex_home": "/tmp/.codex",
                    "platform_family": "unix",
                    "platform_os": "linux"
                }
            }),
            Some("thread/start") => json!({
                "result": { "thread": { "id": "tid-command" } }
            }),
            Some("turn/start") => json!({
                "result": { "turn": { "id": "turn-user-1" } }
            }),
            Some("thread/compact/start") => json!({ "result": {} }),
            Some("review/start") => json!({
                "result": {
                    "turn": { "id": "turn-review-1" },
                    "reviewThreadId": "tid-command"
                }
            }),
            other => json!({
                "error": {
                    "code": -32601,
                    "message": format!("unexpected method {other:?}")
                }
            }),
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief {
        role: "demo".into(),
    };
    let ctx = SpawnCtx {
        slug: "test".into(),
        sid: "codex-1".into(),
        cwd: std::env::temp_dir(),
        project_dir: std::env::temp_dir(),
        extra_args: vec![],
        model_id: None,
    };
    let h = adapter.start_thread(&spec, &ctx).await.unwrap();
    let user_turn = adapter
        .submit_turn(&h, TurnInput::UserText("hello".into()))
        .await
        .unwrap();
    assert_eq!(user_turn.0, "turn-user-1");

    let compact_turn = adapter
        .submit_turn(&h, TurnInput::SystemDirective("/compact".into()))
        .await
        .unwrap();
    assert!(compact_turn.0.starts_with("codex-app-server-compact-"));

    let review_turn = adapter
        .submit_turn(&h, TurnInput::SystemDirective("review".into()))
        .await
        .unwrap();
    assert_eq!(review_turn.0, "turn-review-1");

    let frames = seen.lock().unwrap().clone();
    let methods: Vec<&str> = frames.iter().filter_map(|v| v["method"].as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "initialize",
            "initialized",
            "thread/start",
            "turn/start",
            "thread/compact/start",
            "review/start"
        ]
    );
    let turn = frames.iter().find(|v| v["method"] == "turn/start").unwrap();
    assert_eq!(turn["params"]["threadId"], "tid-command");
    assert_eq!(turn["params"]["input"][0]["type"], "text");
    assert!(
        turn["params"].get("thread_id").is_none(),
        "turn/start must use current Codex v2 threadId field"
    );
    let compact = frames
        .iter()
        .find(|v| v["method"] == "thread/compact/start")
        .unwrap();
    assert_eq!(compact["params"]["threadId"], "tid-command");
    let review = frames
        .iter()
        .find(|v| v["method"] == "review/start")
        .unwrap();
    assert_eq!(review["params"]["threadId"], "tid-command");
    assert_eq!(
        review["params"]["target"],
        json!({ "type": "uncommittedChanges" })
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}
