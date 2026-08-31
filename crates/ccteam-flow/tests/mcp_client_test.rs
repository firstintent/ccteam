//! [`McpFlowClient`] against a scripted `POST /mcp` stub.
//!
//! The unit tests inside `mcp_client.rs` cover how ONE answer is read. These
//! cover the part that only exists across several: which requests the client
//! makes, in what order, and with which arguments.
//!
//! The contract worth pinning is the cursor. `await_outcome` must return the
//! answer to the dispatch it is waiting for, and a session that has already
//! spoken once will happily hand back its PREVIOUS answer to a reader that
//! forgot to say where to start. Getting that wrong is silent: every call
//! returns text, just the wrong text, and the schema-retry loop (which follows
//! up in the same session on purpose) would loop on a stale reply forever.
//!
//! No `$HOME`, no daemon, no vendor: a raw HTTP/1.1 stub on loopback.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use ccteam_flow::{FlowClient, HireSpec, McpEndpoint, McpFlowClient};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A scripted `POST /mcp`. `reply` sees each `tools/call` request and returns
/// the tool BODY (what the daemon puts inside `content[0].text`).
struct Stub {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Stub {
    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// Every `tools/call` the client made, in order, as `(name, arguments)`.
    fn tool_calls(&self) -> Vec<(String, Value)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|req| req["method"] == "tools/call")
            .map(|req| {
                (
                    req["params"]["name"].as_str().unwrap_or("").to_string(),
                    req["params"]["arguments"].clone(),
                )
            })
            .collect()
    }
}

async fn spawn_stub<F>(reply: F) -> Stub
where
    F: Fn(&str, &Value) -> Value + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            let reply = Arc::clone(&reply);
            tokio::spawn(async move {
                let Some(req) = read_request(&mut socket).await else {
                    return;
                };
                seen.lock().unwrap().push(req.clone());
                let response = match req["method"].as_str().unwrap_or("") {
                    "initialize" => Some(json!({
                        "jsonrpc": "2.0", "id": req["id"],
                        "result": {
                            "protocolVersion": "2025-06-18",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "ccteam", "version": "test" },
                        },
                    })),
                    // Notifications answer 202 with no body, exactly like the
                    // daemon; a client that mis-handles that hangs here.
                    "notifications/initialized" => None,
                    _ => {
                        let name = req["params"]["name"].as_str().unwrap_or("");
                        let body = reply(name, &req["params"]["arguments"]);
                        Some(json!({
                            "jsonrpc": "2.0", "id": req["id"],
                            "result": {
                                "content": [{ "type": "text", "text": body.to_string() }],
                                "isError": false,
                            },
                        }))
                    }
                };
                let _ = write_response(&mut socket, response).await;
            });
        }
    });
    Stub { addr, requests }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Value> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())?
            })
            .unwrap_or(0);
        if body.len() >= len {
            return serde_json::from_str(&body[..len]).ok();
        }
    }
    None
}

async fn write_response(socket: &mut tokio::net::TcpStream, body: Option<Value>) -> Option<()> {
    let head = match &body {
        Some(_) => "HTTP/1.1 200 OK",
        None => "HTTP/1.1 202 Accepted",
    };
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    // `connection: close` keeps the stub to one request per connection, which
    // is all a test needs and removes keep-alive framing from the picture.
    let response = format!(
        "{head}\r\ncontent-type: application/json\r\nmcp-session-id: ms_stub\r\n\
         connection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.ok()?;
    socket.shutdown().await.ok()
}

fn client(stub: &Stub) -> McpFlowClient {
    McpFlowClient::new(McpEndpoint {
        url: stub.url(),
        bearer: "ccteam-enroll:deadbeefdeadbeef:secret".into(),
        project: "alpha".into(),
    })
    .expect("build client")
}

fn spec(task: &str) -> HireSpec {
    HireSpec {
        task: task.into(),
        vendor: Some(ccteam_harness::AgentVendor::Codex),
        label: Some("review".into()),
        idempotency_key: "flow-tok-0-abc".into(),
        ..HireSpec::default()
    }
}

/// A hire dispatches async and names everything the ledger needs, then the
/// await loop reads the turn that dispatch produced.
#[tokio::test]
async fn a_hire_dispatches_async_and_its_await_returns_the_turn() {
    let stub = spawn_stub(|name, _args| match name {
        "agent" => json!({ "sid": "s9", "turn_id": "s9-1", "status": "pending" }),
        _ => json!({
            "activity": "idle",
            "cost_usd": 0.75,
            "context_pct": 12,
            "cursor": "t1",
            "turns": [{ "turn_id": "t1", "content": "the answer" }],
        }),
    })
    .await;
    let client = client(&stub);

    let hired = client.hire(spec("go")).await.expect("hire");
    assert_eq!(hired.sid, "s9");

    let outcome = client.await_outcome("s9").await.expect("await");
    assert_eq!(outcome.text, "the answer");
    assert_eq!(outcome.cost_usd, Some(0.75));
    assert_eq!(outcome.context_pct, Some(12.0));
    assert!(!outcome.failed);

    let calls = stub.tool_calls();
    assert_eq!(calls[0].0, "agent");
    let hire = &calls[0].1;
    assert_eq!(hire["task"], "go");
    assert_eq!(hire["wait"], 0, "a hire is always async: {hire}");
    assert_eq!(hire["vendor"], "codex");
    assert_eq!(hire["title"], "review", "the ledger label rides along");
    assert_eq!(
        hire["idempotency_key"], "flow-tok-0-abc",
        "the runner's key is what makes a retried hire safe: {hire}",
    );
    assert_eq!(
        hire["project"], "alpha",
        "a user-scoped credential names no project, so every call must: {hire}",
    );
    // A fresh session has no earlier turns, so the read must NOT be anchored.
    assert_eq!(calls[1].0, "agent_read");
    assert!(calls[1].1.get("since").is_none(), "{:?}", calls[1].1);
}

/// The one that bites: a follow-up must anchor its read past the answer the
/// session already gave, or it returns the previous task's reply.
#[tokio::test]
async fn a_follow_up_anchors_its_read_past_the_previous_answer() {
    let stub = spawn_stub(|name, args| match name {
        "agent" => json!({ "sid": "s9", "turn_id": "s9-2", "status": "pending" }),
        _ => {
            // Before the dispatch the client asks where the transcript stands;
            // after it, it pages from there. Answer each shape honestly.
            if args.get("since").and_then(Value::as_str) == Some("t1") {
                json!({
                    "activity": "idle",
                    "cursor": "t2",
                    "turns": [{ "turn_id": "t2", "content": "the second answer" }],
                })
            } else {
                json!({
                    "activity": "idle",
                    "cursor": "t1",
                    "turns": [{ "turn_id": "t1", "content": "the FIRST answer" }],
                })
            }
        }
    })
    .await;
    let client = client(&stub);

    let outcome = client
        .follow_up("s9", "and now the second thing")
        .await
        .expect("follow up");
    assert_eq!(
        outcome.text, "the second answer",
        "a follow-up that returns the previous turn is a silent wrong answer",
    );

    let calls = stub.tool_calls();
    // cursor probe → dispatch → anchored read.
    assert_eq!(calls[0].0, "agent_read");
    assert_eq!(
        calls[0].1["wait"], 0,
        "the probe must not block: {:?}",
        calls[0].1
    );
    assert_eq!(calls[1].0, "agent");
    assert_eq!(calls[1].1["sid"], "s9");
    assert_eq!(calls[1].1["task"], "and now the second thing");
    assert_eq!(calls[2].0, "agent_read");
    assert_eq!(calls[2].1["since"], "t1", "{:?}", calls[2].1);
}

/// The journal's re-attach path: a `done:false` line names a sid this process
/// never hired. Its turn may be long over, so the newest answer IS the answer —
/// waiting for a NEW one would hang forever.
#[tokio::test]
async fn awaiting_a_sid_this_process_never_hired_takes_the_newest_turn() {
    let stub = spawn_stub(|_name, _args| {
        json!({
            "activity": "idle",
            "cursor": "t5",
            "turns": [
                { "turn_id": "t4", "content": "interim" },
                { "turn_id": "t5", "content": "what the previous run was waiting for" },
            ],
        })
    })
    .await;
    let client = client(&stub);

    let outcome = client.await_outcome("s3").await.expect("re-attach");
    assert_eq!(outcome.text, "what the previous run was waiting for");

    let calls = stub.tool_calls();
    assert_eq!(calls.len(), 1, "re-attaching costs one read: {calls:?}");
    assert!(
        calls[0].1.get("since").is_none(),
        "nothing anchors a turn this process did not dispatch: {:?}",
        calls[0].1,
    );
}

/// A worker that failed resolves to a failure the report can explain, and the
/// stop that follows is accepted.
#[tokio::test]
async fn a_failed_turn_and_a_stop_round_trip() {
    let stub = spawn_stub(|name, _args| match name {
        "agent" => json!({ "sid": "s9", "turn_id": "s9-1", "status": "pending" }),
        "agent_stop" => json!({ "sid": "s9", "stopped": true }),
        _ => json!({
            "activity": "idle",
            "cursor": "t1",
            "turns": [{
                "turn_id": "t1", "content": "", "outcome": "failed",
                "error_kind": "server_overloaded",
                "error": "Selected model is at capacity.",
            }],
        }),
    })
    .await;
    let client = client(&stub);

    client.hire(spec("go")).await.expect("hire");
    let outcome = client.await_outcome("s9").await.expect("await");
    assert!(outcome.failed);
    assert_eq!(
        outcome.error_kind.as_deref(),
        Some("server_overloaded: Selected model is at capacity."),
    );
    client.stop("s9").await.expect("stop");
    assert_eq!(stub.tool_calls().last().unwrap().0, "agent_stop");
}

/// `usage()` hands the account snapshot through untouched — the runner takes no
/// decision on it, so it must not reshape it either.
#[tokio::test]
async fn usage_is_the_status_bodys_usage_member_verbatim() {
    let stub = spawn_stub(|_name, _args| {
        json!({
            "project": "alpha",
            "usage": { "accounts": [{ "vendor": "claude", "pct": 42 }] },
        })
    })
    .await;
    let client = client(&stub);

    let usage = client.usage().await.expect("usage");
    assert_eq!(
        usage,
        json!({ "accounts": [{ "vendor": "claude", "pct": 42 }] })
    );
    let calls = stub.tool_calls();
    assert_eq!(calls[0].0, "status");
    assert_eq!(calls[0].1["detail"], "usage");
}
