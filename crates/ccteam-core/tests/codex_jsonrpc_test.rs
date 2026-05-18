//! V0.6.0 Wave 3 F112 — `codex_jsonrpc` UDS JSON-RPC client tests.
//!
//! Scripted in-process peer over `tokio::io::duplex` so the wire
//! framing + dispatch logic is exercised without dialing a real
//! `codex app-server` daemon.

use ccteam_core::execution::codex_jsonrpc::{CodexJsonRpcClient, Notification};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Construct a (client, peer_rw) pair where the client is wired to the
/// peer end of a duplex stream. Caller scripts the peer's reads/writes
/// in a spawned task.
fn duplex_pair() -> (CodexJsonRpcClient, tokio::io::DuplexStream) {
    let (client_rw, peer_rw) = tokio::io::duplex(8192);
    let (client_r, client_w) = tokio::io::split(client_rw);
    let client = CodexJsonRpcClient::spawn(client_r, client_w);
    (client, peer_rw)
}

#[tokio::test(flavor = "current_thread")]
async fn call_with_thread_start_response_extracts_thread_id() {
    let (client, mut peer) = duplex_pair();
    let peer_task = tokio::spawn(async move {
        let (pr, mut pw) = tokio::io::split(&mut peer);
        let mut pr = BufReader::new(pr);
        let mut buf = String::new();
        pr.read_line(&mut buf).await.unwrap();
        let req: Value = serde_json::from_str(buf.trim()).unwrap();
        assert_eq!(req["method"], "thread/start");
        let id = req["id"].as_i64().unwrap();
        let resp = json!({
            "id": id,
            "result": { "thread": { "thread_id": "t-abc" }, "model": "gpt-4o" }
        });
        let mut line = serde_json::to_vec(&resp).unwrap();
        line.push(b'\n');
        pw.write_all(&line).await.unwrap();
        pw.flush().await.unwrap();
        // hold open briefly so dispatch fires
        tokio::time::sleep(Duration::from_millis(20)).await;
    });

    let result = client
        .call("thread/start", json!({ "cwd": "/tmp" }))
        .await
        .unwrap();
    assert_eq!(result["thread"]["thread_id"], "t-abc");
    peer_task.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn call_propagates_jsonrpc_error() {
    let (client, mut peer) = duplex_pair();
    let peer_task = tokio::spawn(async move {
        let (pr, mut pw) = tokio::io::split(&mut peer);
        let mut pr = BufReader::new(pr);
        let mut buf = String::new();
        pr.read_line(&mut buf).await.unwrap();
        let req: Value = serde_json::from_str(buf.trim()).unwrap();
        let id = req["id"].as_i64().unwrap();
        let resp = json!({
            "id": id,
            "error": { "code": -32601, "message": "method not found" }
        });
        let mut line = serde_json::to_vec(&resp).unwrap();
        line.push(b'\n');
        pw.write_all(&line).await.unwrap();
        pw.flush().await.unwrap();
    });

    let err = client.call("bogus", Value::Null).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("method not found"), "msg={msg}");
    assert!(msg.contains("-32601"), "msg={msg}");
    peer_task.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_receives_server_notifications() {
    let (client, mut peer) = duplex_pair();
    let mut rx = client.subscribe();

    let peer_task = tokio::spawn(async move {
        let (_pr, mut pw) = tokio::io::split(&mut peer);
        // Push 3 notifications back-to-back.
        for tid in ["t-1", "t-2", "t-3"] {
            let notif = json!({
                "method": "thread/started",
                "params": { "thread_id": tid }
            });
            let mut line = serde_json::to_vec(&notif).unwrap();
            line.push(b'\n');
            pw.write_all(&line).await.unwrap();
            pw.flush().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    });

    let mut seen: Vec<Notification> = Vec::new();
    for _ in 0..3 {
        let n = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        seen.push(n);
    }
    peer_task.await.unwrap();

    assert_eq!(seen.len(), 3);
    for n in &seen {
        assert_eq!(n.method, "thread/started");
    }
    let tids: Vec<&str> = seen
        .iter()
        .map(|n| n.params["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(tids, vec!["t-1", "t-2", "t-3"]);
}

#[tokio::test(flavor = "current_thread")]
async fn interleaved_response_and_notification_demuxed() {
    // The reader must NOT confuse an arriving notification with a
    // pending response (and vice versa) when they overlap.
    let (client, mut peer) = duplex_pair();
    let mut rx = client.subscribe();

    let peer_task = tokio::spawn(async move {
        let (pr, mut pw) = tokio::io::split(&mut peer);
        let mut pr = BufReader::new(pr);
        let mut buf = String::new();
        pr.read_line(&mut buf).await.unwrap();
        let req: Value = serde_json::from_str(buf.trim()).unwrap();
        let id = req["id"].as_i64().unwrap();

        // Push notification BEFORE the response.
        let notif = json!({ "method": "turn/started", "params": { "turn_id": "u-1" } });
        let mut line = serde_json::to_vec(&notif).unwrap();
        line.push(b'\n');
        pw.write_all(&line).await.unwrap();

        let resp = json!({ "id": id, "result": { "ok": true } });
        let mut line = serde_json::to_vec(&resp).unwrap();
        line.push(b'\n');
        pw.write_all(&line).await.unwrap();
        pw.flush().await.unwrap();

        tokio::time::sleep(Duration::from_millis(20)).await;
    });

    let result = client.call("turn/start", json!({})).await.unwrap();
    assert_eq!(result["ok"], true);

    let n = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n.method, "turn/started");

    peer_task.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_lines_are_tolerated() {
    // A garbage line in the middle of the stream must not poison the
    // reader; the next valid line should still match the pending id.
    let (client, mut peer) = duplex_pair();
    let peer_task = tokio::spawn(async move {
        let (pr, mut pw) = tokio::io::split(&mut peer);
        let mut pr = BufReader::new(pr);
        let mut buf = String::new();
        pr.read_line(&mut buf).await.unwrap();
        let req: Value = serde_json::from_str(buf.trim()).unwrap();
        let id = req["id"].as_i64().unwrap();
        // Send garbage line first.
        pw.write_all(b"this is not json\n").await.unwrap();
        // Then valid response.
        let resp = json!({ "id": id, "result": { "fine": true } });
        let mut line = serde_json::to_vec(&resp).unwrap();
        line.push(b'\n');
        pw.write_all(&line).await.unwrap();
        pw.flush().await.unwrap();
    });

    let r = client.call("any", Value::Null).await.unwrap();
    assert_eq!(r["fine"], true);
    peer_task.await.unwrap();
}
