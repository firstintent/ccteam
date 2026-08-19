//! Strict JSONL transport and request/response multiplexer for Pi RPC mode.

use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, Mutex, Notify};

use super::protocol::{parse_event, PiEvent, PiResponse};
use super::spawn_spec::PiSpawnSpec;

pub const MAX_JSONL_RECORD_BYTES: usize = 16 * 1024 * 1024;
const STDERR_RING_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum PiTransportEvent {
    Event(PiEvent),
    Error(String),
}

type PendingResult = Result<PiResponse, String>;
type DynWriter = Pin<Box<dyn AsyncWrite + Send>>;

pub struct PiTransport {
    writer: Mutex<Option<DynWriter>>,
    child: Mutex<Option<Child>>,
    pending: StdMutex<HashMap<String, oneshot::Sender<PendingResult>>>,
    event_tx: broadcast::Sender<PiTransportEvent>,
    startup_events: StdMutex<Option<broadcast::Receiver<PiTransportEvent>>>,
    request_seq: AtomicU64,
    alive: AtomicBool,
    closed: Notify,
    stderr_ring: Arc<StdMutex<VecDeque<u8>>>,
    /// `(project_dir, sid)` of the body record written after spawn; cleared
    /// by [`Self::close`] (an observed, explicit end), kept by
    /// [`Self::detach`].
    body_record: StdMutex<Option<(std::path::PathBuf, String)>>,
}

impl std::fmt::Debug for PiTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiTransport")
            .field("alive", &self.is_alive())
            .finish_non_exhaustive()
    }
}

impl PiTransport {
    pub async fn connect_stdio(spec: &PiSpawnSpec) -> Result<Arc<Self>, String> {
        let mut command = Command::new(&spec.bin);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", spec.bin))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Pi child stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Pi child stdout unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Pi child stderr unavailable".to_string())?;
        let transport = Self::from_io(stdout, stdin, Some(child));
        Self::spawn_stderr_reader(Arc::clone(&transport.stderr_ring), stderr);
        Ok(transport)
    }

    fn from_io<R, W>(reader: R, writer: W, child: Option<Child>) -> Arc<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (event_tx, _) = broadcast::channel(256);
        // Subscribe before the stdout task can run. Pi emits extension
        // session_start UI (including the bridge-ready marker) before it
        // begins consuming stdin, so subscribing after spawn can lose the
        // only readiness record.
        let startup_events = event_tx.subscribe();
        let transport = Arc::new(Self {
            writer: Mutex::new(Some(Box::pin(writer))),
            child: Mutex::new(child),
            pending: StdMutex::new(HashMap::new()),
            event_tx,
            startup_events: StdMutex::new(Some(startup_events)),
            request_seq: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            closed: Notify::new(),
            stderr_ring: Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_RING_BYTES))),
            body_record: StdMutex::new(None),
        });
        Self::spawn_stdout_reader(Arc::clone(&transport), reader);
        transport
    }

    fn spawn_stdout_reader<R>(transport: Arc<Self>, mut reader: R)
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        tokio::spawn(async move {
            let mut decoder = JsonlDecoder::new(MAX_JSONL_RECORD_BYTES);
            let mut chunk = [0_u8; 8192];
            let result = 'read: loop {
                match reader.read(&mut chunk).await {
                    Ok(0) => break decoder.finish(),
                    Ok(count) => match decoder.push(&chunk[..count]) {
                        Ok(records) => {
                            for record in records {
                                if let Err(error) = transport.dispatch_record(&record) {
                                    break 'read Err(error);
                                }
                            }
                        }
                        Err(error) => break Err(error),
                    },
                    Err(error) => break Err(format!("Pi stdout read failed: {error}")),
                }
            };
            match result {
                Ok(records) => {
                    for record in records {
                        if let Err(error) = transport.dispatch_record(&record) {
                            transport.finish(error);
                            return;
                        }
                    }
                    transport.finish("Pi child stdout reached EOF".to_string());
                    transport.reap_reader_child(false).await;
                }
                Err(error) => {
                    transport.finish(error);
                    transport.reap_reader_child(true).await;
                }
            }
        });
    }

    fn spawn_stderr_reader<R>(ring: Arc<StdMutex<VecDeque<u8>>>, mut reader: R)
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        tokio::spawn(async move {
            let mut chunk = [0_u8; 4096];
            while let Ok(count) = reader.read(&mut chunk).await {
                if count == 0 {
                    return;
                }
                let mut guard = ring.lock().unwrap();
                for byte in &chunk[..count] {
                    if guard.len() == STDERR_RING_BYTES {
                        guard.pop_front();
                    }
                    guard.push_back(*byte);
                }
            }
        });
    }

    fn dispatch_record(&self, record: &[u8]) -> Result<(), String> {
        let value: Value = serde_json::from_slice(record)
            .map_err(|error| format!("invalid Pi JSONL record: {error}"))?;
        if value.get("type").and_then(Value::as_str) == Some("response") {
            let response: PiResponse = serde_json::from_value(value)
                .map_err(|error| format!("invalid Pi response: {error}"))?;
            let Some(id) = response.id.clone() else {
                tracing::warn!(command = %response.command, "Pi response missing request id");
                return Ok(());
            };
            let pending = self.pending.lock().unwrap().remove(&id);
            if let Some(sender) = pending {
                let _ = sender.send(Ok(response));
            } else {
                tracing::warn!(request_id = %id, "Pi response has unknown request id");
            }
            return Ok(());
        }

        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<missing>")
            .to_string();
        match parse_event(value)? {
            Some(event) => {
                let _ = self.event_tx.send(PiTransportEvent::Event(event));
            }
            None => warn_unknown_event_once(&kind),
        }
        Ok(())
    }

    fn finish(&self, error: String) {
        if !self.alive.swap(false, Ordering::AcqRel) {
            return;
        }
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for (_, sender) in pending {
            let _ = sender.send(Err(error.clone()));
        }
        let _ = self.event_tx.send(PiTransportEvent::Error(error));
        self.closed.notify_waiters();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PiTransportEvent> {
        self.event_tx.subscribe()
    }

    pub fn take_startup_events(&self) -> broadcast::Receiver<PiTransportEvent> {
        self.startup_events
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| self.subscribe())
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub async fn wait_closed(&self) {
        if self.is_alive() {
            self.closed.notified().await;
        }
    }

    pub async fn request(&self, mut command: Value) -> Result<PiResponse, String> {
        if !self.is_alive() {
            return Err("Pi child is not alive".to_string());
        }
        let id = format!(
            "ccteam-{}",
            self.request_seq.fetch_add(1, Ordering::Relaxed)
        );
        let object = command
            .as_object_mut()
            .ok_or_else(|| "Pi command must be a JSON object".to_string())?;
        object.insert("id".to_string(), Value::String(id.clone()));
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), sender);
        if let Err(error) = self.send(command).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("Pi request cancelled by transport shutdown".to_string()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("Pi request {id} timed out"))
            }
        }
    }

    pub async fn send(&self, command: Value) -> Result<(), String> {
        if !self.is_alive() {
            return Err("Pi child is not alive".to_string());
        }
        let mut line = serde_json::to_vec(&command)
            .map_err(|error| format!("serialize Pi command: {error}"))?;
        line.push(b'\n');
        let mut writer = self.writer.lock().await;
        match writer.as_mut() {
            Some(writer) => writer
                .as_mut()
                .write_all(&line)
                .await
                .map_err(|error| format!("write Pi request: {error}")),
            None => Err("write Pi request: Pi stdin closed".to_string()),
        }
    }

    /// The child's OS pid while the transport still holds it.
    pub async fn pid(&self) -> Option<u32> {
        self.child
            .lock()
            .await
            .as_ref()
            .and_then(|child| child.id())
    }

    /// Remember where this transport's body record lives so [`Self::close`]
    /// can clear it (and [`Self::detach`] can deliberately leave it).
    pub fn set_body_record(&self, project_dir: &std::path::Path, sid: &str) {
        if let Ok(mut slot) = self.body_record.lock() {
            *slot = Some((project_dir.to_path_buf(), sid.to_string()));
        }
    }

    /// Let go of the Pi child WITHOUT stopping it (daemon shutdown): close our
    /// stdin end, stop reading, drop the handle with no kill (the child was
    /// spawned with `kill_on_drop(false)`). The body record stays for the next
    /// daemon. Returns the child's pid.
    pub async fn detach(&self) -> Option<u32> {
        let pid = self.pid().await;
        self.writer.lock().await.take();
        self.child.lock().await.take();
        self.finish("Pi transport detached (daemon shutdown)".to_string());
        pid
    }

    pub async fn close(&self) -> Result<(), String> {
        self.writer.lock().await.take();
        if tokio::time::timeout(Duration::from_secs(2), self.wait_closed())
            .await
            .is_err()
        {
            let mut child = self.child.lock().await;
            if let Some(child) = child.as_mut() {
                child
                    .start_kill()
                    .map_err(|error| format!("terminate Pi child: {error}"))?;
                let _ = child.wait().await;
            }
            self.finish("Pi child closed by adapter".to_string());
        }
        if let Some((project_dir, sid)) = self.body_record.lock().ok().and_then(|g| g.clone()) {
            crate::execution::session_body::clear(&project_dir, &sid);
        }
        Ok(())
    }

    pub fn stderr_tail(&self) -> String {
        let bytes: Vec<u8> = self.stderr_ring.lock().unwrap().iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn reap_reader_child(&self, terminate: bool) {
        let mut child = self.child.lock().await;
        if let Some(child) = child.as_mut() {
            if terminate {
                let _ = child.start_kill();
            }
            let _ = child.wait().await;
        }
        child.take();
    }
}

fn warn_unknown_event_once(kind: &str) {
    static WARNED: std::sync::OnceLock<StdMutex<HashSet<String>>> = std::sync::OnceLock::new();
    let warned = WARNED.get_or_init(|| StdMutex::new(HashSet::new()));
    if warned.lock().unwrap().insert(kind.to_string()) {
        tracing::warn!(event_type = kind, "ignoring unknown Pi RPC event");
    }
}

#[derive(Debug)]
struct JsonlDecoder {
    buffer: Vec<u8>,
    max_record_bytes: usize,
}

impl JsonlDecoder {
    fn new(max_record_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_record_bytes,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut records = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            if index > self.max_record_bytes {
                return Err(format!(
                    "Pi JSONL record exceeds {} bytes",
                    self.max_record_bytes
                ));
            }
            let mut record: Vec<u8> = self.buffer.drain(..=index).collect();
            record.pop();
            if record.last() == Some(&b'\r') {
                record.pop();
            }
            if !record.is_empty() {
                records.push(record);
            }
        }
        if self.buffer.len() > self.max_record_bytes {
            return Err(format!(
                "Pi JSONL record exceeds {} bytes",
                self.max_record_bytes
            ));
        }
        Ok(records)
    }

    fn finish(&mut self) -> Result<Vec<Vec<u8>>, String> {
        if self.buffer.len() > self.max_record_bytes {
            return Err(format!(
                "Pi JSONL record exceeds {} bytes",
                self.max_record_bytes
            ));
        }
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }
        let mut record = std::mem::take(&mut self.buffer);
        if record.last() == Some(&b'\r') {
            record.pop();
        }
        Ok((!record.is_empty()).then_some(record).into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn strict_jsonl_handles_fragmentation_crlf_unicode_and_final_record() {
        let mut decoder = JsonlDecoder::new(1024);
        let mut got = decoder.push(b"{\"v\":\"a\xe2\x80").unwrap();
        got.extend(
            decoder
                .push(b"\xa8b\xe2\x80\xa9c\"}\r\n{\"v\":2}\n{\"v\":3}")
                .unwrap(),
        );
        got.extend(decoder.finish().unwrap());
        assert_eq!(got.len(), 3);
        let first: Value = serde_json::from_slice(&got[0]).unwrap();
        assert_eq!(first["v"], "a\u{2028}b\u{2029}c");
        assert_eq!(serde_json::from_slice::<Value>(&got[2]).unwrap()["v"], 3);
    }

    #[test]
    fn strict_jsonl_rejects_oversize_record() {
        let mut decoder = JsonlDecoder::new(4);
        assert!(decoder.push(b"12345").unwrap_err().contains("exceeds"));
    }

    #[tokio::test]
    async fn muxes_out_of_order_responses_interleaved_with_events() {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let transport = PiTransport::from_io(client_read, client_write, None);
        let mut events = transport.subscribe();
        tokio::spawn(async move {
            let (server_read, mut server_write) = tokio::io::split(server);
            let mut lines = BufReader::new(server_read).lines();
            let first: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let second: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            server_write
                .write_all(b"{\"type\":\"future_event\",\"value\":1}\n")
                .await
                .unwrap();
            server_write
                .write_all(b"{\"type\":\"response\",\"id\":\"ghost\",\"command\":\"ghost\",\"success\":true}\n")
                .await
                .unwrap();
            server_write
                .write_all(b"{\"type\":\"agent_start\"}\n")
                .await
                .unwrap();
            server_write
                .write_all(
                    format!("{{\"type\":\"response\",\"id\":{},\"command\":\"two\",\"success\":true}}\n", second["id"]).as_bytes(),
                )
                .await
                .unwrap();
            server_write
                .write_all(
                    format!("{{\"type\":\"response\",\"id\":{},\"command\":\"one\",\"success\":true}}\n", first["id"]).as_bytes(),
                )
                .await
                .unwrap();
        });
        let (one, two) = future::join(
            transport.request(json!({"type":"one"})),
            transport.request(json!({"type":"two"})),
        )
        .await;
        assert_eq!(one.unwrap().command, "one");
        assert_eq!(two.unwrap().command, "two");
        assert!(matches!(
            events.recv().await.unwrap(),
            PiTransportEvent::Event(PiEvent::AgentStart)
        ));
    }

    #[tokio::test]
    async fn invalid_json_is_a_protocol_error() {
        let (client, _server) = tokio::io::duplex(64);
        let (read, write) = tokio::io::split(client);
        let transport = PiTransport::from_io(read, write, None);
        assert!(transport
            .dispatch_record(b"{not-json")
            .unwrap_err()
            .contains("invalid Pi JSONL"));
    }

    #[tokio::test]
    async fn stderr_ring_is_bounded_and_fully_drained() {
        let (reader, mut writer) = tokio::io::duplex(4096);
        let ring = Arc::new(StdMutex::new(VecDeque::new()));
        PiTransport::spawn_stderr_reader(Arc::clone(&ring), reader);
        let payload = vec![b'x'; STDERR_RING_BYTES + 8192];
        writer.write_all(&payload).await.unwrap();
        writer.shutdown().await.unwrap();
        for _ in 0..100 {
            if ring.lock().unwrap().len() == STDERR_RING_BYTES {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(ring.lock().unwrap().len(), STDERR_RING_BYTES);
    }

    #[tokio::test]
    async fn eof_fails_every_pending_request() {
        let (client, server) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client);
        let transport = PiTransport::from_io(client_read, client_write, None);
        tokio::spawn(async move {
            let (server_read, _) = tokio::io::split(server);
            let mut lines = BufReader::new(server_read).lines();
            let _ = lines.next_line().await;
        });
        let result = transport.request(json!({"type":"never"})).await;
        assert!(result.unwrap_err().contains("EOF"));
    }
}
