//! Stdio JSON-RPC 2.0 client for ACP vendors (Grok, OpenCode, …).
//!
//! Modeled on [`crate::execution::codex_jsonrpc`] (req id / pending oneshot /
//! notification broadcast / reader-writer loops) but:
//! - always emits `"jsonrpc":"2.0"`
//! - matches pending responses only by **numeric** outbound ids
//! - tolerates string ids (e.g. grok `"skills-reload"`) without panic
//! - inbound agent→client requests follow [`InboundPolicy`]
//!   (OpenCode skip must auto-allow `session/request_permission` —
//!   not implementing it is treated as reject by opencode)

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

const NOTIFICATION_BUFFER: usize = 512;
const WRITER_BUFFER: usize = 64;
const SCRUBBED_CHILD_ENV_KEYS: &[&str] = &["DSH_SYSTEM_PROMPT"];

type Pending = HashMap<i64, oneshot::Sender<Result<Value, JsonRpcError>>>;

/// Removes an outstanding correlation entry when a caller future is cancelled
/// (for example by the Gateway submit timeout) before the peer replies.
struct PendingRegistration {
    id: i64,
    pending: Arc<StdMutex<Pending>>,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// One-shot stateful barrier released after an ACP request enters the
/// transport's FIFO writer queue. A watch value lets every current and future
/// waiter observe either `sent` or `failed`, so concurrent interjections cannot
/// consume a single permit and strand the rest.
#[derive(Debug)]
pub struct AcpWriteBarrier {
    state: tokio::sync::watch::Sender<u8>,
}

impl Default for AcpWriteBarrier {
    fn default() -> Self {
        let (state, _) = tokio::sync::watch::channel(0);
        Self { state }
    }
}

impl AcpWriteBarrier {
    const SENT: u8 = 1;
    const FAILED: u8 = 2;

    pub async fn wait(&self) -> Result<()> {
        let mut state = self.state.subscribe();
        loop {
            match *state.borrow_and_update() {
                Self::SENT => return Ok(()),
                Self::FAILED => return Err(anyhow!("owning ACP request was not queued")),
                _ => {}
            }
            state
                .changed()
                .await
                .map_err(|_| anyhow!("owning ACP request barrier closed"))?;
        }
    }

    fn release(&self, sent: bool) {
        self.state
            .send_replace(if sent { Self::SENT } else { Self::FAILED });
    }
}

/// How to answer agent→client JSON-RPC requests on this transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InboundPolicy {
    /// Reply with JSON-RPC method-not-found (Grok uses `--always-approve`
    /// so permission is rare; default-safe for unknown methods).
    #[default]
    DefaultDecline,
    /// Auto-allow `session/request_permission` with optionId `once`
    /// (or the first allow-like option). All other inbound methods decline.
    /// **Required for OpenCode skip sessions** — not implementing the method
    /// causes opencode to auto-reject every tool.
    AutoAllowPermission,
}

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
    pending: Arc<StdMutex<Pending>>,
    notifications: broadcast::Sender<Notification>,
    /// Notifications that arrived before anyone subscribed. A broadcast
    /// channel drops those, and the ACP handshake is exactly when they come:
    /// `session/new` answers, then the vendor immediately pushes its command
    /// catalog / initial usage — all before the adapter has a session to hang
    /// a dispatcher on. Held here and handed to the first subscriber.
    early: Arc<StdMutex<Vec<Notification>>>,
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
        Self::spawn_command_full(program, args, cwd, &[], InboundPolicy::DefaultDecline).await
    }

    /// Spawn with an explicit inbound request policy (OpenCode skip →
    /// [`InboundPolicy::AutoAllowPermission`]).
    pub async fn spawn_command_with_policy(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
        inbound: InboundPolicy,
    ) -> Result<Self> {
        Self::spawn_command_full(program, args, cwd, &[], inbound).await
    }

    /// Full-parameter spawn: `envs` are set on the child only — the daemon's
    /// own environment is never mutated (e.g. Grok's
    /// `GROK_CLAUDE_MCPS_ENABLED=false` compat kill-switch).
    pub async fn spawn_command_full(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
        envs: &[(String, String)],
        inbound: InboundPolicy,
    ) -> Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for key in SCRUBBED_CHILD_ENV_KEYS {
            cmd.env_remove(key);
        }
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {program} {:?}", args))?;

        // Drain stderr so a full pipe never stalls the child.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "acp", %line, "acp stderr");
                }
            });
        }

        let stdout = child
            .stdout
            .take()
            .context("acp stdio stdout unavailable")?;
        let stdin = child.stdin.take().context("acp stdio stdin unavailable")?;
        Ok(Self::from_halves_with_policy(
            stdout,
            stdin,
            Some(child),
            inbound,
        ))
    }

    /// Spawn for a MANAGED session: [`Self::spawn_command_full`] plus the
    /// child pid recorded in [`crate::execution::vendor_pids`] BEFORE any
    /// handshake I/O. The vendor dials `/mcp` from inside `session/new`, so a
    /// pid recorded after the handshake returns misses the very `initialize`
    /// provenance auth exists to identify — this constructor makes the
    /// ordering impossible to get wrong at a call site.
    pub async fn spawn_for_session(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
        envs: &[(String, String)],
        inbound: InboundPolicy,
        sid: &str,
    ) -> Result<Self> {
        let transport = Self::spawn_command_full(program, args, cwd, envs, inbound).await?;
        crate::execution::vendor_pids::record(sid, transport.pid());
        Ok(transport)
    }

    /// The child's OS pid, while the transport still holds it.
    pub fn pid(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|child| child.id()))
    }

    /// Build around arbitrary halves (tests use duplex).
    pub fn from_halves<R, W>(reader: R, writer: W, child: Option<Child>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_halves_with_policy(reader, writer, child, InboundPolicy::DefaultDecline)
    }

    pub fn from_halves_with_policy<R, W>(
        reader: R,
        writer: W,
        child: Option<Child>,
        inbound: InboundPolicy,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Arc<StdMutex<Pending>> = Arc::new(StdMutex::new(HashMap::new()));
        let (notif_tx, _) = broadcast::channel(NOTIFICATION_BUFFER);
        let early: Arc<StdMutex<Vec<Notification>>> = Arc::new(StdMutex::new(Vec::new()));
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(WRITER_BUFFER);
        let closed = Arc::new(tokio::sync::Notify::new());

        let writer_task = tokio::spawn(run_writer_loop(writer, out_rx));
        let reader_task = tokio::spawn(run_reader_loop(
            reader,
            Arc::clone(&pending),
            notif_tx.clone(),
            Arc::clone(&early),
            out_tx.clone(),
            Arc::clone(&closed),
            inbound,
        ));

        Self {
            out: out_tx,
            next_id: AtomicI64::new(1),
            pending,
            notifications: notif_tx,
            early,
            _writer_task: writer_task,
            _reader_task: reader_task,
            child: StdMutex::new(child),
            closed,
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_inner(method, params, None).await
    }

    /// Send one request and release `write_barrier` after its frame has entered
    /// the single FIFO writer queue. A later interjection can await this barrier
    /// to guarantee the owning `session/prompt` is ordered first on the wire.
    pub async fn call_with_write_barrier(
        &self,
        method: &str,
        params: Value,
        write_barrier: Arc<AcpWriteBarrier>,
    ) -> Result<Value> {
        self.call_inner(method, params, Some(write_barrier)).await
    }

    async fn call_inner(
        &self,
        method: &str,
        params: Value,
        write_barrier: Option<Arc<AcpWriteBarrier>>,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(id, tx);
        }
        let _registration = PendingRegistration {
            id,
            pending: Arc::clone(&self.pending),
        };

        let mut frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        if !params.is_null() {
            frame["params"] = params;
        }
        let mut line = match serde_json::to_vec(&frame) {
            Ok(line) => line,
            Err(err) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                if let Some(barrier) = write_barrier {
                    barrier.release(false);
                }
                return Err(err.into());
            }
        };
        line.push(b'\n');

        let send = self.out.send(line).await;
        if let Some(barrier) = write_barrier {
            barrier.release(send.is_ok());
        }
        if let Err(err) = send {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(err).with_context(|| format!("send jsonrpc request {method}"));
        }

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

    /// Subscribe AND take everything that arrived before this call.
    ///
    /// The adapter can only build its dispatcher after the handshake returns a
    /// session, so plain `subscribe` silently loses whatever the vendor pushed
    /// during it — for kimi that is its entire `available_commands_update`,
    /// sent once and never repeated. Callers that need the full stream from
    /// byte zero use this instead.
    pub fn subscribe_with_early(&self) -> (Vec<Notification>, broadcast::Receiver<Notification>) {
        let mut early = self.early.lock().unwrap_or_else(|e| e.into_inner());
        // Created under the lock: any dispatch after this sees a receiver and
        // broadcasts instead of buffering.
        let rx = self.notifications.subscribe();
        (std::mem::take(&mut *early), rx)
    }

    pub async fn wait_closed(&self) {
        self.closed.notified().await;
    }

    /// Close stdin (drop writer by aborting writer after drain) + wait/kill child.
    pub async fn shutdown(&self) -> Result<()> {
        // Drop pending callers.
        {
            let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
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
                .map_err(|_| anyhow!("acp child mutex poisoned"))?;
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
            tracing::warn!(error = %err, "acp: writer error");
            break;
        }
        if let Err(err) = writer.flush().await {
            tracing::warn!(error = %err, "acp: writer flush error");
        }
    }
    let _ = writer.shutdown().await;
}

async fn run_reader_loop<R: AsyncRead + Unpin + Send>(
    reader: R,
    pending: Arc<StdMutex<Pending>>,
    notifications: broadcast::Sender<Notification>,
    early: Arc<StdMutex<Vec<Notification>>>,
    out: mpsc::Sender<Vec<u8>>,
    closed: Arc<tokio::sync::Notify>,
    inbound: InboundPolicy,
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
                    Ok(v) => dispatch(v, &pending, &notifications, &early, &out, inbound).await,
                    Err(err) => {
                        tracing::warn!(error = %err, line = %trimmed, "acp: parse failure");
                    }
                }
            }
            Ok(None) => {
                fail_pending(&pending, "jsonrpc peer closed").await;
                closed.notify_waiters();
                break;
            }
            Err(err) => {
                tracing::warn!(error = %err, "acp: read error");
                fail_pending(&pending, &format!("jsonrpc read error: {err}")).await;
                closed.notify_waiters();
                break;
            }
        }
    }
}

async fn fail_pending(pending: &Arc<StdMutex<Pending>>, message: &str) {
    let drained = {
        let mut guard = pending.lock().unwrap_or_else(|e| e.into_inner());
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
    pending: &Arc<StdMutex<Pending>>,
    notifications: &broadcast::Sender<Notification>,
    early: &Arc<StdMutex<Vec<Notification>>>,
    out: &mpsc::Sender<Vec<u8>>,
    inbound: InboundPolicy,
) {
    let method = v.get("method").and_then(|m| m.as_str());
    // Numeric id only matches our outbound requests.
    let numeric_id = v.get("id").and_then(|x| x.as_i64());
    let has_string_id = v.get("id").map(|x| x.is_string()).unwrap_or(false);

    // Response to our request: numeric id + result/error, no method.
    if let Some(id) = numeric_id {
        if method.is_none() && (v.get("result").is_some() || v.get("error").is_some()) {
            let tx = {
                pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
            };
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
                tracing::debug!(id, "acp: response for unknown numeric id");
            }
            return;
        }

        // Inbound agent→client request (id + method).
        if let Some(method) = method {
            let reply = inbound_reply(id, method, v.get("params").unwrap_or(&Value::Null), inbound);
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
            "acp: ignoring string-id response we did not send"
        );
        return;
    }

    // Notification: method, typically no id (or ignore id for push).
    if let Some(method) = method {
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let n = Notification {
            method: method.to_string(),
            params,
        };
        // Buffer-or-broadcast decided under the same lock `subscribe_with_early`
        // takes, so a notification is delivered exactly once: never dropped for
        // want of a subscriber, never replayed to one that already saw it.
        {
            let mut early = early.lock().unwrap_or_else(|e| e.into_inner());
            if notifications.receiver_count() == 0 {
                if early.len() < NOTIFICATION_BUFFER {
                    early.push(n);
                } else {
                    tracing::warn!(
                        method,
                        "acp: dropping pre-subscription notification (backlog full)"
                    );
                }
                return;
            }
        }
        let _ = notifications.send(n);
        return;
    }

    tracing::debug!(value = %v, "acp: unrecognised frame");
}

fn inbound_reply(id: i64, method: &str, params: &Value, policy: InboundPolicy) -> Value {
    let is_permission =
        method == "session/request_permission" || method.ends_with("request_permission");
    match policy {
        InboundPolicy::AutoAllowPermission if is_permission => {
            let option_id = pick_allow_option_id(params);
            tracing::info!(
                id,
                method,
                %option_id,
                "acp: inbound permission auto-allow (skip policy)"
            );
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "outcome": {
                        "outcome": "selected",
                        "optionId": option_id
                    }
                }
            })
        }
        _ => {
            tracing::info!(id, method, ?policy, "acp: inbound request; default-decline");
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!(
                        "ccteam: inbound request '{method}' not handled (default-decline)"
                    ),
                },
            })
        }
    }
}

/// Prefer `once` / `allow_once`; fall back to first option with an id; else `"once"`.
fn pick_allow_option_id(params: &Value) -> String {
    let options = params
        .get("options")
        .and_then(|o| o.as_array())
        .cloned()
        .unwrap_or_default();
    for preferred in ["once", "allow_once", "allow", "always", "allow_always"] {
        for opt in &options {
            if opt.get("optionId").and_then(|v| v.as_str()) == Some(preferred)
                || opt.get("id").and_then(|v| v.as_str()) == Some(preferred)
            {
                return preferred.to_string();
            }
        }
    }
    for opt in &options {
        if let Some(id) = opt
            .get("optionId")
            .or_else(|| opt.get("id"))
            .and_then(|v| v.as_str())
        {
            return id.to_string();
        }
    }
    "once".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn write_barrier_releases_all_current_and_future_waiters() {
        let barrier = Arc::new(AcpWriteBarrier::default());
        let first = {
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move { barrier.wait().await })
        };
        let second = {
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move { barrier.wait().await })
        };
        tokio::task::yield_now().await;
        barrier.release(true);
        tokio::time::timeout(std::time::Duration::from_secs(1), first)
            .await
            .expect("first waiter released")
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .expect("second waiter released")
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(10), barrier.wait())
            .await
            .expect("future waiter observes released state")
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_call_removes_pending_correlation() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = Arc::new(AcpTransport::from_halves(client_r, client_w, None));
        let call = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.call("session/prompt", json!({})).await }
        });

        use tokio::io::{AsyncBufReadExt, BufReader};
        let (peer_r, _peer_w) = tokio::io::split(&mut peer_rw);
        let mut peer_r = BufReader::new(peer_r);
        let mut line = String::new();
        peer_r.read_line(&mut line).await.unwrap();
        assert_eq!(
            client
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            1
        );

        call.abort();
        let _ = call.await;
        assert!(
            client
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "cancelled caller must not leak a pending request id"
        );
    }

    /// A vendor pushes its command catalog during the handshake, before the
    /// adapter has a session to hang a dispatcher on. A plain broadcast drops
    /// that (kimi sends `available_commands_update` exactly once), so the
    /// transport holds pre-subscription notifications — and hands each one over
    /// exactly once: nothing lost before the subscriber, nothing replayed after.
    #[tokio::test]
    async fn pre_subscription_notifications_are_delivered_exactly_once() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(4096);
        let (client_r, client_w) = tokio::io::split(client_rw);
        let client = AcpTransport::from_halves(client_r, client_w, None);

        let push = |v: Value| {
            let mut line = serde_json::to_vec(&v).unwrap();
            line.push(b'\n');
            line
        };
        {
            use tokio::io::AsyncWriteExt;
            peer_rw
                .write_all(&push(json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {"update": {"sessionUpdate": "available_commands_update"}}
                })))
                .await
                .unwrap();
            peer_rw.flush().await.unwrap();
        }
        // Let the reader loop take it while nobody is listening.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let (early, mut sub) = client.subscribe_with_early();
        assert_eq!(early.len(), 1, "handshake notification must be retained");
        assert_eq!(early[0].method, "session/update");

        // A second subscriber must NOT see it again — the backlog is consumed.
        let (early_again, _) = client.subscribe_with_early();
        assert!(early_again.is_empty(), "backlog must not be replayed");

        // With a live subscriber, delivery goes back to the broadcast path.
        {
            use tokio::io::AsyncWriteExt;
            peer_rw
                .write_all(&push(json!({
                    "jsonrpc": "2.0", "method": "session/update", "params": {"n": 2}
                })))
                .await
                .unwrap();
            peer_rw.flush().await.unwrap();
        }
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv())
            .await
            .expect("notification must arrive")
            .expect("channel open");
        assert_eq!(next.params["n"], 2);
    }

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
