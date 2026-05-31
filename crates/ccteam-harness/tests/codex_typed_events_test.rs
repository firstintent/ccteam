//! V0.8 rmux Slice 4 — Codex mode-3 typed-event producer integration tests.
//!
//! Uses `tokio::io::duplex` for an in-process Codex peer (no real
//! `codex app-server` daemon) — same pattern as `codex_jsonrpc_test.rs`
//! but with an explicit oneshot for peer shutdown, NOT a `sleep`. We
//! poll the progress.jsonl file for the expected row count, then signal
//! the peer to exit; the producer task terminates when the broadcast
//! closes (its `Arc<CodexJsonRpcClient>` drops at the end of the test).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ccteam_harness::execution::codex_jsonrpc::{CodexJsonRpcClient, Notification};
use ccteam_harness::execution::codex_typed_events::{maybe_start_codex_typed_event_tap, run_loop};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, oneshot};
use tokio::time::timeout;

/// Read every JSONL line in `path`, returning the parsed values.
fn read_rows(path: &Path) -> Vec<Value> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap();
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid jsonl"))
        .collect()
}

/// Poll `path` until `predicate(rows)` returns true, or panic on
/// timeout. Cheap busy-poll with a small backoff; tests run in
/// `current_thread` so `tokio::time::sleep` yields back to the
/// producer task.
async fn wait_for_rows<F>(path: &Path, mut predicate: F)
where
    F: FnMut(&[Value]) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = read_rows(path);
        if predicate(&rows) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for predicate; current rows={}",
                serde_json::to_string_pretty(&rows).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Force-enable the typed-events flag for this test process. The env
/// var lookup happens fresh inside `maybe_start_codex_typed_event_tap`,
/// not at module-load, so setting it here is sufficient. Tests live in
/// a binary that does NOT touch the flag elsewhere; we set it once on
/// every test entry for idempotency.
///
/// SAFETY: each `cargo test` integration binary runs in its own
/// process, so this env mutation is process-private (per CLAUDE.md
/// "env-mutating tests go into integration test files" guidance).
#[allow(unsafe_code)]
fn enable_typed_events() {
    unsafe {
        std::env::set_var("CCTEAM_TYPED_EVENTS", "1");
    }
}

/// Build a client wired to a duplex peer + a oneshot the peer task
/// awaits before exiting (so it stays alive long enough for the
/// producer to drain the broadcast — but no fixed sleep).
fn duplex_with_shutdown() -> (
    Arc<CodexJsonRpcClient>,
    tokio::io::DuplexStream,
    oneshot::Sender<()>,
    oneshot::Receiver<()>,
) {
    let (client_rw, peer_rw) = tokio::io::duplex(64 * 1024);
    let (client_r, client_w) = tokio::io::split(client_rw);
    let client = Arc::new(CodexJsonRpcClient::spawn(client_r, client_w));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    (client, peer_rw, shutdown_tx, shutdown_rx)
}

/// Spawn a peer task that writes the JSONL-serialised notifications
/// then awaits the `shutdown_rx` oneshot before exiting (so the
/// underlying duplex stays open until the test is ready to close it).
fn spawn_peer(
    mut peer: tokio::io::DuplexStream,
    notifs: Vec<Notification>,
    shutdown_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for n in notifs {
            let mut frame = json!({ "method": n.method });
            if !n.params.is_null() {
                frame["params"] = n.params;
            }
            let mut line = serde_json::to_vec(&frame).unwrap();
            line.push(b'\n');
            peer.write_all(&line).await.unwrap();
        }
        peer.flush().await.unwrap();
        // Hold the duplex open until the test releases us — the
        // producer must see every notification before the broadcast
        // closes. Replaces the timing-fragile `sleep(50ms)` pattern.
        let _ = shutdown_rx.await;
    })
}

#[tokio::test(flavor = "current_thread")]
async fn codex_typed_event_emits_row_for_turn_completed() {
    enable_typed_events();
    let tmp = TempDir::new().unwrap();
    let progress_path = tmp.path().join("progress.jsonl");

    let (client, peer, shutdown_tx, shutdown_rx) = duplex_with_shutdown();
    let peer_task = spawn_peer(
        peer,
        vec![Notification {
            method: "turn/completed".into(),
            params: json!({}),
        }],
        shutdown_rx,
    );
    let producer = maybe_start_codex_typed_event_tap(Arc::clone(&client), progress_path.clone())
        .expect("flag enabled");

    wait_for_rows(&progress_path, |rows| {
        rows.iter().any(|r| {
            r["kind"] == "typed_event" && r["event_kind"] == "turn_done" && r["vendor"] == "codex"
        })
    })
    .await;

    let rows = read_rows(&progress_path);
    assert_eq!(rows.len(), 1, "expected exactly one row, got {rows:?}");
    let r = &rows[0];
    assert_eq!(r["kind"], "typed_event");
    assert_eq!(r["event_kind"], "turn_done");
    assert_eq!(r["vendor"], "codex");
    assert_eq!(r["captured"], "");

    let _ = shutdown_tx.send(());
    peer_task.await.unwrap();
    drop(client);
    let _ = timeout(Duration::from_secs(2), producer).await;
}

#[tokio::test(flavor = "current_thread")]
async fn codex_tool_call_started_completed_both_emit() {
    enable_typed_events();
    let tmp = TempDir::new().unwrap();
    let progress_path = tmp.path().join("progress.jsonl");

    let (client, peer, shutdown_tx, shutdown_rx) = duplex_with_shutdown();
    let peer_task = spawn_peer(
        peer,
        vec![
            Notification {
                method: "item/started".into(),
                params: json!({ "item_id": "i-42" }),
            },
            Notification {
                method: "item/completed".into(),
                params: json!({ "item_id": "i-42" }),
            },
        ],
        shutdown_rx,
    );
    let producer = maybe_start_codex_typed_event_tap(Arc::clone(&client), progress_path.clone())
        .expect("flag enabled");

    wait_for_rows(&progress_path, |rows| rows.len() >= 2).await;

    let rows = read_rows(&progress_path);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["event_kind"], "tool_call_started");
    assert_eq!(rows[0]["captured"], "i-42");
    assert_eq!(rows[0]["vendor"], "codex");
    assert_eq!(rows[1]["event_kind"], "tool_call_completed");
    assert_eq!(rows[1]["captured"], "i-42");
    assert_eq!(rows[1]["vendor"], "codex");

    let _ = shutdown_tx.send(());
    peer_task.await.unwrap();
    drop(client);
    let _ = timeout(Duration::from_secs(2), producer).await;
}

#[tokio::test(flavor = "current_thread")]
async fn codex_no_leak_under_high_notification_volume() {
    enable_typed_events();
    let tmp = TempDir::new().unwrap();
    let progress_path = tmp.path().join("progress.jsonl");

    let (client, peer, shutdown_tx, shutdown_rx) = duplex_with_shutdown();
    let notifs: Vec<_> = (0..100)
        .map(|_| Notification {
            method: "turn/completed".into(),
            params: json!({}),
        })
        .collect();
    let peer_task = spawn_peer(peer, notifs, shutdown_rx);
    let producer = maybe_start_codex_typed_event_tap(Arc::clone(&client), progress_path.clone())
        .expect("flag enabled");

    wait_for_rows(&progress_path, |rows| rows.len() >= 100).await;

    let rows = read_rows(&progress_path);
    assert_eq!(rows.len(), 100, "expected 100 rows, got {}", rows.len());
    assert!(
        rows.iter()
            .all(|r| r["event_kind"] == "turn_done" && r["vendor"] == "codex"),
        "every row must be a codex turn_done"
    );

    let _ = shutdown_tx.send(());
    peer_task.await.unwrap();
    drop(client);
    let _ = timeout(Duration::from_secs(2), producer).await;
}

#[tokio::test(flavor = "current_thread")]
async fn codex_lagged_broadcast_is_handled() {
    enable_typed_events();
    let tmp = TempDir::new().unwrap();
    let progress_path = tmp.path().join("progress.jsonl");

    // Hand-roll a tiny broadcast channel so we can force Lagged
    // deterministically. Tap directly into `run_loop` (the same
    // function the public spawn entrypoint calls); this is the
    // recommended test escape hatch for forcing back-pressure.
    let (tx, rx) = broadcast::channel::<Notification>(2);

    let progress_for_loop = progress_path.clone();
    let producer = tokio::spawn(async move {
        run_loop(rx, progress_for_loop).await;
    });

    // The receiver isn't being drained yet (the spawned task is
    // pending — `tokio::spawn` queues but hasn't been polled). Fill
    // the buffer with 4 notifs into a capacity-2 channel: the front
    // 2 get evicted, and the receiver's next `recv()` returns
    // `Lagged(2)`.
    //
    // Actually we need the producer to have polled at least once to
    // bind `rx`. Trick: send one notification, yield, then over-fill.
    tx.send(Notification {
        method: "turn/completed".into(),
        params: json!({}),
    })
    .unwrap();
    // Yield so the producer task gets to poll once and consume it.
    tokio::task::yield_now().await;
    wait_for_rows(&progress_path, |rows| !rows.is_empty()).await;

    // Now over-fill: capacity-2 channel, 10 sends, no consumer
    // running for a moment. Some of these get evicted; the next
    // `recv()` will return `Lagged(n)`.
    for _ in 0..10 {
        let _ = tx.send(Notification {
            method: "turn/completed".into(),
            params: json!({}),
        });
    }

    // Send a final marker notification with a distinguishing
    // identity. If the producer keeps consuming after `Lagged`, the
    // marker row eventually lands in progress.jsonl.
    tx.send(Notification {
        method: "item/started".into(),
        params: json!({ "item_id": "post-lag-marker" }),
    })
    .unwrap();

    wait_for_rows(&progress_path, |rows| {
        rows.iter().any(|r| r["captured"] == "post-lag-marker")
    })
    .await;

    // Drop the sender → broadcast closes → producer exits cleanly.
    drop(tx);
    let _ = timeout(Duration::from_secs(2), producer)
        .await
        .expect("producer exited");
}

#[tokio::test(flavor = "current_thread")]
async fn codex_out_of_order_started_completed_does_not_pair() {
    enable_typed_events();
    let tmp = TempDir::new().unwrap();
    let progress_path = tmp.path().join("progress.jsonl");

    let (client, peer, shutdown_tx, shutdown_rx) = duplex_with_shutdown();
    // Completed arrives BEFORE started for the same item_id. The
    // producer bypasses the merger, so no pairing is attempted:
    // both rows land independently, in arrival order.
    let peer_task = spawn_peer(
        peer,
        vec![
            Notification {
                method: "item/completed".into(),
                params: json!({ "item_id": "swap-test" }),
            },
            Notification {
                method: "item/started".into(),
                params: json!({ "item_id": "swap-test" }),
            },
        ],
        shutdown_rx,
    );
    let producer = maybe_start_codex_typed_event_tap(Arc::clone(&client), progress_path.clone())
        .expect("flag enabled");

    wait_for_rows(&progress_path, |rows| rows.len() >= 2).await;

    let rows = read_rows(&progress_path);
    assert_eq!(rows.len(), 2);
    // Order = arrival order; completed first, then started.
    assert_eq!(rows[0]["event_kind"], "tool_call_completed");
    assert_eq!(rows[0]["captured"], "swap-test");
    assert_eq!(rows[1]["event_kind"], "tool_call_started");
    assert_eq!(rows[1]["captured"], "swap-test");

    let _ = shutdown_tx.send(());
    peer_task.await.unwrap();
    drop(client);
    let _ = timeout(Duration::from_secs(2), producer).await;
}

#[tokio::test(flavor = "current_thread")]
async fn codex_unmapped_methods_do_not_emit() {
    enable_typed_events();
    let tmp = TempDir::new().unwrap();
    let progress_path = tmp.path().join("progress.jsonl");

    let (client, peer, shutdown_tx, shutdown_rx) = duplex_with_shutdown();
    let peer_task = spawn_peer(
        peer,
        vec![
            Notification {
                method: "turn/plan/updated".into(),
                params: json!({ "plan": [] }),
            },
            Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({ "delta": "hello" }),
            },
            // Real mapped event at the tail so we can `wait_for_rows`
            // on something — confirms the producer didn't crash on the
            // unmapped events, just skipped them.
            Notification {
                method: "turn/completed".into(),
                params: json!({}),
            },
        ],
        shutdown_rx,
    );
    let producer = maybe_start_codex_typed_event_tap(Arc::clone(&client), progress_path.clone())
        .expect("flag enabled");

    wait_for_rows(&progress_path, |rows| {
        rows.iter().any(|r| r["event_kind"] == "turn_done")
    })
    .await;

    let rows = read_rows(&progress_path);
    assert_eq!(rows.len(), 1, "only the tail turn/completed should emit");
    assert_eq!(rows[0]["event_kind"], "turn_done");

    let _ = shutdown_tx.send(());
    peer_task.await.unwrap();
    drop(client);
    let _ = timeout(Duration::from_secs(2), producer).await;
}
