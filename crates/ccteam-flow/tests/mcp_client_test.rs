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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

    /// Just the `agent_read` arguments, in order.
    fn reads(&self) -> Vec<Value> {
        self.tool_calls()
            .into_iter()
            .filter(|(name, _)| name == "agent_read")
            .map(|(_, args)| args)
            .collect()
    }

    /// How many bindings the client opened. One per client is the contract:
    /// every extra `initialize` is another enrolled ledger node at the daemon.
    fn initializes(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|req| req["method"] == "initialize")
            .count()
    }
}

/// What the stub does with one request.
enum Reply {
    /// 200 carrying this JSON-RPC envelope.
    Json(Value),
    /// The same, after a delay — the stub honouring a long poll's `wait`.
    Slow(Duration, Value),
    /// 202 with no body: how the daemon answers a notification.
    Accepted,
    /// Close the socket without answering — a transport blip.
    Hangup,
}

/// Full control over the JSON-RPC envelope, for the malformed-answer and
/// transport-failure cases. [`spawn_stub`] is this with the well-formed
/// envelope filled in.
async fn spawn_raw_stub<F>(reply: F) -> Stub
where
    F: Fn(&Value) -> Reply + Send + Sync + 'static,
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
                let body = match reply(&req) {
                    Reply::Json(body) => Some(body),
                    Reply::Slow(delay, body) => {
                        tokio::time::sleep(delay).await;
                        Some(body)
                    }
                    Reply::Accepted => None,
                    Reply::Hangup => return,
                };
                let _ = write_response(&mut socket, body).await;
            });
        }
    });
    Stub { addr, requests }
}

async fn spawn_stub<F>(reply: F) -> Stub
where
    F: Fn(&str, &Value) -> Value + Send + Sync + 'static,
{
    spawn_raw_stub(move |req| match req["method"].as_str().unwrap_or("") {
        "initialize" => Reply::Json(initialize_ok(&req["id"])),
        // Notifications answer 202 with no body, exactly like the daemon; a
        // client that mis-handles that hangs here.
        "notifications/initialized" => Reply::Accepted,
        _ => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let body = reply(name, &req["params"]["arguments"]);
            Reply::Json(tool_ok(&req["id"], &body))
        }
    })
    .await
}

fn initialize_ok(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id,
        "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "ccteam", "version": "test" },
        },
    })
}

/// The daemon's shape: the tool body is JSON inside `content[0].text`.
fn tool_ok(id: &Value, body: &Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id,
        "result": {
            "content": [{ "type": "text", "text": body.to_string() }],
            "isError": false,
        },
    })
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

/// A wait killed by transport is NOT a turn boundary. The dispatch is still
/// unanswered, so the retry must resume from the SAME anchor; a client that
/// dropped the cursor re-attaches blind and hands back the session's PREVIOUS
/// answer as this task's result — every call returns text, just the wrong text.
#[tokio::test]
async fn a_wait_killed_by_transport_resumes_from_the_same_cursor() {
    let anchored = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::clone(&anchored);
    let stub = spawn_raw_stub(move |req| {
        match req["method"].as_str().unwrap_or("") {
            "initialize" => return Reply::Json(initialize_ok(&req["id"])),
            "notifications/initialized" => return Reply::Accepted,
            _ => {}
        }
        let id = &req["id"];
        let args = &req["params"]["arguments"];
        if req["params"]["name"] == "agent" {
            return Reply::Json(tool_ok(
                id,
                &json!({ "sid": "s9", "turn_id": "s9-2", "status": "pending" }),
            ));
        }
        match args.get("since").and_then(Value::as_str) {
            // The pre-dispatch cursor probe — and anything else that forgot to
            // anchor, which is the bug: it takes the session's first answer.
            None => Reply::Json(tool_ok(
                id,
                &json!({
                    "activity": "idle",
                    "cursor": "t1",
                    "turns": [{ "turn_id": "t1", "content": "the STALE first answer" }],
                }),
            )),
            // Anchored reads: the first FOUR die on transport — two waits, each
            // spending the one retry `call_tool` allows — and the rest answer
            // honestly. Two in a row on purpose: the anchor has to survive
            // every failed wait, not just the first.
            Some("t1") if attempts.fetch_add(1, Ordering::SeqCst) < 4 => Reply::Hangup,
            Some("t1") => Reply::Json(tool_ok(
                id,
                &json!({
                    "activity": "idle",
                    "cursor": "t2",
                    "turns": [{ "turn_id": "t2", "content": "the answer to the follow-up" }],
                }),
            )),
            Some(_) => Reply::Hangup,
        }
    })
    .await;
    let client = client(&stub);

    let died = client.follow_up("s9", "and now the second thing").await;
    assert!(died.is_err(), "the follow-up's wait died: {died:?}");
    let died_again = client.await_outcome("s9").await;
    assert!(died_again.is_err(), "the retry died too: {died_again:?}");

    let outcome = client.await_outcome("s9").await.expect("resumed wait");
    assert_eq!(
        outcome.text, "the answer to the follow-up",
        "a dead wait must not retire the cursor",
    );

    let reads = stub.reads();
    assert!(
        reads.len() >= 6,
        "probe + four dead reads + a live one: {reads:?}",
    );
    assert!(
        reads[1..].iter().all(|args| args["since"] == "t1"),
        "every read after the probe resumes from the SAME cursor: {reads:?}",
    );
}

/// `stuck` is the daemon's word for a LIVE turn gone silent past the watchdog
/// window, not for a finished one. Accepting it as "turn over" returns the last
/// interim narration row as the final answer.
#[tokio::test]
async fn a_stuck_session_is_still_mid_turn() {
    let polls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&polls);
    let stub = spawn_stub(move |name, _args| match name {
        "agent" => json!({ "sid": "s9", "turn_id": "s9-1", "status": "pending" }),
        _ if seen.fetch_add(1, Ordering::SeqCst) == 0 => json!({
            "activity": "stuck",
            "cursor": "t1",
            "turns": [{ "turn_id": "t1", "content": "still thinking out loud" }],
        }),
        _ => json!({
            "activity": "idle",
            "cursor": "t2",
            "turns": [{ "turn_id": "t2", "content": "the answer" }],
        }),
    })
    .await;
    let client = client(&stub);

    client.hire(spec("go")).await.expect("hire");
    let outcome = client.await_outcome("s9").await.expect("await");
    assert_eq!(
        outcome.text, "the answer",
        "a silent worker has not answered — `stuck` is the same turn, still in flight",
    );
    assert!(
        polls.load(Ordering::SeqCst) >= 2,
        "the client kept polling past the stuck snapshot",
    );
}

/// The turn timeout is the caller's brake, so it has to bound the REQUEST: a
/// 240s long poll consulted only after it returns makes a 2s budget wait four
/// minutes. The stub honours whatever `wait` it is asked for, so a client that
/// still asks for 240 blows the wall-clock assertion.
#[tokio::test]
async fn a_short_turn_timeout_bounds_the_long_poll_it_asks_for() {
    let stub = spawn_raw_stub(|req| {
        match req["method"].as_str().unwrap_or("") {
            "initialize" => return Reply::Json(initialize_ok(&req["id"])),
            "notifications/initialized" => return Reply::Accepted,
            _ => {}
        }
        if req["params"]["name"] == "agent" {
            return Reply::Json(tool_ok(
                &req["id"],
                &json!({ "sid": "s9", "turn_id": "s9-1", "status": "pending" }),
            ));
        }
        // Block for what was asked, bounded so a regression fails the test
        // instead of hanging it.
        let wait = req["params"]["arguments"]["wait"].as_u64().unwrap_or(0);
        Reply::Slow(
            Duration::from_secs(wait.min(20)),
            tool_ok(&req["id"], &json!({ "activity": "working", "turns": [] })),
        )
    })
    .await;
    let client = client(&stub).with_turn_timeout(Duration::from_secs(2));

    client.hire(spec("go")).await.expect("hire");
    let started = std::time::Instant::now();
    let outcome = client.await_outcome("s9").await.expect("await");
    let elapsed = started.elapsed();

    assert!(outcome.failed, "a spent deadline is a failure: {outcome:?}");
    assert_eq!(outcome.error_kind.as_deref(), Some("timeout"));
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline is honoured promptly, not after a full long poll: {elapsed:?}",
    );
    let reads = stub.reads();
    assert!(!reads.is_empty(), "it did poll at least once");
    assert!(
        reads.iter().all(|args| args["wait"].as_u64() <= Some(2)),
        "each poll asks for at most what is left of the deadline: {reads:?}",
    );
}

/// `initialize` is the one call whose answer becomes long-lived state. An
/// envelope with no `result` object is a proxy page or a JSON-RPC error, not a
/// binding — accepting it leaves the client holding a session the server never
/// confirmed and misattributes every later failure.
#[tokio::test]
async fn a_malformed_initialize_is_a_readable_failure() {
    let stub = spawn_raw_stub(|req| match req["method"].as_str().unwrap_or("") {
        "initialize" => Reply::Json(json!({ "jsonrpc": "2.0", "id": req["id"] })),
        _ => Reply::Accepted,
    })
    .await;

    let err = client(&stub)
        .usage()
        .await
        .expect_err("no binding, no call");
    assert!(err.to_string().contains("initialize"), "{err}");
    assert!(err.to_string().contains("result object"), "{err}");
    assert_eq!(stub.initializes(), 1, "it did not retry a malformation");
}

/// A `tools/call` success envelope with no `content[0].text` used to become
/// `Body(Null)` and then "answer carried no sid" — the malformation blamed on
/// the tool. Name it instead.
#[tokio::test]
async fn a_success_envelope_without_a_body_names_the_malformation() {
    let stub = spawn_raw_stub(|req| match req["method"].as_str().unwrap_or("") {
        "initialize" => Reply::Json(initialize_ok(&req["id"])),
        "notifications/initialized" => Reply::Accepted,
        _ => Reply::Json(json!({
            "jsonrpc": "2.0", "id": req["id"],
            "result": { "content": [], "isError": false },
        })),
    })
    .await;

    let err = client(&stub).hire(spec("go")).await.expect_err("malformed");
    assert!(err.to_string().contains("content[0].text"), "{err}");
    assert!(!err.to_string().contains("no sid"), "{err}");
}

/// One client is ONE binding. Concurrent first hires all find `session: None`;
/// without single-flight each opens its own, and the daemon mints an enrolled
/// ledger node per binding — one flow run appearing as several unrelated
/// parents (measured live: leaves under s494/s495/s496).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_hires_open_exactly_one_binding() {
    let stub = spawn_stub(|name, args| match name {
        "agent" => json!({
            "sid": format!("s-{}", args["task"].as_str().unwrap_or("?")),
            "status": "pending",
        }),
        _ => json!({ "activity": "idle", "turns": [] }),
    })
    .await;
    let client = Arc::new(client(&stub));

    let racing: Vec<_> = (0..6)
        .map(|i| {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.hire(spec(&format!("task{i}"))).await })
        })
        .collect();
    for hire in racing {
        hire.await.expect("join").expect("hire");
    }

    assert_eq!(
        stub.initializes(),
        1,
        "six racing hires share one binding: {:?}",
        stub.tool_calls(),
    );
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
