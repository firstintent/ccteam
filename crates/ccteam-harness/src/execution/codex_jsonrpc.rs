//! Thin JSON-RPC client over a Unix Domain Socket
//! for talking to `codex app-server --listen unix://<sock>`.
//!
//! Codex's wire format is "JSON-RPC lite" — line-delimited JSON, no
//! `jsonrpc: "2.0"` discriminator. Each line is either:
//!
//! - **Request**:      `{ "id": <int>, "method": "<m>", "params": {...} }`
//! - **Response**:     `{ "id": <int>, "result": {...} }`
//! - **Response err**: `{ "id": <int>, "error": {...} }`
//! - **Notification**: `{ "method": "<m>", "params": {...} }` (no `id`)
//!
//! ## Concurrency shape
//!
//! Each connection owns two background tasks:
//!
//! - **Writer task** — drains an `mpsc<Vec<u8>>` of pre-serialised
//!   JSONL frames into the socket. Letting callers push bytes via
//!   channel (instead of holding a `Mutex<WriteHalf>`) keeps `call()`
//!   non-blocking even when the kernel buffer back-pressures.
//! - **Reader task** — parses each inbound line, then dispatches to
//!   pending oneshots (responses) or the broadcast (notifications).
//!
//! Tests drive the client against a hand-rolled UDS scripted server in
//! `tests/codex_jsonrpc_test.rs` so we don't depend on a real codex
//! `app-server` process.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

/// Broadcast buffer size for incoming notifications. Codex bursts items
/// per turn so 256 is comfortable headroom for a single subscriber.
const NOTIFICATION_BUFFER: usize = 256;

/// Outbound buffer size: how many JSONL frames may be queued by `call()`
/// before backpressure. 64 is plenty — callers serialise per-turn.
const WRITER_BUFFER: usize = 64;

type Pending = HashMap<i64, oneshot::Sender<Result<Value, JsonRpcError>>>;

/// Parsed JSON-RPC error body. `code` is optional in codex's "lite"
/// dialect; only `message` is mandatory.
#[derive(Debug, Clone)]
pub struct JsonRpcError {
    pub code: Option<i64>,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(c) => write!(f, "jsonrpc error {c}: {}", self.message),
            None => write!(f, "jsonrpc error: {}", self.message),
        }
    }
}

impl std::error::Error for JsonRpcError {}

/// Server → client notification (`method` + `params`, no `id`).
#[derive(Debug, Clone)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// Thin JSON-RPC client over an existing connection (UDS today; the
/// surface is transport-agnostic — reader/writer task ownership of the
/// concrete halves keeps the public API channel-based, so a future
/// stdio / TCP transport drops in without API churn).
pub struct CodexJsonRpcClient {
    out: mpsc::Sender<Vec<u8>>,
    next_id: AtomicI64,
    pending: Arc<Mutex<Pending>>,
    notifications: broadcast::Sender<Notification>,
    _writer_task: JoinHandle<()>,
    _reader_task: JoinHandle<()>,
    _child: Option<Child>,
}

impl CodexJsonRpcClient {
    /// Connect to `codex app-server` over a UDS socket. Both reader and
    /// writer background tasks are spawned immediately.
    pub async fn connect_uds(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path).await.with_context(|| {
            format!("connect codex app-server UDS at {}", socket_path.display())
        })?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self::spawn(read_half, write_half))
    }

    /// Spawn `codex app-server --listen stdio://` and speak JSON-RPC
    /// over its stdio pipes. This is used as a real-binary fallback for
    /// npm-managed Codex installs whose foreground `unix://` listener is
    /// not the same raw JSONL control protocol exposed by the standalone
    /// daemon socket.
    pub async fn connect_stdio_command(program: &str) -> Result<Self> {
        let mut child = Command::new(program)
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("spawn {program} app-server --listen stdio://"))?;
        let stdout = child
            .stdout
            .take()
            .context("codex app-server stdio stdout unavailable")?;
        let stdin = child
            .stdin
            .take()
            .context("codex app-server stdio stdin unavailable")?;
        Ok(Self::spawn_with_child(stdout, stdin, Some(child)))
    }

    /// Build a client around an arbitrary split read/write pair.
    /// Tests use this with a `tokio::io::duplex` pair for scripted
    /// peers.
    pub fn spawn<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::spawn_with_child(reader, writer, None)
    }

    fn spawn_with_child<R, W>(reader: R, writer: W, child: Option<Child>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, _) = broadcast::channel(NOTIFICATION_BUFFER);
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(WRITER_BUFFER);

        let writer_task = tokio::spawn(run_writer_loop(writer, out_rx));
        let reader_task = tokio::spawn(run_reader_loop(
            reader,
            Arc::clone(&pending),
            notif_tx.clone(),
            out_tx.clone(),
        ));

        Self {
            out: out_tx,
            next_id: AtomicI64::new(1),
            pending,
            notifications: notif_tx,
            _writer_task: writer_task,
            _reader_task: reader_task,
            _child: child,
        }
    }

    /// Issue a JSON-RPC request and await the matching response. `params`
    /// may be `Value::Null` for no-param methods (codex tolerates either
    /// omission or null).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().await;
            guard.insert(id, tx);
        }

        let mut frame = json!({
            "id": id,
            "method": method,
        });
        if !params.is_null() {
            frame["params"] = params;
        }
        let mut line = serde_json::to_vec(&frame)?;
        line.push(b'\n');

        self.out
            .send(line)
            .await
            .with_context(|| format!("send jsonrpc request {method}"))?;

        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(_) => Err(anyhow!(
                "jsonrpc reader task dropped pending request id={id} method={method}"
            )),
        }
    }

    /// Subscribe to server-side notifications (push events outside the
    /// request/response cycle). Returns a fresh receiver — multiple
    /// subscribers are supported.
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    /// Send a one-way notification (no id, no response expected).
    /// Codex's app-server protocol doesn't have client-originated
    /// notifications today, but the wire allows them — keep the
    /// helper for future use (e.g. `client/cancel`).
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let mut frame = json!({ "method": method });
        if !params.is_null() {
            frame["params"] = params;
        }
        let mut line = serde_json::to_vec(&frame)?;
        line.push(b'\n');
        self.out
            .send(line)
            .await
            .with_context(|| format!("send jsonrpc notification {method}"))?;
        Ok(())
    }
}

async fn run_writer_loop<W: AsyncWrite + Unpin + Send>(
    mut writer: W,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(buf) = rx.recv().await {
        if let Err(err) = writer.write_all(&buf).await {
            tracing::warn!(error = %err, "jsonrpc: writer error, stopping");
            break;
        }
        if let Err(err) = writer.flush().await {
            tracing::warn!(error = %err, "jsonrpc: writer flush error");
        }
    }
    let _ = writer.shutdown().await;
}

async fn run_reader_loop<R: AsyncRead + Unpin + Send>(
    reader: R,
    pending: Arc<Mutex<Pending>>,
    notifications: broadcast::Sender<Notification>,
    out: mpsc::Sender<Vec<u8>>,
) {
    let buf = BufReader::new(reader);
    let mut lines = buf.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(v) => dispatch(v, &pending, &notifications, &out).await,
                    Err(err) => {
                        tracing::warn!(error = %err, line = %trimmed, "jsonrpc: parse failure");
                    }
                }
            }
            Ok(None) => {
                tracing::debug!("jsonrpc: peer closed");
                fail_pending(&pending, "jsonrpc peer closed").await;
                break;
            }
            Err(err) => {
                tracing::warn!(error = %err, "jsonrpc: read error");
                fail_pending(&pending, &format!("jsonrpc read error: {err}")).await;
                break;
            }
        }
    }
}

async fn fail_pending(pending: &Arc<Mutex<Pending>>, message: &str) {
    let drained = {
        let mut guard = pending.lock().await;
        guard.drain().map(|(_, tx)| tx).collect::<Vec<_>>()
    };
    for tx in drained {
        let _ = tx.send(Err(JsonRpcError {
            code: None,
            message: message.to_string(),
            data: None,
        }));
    }
}

async fn dispatch(
    v: Value,
    pending: &Arc<Mutex<Pending>>,
    notifications: &broadcast::Sender<Notification>,
    out: &mpsc::Sender<Vec<u8>>,
) {
    let id = v.get("id").and_then(|x| x.as_i64());
    let method = v.get("method").and_then(|m| m.as_str());

    // Response (server → client reply to one of our requests): has `id`
    // AND (`result` or `error`), and NO `method`.
    if let Some(id) = id {
        if method.is_none() && (v.get("result").is_some() || v.get("error").is_some()) {
            let tx = { pending.lock().await.remove(&id) };
            if let Some(tx) = tx {
                let outcome = if let Some(err) = v.get("error") {
                    Err(JsonRpcError {
                        code: err.get("code").and_then(|c| c.as_i64()),
                        message: err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("(no message)")
                            .to_string(),
                        data: err.get("data").cloned(),
                    })
                } else {
                    Ok(v.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(outcome);
            } else {
                tracing::debug!(id, "jsonrpc: response for unknown id");
            }
            return;
        }

        // Server-initiated REQUEST: has BOTH `id` AND `method` (no
        // result/error). The Codex app-server BLOCKS the turn until the
        // client replies (W3b catalog §2.2 + §8.3 — sandbox-violation
        // elicitation, file-change approval, MCP elicitation, ...). The
        // previous dispatch matched only `method` and (mis)routed these
        // into the notification broadcast, dropping the `id` — so any
        // Codex turn that asked for approval HUNG until the server timed
        // out.
        //
        // W4 SAFE DEFAULT: reply with a JSON-RPC *error* response keyed by
        // the request `id`. An error reply is the protocol-canonical way to
        // unblock ANY server request — including the complex-payload ones
        // (PermissionsRequestApprovalResponse, ToolRequestUserInputResponse,
        // DynamicToolCallResponse) — without ccteam having to construct a
        // typed decision payload it might get wrong. Codex treats a failed
        // approval callback as "denied / cancel the affected action" rather
        // than auto-approving, which is the conservative, safe direction.
        //
        // W4-FOLLOWUP: route these into the V0.6.1 F98 `plan_decision` HITL
        // flow and reply with the typed `{decision:"decline"}` /
        // `{action:"decline"}` payloads so the user can actually approve via
        // IM round-trip. See docs/versions/v0-8-rmux/w4-codex-in-mux-plan.md.
        if let Some(method) = method {
            tracing::info!(
                id,
                method,
                "jsonrpc: server-initiated request; replying with default-decline \
                 error to unblock the turn (HITL routing is W4-followup)"
            );
            let reply = json!({
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!(
                        "ccteam: server-initiated request '{method}' not yet handled \
                         (default-decline to unblock turn)"
                    ),
                },
            });
            if let Ok(mut line) = serde_json::to_vec(&reply) {
                line.push(b'\n');
                if let Err(err) = out.send(line).await {
                    tracing::warn!(error = %err, "jsonrpc: failed to send default server-request reply");
                }
            }
            return;
        }
    }

    // Notification: has `method` but no `id` (and no result/error).
    if let Some(method) = method {
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let _ = notifications.send(Notification {
            method: method.to_string(),
            params,
        });
        return;
    }
    tracing::debug!(value = %v, "jsonrpc: unrecognised frame");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_error_display_with_code() {
        let e = JsonRpcError {
            code: Some(-32601),
            message: "method not found".into(),
            data: None,
        };
        assert!(e.to_string().contains("-32601"));
        assert!(e.to_string().contains("method not found"));
    }

    #[test]
    fn jsonrpc_error_display_without_code() {
        let e = JsonRpcError {
            code: None,
            message: "boom".into(),
            data: None,
        };
        assert!(e.to_string().contains("boom"));
    }

    /// Smoke-test the spawn + call + notification round trip using
    /// `tokio::io::duplex` for a scripted in-process peer.
    #[tokio::test]
    async fn call_response_roundtrip() {
        // client_side: read = client's reads (peer writes), write = client's writes (peer reads)
        let (client_rw, mut peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = CodexJsonRpcClient::spawn(client_r, client_w);

        // Peer task: read one line, respond with matching id.
        let peer = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let (pr, mut pw) = tokio::io::split(&mut peer_rw);
            let mut pr = BufReader::new(pr);
            let mut buf = String::new();
            pr.read_line(&mut buf).await.unwrap();
            let req: Value = serde_json::from_str(buf.trim()).unwrap();
            let id = req["id"].as_i64().unwrap();
            assert_eq!(req["method"], "thread/start");
            let resp = json!({
                "id": id,
                "result": { "thread": { "thread_id": "t-1" } }
            });
            let mut line = serde_json::to_vec(&resp).unwrap();
            line.push(b'\n');
            pw.write_all(&line).await.unwrap();
            pw.flush().await.unwrap();
            // Also push a notification.
            let notif = json!({
                "method": "thread/started",
                "params": { "thread_id": "t-1" }
            });
            let mut line = serde_json::to_vec(&notif).unwrap();
            line.push(b'\n');
            pw.write_all(&line).await.unwrap();
            pw.flush().await.unwrap();
            // Keep peer alive briefly so notification reaches subscriber.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let mut rx = client.subscribe();
        let result = client
            .call("thread/start", json!({ "cwd": "/tmp" }))
            .await
            .unwrap();
        assert_eq!(result["thread"]["thread_id"], "t-1");

        let notif = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notif.method, "thread/started");
        assert_eq!(notif.params["thread_id"], "t-1");

        peer.await.unwrap();
    }

    #[tokio::test]
    async fn call_error_response_propagates() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = CodexJsonRpcClient::spawn(client_r, client_w);
        let peer = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let (pr, mut pw) = tokio::io::split(&mut peer_rw);
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
        assert!(err.to_string().contains("method not found"));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn peer_close_fails_pending_call() {
        let (client_rw, peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = CodexJsonRpcClient::spawn(client_r, client_w);
        let peer = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let (pr, _pw) = tokio::io::split(peer_rw);
            let mut pr = BufReader::new(pr);
            let mut buf = String::new();
            pr.read_line(&mut buf).await.unwrap();
            drop(pr);
        });

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.call("thread/start", json!({ "cwd": "/tmp" })),
        )
        .await
        .expect("pending call must fail promptly when the peer closes")
        .unwrap_err();
        assert!(err.to_string().contains("jsonrpc peer closed"));
        peer.await.unwrap();
    }
}
