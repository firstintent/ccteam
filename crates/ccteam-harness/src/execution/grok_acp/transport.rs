//! Stdio JSON-RPC 2.0 client for Grok ACP.
//!
//! Modeled on [`crate::execution::codex_jsonrpc`] (req id / pending oneshot /
//! notification broadcast / reader-writer loops) but:
//! - always emits `"jsonrpc":"2.0"`
//! - matches pending responses only by **numeric** outbound ids
//! - tolerates string ids (e.g. grok `"skills-reload"`) without panic
//! - tolerates agent→client inbound requests (default-decline error reply)

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

const NOTIFICATION_BUFFER: usize = 512;
const WRITER_BUFFER: usize = 64;

type Pending = HashMap<i64, oneshot::Sender<Result<Value, JsonRpcError>>>;

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

#[derive(Debug, Clone)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// One live ACP stdio connection (owns the child process).
pub struct AcpTransport {
    out: mpsc::Sender<Vec<u8>>,
    next_id: AtomicI64,
    pending: Arc<Mutex<Pending>>,
    notifications: broadcast::Sender<Notification>,
    _writer_task: JoinHandle<()>,
    _reader_task: JoinHandle<()>,
    child: StdMutex<Option<Child>>,
    closed: Arc<tokio::sync::Notify>,
}

impl AcpTransport {
    /// Spawn `program` with `args` and speak JSON-RPC over its stdio.
    pub async fn spawn_command(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
    ) -> Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {program} {:?}", args))?;

        // Drain stderr so a full pipe never stalls the child.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "grok_acp", %line, "grok stderr");
                }
            });
        }

        let stdout = child
            .stdout
            .take()
            .context("grok agent stdio stdout unavailable")?;
        let stdin = child
            .stdin
            .take()
            .context("grok agent stdio stdin unavailable")?;
        Ok(Self::from_halves(stdout, stdin, Some(child)))
    }

    /// Build around arbitrary halves (tests use duplex).
    pub fn from_halves<R, W>(reader: R, writer: W, child: Option<Child>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, _) = broadcast::channel(NOTIFICATION_BUFFER);
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(WRITER_BUFFER);
        let closed = Arc::new(tokio::sync::Notify::new());

        let writer_task = tokio::spawn(run_writer_loop(writer, out_rx));
        let reader_task = tokio::spawn(run_reader_loop(
            reader,
            Arc::clone(&pending),
            notif_tx.clone(),
            out_tx.clone(),
            Arc::clone(&closed),
        ));

        Self {
            out: out_tx,
            next_id: AtomicI64::new(1),
            pending,
            notifications: notif_tx,
            _writer_task: writer_task,
            _reader_task: reader_task,
            child: StdMutex::new(child),
            closed,
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().await;
            guard.insert(id, tx);
        }

        let mut frame = json!({
            "jsonrpc": "2.0",
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
                "jsonrpc reader dropped pending id={id} method={method}"
            )),
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let mut frame = json!({
            "jsonrpc": "2.0",
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
            .with_context(|| format!("send jsonrpc notification {method}"))?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    pub async fn wait_closed(&self) {
        self.closed.notified().await;
    }

    /// Close stdin (drop writer by aborting writer after drain) + wait/kill child.
    pub async fn shutdown(&self) -> Result<()> {
        // Drop pending callers.
        {
            let mut guard = self.pending.lock().await;
            for (_, tx) in guard.drain() {
                let _ = tx.send(Err(JsonRpcError {
                    code: None,
                    message: "transport shutting down".into(),
                    data: None,
                }));
            }
        }
        self._writer_task.abort();
        self._reader_task.abort();
        self.closed.notify_waiters();

        let child = {
            let mut guard = self
                .child
                .lock()
                .map_err(|_| anyhow!("grok child mutex poisoned"))?;
            guard.take()
        };
        if let Some(mut child) = child {
            // Best-effort graceful: try wait briefly then kill.
            match tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await {
                Ok(Ok(_)) => {}
                _ => {
                    let _ = child.kill().await;
                }
            }
        }
        Ok(())
    }
}

async fn run_writer_loop<W: AsyncWrite + Unpin + Send>(
    mut writer: W,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(buf) = rx.recv().await {
        if let Err(err) = writer.write_all(&buf).await {
            tracing::warn!(error = %err, "grok_acp: writer error");
            break;
        }
        if let Err(err) = writer.flush().await {
            tracing::warn!(error = %err, "grok_acp: writer flush error");
        }
    }
    let _ = writer.shutdown().await;
}

async fn run_reader_loop<R: AsyncRead + Unpin + Send>(
    reader: R,
    pending: Arc<Mutex<Pending>>,
    notifications: broadcast::Sender<Notification>,
    out: mpsc::Sender<Vec<u8>>,
    closed: Arc<tokio::sync::Notify>,
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
                        tracing::warn!(error = %err, line = %trimmed, "grok_acp: parse failure");
                    }
                }
            }
            Ok(None) => {
                fail_pending(&pending, "jsonrpc peer closed").await;
                closed.notify_waiters();
                break;
            }
            Err(err) => {
                tracing::warn!(error = %err, "grok_acp: read error");
                fail_pending(&pending, &format!("jsonrpc read error: {err}")).await;
                closed.notify_waiters();
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
    let method = v.get("method").and_then(|m| m.as_str());
    // Numeric id only matches our outbound requests.
    let numeric_id = v.get("id").and_then(|x| x.as_i64());
    let has_string_id = v.get("id").map(|x| x.is_string()).unwrap_or(false);

    // Response to our request: numeric id + result/error, no method.
    if let Some(id) = numeric_id {
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
                tracing::debug!(id, "grok_acp: response for unknown numeric id");
            }
            return;
        }

        // Inbound agent→client request (id + method). Default-decline (W5 HITL later).
        if let Some(method) = method {
            tracing::info!(
                id,
                method,
                "grok_acp: inbound request; default-decline (HITL is W5)"
            );
            let reply = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!(
                        "ccteam: inbound request '{method}' not handled (default-decline)"
                    ),
                },
            });
            if let Ok(mut line) = serde_json::to_vec(&reply) {
                line.push(b'\n');
                let _ = out.send(line).await;
            }
            return;
        }
    }

    // String-id frames we never sent (e.g. skills-reload) — skip, no panic.
    if has_string_id && method.is_none() {
        tracing::debug!(
            id = ?v.get("id"),
            "grok_acp: ignoring string-id response we did not send"
        );
        return;
    }

    // Notification: method, typically no id (or ignore id for push).
    if let Some(method) = method {
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let _ = notifications.send(Notification {
            method: method.to_string(),
            params,
        });
        return;
    }

    tracing::debug!(value = %v, "grok_acp: unrecognised frame");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn call_includes_jsonrpc_2_0() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = AcpTransport::from_halves(client_r, client_w, None);
        let peer = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let (pr, mut pw) = tokio::io::split(&mut peer_rw);
            let mut pr = BufReader::new(pr);
            let mut buf = String::new();
            pr.read_line(&mut buf).await.unwrap();
            let req: Value = serde_json::from_str(buf.trim()).unwrap();
            assert_eq!(req["jsonrpc"], "2.0");
            assert_eq!(req["method"], "initialize");
            let id = req["id"].as_i64().unwrap();
            let resp = json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1}});
            let mut line = serde_json::to_vec(&resp).unwrap();
            line.push(b'\n');
            pw.write_all(&line).await.unwrap();
        });
        let result = client.call("initialize", json!({})).await.unwrap();
        assert_eq!(result["protocolVersion"], 1);
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn string_id_response_does_not_panic() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = AcpTransport::from_halves(client_r, client_w, None);
        let peer = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let (pr, mut pw) = tokio::io::split(&mut peer_rw);
            let mut pr = BufReader::new(pr);
            // Spontaneous string-id result (skills-reload).
            let noise = json!({"jsonrpc":"2.0","id":"skills-reload","result":{}});
            let mut line = serde_json::to_vec(&noise).unwrap();
            line.push(b'\n');
            pw.write_all(&line).await.unwrap();
            // Then answer a real request.
            let mut buf = String::new();
            pr.read_line(&mut buf).await.unwrap();
            let req: Value = serde_json::from_str(buf.trim()).unwrap();
            let id = req["id"].as_i64().unwrap();
            let resp = json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}});
            let mut line = serde_json::to_vec(&resp).unwrap();
            line.push(b'\n');
            pw.write_all(&line).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let result = client.call("ping", Value::Null).await.unwrap();
        assert_eq!(result["ok"], true);
        peer.await.unwrap();
    }
}
