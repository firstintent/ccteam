//! Seam ② (PRD §七) — the stream-json **transport**: bidirectional NDJSON
//! over a child's stdio, exposed so the NDJSON *consumer* (the adapter's
//! `events()` / HITL dispatcher) never holds the [`Child`] handle. The
//! transport is built from a generic `(reader, writer)` pair
//! ([`StreamJsonTransport::spawn_from_io`]) — exactly the seam a v0.9
//! satellite host needs to swap the local pipe for a WebSocket without the
//! consumer noticing (mirrors `codex_jsonrpc`'s proven shape; tests drive
//! it over `tokio::io::duplex`).
//!
//! Concurrency shape (one set of tasks per live session):
//! - **writer task** drains an `mpsc<String>` of NDJSON lines into stdin;
//!   aborting it drops `ChildStdin` → EOF → claude exits gracefully.
//! - **reader task** parses each stdout line into [`Outbound`], captures
//!   the one-time `system:init` into an init slot, routes
//!   `control_response`s to pending waiters, and broadcasts every message.
//! - **stderr task** drains the child's stderr (claude's `--debug` output)
//!   so it can never back-pressure or pollute the NDJSON stream.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Notify};
use tokio::task::JoinHandle;

use super::protocol::{ControlResponseBody, Outbound, SystemMsg};

/// Broadcast buffer for outbound messages. Chat cadence is ≤ a few
/// messages/sec; 256 is comfortable headroom for one slow subscriber.
const OUTBOUND_BUFFER: usize = 256;
/// Writer queue depth before back-pressure (callers serialize per turn).
const WRITER_BUFFER: usize = 64;

/// One-time `system:init` rendezvous: the reader fills it, `wait_for_init`
/// awaits it. Independent of the broadcast (which only delivers
/// post-subscribe), so init is never missed by a late subscriber.
#[derive(Default)]
struct InitSlot {
    payload: StdMutex<Option<SystemMsg>>,
    notify: Notify,
}

type PendingControls = Arc<Mutex<HashMap<String, oneshot::Sender<ControlResponseBody>>>>;

/// Session-close signal: the reader fires this on stdout EOF/error (child
/// death / idle stdin-close). The broadcast sender lives on in the
/// transport, so subscribers would otherwise never observe closure — this
/// is how `events()` knows to terminate its stream.
#[derive(Default)]
struct CloseSignal {
    closed: AtomicBool,
    notify: Notify,
}

/// A live stream-json transport. The consumer holds this; the [`Child`]
/// lives privately inside (only `shutdown` touches it).
pub struct StreamJsonTransport {
    out: mpsc::Sender<String>,
    outbound: broadcast::Sender<Outbound>,
    init: Arc<InitSlot>,
    pending: PendingControls,
    close: Arc<CloseSignal>,
    /// Set by [`Self::detach`]: the daemon let go of the body WITHOUT stopping
    /// it. Readers that would otherwise treat "closed" as "the child exited"
    /// (the body-record clearer) must check this first.
    detached: AtomicBool,
    child: StdMutex<Option<Child>>,
    writer_task: StdMutex<Option<JoinHandle<()>>>,
    reader_task: StdMutex<Option<JoinHandle<()>>>,
    stderr_task: StdMutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for StreamJsonTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamJsonTransport")
            .finish_non_exhaustive()
    }
}

impl StreamJsonTransport {
    /// Spawn `claude` with the given argv/env/cwd and speak stream-json
    /// over its stdio pipes. `argv[0]` is the program; `argv[1..]` the
    /// flags ([`super::spawn_spec::build_argv`]).
    pub async fn connect_stdio(
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<Self> {
        let program = argv.first().context("empty argv")?;
        let mut cmd = Command::new(program);
        cmd.args(&argv[1..])
            .current_dir(cwd)
            .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
            // The body's life is decided EXPLICITLY: `shutdown` kills it (a
            // user stop), `detach` lets it go on living (daemon shutdown).
            // A dropped handle must never decide by itself — with
            // kill_on_drop the outcome of a daemon exit depended on which
            // Arc happened to be released first, and a body that should
            // have finished its turn could be SIGKILLed mid-write.
            .kill_on_drop(false)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {program} (stream-json)"))?;
        let stdout = child
            .stdout
            .take()
            .context("stream-json stdout unavailable")?;
        let stdin = child
            .stdin
            .take()
            .context("stream-json stdin unavailable")?;
        let stderr = child.stderr.take();
        let stderr_task = stderr.map(|e| tokio::spawn(drain_stderr(e)));
        let mut t = Self::spawn_from_io(stdout, stdin, Some(child));
        *t.stderr_task.get_mut().unwrap() = stderr_task;
        Ok(t)
    }

    /// Build a transport around an arbitrary split read/write pair. Tests
    /// pass `tokio::io::duplex` halves + `None` child. This is the WS-ready
    /// seam: a future remote transport supplies WS read/write halves here.
    pub fn spawn_from_io<R, W>(reader: R, writer: W, child: Option<Child>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel::<String>(WRITER_BUFFER);
        let (bcast_tx, _) = broadcast::channel(OUTBOUND_BUFFER);
        let init = Arc::new(InitSlot::default());
        let pending: PendingControls = Arc::new(Mutex::new(HashMap::new()));
        let close = Arc::new(CloseSignal::default());

        let writer_task = tokio::spawn(run_writer(writer, out_rx));
        let reader_task = tokio::spawn(run_reader(
            reader,
            Arc::clone(&init),
            Arc::clone(&pending),
            bcast_tx.clone(),
            Arc::clone(&close),
        ));

        Self {
            out: out_tx,
            outbound: bcast_tx,
            init,
            pending,
            close,
            detached: AtomicBool::new(false),
            child: StdMutex::new(child),
            writer_task: StdMutex::new(Some(writer_task)),
            reader_task: StdMutex::new(Some(reader_task)),
            stderr_task: StdMutex::new(None),
        }
    }

    /// Queue one NDJSON line (no trailing newline needed). Returns an error
    /// only when the writer task is gone (child exited).
    pub async fn send_line(&self, line: String) -> Result<()> {
        self.out
            .send(line)
            .await
            .map_err(|_| anyhow!("stream-json writer closed (child exited)"))
    }

    /// Subscribe to every outbound message (post-subscribe delivery).
    pub fn subscribe(&self) -> broadcast::Receiver<Outbound> {
        self.outbound.subscribe()
    }

    /// Issue a client→CLI `control_request` (e.g. `interrupt`, `set_model`,
    /// `initialize`) and await the correlated `control_response`. The
    /// `request` value is the body MINUS `subtype` (which is supplied
    /// separately). Used by the Wave 2 slash/HITL surface; built here so
    /// the correlation table (`pending`) lives with the reader that fills
    /// it.
    pub async fn request_control(
        &self,
        subtype: &str,
        request: serde_json::Value,
        deadline: Duration,
    ) -> Result<ControlResponseBody> {
        let request_id = mint_request_id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), tx);

        let mut body = request;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("subtype".into(), subtype.into());
        } else {
            body = serde_json::json!({"subtype": subtype});
        }
        let line = serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": body,
        })
        .to_string();

        if let Err(err) = self.send_line(line).await {
            self.pending.lock().await.remove(&request_id);
            return Err(err);
        }
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(body)) => Ok(body),
            Ok(Err(_)) => Err(anyhow!(
                "control request {request_id} cancelled (child exited)"
            )),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                Err(anyhow!("control request {subtype} timed out"))
            }
        }
    }

    /// Await the one-time `system:init`. Returns immediately if it already
    /// arrived; otherwise waits up to `deadline`.
    pub async fn wait_for_init(&self, deadline: Duration) -> Result<SystemMsg> {
        if let Some(p) = self.init.payload.lock().unwrap().clone() {
            return Ok(p);
        }
        let wait = async {
            loop {
                let notified = self.init.notify.notified();
                if let Some(p) = self.init.payload.lock().unwrap().clone() {
                    return p;
                }
                notified.await;
                if let Some(p) = self.init.payload.lock().unwrap().clone() {
                    return p;
                }
            }
        };
        tokio::time::timeout(deadline, wait)
            .await
            .map_err(|_| anyhow!("timed out waiting for claude system:init"))
    }

    /// True once `system:init` has been seen (non-blocking).
    pub fn is_initialized(&self) -> bool {
        self.init.payload.lock().unwrap().is_some()
    }

    /// True once the session has closed (child stdout EOF / death / a
    /// `shutdown`). `events()` polls this to terminate its stream.
    pub fn is_session_closed(&self) -> bool {
        self.close.closed.load(Ordering::Acquire)
    }

    /// Await the close signal (resolves immediately if already closed).
    pub async fn wait_closed(&self) {
        if self.close.closed.load(Ordering::Acquire) {
            return;
        }
        let notified = self.close.notify.notified();
        if self.close.closed.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    /// Graceful stop: abort the writer task (drops `ChildStdin` → EOF →
    /// claude exits), then kill the child as a safety net and abort the
    /// reader/stderr tasks. Idempotent.
    /// The child's OS pid, while the transport still holds it (`None` for a
    /// remote / test transport, or after `shutdown` / `detach`).
    pub fn pid(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|child| child.id()))
    }

    /// True once [`Self::detach`] ran: the session is closed for THIS daemon
    /// but the body lives on.
    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }

    /// Let go of the body WITHOUT stopping it (daemon shutdown): close our
    /// end of stdin (EOF — an idle `claude` exits on it, a busy one finishes
    /// its turn first), stop reading, and drop the child handle with no kill.
    /// Returns the pid the next daemon will find in the body record.
    pub async fn detach(&self) -> Option<u32> {
        self.detached.store(true, Ordering::Release);
        let pid = self.pid();
        self.close.closed.store(true, Ordering::Release);
        self.close.notify.notify_waiters();
        if let Some(h) = self.writer_task.lock().unwrap().take() {
            h.abort();
        }
        // Dropping a tokio `Child` spawned with `kill_on_drop(false)` only
        // hands it to tokio's orphan reaper (no signal is sent).
        let _ = self.child.lock().unwrap().take();
        if let Some(h) = self.reader_task.lock().unwrap().take() {
            h.abort();
        }
        if let Some(h) = self.stderr_task.lock().unwrap().take() {
            h.abort();
        }
        pid
    }

    pub async fn shutdown(&self) {
        // Mark closed first so any `events()` task ends promptly even if
        // the child lingers past the stdin-EOF window.
        self.close.closed.store(true, Ordering::Release);
        self.close.notify.notify_waiters();
        if let Some(h) = self.writer_task.lock().unwrap().take() {
            h.abort();
        }
        // Give the child a brief window to exit on stdin EOF before the
        // safety-net kill.
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        if let Some(h) = self.reader_task.lock().unwrap().take() {
            h.abort();
        }
        if let Some(h) = self.stderr_task.lock().unwrap().take() {
            h.abort();
        }
    }
}

async fn run_writer<W: AsyncWrite + Unpin + Send>(mut writer: W, mut rx: mpsc::Receiver<String>) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        if let Err(err) = writer.write_all(line.as_bytes()).await {
            tracing::warn!(error = %err, "stream-json: stdin write failed");
            break;
        }
        if let Err(err) = writer.flush().await {
            tracing::warn!(error = %err, "stream-json: stdin flush failed");
            break;
        }
    }
    let _ = writer.shutdown().await;
}

async fn run_reader<R: AsyncRead + Unpin + Send>(
    reader: R,
    init: Arc<InitSlot>,
    pending: PendingControls,
    outbound: broadcast::Sender<Outbound>,
    close: Arc<CloseSignal>,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: Outbound = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::warn!(error = %err, line = %trimmed, "stream-json: parse failure");
                        continue;
                    }
                };
                // Capture the one-time init.
                if let Outbound::System(ref sys) = parsed {
                    if sys.is_init() {
                        *init.payload.lock().unwrap() = Some((**sys).clone());
                        init.notify.notify_waiters();
                    }
                }
                // Route control_response to its waiter BEFORE broadcasting.
                if let Outbound::ControlResponse(ref env) = parsed {
                    let rid = env.response.request_id.clone();
                    if !rid.is_empty() {
                        let waiter = { pending.lock().await.remove(&rid) };
                        if let Some(tx) = waiter {
                            let _ = tx.send(env.response.clone());
                        }
                    }
                }
                let _ = outbound.send(parsed);
            }
            Ok(None) => {
                tracing::debug!("stream-json: child stdout closed");
                break;
            }
            Err(err) => {
                tracing::warn!(error = %err, "stream-json: stdout read error");
                break;
            }
        }
    }
    // Signal closure (so `events()` terminates) and drain pending control
    // waiters so they don't hang forever.
    close.closed.store(true, Ordering::Release);
    close.notify.notify_waiters();
    pending.lock().await.clear();
}

/// Mint a unique control request id (dep-light, nanos + a process salt).
fn mint_request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ccr-{:x}-{:x}", std::process::id(), nanos)
}

async fn drain_stderr<E: AsyncRead + Unpin + Send>(stderr: E) {
    let mut lines = BufReader::new(stderr).lines();
    // Discard — `--debug-to-stderr` can be voluminous; we only keep stdout
    // NDJSON. Loop exits on EOF/error.
    while let Ok(Some(_line)) = lines.next_line().await {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};

    /// A scripted in-process peer over `tokio::io::duplex`: emits the lines
    /// it's given on the client's read side, and (optionally) records what
    /// the client writes.
    #[tokio::test]
    async fn captures_init_and_broadcasts_messages() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client_rw);
        let transport = StreamJsonTransport::spawn_from_io(cr, cw, None);

        let mut rx = transport.subscribe();

        // Peer emits init then an assistant message.
        tokio::spawn(async move {
            let (_pr, mut pw) = tokio::io::split(&mut peer_rw);
            for line in [
                json!({"type":"system","subtype":"init","session_id":"u-1",
                       "slash_commands":["compact"]})
                .to_string(),
                json!({"type":"assistant","session_id":"u-1",
                       "message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}})
                .to_string(),
            ] {
                pw.write_all(format!("{line}\n").as_bytes()).await.unwrap();
                pw.flush().await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let init = transport
            .wait_for_init(Duration::from_secs(2))
            .await
            .expect("init");
        assert_eq!(init.session_id, "u-1");
        assert!(transport.is_initialized());

        // The assistant message reaches the subscriber.
        let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timeout")
            .expect("recv");
        // (init may or may not be seen by this subscriber depending on
        // ordering; the assistant message must be.)
        let saw_assistant = matches!(got, Outbound::Assistant(_))
            || matches!(
                tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .expect("recv2 timeout")
                    .expect("recv2"),
                Outbound::Assistant(_)
            );
        assert!(saw_assistant);
    }

    #[tokio::test]
    async fn send_line_reaches_peer_with_newline() {
        let (client_rw, peer_rw) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client_rw);
        let transport = StreamJsonTransport::spawn_from_io(cr, cw, None);

        let peer = tokio::spawn(async move {
            let (pr, _pw) = tokio::io::split(peer_rw);
            let mut reader = TokioBufReader::new(pr);
            let mut buf = String::new();
            reader.read_line(&mut buf).await.unwrap();
            buf
        });

        transport
            .send_line(super::super::protocol::user_text_line("hello"))
            .await
            .unwrap();
        let got = peer.await.unwrap();
        assert!(got.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(got.trim()).unwrap();
        assert_eq!(v["message"]["content"][0]["text"], "hello");
    }

    #[tokio::test]
    async fn request_control_correlates_response_by_nested_request_id() {
        let (client_rw, mut peer_rw) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client_rw);
        let transport = Arc::new(StreamJsonTransport::spawn_from_io(cr, cw, None));

        // Peer reads our control_request and replies with a control_response
        // carrying the request_id nested inside `response`.
        tokio::spawn(async move {
            let (pr, mut pw) = tokio::io::split(&mut peer_rw);
            let mut reader = TokioBufReader::new(pr);
            let mut buf = String::new();
            reader.read_line(&mut buf).await.unwrap();
            let req: serde_json::Value = serde_json::from_str(buf.trim()).unwrap();
            assert_eq!(req["type"], "control_request");
            assert_eq!(req["request"]["subtype"], "set_model");
            let rid = req["request_id"].as_str().unwrap().to_string();
            let resp = json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": rid, "response": {"ok": true}},
            });
            pw.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
            pw.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let body = transport
            .request_control(
                "set_model",
                json!({"model": "claude-opus-4-8"}),
                Duration::from_secs(2),
            )
            .await
            .expect("control response");
        assert_eq!(body.subtype, "success");
        assert_eq!(body.response.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn request_control_times_out_when_unanswered() {
        let (client_rw, peer_rw) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client_rw);
        let transport = StreamJsonTransport::spawn_from_io(cr, cw, None);
        // Drain the peer's write side so the channel doesn't block, but
        // never reply.
        tokio::spawn(async move {
            let (_pr, _pw) = tokio::io::split(peer_rw);
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let err = transport
            .request_control("interrupt", json!({}), Duration::from_millis(120))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn wait_for_init_times_out_when_silent() {
        let (client_rw, _peer_rw) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client_rw);
        let transport = StreamJsonTransport::spawn_from_io(cr, cw, None);
        let err = transport
            .wait_for_init(Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("system:init"));
    }
}
