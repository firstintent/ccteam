//! V0.6.1 F122 — `CodexAppServerAdapter` progress.jsonl bridge tests.
//!
//! Closes the V0.6.0 Wave 3 D9 retained risk: when codex
//! notifications arrive on the app-server UDS, the adapter mirrors
//! `turn/completed` / `turn/failed` / `error` events into the
//! project's `progress.jsonl` as `agent_done` rows tagged
//! `vendor: codex` so `compute_cost_summary` rolls them into
//! `cost_24h_by_vendor["codex"]` without needing the orchestrator to
//! wire a separate poller.
//!
//! Two complementary paths are exercised:
//!
//! 1. **Unit-level bridge** — pre-register a ctx via
//!    [`CodexAppServerAdapter::register_bridge_for_test`] then call
//!    [`ccteam_harness::execution::codex_app_server::build_progress_line`]
//!    directly. Verifies the row shape + cost roll-up without a real
//!    UDS round-trip (deterministic; runs on every box).
//! 2. **End-to-end UDS** — mock a JSON-RPC peer, drive
//!    `start_thread` → push a `turn/completed` notification → poll
//!    `events()` → assert `progress.jsonl` ends up with the expected
//!    `agent_done` row.
//!
//! The end-to-end path mirrors `codex_app_server_test.rs`'s scripted-
//! peer harness so any future `app-server` protocol drift surfaces in
//! both suites.

use ccteam_core::queries::cost_summary_from_events;
use ccteam_core::CcteamPaths;
use ccteam_harness::execution::codex_app_server::{
    build_progress_line, CodexAppServerAdapter, ProgressBridgeCtx, APP_SERVER_SOCKET_ENV,
};
use ccteam_harness::{
    AgentSpecBrief, HarnessAdapter, SpawnCtx, ThreadErrorEvent, ThreadEvent, UnifiedTokenUsage,
};
use futures::StreamExt;
use serde_json::{json, Value};
use serial_test::serial;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Bind a unique UDS socket under /tmp. Each test gets its own so the
/// parallel runner doesn't trample `APP_SERVER_SOCKET_ENV`.
fn unique_socket_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "ccteam-v061-codex-bridge-{tag}-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// Read + parse every progress.jsonl row. Returns `vec![]` when the
/// file is absent (the bridge is a no-op until the first relevant
/// event lands).
fn read_progress(path: &std::path::Path) -> Vec<Value> {
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .expect("read progress.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("parse progress row"))
        .collect()
}

/// Scripted UDS peer that echoes `thread/start` and lets the test
/// push notifications out-of-band via the returned channel.
async fn spawn_scripted_peer(
    sock: PathBuf,
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
                            let mut resp = if req["method"] == "thread/start" {
                                json!({ "result": { "thread": { "thread_id": "codex-tid-7" } } })
                            } else {
                                json!({ "result": {} })
                            };
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

// --------- 1. Unit-level bridge: build_progress_line shape ---------

#[test]
fn build_progress_line_turn_completed_tags_vendor_codex() {
    let ctx = ProgressBridgeCtx {
        progress_path: PathBuf::from("/tmp/unused.jsonl"),
        role: "codex-bot".into(),
        sid: "sid-1".into(),
        slug: "demo".into(),
        // A real table model (gpt-5.5) so the deterministic price is real.
        model: Some("gpt-5.5".into()),
    };
    let usage = UnifiedTokenUsage {
        input_tokens: 1_000,
        output_tokens: 500,
        ..Default::default()
    };
    let evt = ThreadEvent::TurnCompleted {
        turn_id: "turn-9".into(),
        usage,
        model: None,
    };
    let row = build_progress_line(&evt, "codex-tid-7", &ctx).expect("turn/completed must bridge");
    assert_eq!(row["event"], "agent_done");
    assert_eq!(row["vendor"], "codex");
    assert_eq!(row["role"], "codex-bot");
    assert_eq!(row["slug"], "demo");
    assert_eq!(row["session_id"], "sid-1");
    assert_eq!(row["status"], "completed");
    assert_eq!(row["thread_id"], "codex-tid-7");
    assert_eq!(row["turn_id"], "turn-9");
    let cost = row["cost_usd"].as_f64().expect("cost_usd numeric");
    assert!(cost > 0.0, "non-zero cost from non-zero usage");
}

#[test]
fn build_progress_line_turn_failed_marks_errored() {
    let ctx = ProgressBridgeCtx {
        progress_path: PathBuf::from("/tmp/unused.jsonl"),
        role: "codex-bot".into(),
        sid: "sid-1".into(),
        slug: "demo".into(),
        model: None,
    };
    let evt = ThreadEvent::TurnFailed {
        turn_id: "turn-9".into(),
        err: ThreadErrorEvent {
            kind: "turn_failed".into(),
            message: "model unavailable".into(),
        },
        usage: UnifiedTokenUsage::default(),
        model: None,
    };
    let row = build_progress_line(&evt, "codex-tid-7", &ctx).expect("turn/failed must bridge");
    assert_eq!(row["event"], "agent_done");
    assert_eq!(row["vendor"], "codex");
    assert_eq!(row["status"], "errored");
    assert_eq!(row["error"], "model unavailable");
}

#[test]
fn build_progress_line_silent_for_item_events() {
    let ctx = ProgressBridgeCtx {
        progress_path: PathBuf::from("/tmp/unused.jsonl"),
        role: "codex-bot".into(),
        sid: "sid-1".into(),
        slug: "demo".into(),
        model: None,
    };
    let evt = ThreadEvent::TurnStarted {
        turn_id: "turn-9".into(),
    };
    assert!(
        build_progress_line(&evt, "codex-tid-7", &ctx).is_none(),
        "turn/started is presentation-only — must not write progress.jsonl"
    );
}

// --------- 2. Cost roll-up — the SoT the bridge must feed ---------

#[test]
fn cost_summary_rolls_bridged_agent_done_into_codex_bucket() {
    let ctx = ProgressBridgeCtx {
        progress_path: PathBuf::from("/tmp/unused.jsonl"),
        role: "codex-bot".into(),
        sid: "sid-1".into(),
        slug: "demo".into(),
        // A real table model (gpt-5.5) so the deterministic price is real.
        model: Some("gpt-5.5".into()),
    };
    let usage = UnifiedTokenUsage {
        input_tokens: 10_000,
        output_tokens: 5_000,
        ..Default::default()
    };
    let evt = ThreadEvent::TurnCompleted {
        turn_id: "turn-1".into(),
        usage,
        model: None,
    };
    let row = build_progress_line(&evt, "codex-tid-7", &ctx).unwrap();
    let summary = cost_summary_from_events(&[row]).expect("cost summary");
    let codex_total = summary
        .cost_24h_by_vendor
        .get("codex")
        .copied()
        .unwrap_or(0.0);
    assert!(
        codex_total > 0.0,
        "bridged agent_done must roll into cost_24h_by_vendor[\"codex\"]; got {codex_total}"
    );
    // Total + 24h should match per-vendor codex roll-up (single event).
    assert!((summary.cost_24h_usd - codex_total).abs() < 1e-12);
    assert!((summary.cost_total_usd - codex_total).abs() < 1e-12);
}

// --------- 3. End-to-end: adapter + scripted UDS peer ---------

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn end_to_end_turn_completed_writes_progress_jsonl_with_vendor_codex() {
    // CCTEAM_HOME → tempdir so the resolved progress.jsonl lands
    // somewhere disposable.
    let home = tempdir().unwrap();
    let prev_home = std::env::var_os("CCTEAM_HOME");
    std::env::set_var("CCTEAM_HOME", home.path());

    let sock = unique_socket_path("e2e-turn-completed");
    std::env::set_var(APP_SERVER_SOCKET_ENV, &sock);

    let (peer, notif_tx) = spawn_scripted_peer(sock.clone()).await;
    // Give the listener a moment.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let adapter = CodexAppServerAdapter::new();
    let spec = AgentSpecBrief {
        role: "codex-bot".into(),
    };
    let ctx = SpawnCtx {
        slug: "demo-codex".into(),
        sid: "codex-sid-1".into(),
        owner: "user:web-api".into(),
        cwd: home.path().to_path_buf(),
        project_dir: home.path().to_path_buf(),
        extra_args: vec![],
        // A real table model (gpt-5.5) so the deterministic per-turn price
        // resolves (an unknown model would correctly OMIT cost_usd now).
        model_id: Some("gpt-5.5".into()),
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    };
    let h = adapter
        .start_thread(&spec, &ctx)
        .await
        .expect("start_thread");
    assert_eq!(h.identity, "codex-tid-7");

    // Start consuming events in the background so the bridge fires
    // on the pushed turn/completed notification.
    let mut events = adapter.events(&h);
    let collector = tokio::spawn(async move {
        // Pull a single event then stop — the stream stays open but
        // the bridge has already written by the time we yield.
        events.next().await
    });
    // Tiny grace so the subscriber registers before we push.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let notif = json!({
        "jsonrpc": "2.0",
        "method": "turn/completed",
        "params": {
            "thread_id": "codex-tid-7",
            "turn_id": "turn-42",
            "usage": {
                "input_tokens": 2_000,
                "output_tokens": 1_000,
                "cached_input_tokens": 0,
            },
        }
    });
    notif_tx.send(notif).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), collector)
        .await
        .expect("collector timed out")
        .expect("collector join");
    let evt = got.expect("expected one ThreadEvent");
    matches!(evt, ThreadEvent::TurnCompleted { .. })
        .then_some(())
        .expect("expected ThreadEvent::TurnCompleted");

    // The bridge appends synchronously inside the events() stream
    // poll — by the time the consumer sees the ThreadEvent the
    // progress.jsonl row is on disk.
    let paths = CcteamPaths::from_env().expect("paths");
    let progress_path = paths.progress_jsonl("demo-codex");
    let rows = read_progress(&progress_path);
    assert!(
        !rows.is_empty(),
        "expected at least one progress.jsonl row at {}",
        progress_path.display()
    );
    let last = rows.last().unwrap();
    assert_eq!(last["event"], "agent_done");
    assert_eq!(last["vendor"], "codex");
    assert_eq!(last["role"], "codex-bot");
    assert_eq!(last["slug"], "demo-codex");
    assert_eq!(last["thread_id"], "codex-tid-7");
    assert_eq!(last["turn_id"], "turn-42");
    let cost = last["cost_usd"].as_f64().expect("cost_usd numeric");
    assert!(cost > 0.0, "expected non-zero cost; got {cost}");

    // And the cost summary picks it up on the codex bucket.
    let summary = cost_summary_from_events(&rows).expect("summary");
    let codex_total = summary
        .cost_24h_by_vendor
        .get("codex")
        .copied()
        .unwrap_or(0.0);
    assert!(
        (codex_total - cost).abs() < 1e-12,
        "cost_24h_by_vendor[\"codex\"]={codex_total} must equal the bridged row's cost {cost}"
    );

    // Cleanup
    drop(peer);
    let _ = std::fs::remove_file(&sock);
    std::env::remove_var(APP_SERVER_SOCKET_ENV);
    match prev_home {
        Some(p) => std::env::set_var("CCTEAM_HOME", p),
        None => std::env::remove_var("CCTEAM_HOME"),
    }
}
