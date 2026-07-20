//! V0.6.0 Wave 3 F112 — `CodexAppServerAdapter` tests over a UDS
//! socket served by a scripted in-process JSON-RPC peer. We dial the
//! peer via `CCTEAM_CODEX_APP_SERVER_SOCKET` env override so the
//! adapter's `client()` connects to the test socket rather than
//! `$CODEX_HOME/app-server-control/app-server-control.sock`.

use ccteam_harness::execution::codex_app_server::{
    resolve_codex_transport, translate_notification, turn_input_to_items, CodexAppServerAdapter,
    CodexTransport, APP_SERVER_SOCKET_ENV,
};
use ccteam_harness::execution::codex_jsonrpc::Notification;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ChoiceSelection, Directive, DirectiveOutcome, ExecutionMode,
    HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent, TurnInput,
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
    // F10: transport is single-axis on CCTEAM_CODEX_APP_SERVER_SOCKET. If
    // the socket override is set (power-user / self-managed daemon) it must
    // exist; otherwise we exercise the default stdio child-spawn path.
    if let Ok(socket) = std::env::var(APP_SERVER_SOCKET_ENV) {
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
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: String::new(),
                remote: None,
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

/// Real end-to-end round-trip proving the chat reply ccteam surfaces is
/// genuinely the Codex MODEL's output — not an echo, a stub, or a Claude
/// process. We drive the production `CodexAppServerAdapter` over the
/// stdio transport (it spawns a real `codex app-server --listen
/// stdio://` child itself), `submit_turn` a prompt whose correct answer
/// requires real inference (17 * 23 = 391, which an echo could never
/// produce), and assert the `ThreadItemDetails::AgentMessage` the
/// adapter parses out of the live JSON-RPC stream contains `391`.
///
/// Gated behind `CCTEAM_REAL_CODEX_REPLY=1` (real model call → needs
/// `codex login` + network + a few tokens of spend), so it no-ops in the
/// normal `cargo test` baseline exactly like the smoke test above.
#[tokio::test]
#[serial]
async fn real_codex_reply_roundtrip_proves_model_output() {
    use ccteam_harness::ThreadItemDetails;
    use futures::StreamExt;

    if std::env::var("CCTEAM_REAL_CODEX_REPLY").ok().as_deref() != Some("1") {
        eprintln!("skip: set CCTEAM_REAL_CODEX_REPLY=1 for the real codex reply round-trip");
        return;
    }

    let tmp = TempDir::new().unwrap();
    // F10: stdio is the DEFAULT transport — with no socket override the
    // adapter spawns codex itself; no external daemon/socket. CCTEAM_HOME
    // keeps any progress-bridge writes in tmp.
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
    std::env::set_var("CCTEAM_HOME", tmp.path());

    let adapter = CodexAppServerAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(30),
        adapter.start_thread(
            &AgentSpecBrief {
                role: "real-reply".to_string(),
            },
            &SpawnCtx {
                slug: "real-codex-reply".to_string(),
                sid: "s-real-reply".to_string(),
                cwd: tmp.path().to_path_buf(),
                project_dir: tmp.path().to_path_buf(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: String::new(),
                remote: None,
            },
        ),
    )
    .await
    .expect("real codex thread/start timed out")
    .expect("real codex thread/start should succeed");
    assert_eq!(handle.vendor, AgentVendor::Codex);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    eprintln!(
        "[real-codex] thread_id={} extras={}",
        handle.identity, handle.raw_extras
    );

    // Subscribe BEFORE submitting so we don't miss the agent-message
    // notifications the server emits while the turn runs.
    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut text = String::new();
        let mut usage_line = String::new();
        // Count which event variants carry the agent message so the test
        // documents codex's dual emission (streaming `ItemUpdated` deltas
        // + a final `ItemCompleted`) — the input the gateway pump must
        // dedupe to avoid a doubled chat reply.
        let mut agent_msg_via_updated = 0u32;
        let mut agent_msg_via_completed = 0u32;
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemUpdated { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        eprintln!("[real-codex] ItemUpdated  AgentMessage={t:?}");
                        agent_msg_via_updated += 1;
                        text.push_str(&t);
                    }
                }
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        eprintln!("[real-codex] ItemCompleted AgentMessage={t:?}");
                        agent_msg_via_completed += 1;
                        text.push_str(&t);
                    }
                }
                ThreadEvent::ItemStarted { .. } => {}
                ThreadEvent::TurnCompleted { turn_id, usage, .. } => {
                    usage_line = format!(
                        "turn={turn_id} usage={usage:?} agentMessage via ItemUpdated={agent_msg_via_updated} via ItemCompleted={agent_msg_via_completed}"
                    );
                    break;
                }
                ThreadEvent::TurnFailed { turn_id, err } => {
                    return Err(format!("turn {turn_id} failed: {err:?}"));
                }
                ThreadEvent::Error(e) => return Err(format!("stream error: {e:?}")),
                _ => {}
            }
        }
        Ok((text, usage_line))
    });
    // Let the collector run its first poll (broadcast subscribe) before
    // the turn — model latency makes 500ms a comfortable margin.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let turn = adapter
        .submit_turn(
            &handle,
            TurnInput::UserText(
                "Compute 17 * 23 and reply with ONLY the integer result, no words.".to_string(),
            ),
        )
        .await
        .expect("real codex submit_turn should succeed");
    eprintln!("[real-codex] submitted turn_id={}", turn.0);

    let (reply, usage_line) = tokio::time::timeout(Duration::from_secs(120), collector)
        .await
        .expect("real codex reply timed out")
        .expect("collector task panicked")
        .expect("real codex turn should not fail");
    eprintln!("[real-codex] REPLY = {reply:?}");
    eprintln!("[real-codex] {usage_line}");

    assert!(
        reply.contains("391"),
        "codex MODEL reply must contain the computed product 391 \
         (an echo/stub/Claude-mislabel could not produce it); got: {reply:?}"
    );

    let _ = tokio::time::timeout(Duration::from_secs(10), adapter.close_thread(&handle)).await;
    std::env::remove_var("CCTEAM_HOME");
}

/// Restore an env var to its prior value (or remove it if it was unset).
fn restore_env(key: &str, prior: Option<std::ffi::OsString>) {
    match prior {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

/// F10 (arch §8-1): `resolve_codex_transport` is pure + single-axis.
/// No socket env ⇒ `Stdio`; `CCTEAM_CODEX_APP_SERVER_SOCKET` set ⇒
/// `Socket`. Env-mutating ⇒ serial + restore.
#[test]
#[serial]
fn resolve_codex_transport_single_axis() {
    let prior_sock = std::env::var_os(APP_SERVER_SOCKET_ENV);
    let prior_bin = std::env::var_os("CCTEAM_CODEX_BIN");

    // No socket env ⇒ Stdio (program = CCTEAM_CODEX_BIN | "codex").
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
    std::env::remove_var("CCTEAM_CODEX_BIN");
    assert_eq!(
        resolve_codex_transport(),
        CodexTransport::Stdio {
            program: "codex".to_string()
        },
        "no socket env ⇒ default Stdio with `codex`"
    );

    // CCTEAM_CODEX_BIN overrides the stdio program.
    std::env::set_var("CCTEAM_CODEX_BIN", "/custom/codex");
    assert_eq!(
        resolve_codex_transport(),
        CodexTransport::Stdio {
            program: "/custom/codex".to_string()
        },
        "CCTEAM_CODEX_BIN must set the stdio program"
    );

    // Socket env set ⇒ Socket (overrides even when CCTEAM_CODEX_BIN is set).
    std::env::set_var(APP_SERVER_SOCKET_ENV, "/tmp/ccteam-f10-resolve.sock");
    assert_eq!(
        resolve_codex_transport(),
        CodexTransport::Socket {
            path: PathBuf::from("/tmp/ccteam-f10-resolve.sock")
        },
        "socket env ⇒ Socket override"
    );

    restore_env(APP_SERVER_SOCKET_ENV, prior_sock);
    restore_env("CCTEAM_CODEX_BIN", prior_bin);
}

/// F10 (arch §8-3): `ThreadHandle.raw_extras.transport` reports the
/// RESOLVED transport tag — `"stdio"` with no socket env (default
/// child-spawn), driven against a scripted stdio app-server is overkill
/// here, so we assert the tag via a `start_thread` over a UDS override is
/// `"socket"`, and the no-socket default resolves to `"stdio"` via the
/// pure resolver. The end-to-end `"stdio"` extras tag is additionally
/// proven by `real_codex_reply_roundtrip_proves_model_output`'s eprintln.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn raw_extras_transport_is_resolved_tag() {
    // Default (no socket env) ⇒ resolver yields Stdio ⇒ tag would be "stdio".
    let prior_sock = std::env::var_os(APP_SERVER_SOCKET_ENV);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
    assert!(
        matches!(resolve_codex_transport(), CodexTransport::Stdio { .. }),
        "no socket env ⇒ Stdio ⇒ raw_extras.transport would be \"stdio\""
    );

    // Socket override ⇒ start_thread over a scripted UDS peer ⇒ tag "socket".
    let sock = unique_socket_path("raw-extras-transport");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);
    let (peer, _notif) = spawn_scripted_peer(sock.clone(), |req| match req["method"].as_str() {
        Some("initialize") => json!({
            "result": {
                "user_agent": "codex-test/0.0.0",
                "codex_home": "/tmp/.codex",
                "platform_family": "unix",
                "platform_os": "linux"
            }
        }),
        Some("thread/start") => json!({ "result": { "thread": { "thread_id": "tid-extras" } } }),
        _ => json!({ "error": { "code": -32601, "message": "unexpected" } }),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let h = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "demo".into(),
            },
            &SpawnCtx {
                slug: "test".into(),
                sid: "codex-1".into(),
                cwd: std::env::temp_dir(),
                project_dir: std::env::temp_dir(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: String::new(),
                remote: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        h.raw_extras["transport"], "socket",
        "socket override ⇒ raw_extras.transport must be the resolved \"socket\" tag"
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    restore_env(APP_SERVER_SOCKET_ENV, prior_sock);
}

/// Deterministic resume-before-turn precondition (the codex "thread not found"
/// fix): `submit_turn` must `ensure_thread_loaded` first — when the thread is
/// NOT loaded on the current connection (post-reconnect / restored-session /
/// the bug's evicted-thread shape) it issues `thread/resume` BEFORE
/// `turn/start`, so `turn/start` can never hit `thread not found`. And it
/// resumes at most once per connection epoch (a thread already loaded skips
/// straight to the turn).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn submit_turn_resumes_unloaded_thread_before_turn_start() {
    let prior_sock = std::env::var_os(APP_SERVER_SOCKET_ENV);
    let sock = unique_socket_path("ensure-loaded-resume");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    // Record every id-bearing RPC method the peer sees (skip notifications).
    let methods: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let methods_h = Arc::clone(&methods);
    let (peer, _notif) = spawn_scripted_peer(sock.clone(), move |req| {
        let m = req["method"].as_str().unwrap_or("").to_string();
        if req.get("id").is_some() {
            methods_h.lock().unwrap().push(m.clone());
        }
        match m.as_str() {
            "initialize" => json!({ "result": {
                "user_agent": "t/0", "codex_home": "/tmp/.codex",
                "platform_family": "unix", "platform_os": "linux" } }),
            "thread/start" => json!({ "result": { "thread": { "thread_id": "t-ensure" } } }),
            "thread/resume" => json!({ "result": { "thread": { "thread_id": "t-ensure" } } }),
            "turn/start" => json!({ "result": { "turn": { "id": "turn-ok" } } }),
            _ => json!({ "error": { "code": -32601, "message": "unexpected" } }),
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let h = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "demo".into(),
            },
            &SpawnCtx {
                slug: "ensure-loaded".into(),
                sid: "codex-el".into(),
                cwd: std::env::temp_dir(),
                project_dir: std::env::temp_dir(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: String::new(),
                remote: None,
            },
        )
        .await
        .unwrap();

    // start_thread marked the thread loaded → the first turn goes straight to
    // turn/start with NO resume.
    let t1 = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .expect("first submit_turn succeeds");
    assert_eq!(t1.0, "turn-ok");
    assert_eq!(
        methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "thread/resume")
            .count(),
        0,
        "a freshly-started (loaded) thread must NOT be resumed: {:?}",
        methods.lock().unwrap()
    );

    // Simulate the thread leaving the app-server's memory (connection replaced
    // / idle eviction): clear the loaded set. The NEXT turn must resume first.
    adapter.forget_loaded_for_test().await;
    let t2 = adapter
        .submit_turn(&h, TurnInput::UserText("again".into()))
        .await
        .expect("submit_turn must resume-then-start, never surface thread-not-found");
    assert_eq!(t2.0, "turn-ok");

    let seen = methods.lock().unwrap().clone();
    let resume_idx = seen.iter().rposition(|m| m == "thread/resume");
    let last_turn_idx = seen.iter().rposition(|m| m == "turn/start");
    assert!(
        resume_idx.is_some(),
        "an unloaded thread must be resumed before the turn: {seen:?}"
    );
    assert!(
        resume_idx < last_turn_idx,
        "thread/resume must precede the turn/start it enables: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|m| *m == "thread/resume").count(),
        1,
        "resume is once-per-connection-epoch, not a per-turn fallback: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|m| *m == "turn/start").count(),
        2,
        "both turns must have reached turn/start: {seen:?}"
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    restore_env(APP_SERVER_SOCKET_ENV, prior_sock);
}

/// F10 W3 step-0 GATE — real `codex` binary, DEFAULT (stdio) transport.
/// Constructs a `CodexAppServerAdapter` with NO socket env, so it must
/// spawn `codex app-server --listen stdio://` itself and complete the
/// JSON-RPC handshake + `thread/start` (the `/new codex` path). `#[ignore]`d
/// so CI stays hermetic — run explicitly:
///   `cargo test -p ccteam-harness --test codex_app_server_test \
///        f10_real_codex_stdio_new_smoke -- --ignored --nocapture`
#[tokio::test(flavor = "current_thread")]
#[serial]
#[ignore = "real codex binary + login; W3 step-0 gate, run with --ignored"]
async fn f10_real_codex_stdio_new_smoke() {
    let tmp = TempDir::new().unwrap();
    let prior_sock = std::env::var_os(APP_SERVER_SOCKET_ENV);
    let prior_home = std::env::var_os("CCTEAM_HOME");
    // DEFAULT path: no socket env ⇒ stdio child-spawn.
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
    std::env::set_var("CCTEAM_HOME", tmp.path());

    let adapter = CodexAppServerAdapter::new();
    assert!(
        matches!(adapter.transport(), CodexTransport::Stdio { .. }),
        "default adapter must resolve to stdio transport"
    );

    let res = tokio::time::timeout(
        Duration::from_secs(30),
        adapter.start_thread(
            &AgentSpecBrief {
                role: "f10-stdio-smoke".to_string(),
            },
            &SpawnCtx {
                slug: "f10-stdio-smoke".to_string(),
                sid: "s-f10".to_string(),
                cwd: tmp.path().to_path_buf(),
                project_dir: tmp.path().to_path_buf(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: String::new(),
                remote: None,
            },
        ),
    )
    .await
    .expect("stdio thread/start timed out");

    restore_env(APP_SERVER_SOCKET_ENV, prior_sock);
    restore_env("CCTEAM_HOME", prior_home);

    let handle = res.expect("stdio /new: handshake + thread/start must succeed");
    eprintln!(
        "[f10-stdio-smoke] PASS thread_id={} extras={}",
        handle.identity, handle.raw_extras
    );
    assert_eq!(handle.vendor, AgentVendor::Codex);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    assert_eq!(
        handle.raw_extras["transport"], "stdio",
        "default /new path must report the resolved stdio transport"
    );
    assert!(!handle.identity.trim().is_empty());
    let _ = tokio::time::timeout(Duration::from_secs(10), adapter.close_thread(&handle)).await;
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
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
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
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
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
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
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
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    };
    let h = adapter.start_thread(&spec, &ctx).await.unwrap();
    let user_turn = adapter
        .submit_turn(&h, TurnInput::UserText("hello".into()))
        .await
        .unwrap();
    assert_eq!(user_turn.0, "turn-user-1");

    let compact_turn = match adapter
        .handle_directive(
            &h,
            Directive {
                name: "compact".to_string(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap()
    {
        DirectiveOutcome::Turn(id) => id,
        other => panic!("expected DirectiveOutcome::Turn for /compact, got {other:?}"),
    };
    assert!(compact_turn.0.starts_with("codex-app-server-compact-"));

    // D4: bare /review now → NeedsChoice; picking "uncommitted" (a choice
    // re-entry) is what fires review/start { uncommittedChanges } → Turn.
    let review_turn = match adapter
        .handle_directive(
            &h,
            Directive {
                name: "review".to_string(),
                args: String::new(),
                choice: Some(ChoiceSelection {
                    token: "cx-test".to_string(),
                    ids: vec!["uncommitted".to_string()],
                    free_text: None,
                }),
            },
        )
        .await
        .unwrap()
    {
        DirectiveOutcome::Turn(id) => id,
        other => panic!("expected DirectiveOutcome::Turn for review, got {other:?}"),
    };
    assert_eq!(review_turn.0, "turn-review-1");

    let frames = seen.lock().unwrap().clone();
    let methods: Vec<&str> = frames.iter().filter_map(|v| v["method"].as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "initialize",
            "initialized",
            "thread/start",
            "model/list",
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

// =====================================================================
// v0.8.5 W3 D2 — full Codex command surface (handle_directive).
//
// A scripted peer with one arm per RPC records every inbound frame so
// each test can assert handle_directive sends the EXACT method + params
// anchored to codex b2344d8.
// =====================================================================

/// The full D2 scripted-peer handler: every RPC handle_directive can send
/// gets a canned response. Records nothing itself — pair with a `seen`
/// capture (see `d2_start`).
fn d2_response(req: &Value) -> Value {
    match req["method"].as_str() {
        Some("initialize") => json!({ "result": {
            "user_agent": "codex-test/0.0.0", "codex_home": "/tmp/.codex",
            "platform_family": "unix", "platform_os": "linux"
        }}),
        Some("thread/start") => json!({ "result": { "thread": { "id": "tid-d2" } } }),
        Some("turn/start") => json!({ "result": { "turn": { "id": "turn-d2" } } }),
        Some("turn/steer") => json!({ "result": { "turn": { "id": "turn-steer" } } }),
        Some("turn/interrupt") => json!({ "result": {} }),
        Some("thread/compact/start") => json!({ "result": {} }),
        Some("review/start") => json!({ "result": {
            "turn": { "id": "turn-review" }, "reviewThreadId": "tid-d2"
        }}),
        Some("thread/fork") => json!({ "result": {
            "thread": { "id": "tid-forked" }, "model": "gpt-x", "modelProvider": "openai",
            "serviceTier": null, "cwd": "/tmp", "approvalPolicy": "onRequest",
            "approvalsReviewer": "model", "sandbox": { "mode": "workspace-write" }
        }}),
        Some("thread/rollback") => json!({ "result": { "thread": { "id": "tid-d2" } } }),
        Some("thread/name/set") => json!({ "result": {} }),
        Some("thread/goal/set") => json!({ "result": { "goal": {
            "threadId": "tid-d2", "objective": "ship", "status": "active",
            "tokenBudget": null, "tokensUsed": 0, "timeUsedSeconds": 0,
            "createdAt": 0, "updatedAt": 0
        }}}),
        Some("thread/goal/get") => json!({ "result": { "goal": {
            "threadId": "tid-d2", "objective": "current goal", "status": "active",
            "tokenBudget": null, "tokensUsed": 0, "timeUsedSeconds": 0,
            "createdAt": 0, "updatedAt": 0
        }}}),
        Some("thread/goal/clear") => json!({ "result": { "cleared": true } }),
        Some("thread/backgroundTerminals/clean") => json!({ "result": {} }),
        Some("thread/memoryMode/set") => json!({ "result": {} }),
        Some("command/exec") => json!({ "result": { "stdout": "diff --git a/x", "exitCode": 0 } }),
        Some("account/login/start") => json!({ "result": {} }),
        Some("account/logout") => json!({ "result": {} }),
        Some("model/list") => json!({ "result": { "data": [
            { "id": "gpt-5", "supportedReasoningEfforts": [
                { "reasoningEffort": "low", "description": "" },
                { "reasoningEffort": "high", "description": "" }
            ]}
        ], "nextCursor": null }}),
        Some("skills/list") => json!({ "result": { "data": [
            { "cwd": "/repo", "skills": [
                { "name": "deploy", "path": "/repo/.agents/skills/deploy", "enabled": true }
            ], "errors": [] }
        ]}}),
        Some("mcpServerStatus/list") => json!({ "result": { "data": [ {}, {} ] } }),
        Some("hooks/list") => json!({ "result": { "data": [ {} ] } }),
        Some("app/list") => json!({ "result": { "data": [] } }),
        // D4 list sources.
        Some("collaborationMode/list") => json!({ "result": { "data": [
            { "name": "Plan", "mode": "plan", "model": null, "reasoning_effort": null },
            { "name": "Default", "mode": "default", "model": null, "reasoning_effort": null }
        ]}}),
        Some("thread/list") => json!({ "result": { "data": [
            { "id": "tid-old-1", "sessionId": "s1", "forkedFromId": null,
              "parentThreadId": null, "preview": "earlier chat about auth",
              "ephemeral": false, "modelProvider": "openai", "createdAt": 0,
              "updatedAt": 0, "status": { "type": "idle" }, "path": null,
              "cwd": "/tmp", "cliVersion": "0", "source": "appServer",
              "name": "Auth work", "turns": [] }
        ], "nextCursor": null, "backwardsCursor": null }}),
        other => json!({ "error": { "code": -32601, "message": format!("unexpected {other:?}") } }),
    }
}

/// Start a codex thread against a recording D2 peer. Returns the adapter,
/// the handle, the shared `seen` frame log, the peer task, and the socket
/// path. Caller drops the peer + removes the socket + restores the env.
async fn d2_start(
    tag: &str,
) -> (
    CodexAppServerAdapter,
    ccteam_harness::ThreadHandle,
    Arc<StdMutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let (adapter, h, seen, peer, _notif, sock) = d2_start_with_notif(tag).await;
    (adapter, h, seen, peer, sock)
}

/// Variant of `d2_start` that also returns the notification sender so a
/// test can push server→client notifications into the dispatcher.
async fn d2_start_with_notif(
    tag: &str,
) -> (
    CodexAppServerAdapter,
    ccteam_harness::ThreadHandle,
    Arc<StdMutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Sender<Value>,
    PathBuf,
) {
    let sock = unique_socket_path(tag);
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);
    let seen = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let seen_for_peer = Arc::clone(&seen);
    let (peer, notif) = spawn_scripted_peer(sock.clone(), move |req| {
        seen_for_peer.lock().unwrap().push(req.clone());
        d2_response(req)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let adapter = CodexAppServerAdapter::new();
    let h = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "demo".into(),
            },
            &SpawnCtx {
                slug: "test".into(),
                sid: "codex-1".into(),
                cwd: std::env::temp_dir(),
                project_dir: std::env::temp_dir(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: String::new(),
                remote: None,
            },
        )
        .await
        .unwrap();
    (adapter, h, seen, peer, notif, sock)
}

fn dir(name: &str, args: &str) -> Directive {
    Directive {
        name: name.to_string(),
        args: args.to_string(),
        choice: None,
    }
}

fn find_frame<'a>(frames: &'a [Value], method: &str) -> Option<&'a Value> {
    frames.iter().find(|v| v["method"] == method)
}

/// Poll `cond` until true (≤2s) so dispatcher-driven tracker updates settle
/// without a fixed sleep.
async fn wait_until<F, Fut>(mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within 2s");
}

/// D2.1 — RPC direct-map class: compact / interrupt / fork / rollback /
/// rename / goal / stop / memories / diff / init / login / logout each send
/// the exact b2344d8 method + params.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_rpc_direct_map_methods_and_params() {
    let (adapter, h, seen, peer, sock) = d2_start("d2-rpc-map").await;

    // /compact → thread/compact/start { threadId } (common.rs:541) → Turn.
    let out = adapter
        .handle_directive(&h, dir("compact", ""))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Turn(_)));

    // /rename <name> → thread/name/set { threadId, name } (thread.rs:660).
    let out = adapter
        .handle_directive(&h, dir("rename", "my thread"))
        .await
        .unwrap();
    assert!(
        matches!(out, DirectiveOutcome::Done { .. }),
        "rename → Done"
    );

    // /rollback 2 → thread/rollback { threadId, numTurns } (thread.rs:938).
    let out = adapter
        .handle_directive(&h, dir("rollback", "2"))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Done { .. }));

    // /goal <obj> → thread/goal/set { threadId, objective } (common.rs:497).
    let _ = adapter
        .handle_directive(&h, dir("goal", "ship v1"))
        .await
        .unwrap();
    // /goal (no args) → thread/goal/get (common.rs:502).
    let out = adapter.handle_directive(&h, dir("goal", "")).await.unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => assert!(receipt.contains("current goal")),
        other => panic!("expected Done for /goal get, got {other:?}"),
    }
    // /goal clear → thread/goal/clear (common.rs:507).
    let _ = adapter
        .handle_directive(&h, dir("goal", "clear"))
        .await
        .unwrap();

    // /stop → thread/backgroundTerminals/clean (common.rs:556).
    let _ = adapter.handle_directive(&h, dir("stop", "")).await.unwrap();

    // /memories on → thread/memoryMode/set { mode: "enabled" } (common.rs:524).
    let _ = adapter
        .handle_directive(&h, dir("memories", "on"))
        .await
        .unwrap();

    // /diff → command/exec { command: ["git","diff"] } (common.rs:965).
    let out = adapter.handle_directive(&h, dir("diff", "")).await.unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => assert!(receipt.contains("diff --git")),
        other => panic!("expected Done for /diff, got {other:?}"),
    }

    // /init → turn/start fixed prompt (common.rs:756) → Turn.
    let out = adapter.handle_directive(&h, dir("init", "")).await.unwrap();
    assert!(matches!(out, DirectiveOutcome::Turn(_)));

    // /login → account/login/start; /logout → account/logout (common.rs:911/931).
    let _ = adapter
        .handle_directive(&h, dir("login", ""))
        .await
        .unwrap();
    let _ = adapter
        .handle_directive(&h, dir("logout", ""))
        .await
        .unwrap();

    let frames = seen.lock().unwrap().clone();
    // Exact params per the b2344d8 wire.
    assert_eq!(
        find_frame(&frames, "thread/compact/start").unwrap()["params"]["threadId"],
        "tid-d2"
    );
    assert_eq!(
        find_frame(&frames, "thread/name/set").unwrap()["params"],
        json!({ "threadId": "tid-d2", "name": "my thread" })
    );
    assert_eq!(
        find_frame(&frames, "thread/rollback").unwrap()["params"],
        json!({ "threadId": "tid-d2", "numTurns": 2 })
    );
    assert_eq!(
        find_frame(&frames, "thread/goal/set").unwrap()["params"],
        json!({ "threadId": "tid-d2", "objective": "ship v1" })
    );
    assert!(find_frame(&frames, "thread/goal/get").is_some());
    assert!(find_frame(&frames, "thread/goal/clear").is_some());
    assert_eq!(
        find_frame(&frames, "thread/backgroundTerminals/clean").unwrap()["params"]["threadId"],
        "tid-d2"
    );
    assert_eq!(
        find_frame(&frames, "thread/memoryMode/set").unwrap()["params"],
        json!({ "threadId": "tid-d2", "mode": "enabled" })
    );
    assert_eq!(
        find_frame(&frames, "command/exec").unwrap()["params"],
        json!({ "command": ["git", "diff"] })
    );
    assert_eq!(
        find_frame(&frames, "account/login/start").unwrap()["params"],
        json!({})
    );
    assert!(find_frame(&frames, "account/logout").is_some());

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2.1 — /fork sends thread/fork and surfaces the new thread id from the
/// response (`thread.id`, thread.rs:553) for the gateway to register.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_fork_surfaces_new_thread_id() {
    let (adapter, h, seen, peer, sock) = d2_start("d2-fork").await;
    let out = adapter.handle_directive(&h, dir("fork", "")).await.unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("tid-forked"),
                "fork receipt must carry new id: {receipt}"
            );
        }
        other => panic!("expected Done for /fork, got {other:?}"),
    }
    let frames = seen.lock().unwrap().clone();
    assert_eq!(
        find_frame(&frames, "thread/fork").unwrap()["params"]["threadId"],
        "tid-d2"
    );
    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2.1 — /review WITH ARGS maps the three keyword ReviewTarget variants
/// directly (review.rs:43-65). (Bare /review is D4 NeedsChoice; the
/// uncommitted pick is covered by
/// d4_review_bare_needschoice_with_2nd_hop_and_args_unchanged.)
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_review_all_four_targets() {
    let (adapter, h, seen, peer, sock) = d2_start("d2-review").await;

    for (args, expected) in [
        (
            "branch main",
            json!({ "type": "baseBranch", "branch": "main" }),
        ),
        (
            "commit abc123",
            json!({ "type": "commit", "sha": "abc123", "title": null }),
        ),
        (
            "focus on auth",
            json!({ "type": "custom", "instructions": "focus on auth" }),
        ),
    ] {
        seen.lock().unwrap().clear();
        let out = adapter
            .handle_directive(&h, dir("review", args))
            .await
            .unwrap();
        assert!(
            matches!(out, DirectiveOutcome::Turn(_)),
            "review {args:?} → Turn"
        );
        let frames = seen.lock().unwrap().clone();
        let review = find_frame(&frames, "review/start").unwrap();
        assert_eq!(review["params"]["threadId"], "tid-d2");
        assert_eq!(review["params"]["target"], expected, "target for {args:?}");
    }

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2.1 — query-synth class returns Done{receipt} from the right RPC:
/// /status (tracker, no RPC), /model (model/list), /skills (skills/list),
/// /mcp, /hooks, /apps.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_query_synth_class() {
    let (adapter, h, seen, peer, sock) = d2_start("d2-query").await;

    // /status reads the tracker (model seeded below) — sends NO RPC.
    adapter
        .tracker_seed_model_for_test(&h.identity, Some("gpt-5".into()))
        .await;
    let out = adapter
        .handle_directive(&h, dir("status", ""))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => assert!(receipt.contains("gpt-5")),
        other => panic!("expected Done for /status, got {other:?}"),
    }

    // /model (bare) → model/list → D4 NeedsChoice (one option per model+effort).
    // (D4 SUPERSEDES the D2 text receipt for the bare case — see d4_* tests.)
    let out = adapter
        .handle_directive(&h, dir("model", ""))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert!(prompt.options.iter().any(|o| o.id == "gpt-5 low"));
            assert!(prompt.options.iter().any(|o| o.id == "gpt-5 high"));
        }
        other => panic!("expected NeedsChoice for bare /model, got {other:?}"),
    }

    // /skills (bare) → skills/list → D4 NeedsChoice (pick to view detail).
    let out = adapter
        .handle_directive(&h, dir("skills", ""))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert!(prompt.options.iter().any(|o| o.id == "deploy"));
        }
        other => panic!("expected NeedsChoice for bare /skills, got {other:?}"),
    }

    // /mcp /hooks /apps → count receipts.
    let _ = adapter.handle_directive(&h, dir("mcp", "")).await.unwrap();
    let _ = adapter
        .handle_directive(&h, dir("hooks", ""))
        .await
        .unwrap();
    let _ = adapter.handle_directive(&h, dir("apps", "")).await.unwrap();

    let frames = seen.lock().unwrap().clone();
    // /status sent no RPC.
    assert!(find_frame(&frames, "thread/read").is_none());
    assert!(find_frame(&frames, "model/list").is_some());
    assert!(find_frame(&frames, "skills/list").is_some());
    assert!(find_frame(&frames, "mcpServerStatus/list").is_some());
    assert!(find_frame(&frames, "hooks/list").is_some());
    assert!(find_frame(&frames, "app/list").is_some());

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2.1 — per-session override class: /model <id> [effort], /personality,
/// /collab, /permissions store overrides (Done, no immediate RPC) and the
/// NEXT turn/start carries them.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_override_class_applies_on_next_turn() {
    let (adapter, h, seen, peer, sock) = d2_start("d2-override").await;

    // /model gpt-5 high → override (no RPC yet).
    let out = adapter
        .handle_directive(&h, dir("model", "gpt-5 high"))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Done { .. }));
    // /personality friendly, /collab plan, /permissions read-only.
    let _ = adapter
        .handle_directive(&h, dir("personality", "friendly"))
        .await
        .unwrap();
    let _ = adapter
        .handle_directive(&h, dir("collab", "plan"))
        .await
        .unwrap();
    let _ = adapter
        .handle_directive(&h, dir("permissions", "read-only"))
        .await
        .unwrap();

    // The override map holds them all.
    let ov = adapter.override_for_test(&h.identity).await;
    assert_eq!(ov.model.as_deref(), Some("gpt-5"));
    assert_eq!(ov.effort.as_deref(), Some("high"));
    assert_eq!(ov.personality.as_deref(), Some("friendly"));
    // collaboration mode is stored as the bare ModeKind; the full object is
    // built at apply time (settings.model required).
    assert_eq!(ov.collaboration_mode.as_deref(), Some("plan"));
    assert_eq!(ov.approval_policy.as_deref(), Some("on-request"));
    // SandboxPolicy is an internally-tagged object, not a bare string.
    assert_eq!(ov.sandbox_policy, Some(json!({ "type": "readOnly" })));

    // No turn/start was sent by the override directives themselves.
    assert!(find_frame(&seen.lock().unwrap(), "turn/start").is_none());

    // Now a plain turn carries the overrides.
    seen.lock().unwrap().clear();
    let _ = adapter
        .submit_turn(&h, TurnInput::UserText("go".into()))
        .await
        .unwrap();
    let frames = seen.lock().unwrap().clone();
    let ts = find_frame(&frames, "turn/start").expect("plain turn → turn/start");
    assert_eq!(ts["params"]["model"], "gpt-5");
    assert_eq!(ts["params"]["effort"], "high");
    assert_eq!(ts["params"]["personality"], "friendly");
    // CollaborationMode = { mode, settings: { model } }; settings.model is
    // resolved from the override model (gpt-5).
    assert_eq!(
        ts["params"]["collaborationMode"],
        json!({ "mode": "plan", "settings": { "model": "gpt-5" } })
    );
    assert_eq!(ts["params"]["approvalPolicy"], "on-request");
    assert_eq!(ts["params"]["sandboxPolicy"], json!({ "type": "readOnly" }));

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2.2 — /interrupt sends turn/interrupt { threadId, turnId } using the
/// active turn id the tracker holds (turn.rs:188).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_interrupt_uses_active_turn_from_tracker() {
    let (adapter, h, seen, peer, notif, sock) = d2_start_with_notif("d2-interrupt").await;

    // No active turn → Done "nothing to interrupt", no RPC.
    let out = adapter
        .handle_directive(&h, dir("interrupt", ""))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => assert!(receipt.contains("no active turn")),
        other => panic!("expected Done, got {other:?}"),
    }
    assert!(find_frame(&seen.lock().unwrap(), "turn/interrupt").is_none());

    // Drive turn/started into the tracker via the dispatcher.
    notif
        .send(json!({
            "method": "turn/started",
            "params": { "threadId": "tid-d2", "turn": { "id": "turn-live" } }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .and_then(|t| t.active_turn)
                == Some("turn-live".to_string())
        }
    })
    .await;

    seen.lock().unwrap().clear();
    let _ = adapter
        .handle_directive(&h, dir("interrupt", ""))
        .await
        .unwrap();
    let frames = seen.lock().unwrap().clone();
    assert_eq!(
        find_frame(&frames, "turn/interrupt").unwrap()["params"],
        json!({ "threadId": "tid-d2", "turnId": "turn-live" })
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// v0.8.19 — the `interrupt_turn` TRAIT method (the gateway `/interrupt` path,
/// distinct from the `/interrupt` directive) also sends `turn/interrupt`
/// { threadId, turnId } using the active turn the tracker holds. With NO active
/// turn it's a clean no-op (no RPC) — so a gateway `/interrupt` on an idle codex
/// session is harmless. The thread is NOT closed either way (no thread/archive).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn interrupt_turn_method_uses_active_turn_and_noops_when_idle() {
    let (adapter, h, seen, peer, notif, sock) = d2_start_with_notif("interrupt-method").await;

    // No active turn → Ok, no RPC frame.
    adapter
        .interrupt_turn(&h)
        .await
        .expect("idle interrupt is a no-op success");
    assert!(find_frame(&seen.lock().unwrap(), "turn/interrupt").is_none());
    assert!(
        find_frame(&seen.lock().unwrap(), "thread/archive").is_none(),
        "interrupt must NOT close the thread"
    );

    // Drive turn/started into the tracker, then interrupt → turn/interrupt RPC.
    notif
        .send(json!({
            "method": "turn/started",
            "params": { "threadId": "tid-d2", "turn": { "id": "turn-live" } }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .and_then(|t| t.active_turn)
                == Some("turn-live".to_string())
        }
    })
    .await;

    seen.lock().unwrap().clear();
    adapter.interrupt_turn(&h).await.unwrap();
    let frames = seen.lock().unwrap().clone();
    assert_eq!(
        find_frame(&frames, "turn/interrupt").unwrap()["params"],
        json!({ "threadId": "tid-d2", "turnId": "turn-live" })
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2.2 — a plain UserText with an active turn goes via turn/steer
/// { expectedTurnId } instead of turn/start (turn.rs:160).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_active_turn_steers_plain_text() {
    let (adapter, h, seen, peer, notif, sock) = d2_start_with_notif("d2-steer").await;

    notif
        .send(json!({
            "method": "turn/started",
            "params": { "threadId": "tid-d2", "turn": { "id": "turn-live" } }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .and_then(|t| t.active_turn)
                .is_some()
        }
    })
    .await;

    seen.lock().unwrap().clear();
    let _ = adapter
        .submit_turn(&h, TurnInput::UserText("more context".into()))
        .await
        .unwrap();
    let frames = seen.lock().unwrap().clone();
    assert!(
        find_frame(&frames, "turn/start").is_none(),
        "must steer, not start"
    );
    let steer = find_frame(&frames, "turn/steer").expect("active turn → turn/steer");
    assert_eq!(steer["params"]["threadId"], "tid-d2");
    assert_eq!(steer["params"]["expectedTurnId"], "turn-live");
    assert_eq!(steer["params"]["input"][0]["type"], "text");

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2 — skill resolution: a name that misses the builtin table but matches
/// skills/list → turn/start with a Skill input (turn.rs:290).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_skill_resolution_name_hit() {
    let (adapter, h, seen, peer, sock) = d2_start("d2-skill-hit").await;

    // case-insensitive: "/Deploy" matches the "deploy" skill from skills/list.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir("Deploy", "prod now"))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Turn(_)), "skill → Turn");
    let frames = seen.lock().unwrap().clone();
    // skills/list was consulted, then turn/start with a Skill + the args Text.
    assert!(find_frame(&frames, "skills/list").is_some());
    let ts = find_frame(&frames, "turn/start").expect("skill → turn/start");
    assert_eq!(ts["params"]["input"][0]["type"], "skill");
    assert_eq!(ts["params"]["input"][0]["name"], "deploy");
    assert_eq!(
        ts["params"]["input"][0]["path"],
        "/repo/.agents/skills/deploy"
    );
    assert_eq!(ts["params"]["input"][1]["type"], "text");
    assert_eq!(ts["params"]["input"][1]["text"], "prod now");

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2 — a miss in both builtin + skills → Rejected with nearest candidates
/// + /skills hint.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_miss_rejected_with_candidates() {
    let (adapter, h, _seen, peer, sock) = d2_start("d2-miss").await;
    // "deplyo" is a typo of the "deploy" skill (shared prefix 'd').
    let out = adapter
        .handle_directive(&h, dir("deplyo", ""))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Rejected { reason } => {
            assert!(reason.contains("/deplyo"));
            assert!(
                reason.contains("/deploy"),
                "should suggest /deploy: {reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2 — TUI-only commands are Rejected (never blind-sent); /new /clear
/// /resume are Redirect.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d2_tui_only_rejected_and_redirects() {
    let (adapter, h, _seen, peer, sock) = d2_start("d2-tui").await;
    for name in ["theme", "vim", "quit", "ide", "statusline"] {
        let out = adapter.handle_directive(&h, dir(name, "")).await.unwrap();
        assert!(
            matches!(out, DirectiveOutcome::Rejected { .. }),
            "/{name} must be Rejected (TUI-only)"
        );
    }
    // /new + /clear are pure Redirect; /resume is now D4 (bare → NeedsChoice
    // from thread/list), covered by d4_resume_lists_threads_then_redirects.
    for name in ["new", "clear"] {
        let out = adapter.handle_directive(&h, dir(name, "")).await.unwrap();
        assert!(
            matches!(out, DirectiveOutcome::Redirect { .. }),
            "/{name} must Redirect"
        );
    }
    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

// =====================================================================
// v0.8.5 W3 §8-6 — CodexThreadTracker (D2.4).
// =====================================================================

/// §8-6 — the tracker reflects usage SOURCED FROM thread/tokenUsage/updated
/// (NOT TurnCompleted, which has no usage on the real wire), and active-turn
/// is set on turn/started then cleared on turn/completed.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn tracker_usage_from_token_usage_and_active_turn_lifecycle() {
    let (adapter, h, _seen, peer, notif, sock) = d2_start_with_notif("tracker-life").await;

    // turn/started → active_turn set.
    notif
        .send(json!({
            "method": "turn/started",
            "params": { "threadId": "tid-d2", "turn": { "id": "turn-1" } }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .and_then(|t| t.active_turn)
                == Some("turn-1".to_string())
        }
    })
    .await;

    // thread/tokenUsage/updated → usage. CRITICAL: this is the only usage
    // source; turn/completed carries none on the real wire. Occupancy comes
    // from `last` (the current active context size), NOT `total` (the
    // cumulative session sum, which balloons past the window).
    notif
        .send(json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "tid-d2", "turnId": "turn-1",
                "tokenUsage": {
                    "total": { "totalTokens": 4000000, "inputTokens": 3900000, "outputTokens": 100000,
                               "cachedInputTokens": 0, "reasoningOutputTokens": 0 },
                    "last": { "totalTokens": 188000, "inputTokens": 180000, "outputTokens": 8000,
                              "cachedInputTokens": 0, "reasoningOutputTokens": 0 },
                    "modelContextWindow": 1000000
                }
            }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .and_then(|t| t.usage)
                .map(|u| u.used_tokens == 188000 && u.window_tokens == 1_000_000)
                .unwrap_or(false)
        }
    })
    .await;

    // turn/completed (NO usage field) → active_turn cleared, usage UNCHANGED.
    notif
        .send(json!({
            "method": "turn/completed",
            "params": { "threadId": "tid-d2", "turn": { "id": "turn-1", "status": "completed" } }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .map(|t| t.active_turn.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    // thread_status reflects the usage sourced from tokenUsage.
    let status = adapter.thread_status(&h).await.unwrap();
    let ctx = status.context.expect("usage must be present");
    assert_eq!(
        ctx.used_tokens, 188000,
        "usage from tokenUsage.last (active context size), not .total (cumulative sum)"
    );
    assert_eq!(ctx.window_tokens, 1_000_000);
    // active turn cleared.
    assert!(adapter
        .tracker_snapshot("tid-d2")
        .await
        .unwrap()
        .active_turn
        .is_none());

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// §8-6 — opening TWO events() streams must NOT double-count: the tracker is
/// fed by ONE dispatcher (spawned in client()), independent of events()
/// subscriptions. tokenUsage is an absolute snapshot, so a re-sent identical
/// value must stay the single value (the two subscribers do NOT also write
/// the tracker).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn tracker_single_dispatcher_not_per_subscribe() {
    use futures::StreamExt;
    let (adapter, h, _seen, peer, notif, sock) = d2_start_with_notif("tracker-single").await;

    // Open TWO events() streams — each opens its own broadcast subscriber.
    let mut s1 = adapter.events(&h);
    let mut s2 = adapter.events(&h);
    tokio::time::sleep(Duration::from_millis(30)).await;

    notif
        .send(json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "tid-d2", "turnId": "turn-1",
                "tokenUsage": {
                    "total": { "totalTokens": 500 },
                    "last": { "totalTokens": 500 },
                    "modelContextWindow": 200000
                }
            }
        }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move {
            a.tracker_snapshot("tid-d2")
                .await
                .and_then(|t| t.usage)
                .map(|u| u.used_tokens == 500)
                .unwrap_or(false)
        }
    })
    .await;

    notif
        .send(json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "tid-d2", "turnId": "turn-2",
                "tokenUsage": {
                    "total": { "totalTokens": 500 },
                    "last": { "totalTokens": 500 },
                    "modelContextWindow": 200000
                }
            }
        }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let usage = adapter
        .tracker_snapshot("tid-d2")
        .await
        .unwrap()
        .usage
        .unwrap();
    assert_eq!(
        usage.used_tokens, 500,
        "single dispatcher → no double-count"
    );

    // Drain a little so the streams don't drop mid-recv (cosmetic).
    let _ = tokio::time::timeout(Duration::from_millis(10), s1.next()).await;
    let _ = tokio::time::timeout(Duration::from_millis(10), s2.next()).await;

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D2 — skills/changed notification invalidates the cache (arch §1.3): after
/// priming the cache, a skills/changed clears it (next lookup re-fetches).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn skills_changed_invalidates_cache() {
    use ccteam_harness::execution::codex_app_server::CachedSkill;
    let (adapter, _h, _seen, peer, notif, sock) = d2_start_with_notif("skills-changed").await;

    // Prime the cache directly.
    adapter
        .prime_skills_cache_for_test(vec![CachedSkill {
            name: "old".into(),
            path: "/old".into(),
            enabled: true,
        }])
        .await;
    assert!(adapter.skills_cache_is_some_for_test().await);

    // skills/changed → dispatcher clears the cache.
    notif
        .send(json!({ "method": "skills/changed", "params": {} }))
        .await
        .unwrap();
    wait_until(|| {
        let a = adapter.clone();
        async move { !a.skills_cache_is_some_for_test().await }
    })
    .await;

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// Unit: parse_review_target covers all four ReviewTarget variants without a
/// running peer (review.rs:43-65 shapes).
#[test]
fn parse_review_target_variants() {
    use ccteam_harness::execution::codex_app_server::parse_review_target;
    assert_eq!(
        parse_review_target(""),
        json!({ "type": "uncommittedChanges" })
    );
    assert_eq!(
        parse_review_target("branch develop"),
        json!({ "type": "baseBranch", "branch": "develop" })
    );
    assert_eq!(
        parse_review_target("commit deadbeef"),
        json!({ "type": "commit", "sha": "deadbeef", "title": null })
    );
    assert_eq!(
        parse_review_target("look at the error handling"),
        json!({ "type": "custom", "instructions": "look at the error handling" })
    );
    // bare `branch` / `commit` with no arg → custom (not a malformed target).
    assert_eq!(
        parse_review_target("branch"),
        json!({ "type": "custom", "instructions": "branch" })
    );
}

// =====================================================================
// v0.8.5 W3 D4 — bare-popup → two-step NeedsChoice.
//
// For the eight popup commands, a BARE invocation returns NeedsChoice
// built from the list source; a re-entry carrying `d.choice` applies the
// picked id; a WITH-ARGS invocation keeps the D2 direct-apply path.
// =====================================================================

/// Build a re-entry directive: bare name + the picked id on `d.choice`,
/// as the gateway does after the user selects an option.
fn dir_choice(name: &str, token: &str, ids: &[&str]) -> Directive {
    Directive {
        name: name.to_string(),
        args: String::new(),
        choice: Some(ChoiceSelection {
            token: token.to_string(),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            free_text: None,
        }),
    }
}

/// D4 — every ChoicePrompt token must satisfy the contract: ASCII, ≤16
/// bytes, no `:` (the transport packs `"{token}:{idx}"`).
fn assert_token_ok(token: &str) {
    assert!(token.is_ascii(), "token must be ASCII: {token:?}");
    assert!(token.len() <= 16, "token must be ≤16 bytes: {token:?}");
    assert!(
        !token.contains(':'),
        "token must not contain ':' : {token:?}"
    );
}

/// D4 — bare /model → NeedsChoice from model/list (one option per
/// model+effort), with a contract-valid token. /model <id> with args is
/// unchanged (override, Done), and a choice re-entry applies the picked id
/// as the override.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_model_bare_needschoice_with_args_unchanged_choice_applies() {
    let (adapter, h, seen, peer, sock) = d2_start("d4-model").await;

    // Bare → NeedsChoice from a scripted model/list peer arm.
    let out = adapter
        .handle_directive(&h, dir("model", ""))
        .await
        .unwrap();
    let token = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            assert_eq!(prompt.title, "Choose a model + reasoning effort:");
            // model/list returns gpt-5 with [low, high] → two options.
            let ids: Vec<&str> = prompt.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["gpt-5 low", "gpt-5 high"]);
            assert!(!prompt.multi);
            prompt.token
        }
        other => panic!("bare /model must NeedsChoice, got {other:?}"),
    };
    // The bare path consulted model/list and sent NO override/turn.
    assert!(find_frame(&seen.lock().unwrap(), "model/list").is_some());
    assert!(adapter.override_for_test(&h.identity).await.model.is_none());

    // /model <id> [effort] with args → override (Done), unchanged from D2.
    let out = adapter
        .handle_directive(&h, dir("model", "gpt-5 high"))
        .await
        .unwrap();
    assert!(
        matches!(out, DirectiveOutcome::Done { .. }),
        "with-args → Done"
    );
    let ov = adapter.override_for_test(&h.identity).await;
    assert_eq!(ov.model.as_deref(), Some("gpt-5"));
    assert_eq!(ov.effort.as_deref(), Some("high"));

    // Choice re-entry (bare name + picked id) → override applied.
    let out = adapter
        .handle_directive(&h, dir_choice("model", &token, &["gpt-5 low"]))
        .await
        .unwrap();
    assert!(
        matches!(out, DirectiveOutcome::Done { .. }),
        "choice → Done"
    );
    let ov = adapter.override_for_test(&h.identity).await;
    assert_eq!(ov.model.as_deref(), Some("gpt-5"));
    assert_eq!(ov.effort.as_deref(), Some("low"), "choice effort applied");

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D4 — bare /personality + /memories → NeedsChoice from static enums (no
/// RPC); a choice re-entry applies the picked id.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_static_enum_popups_personality_and_memories() {
    let (adapter, h, seen, peer, sock) = d2_start("d4-static").await;

    // /personality bare → NeedsChoice (none/friendly/pragmatic), NO RPC.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir("personality", ""))
        .await
        .unwrap();
    let ptoken = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            let ids: Vec<&str> = prompt.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["none", "friendly", "pragmatic"]);
            prompt.token
        }
        other => panic!("bare /personality must NeedsChoice, got {other:?}"),
    };
    assert!(
        seen.lock().unwrap().is_empty(),
        "static-enum popup sends no RPC"
    );

    // Choice re-entry → override applied.
    let _ = adapter
        .handle_directive(&h, dir_choice("personality", &ptoken, &["pragmatic"]))
        .await
        .unwrap();
    assert_eq!(
        adapter
            .override_for_test(&h.identity)
            .await
            .personality
            .as_deref(),
        Some("pragmatic")
    );

    // /memories bare → NeedsChoice (enabled/disabled as on/off ids), NO RPC.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir("memories", ""))
        .await
        .unwrap();
    let mtoken = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            let ids: Vec<&str> = prompt.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["on", "off"]);
            prompt.token
        }
        other => panic!("bare /memories must NeedsChoice, got {other:?}"),
    };
    assert!(
        find_frame(&seen.lock().unwrap(), "thread/memoryMode/set").is_none(),
        "bare /memories sends no set RPC"
    );

    // Choice re-entry (pick "off") → thread/memoryMode/set { mode: "disabled" }.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir_choice("memories", &mtoken, &["off"]))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Done { .. }));
    assert_eq!(
        find_frame(&seen.lock().unwrap(), "thread/memoryMode/set").unwrap()["params"],
        json!({ "threadId": "tid-d2", "mode": "disabled" })
    );

    // /memories on with args → direct apply (unchanged from D2).
    seen.lock().unwrap().clear();
    let _ = adapter
        .handle_directive(&h, dir("memories", "on"))
        .await
        .unwrap();
    assert_eq!(
        find_frame(&seen.lock().unwrap(), "thread/memoryMode/set").unwrap()["params"]["mode"],
        "enabled"
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D4 — bare /permissions → NeedsChoice (static presets); a choice re-entry
/// applies approval+sandbox; with-args unchanged.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_permissions_bare_needschoice_choice_applies() {
    let (adapter, h, _seen, peer, sock) = d2_start("d4-perms").await;

    let out = adapter
        .handle_directive(&h, dir("permissions", ""))
        .await
        .unwrap();
    let token = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            let ids: Vec<&str> = prompt.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["read-only", "auto", "full-access"]);
            prompt.token
        }
        other => panic!("bare /permissions must NeedsChoice, got {other:?}"),
    };

    // Choice re-entry (pick "full-access") → override (never + dangerFullAccess).
    let _ = adapter
        .handle_directive(&h, dir_choice("permissions", &token, &["full-access"]))
        .await
        .unwrap();
    let ov = adapter.override_for_test(&h.identity).await;
    assert_eq!(ov.approval_policy.as_deref(), Some("never"));
    assert_eq!(
        ov.sandbox_policy,
        Some(json!({ "type": "dangerFullAccess" }))
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D4 — bare /collab → NeedsChoice from the EXPERIMENTAL
/// collaborationMode/list; a choice re-entry stores the picked ModeKind.
/// /plan stays a direct apply (no popup). /collab <m> with args unchanged.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_collab_lists_modes_plan_is_direct() {
    let (adapter, h, seen, peer, sock) = d2_start("d4-collab").await;

    // /collab bare → NeedsChoice from collaborationMode/list (EXPERIMENTAL).
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir("collab", ""))
        .await
        .unwrap();
    let token = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            // mask ids are the ModeKind (plan/default).
            let ids: Vec<&str> = prompt.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["plan", "default"]);
            prompt.token
        }
        other => panic!("bare /collab must NeedsChoice, got {other:?}"),
    };
    assert!(find_frame(&seen.lock().unwrap(), "collaborationMode/list").is_some());

    // Choice re-entry → stores the picked ModeKind.
    let _ = adapter
        .handle_directive(&h, dir_choice("collab", &token, &["default"]))
        .await
        .unwrap();
    assert_eq!(
        adapter
            .override_for_test(&h.identity)
            .await
            .collaboration_mode
            .as_deref(),
        Some("default")
    );

    // /plan is a directed alias → direct apply, NO collaborationMode/list RPC.
    seen.lock().unwrap().clear();
    let out = adapter.handle_directive(&h, dir("plan", "")).await.unwrap();
    assert!(matches!(out, DirectiveOutcome::Done { .. }), "/plan → Done");
    assert!(
        find_frame(&seen.lock().unwrap(), "collaborationMode/list").is_none(),
        "/plan must not list modes"
    );
    assert_eq!(
        adapter
            .override_for_test(&h.identity)
            .await
            .collaboration_mode
            .as_deref(),
        Some("plan")
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D4 — bare /review → NeedsChoice with the 4 fixed ReviewTarget options;
/// `uncommitted` pick fires review/start; `branch` pick is a 2nd-hop
/// NeedsChoice (needs the branch); `/review branch X` with args still →
/// BaseBranch directly (D2, unchanged).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_review_bare_needschoice_with_2nd_hop_and_args_unchanged() {
    let (adapter, h, seen, peer, sock) = d2_start("d4-review").await;

    // Bare → NeedsChoice with 4 options, NO review/start RPC yet.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir("review", ""))
        .await
        .unwrap();
    let token = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            let ids: Vec<&str> = prompt.options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, vec!["uncommitted", "branch", "commit", "custom"]);
            prompt.token
        }
        other => panic!("bare /review must NeedsChoice, got {other:?}"),
    };
    assert!(find_frame(&seen.lock().unwrap(), "review/start").is_none());

    // Pick "uncommitted" → review/start { uncommittedChanges } → Turn.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir_choice("review", &token, &["uncommitted"]))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Turn(_)));
    assert_eq!(
        find_frame(&seen.lock().unwrap(), "review/start").unwrap()["params"]["target"],
        json!({ "type": "uncommittedChanges" })
    );

    // Pick "branch" with NO free_text → 2nd-hop NeedsChoice (asks for branch),
    // NO review/start yet.
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir_choice("review", &token, &["branch"]))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            assert!(prompt.title.to_lowercase().contains("branch"));
        }
        other => panic!("branch pick must 2nd-hop NeedsChoice, got {other:?}"),
    }
    assert!(
        find_frame(&seen.lock().unwrap(), "review/start").is_none(),
        "branch 2nd-hop must not fire review yet"
    );

    // 2nd-hop answer: branch picked WITH free_text → review/start { baseBranch }.
    seen.lock().unwrap().clear();
    let sel = Directive {
        name: "review".to_string(),
        args: String::new(),
        choice: Some(ChoiceSelection {
            token: token.clone(),
            ids: vec!["branch".to_string()],
            free_text: Some("main".to_string()),
        }),
    };
    let out = adapter.handle_directive(&h, sel).await.unwrap();
    assert!(matches!(out, DirectiveOutcome::Turn(_)));
    assert_eq!(
        find_frame(&seen.lock().unwrap(), "review/start").unwrap()["params"]["target"],
        json!({ "type": "baseBranch", "branch": "main" })
    );

    // /review branch X with args → BaseBranch directly (D2, unchanged).
    seen.lock().unwrap().clear();
    let out = adapter
        .handle_directive(&h, dir("review", "branch develop"))
        .await
        .unwrap();
    assert!(matches!(out, DirectiveOutcome::Turn(_)));
    assert_eq!(
        find_frame(&seen.lock().unwrap(), "review/start").unwrap()["params"]["target"],
        json!({ "type": "baseBranch", "branch": "develop" })
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D4 — bare /resume → NeedsChoice from thread/list; a choice re-entry →
/// Redirect carrying `/use <id>` (the gateway switches sessions).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_resume_lists_threads_then_redirects() {
    let (adapter, h, seen, peer, sock) = d2_start("d4-resume").await;

    let out = adapter
        .handle_directive(&h, dir("resume", ""))
        .await
        .unwrap();
    let token = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            // id is the codex thread id; label prefers the thread name.
            assert_eq!(prompt.options[0].id, "tid-old-1");
            assert_eq!(prompt.options[0].label, "Auth work");
            prompt.token
        }
        other => panic!("bare /resume must NeedsChoice, got {other:?}"),
    };
    assert!(find_frame(&seen.lock().unwrap(), "thread/list").is_some());

    // Choice re-entry → Redirect with `/use <picked id>`.
    let out = adapter
        .handle_directive(&h, dir_choice("resume", &token, &["tid-old-1"]))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Redirect { hint } => assert_eq!(hint, "/use tid-old-1"),
        other => panic!("resume choice must Redirect, got {other:?}"),
    }

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

/// D4 — bare /skills → NeedsChoice (pick to view); a choice re-entry shows
/// the picked skill's detail.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn d4_skills_bare_needschoice_pick_shows_detail() {
    let (adapter, h, _seen, peer, sock) = d2_start("d4-skills").await;

    let out = adapter
        .handle_directive(&h, dir("skills", ""))
        .await
        .unwrap();
    let token = match out {
        DirectiveOutcome::NeedsChoice(prompt) => {
            assert_token_ok(&prompt.token);
            assert!(prompt.options.iter().any(|o| o.id == "deploy"));
            prompt.token
        }
        other => panic!("bare /skills must NeedsChoice, got {other:?}"),
    };

    // Pick "deploy" → Done with the skill detail (path).
    let out = adapter
        .handle_directive(&h, dir_choice("skills", &token, &["deploy"]))
        .await
        .unwrap();
    match out {
        DirectiveOutcome::Done { receipt } => {
            assert!(receipt.contains("deploy"));
            assert!(receipt.contains("/repo/.agents/skills/deploy"));
        }
        other => panic!("skills choice must show detail (Done), got {other:?}"),
    }

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
}

// =====================================================================
// v0.8.5 W3 D2.5 — Codex SlashCommand enum-name drift snapshot.
//
// A hand-synced snapshot of the EXACT Codex `SlashCommand` enum variant
// names from `references/codex/codex-rs/tui/src/slash_command.rs`
// @ b2344d8. This is NOT a codex crate runtime dep (red line) — it is a
// manually transcribed const list. The test asserts every enum name is
// classified by either the builtin mapping OR the TUI-only reject list in
// codex_app_server.rs; an uncategorized name fails the test (drift signal).
//
// ⚠️ RE-SYNC when bumping the codex reference: re-transcribe the enum from
// `tui/src/slash_command.rs` and re-categorise any new names below.
// =====================================================================

/// The 51 `SlashCommand` enum VARIANT names (Rust identifiers, NOT the
/// kebab-case wire strings) at b2344d8. Transcribed by hand from
/// `references/codex/codex-rs/tui/src/slash_command.rs:15-77`.
const CODEX_SLASH_COMMAND_VARIANTS_B2344D8: &[&str] = &[
    "Model",
    "Ide",
    "Permissions",
    "Keymap",
    "Vim",
    "ElevateSandbox",
    "SandboxReadRoot",
    "Experimental",
    "AutoReview",
    "Memories",
    "Skills",
    "Hooks",
    "Review",
    "Rename",
    "New",
    "Archive",
    "Resume",
    "Fork",
    "Init",
    "Compact",
    "Plan",
    "Goal",
    "Agent",
    "Side",
    "Btw",
    "Copy",
    "Raw",
    "Diff",
    "Mention",
    "Status",
    "DebugConfig",
    "Title",
    "Statusline",
    "Theme",
    "Pets",
    "Mcp",
    "Apps",
    "Plugins",
    "Logout",
    "Quit",
    "Exit",
    "Feedback",
    "Rollout",
    "Ps",
    "Stop",
    "Clear",
    "Personality",
    "Realtime",
    "Settings",
    "TestApproval",
    "MultiAgents",
    "MemoryDrop",
    "MemoryUpdate",
];

/// Map a `SlashCommand` enum variant name (Rust ident) to the ccteam
/// command token(s) handle_directive dispatches on. A name maps to one or
/// more tokens; the test asserts at least one is classified (builtin or
/// reject). The wire/alias forms come from `slash_command.rs`'s
/// `#[strum(...)]` attributes (e.g. AutoReview → `approve`, Stop → `stop`,
/// MultiAgents → `subagents`).
fn variant_to_command_tokens(variant: &str) -> Vec<&'static str> {
    match variant {
        "Model" => vec!["model"],
        "Ide" => vec!["ide"],
        "Permissions" => vec!["permissions"],
        "Keymap" => vec!["keymap"],
        "Vim" => vec!["vim"],
        "ElevateSandbox" => vec!["setup-default-sandbox"],
        "SandboxReadRoot" => vec!["sandbox-add-read-dir"],
        "Experimental" => vec!["experimental"],
        "AutoReview" => vec!["approve"],
        "Memories" => vec!["memories"],
        "Skills" => vec!["skills"],
        "Hooks" => vec!["hooks"],
        "Review" => vec!["review"],
        "Rename" => vec!["rename"],
        "New" => vec!["new"],
        "Archive" => vec!["archive"],
        "Resume" => vec!["resume"],
        "Fork" => vec!["fork"],
        "Init" => vec!["init"],
        "Compact" => vec!["compact"],
        "Plan" => vec!["plan"],
        "Goal" => vec!["goal"],
        "Agent" => vec!["agent"],
        "Side" => vec!["side"],
        "Btw" => vec!["btw"],
        "Copy" => vec!["copy"],
        "Raw" => vec!["raw"],
        "Diff" => vec!["diff"],
        "Mention" => vec!["mention"],
        "Status" => vec!["status"],
        "DebugConfig" => vec!["debug-config"],
        "Title" => vec!["title"],
        "Statusline" => vec!["statusline"],
        "Theme" => vec!["theme"],
        "Pets" => vec!["pets"],
        "Mcp" => vec!["mcp"],
        "Apps" => vec!["apps"],
        "Plugins" => vec!["plugins"],
        "Logout" => vec!["logout"],
        "Quit" => vec!["quit"],
        "Exit" => vec!["exit"],
        "Feedback" => vec!["feedback"],
        "Rollout" => vec!["rollout"],
        "Ps" => vec!["ps"],
        "Stop" => vec!["stop"],
        "Clear" => vec!["clear"],
        "Personality" => vec!["personality"],
        "Realtime" => vec!["realtime"],
        "Settings" => vec!["settings"],
        "TestApproval" => vec!["test-approval"],
        "MultiAgents" => vec!["subagents"],
        "MemoryDrop" => vec!["debug-m-drop"],
        "MemoryUpdate" => vec!["debug-m-update"],
        _ => vec![],
    }
}

/// D2.5 — the drift guard. Every Codex `SlashCommand` enum name in the
/// pinned snapshot must be covered by EITHER the builtin mapping OR the
/// TUI-only reject list in codex_app_server.rs. An uncategorized name ⇒
/// FAIL (the codex reference grew a command ccteam doesn't classify).
#[test]
fn d2_5_every_codex_slash_command_is_builtin_or_rejected() {
    use ccteam_harness::execution::codex_app_server::{is_builtin_command, is_rejected_command};

    let mut builtin = 0usize;
    let mut rejected = 0usize;
    let mut uncovered: Vec<String> = Vec::new();

    for variant in CODEX_SLASH_COMMAND_VARIANTS_B2344D8 {
        let tokens = variant_to_command_tokens(variant);
        assert!(
            !tokens.is_empty(),
            "drift snapshot: variant {variant:?} has no command-token mapping — \
             add it to variant_to_command_tokens (re-sync with the codex reference)"
        );
        let is_builtin = tokens.iter().any(|t| is_builtin_command(t));
        let is_rejected = tokens.iter().any(|t| is_rejected_command(t));
        if is_builtin {
            builtin += 1;
        } else if is_rejected {
            rejected += 1;
        } else {
            uncovered.push((*variant).to_string());
        }
    }

    assert!(
        uncovered.is_empty(),
        "drift: {} Codex SlashCommand name(s) are neither builtin nor rejected — \
         classify them in codex_app_server.rs (builtin table or is_codex_tui_only): {:?}",
        uncovered.len(),
        uncovered
    );
    // Sanity: the snapshot is non-trivial and every name landed in exactly
    // one bucket.
    assert_eq!(
        builtin + rejected,
        CODEX_SLASH_COMMAND_VARIANTS_B2344D8.len(),
        "every snapshot name must be classified exactly once"
    );
    // Pin the snapshot size so a silent truncation of the list also trips.
    assert_eq!(
        CODEX_SLASH_COMMAND_VARIANTS_B2344D8.len(),
        53,
        "snapshot count drift — re-sync the SlashCommand enum from b2344d8"
    );
}

/// A fresh `start_thread` with a non-empty secret injects the ccteam HTTP MCP
/// server into `thread/start.config.mcp_servers.ccteam` (snake_case Codex
/// schema, per-session bearer) and persists the resulting thread id as
/// `raw_extras.vendor_uuid` (G5, for resume).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn start_thread_injects_per_thread_mcp_config_with_identity() {
    let prior_sock = std::env::var_os(APP_SERVER_SOCKET_ENV);
    let sock = unique_socket_path("codex-mcp-config");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let reqs: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
    let reqs_h = Arc::clone(&reqs);
    let (peer, _notif) = spawn_scripted_peer(sock.clone(), move |req| {
        if req.get("id").is_some() {
            reqs_h.lock().unwrap().push(req.clone());
        }
        match req["method"].as_str() {
            Some("initialize") => json!({ "result": {
                "user_agent": "t/0", "codex_home": "/tmp/.codex",
                "platform_family": "unix", "platform_os": "linux" } }),
            Some("thread/start") => json!({ "result": { "thread": { "thread_id": "t-cfg" } } }),
            _ => json!({ "error": { "code": -32601, "message": "unexpected" } }),
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let tmp = TempDir::new().unwrap();
    let adapter = CodexAppServerAdapter::new();
    let h = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "reviewer".into(),
            },
            &SpawnCtx {
                slug: "demo".into(),
                sid: "codex-w1".into(),
                cwd: tmp.path().to_path_buf(),
                project_dir: tmp.path().to_path_buf(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: "seKret1234".into(),
                remote: None,
            },
        )
        .await
        .unwrap();
    // G5 — vendor_uuid persisted (apply_new_session reads this into meta.json).
    assert_eq!(h.raw_extras["vendor_uuid"], "t-cfg");

    let start = reqs
        .lock()
        .unwrap()
        .iter()
        .find(|r| r["method"] == "thread/start")
        .cloned()
        .expect("thread/start seen");
    let srv = &start["params"]["config"]["mcp_servers"]["ccteam"];
    assert_eq!(
        srv["url"],
        ccteam_harness::execution::mcp_config::default_mcp_http_url(),
        "per-thread config must target the daemon HTTP MCP endpoint: {start}"
    );
    assert_eq!(
        srv["http_headers"]["Authorization"], "Bearer ccteam-sid:codex-w1:seKret1234",
        "per-thread config must carry the session principal: {start}"
    );
    assert!(
        srv.get("command").is_none(),
        "must not spawn mcp-serve: {start}"
    );
    assert!(
        srv.get("args").is_none(),
        "must not spawn mcp-serve: {start}"
    );
    assert!(srv.get("env").is_none(), "identity rides HTTP: {start}");

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    restore_env(APP_SERVER_SOCKET_ENV, prior_sock);
}

/// v0.9.0 W1 (G5) — after a daemon restart (a fresh adapter), `start_thread`
/// reads the persisted `meta.vendor_uuid` and issues `thread/resume` for that
/// exact id (carrying the same per-thread config) INSTEAD of silently starting
/// a fresh thread and dropping the conversation.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn start_thread_resumes_persisted_vendor_uuid_after_restart() {
    let prior_sock = std::env::var_os(APP_SERVER_SOCKET_ENV);
    let sock = unique_socket_path("codex-resume-persisted");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let reqs: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
    let reqs_h = Arc::clone(&reqs);
    let (peer, _notif) = spawn_scripted_peer(sock.clone(), move |req| {
        if req.get("id").is_some() {
            reqs_h.lock().unwrap().push(req.clone());
        }
        match req["method"].as_str() {
            Some("initialize") => json!({ "result": {
                "user_agent": "t/0", "codex_home": "/tmp/.codex",
                "platform_family": "unix", "platform_os": "linux" } }),
            Some("thread/resume") => json!({ "result": { "thread": { "thread_id": "t-prior" } } }),
            Some("thread/start") => json!({ "result": { "thread": { "thread_id": "t-fresh" } } }),
            _ => json!({ "error": { "code": -32601, "message": "unexpected" } }),
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let tmp = TempDir::new().unwrap();
    // Simulate the "before the restart" state: a prior codex session whose
    // vendor_uuid is persisted in meta.json (what apply_new_session writes).
    let meta = ccteam_harness::SessionMeta {
        sid: "codex-r1".into(),
        slug: "demo".into(),
        vendor: AgentVendor::Codex,
        protocol: ccteam_harness::SessionProtocol::StreamJson,
        role: "reviewer".into(),
        permission_mode: ccteam_harness::PermissionMode::Skip,
        owner: "user:web-api".into(),
        vendor_uuid: "t-prior".into(),
        model: None,
        host: "local".into(),
        created_at: String::new(),
        last_active: String::new(),
        origin: ccteam_harness::SessionOrigin::Ccteam,
        title: None,
        title_source: None,
        turn_count: 0,
        cost_usd: None,
        tokens_total: None,
        role_sha: None,
        skills_sha: None,
        trigger: None,
        parent_sid: None,
        spawned_by_role: None,
        delegation_depth: 0,
    };
    ccteam_harness::write_session_meta(tmp.path(), &meta).unwrap();

    // Fresh adapter (= daemon restart) resumes the persisted id.
    let adapter = CodexAppServerAdapter::new();
    let h = adapter
        .start_thread(
            &AgentSpecBrief {
                role: "reviewer".into(),
            },
            &SpawnCtx {
                slug: "demo".into(),
                sid: "codex-r1".into(),
                cwd: tmp.path().to_path_buf(),
                project_dir: tmp.path().to_path_buf(),
                extra_args: vec![],
                model_id: None,
                effort: None,
                permission_mode: ccteam_harness::PermissionMode::Skip,
                secret: "seKret".into(),
                remote: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(h.identity, "t-prior", "must resume the persisted thread id");

    let seen = reqs.lock().unwrap().clone();
    let resume = seen
        .iter()
        .find(|r| r["method"] == "thread/resume")
        .unwrap_or_else(|| panic!("second start_thread must thread/resume: {seen:?}"));
    assert_eq!(resume["params"]["threadId"], "t-prior");
    assert_eq!(
        resume["params"]["config"]["mcp_servers"]["ccteam"]["http_headers"]["Authorization"],
        "Bearer ccteam-sid:codex-r1:seKret",
        "resume must carry the per-thread HTTP principal too"
    );
    assert!(
        !seen.iter().any(|r| r["method"] == "thread/start"),
        "resume succeeded → must NOT fall back to a fresh thread/start: {seen:?}"
    );

    drop(peer);
    let _ = std::fs::remove_file(&sock);
    restore_env(APP_SERVER_SOCKET_ENV, prior_sock);
}

/// v0.9.0 W3 (F3) — remote execution is claude-only this version; codex
/// must fail clean + readable rather than silently spawning the
/// daemon-singleton app-server locally under a remote host id. No fake
/// socket needed — the guard runs before any transport dial.
#[tokio::test(flavor = "current_thread")]
async fn start_thread_rejects_remote_ctx_readable() {
    let tmp = TempDir::new().unwrap();
    let ctx = SpawnCtx {
        slug: "demo".into(),
        sid: "s-remote".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: Some(ccteam_harness::RemoteExecTarget {
            host_id: "sat".into(),
            wire_slug: "demo".into(),
            hub: std::sync::Arc::new(ccteam_harness::HostChannelHub::default()),
        }),
    };
    let adapter = CodexAppServerAdapter::new();
    let err = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect_err("remote codex must be rejected");
    assert!(matches!(err, HarnessError::NotImplemented { .. }));
    assert!(err.to_string().contains("not yet supported for codex"));
}
