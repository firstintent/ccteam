//! `ccteam-exec.v1` — the satellite exec-bridge wire protocol + WS client
//! (v0.9.0 W3 F3, tech-design §4.2 / §4.3).
//!
//! Tech-design §0.4 (transport law): execution location is a **transport
//! parameter**, never an adapter branch. Every stdio-protocol transport
//! constructor is already generic over `(reader, writer)`
//! (`StreamJsonTransport::spawn_from_io`, `AcpTransport::from_halves`,
//! `CodexJsonRpcClient::spawn`) and holds no `Child`. [`connect`] supplies
//! exactly that pair over a WebSocket to a satellite's `GET /ws/exec` — the
//! adapter's protocol logic is unaware whether its stdio is a local pipe or
//! a remote byte pump.
//!
//! The satellite is a **protocol-blind byte pump**: it never parses
//! stream-json / ACP / JSON-RPC, only bytes (red line: no scraping, no
//! interpretation). Wire shape (`docs-local/versions/v0-9-0/tech-design.md`
//! §4.2):
//!
//! ```text
//! C→S first frame (Text)  ExecSpec   — what to run
//! S→C first frame (Text)  ExecStarted — ok / readable rejection
//! …                       C→S Binary = child stdin, S→C Binary = child stdout
//! C→S Text {"op":"stdin_close"}       = half-close
//! S→C Text ExecExit                   = tail frame, then Close
//! ```

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::{Sink, SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// WS subprotocol both sides negotiate (`Sec-WebSocket-Protocol`).
pub const EXEC_SUBPROTOCOL: &str = "ccteam-exec.v1";

/// Wire version. Bump on any breaking frame-shape change.
pub const EXEC_WIRE_VERSION: u32 = 1;

/// How long [`connect`] waits for the TCP/WS handshake and for the
/// satellite's [`ExecStarted`] ack before giving up (F7 reliability
/// contract: "connect timeout 5s").
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One file to materialize under `<project>/.ccteam/chat/<sid>/` on the
/// satellite before spawn (e.g. the curated `mcp.json`). `content` MAY
/// contain the literal template token `{{DAEMON_URL}}`, substituted by the
/// satellite from its own `SatelliteSelf::daemon_url` — the main daemon
/// never has to guess its own LAN-reachable address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecFile {
    pub relpath: String,
    pub content: String,
}

/// First client→satellite frame (Text, JSON): what to run + how.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecSpec {
    pub v: u32,
    /// Vendor token (`claude` / `codex` / `grok` / `opencode`). The
    /// satellite resolves ITS OWN binary from this — the wire never
    /// carries a binary path (red line: never trust a wire binary path).
    pub vendor: String,
    /// Full argv EXCLUDING argv\[0\] — the satellite prepends its own
    /// resolved binary.
    pub args: Vec<String>,
    /// Env overlay. The satellite keeps ONLY `CCTEAM_*` keys and merges
    /// them over its own environment (allowlist, never a raw passthrough).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub slug: String,
    pub sid: String,
    /// Session protocol token (`stream-json` / `acp`) — informational; the
    /// satellite does not branch on it (byte-pump only).
    pub protocol: String,
    /// Files to materialize under `<project>/.ccteam/chat/<sid>/` before
    /// spawn.
    #[serde(default)]
    pub files: Vec<ExecFile>,
}

impl ExecSpec {
    /// The literal template token a `files[].content` body may embed for
    /// the satellite to substitute with its own `daemon_url`.
    pub const DAEMON_URL_TOKEN: &'static str = "{{DAEMON_URL}}";

    pub fn new(
        vendor: impl Into<String>,
        slug: impl Into<String>,
        sid: impl Into<String>,
        protocol: impl Into<String>,
    ) -> Self {
        Self {
            v: EXEC_WIRE_VERSION,
            vendor: vendor.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            slug: slug.into(),
            sid: sid.into(),
            protocol: protocol.into(),
            files: Vec::new(),
        }
    }
}

/// Satellite→client ack for the first frame.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecStarted {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Machine-readable rejection code (`vendor-not-allowed` /
    /// `unknown-slug` / `spawn-failed` / `bad-spec`) — present iff `!ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Satellite→client tail frame on child exit.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecExit {
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub signal: Option<String>,
}

/// What the gateway needs to reach a satellite's exec bridge for one
/// spawn/rebuild. Built by `ccteam_im::remote_host::prepare_host_for_spawn`
/// from the host registry record + threaded into `SpawnCtx::remote`.
#[derive(Debug, Clone)]
pub struct RemoteExecTarget {
    /// Full `ws://host:port/ws/exec` (or `wss://…`) URL.
    pub exec_ws_url: String,
    /// Bearer token the satellite's `GET /ws/exec` checks (its
    /// `SatelliteSelf::agent_token`, constant-time compared).
    pub agent_token: String,
}

/// Concrete WS stream type `tokio_tungstenite::connect_async` returns.
type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Dial `target`, send `spec` as the first frame, await [`ExecStarted`],
/// then hand back a byte-plumbed `(reader, writer)` pair — exactly what
/// `StreamJsonTransport::spawn_from_io` / `AcpTransport::from_halves` /
/// `CodexJsonRpcClient::spawn` already accept (tech-design §0.4). An `!ok`
/// [`ExecStarted`] or any handshake failure is a readable `Err` — the
/// caller must not fall back to a local spawn (red line: never silently
/// respawn locally).
pub async fn connect(
    target: &RemoteExecTarget,
    spec: ExecSpec,
) -> Result<(
    impl AsyncRead + Unpin + Send + 'static,
    impl AsyncWrite + Unpin + Send + 'static,
)> {
    let mut request = target
        .exec_ws_url
        .as_str()
        .into_client_request()
        .with_context(|| format!("invalid exec ws url: {}", target.exec_ws_url))?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", target.agent_token))
            .context("agent token is not a valid header value")?,
    );
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(EXEC_SUBPROTOCOL),
    );

    let (ws, _resp): (WsStream, _) =
        tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
            .await
            .map_err(|_| anyhow!("ccteam-exec.v1 connect to {} timed out", target.exec_ws_url))?
            .with_context(|| format!("ccteam-exec.v1 connect to {} failed", target.exec_ws_url))?;

    let (mut sink, mut stream) = ws.split();

    let spec_json = serde_json::to_string(&spec).context("serialize ExecSpec")?;
    tokio::time::timeout(CONNECT_TIMEOUT, sink.send(Message::Text(spec_json)))
        .await
        .map_err(|_| anyhow!("ccteam-exec.v1 send ExecSpec timed out"))?
        .context("send ExecSpec")?;

    let started = await_exec_started(&mut stream).await?;
    if !started.ok {
        bail!(
            "satellite rejected exec spawn ({}): {}",
            started.code.as_deref().unwrap_or("unknown"),
            started.message.as_deref().unwrap_or("no message"),
        );
    }

    let (reader, writer) = ws_stream_io(stream, sink);
    Ok((reader, writer))
}

async fn await_exec_started<S>(stream: &mut S) -> Result<ExecStarted>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    let started = tokio::time::timeout(CONNECT_TIMEOUT, async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => {
                    return serde_json::from_str::<ExecStarted>(&t)
                        .with_context(|| format!("parse ExecStarted: {t}"));
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(other)) => {
                    bail!("expected ExecStarted (Text), got unexpected frame: {other:?}")
                }
                Some(Err(e)) => bail!("ws error awaiting ExecStarted: {e}"),
                None => bail!("satellite closed the connection before ExecStarted"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for ExecStarted"))??;
    Ok(started)
}

/// Bridge a live WS (already split into stream + sink halves) to
/// `AsyncRead`/`AsyncWrite` bytes: Binary frames carry payload bytes in
/// both directions; a Text frame in the read direction (the [`ExecExit`]
/// tail, or any other control message) is treated as clean EOF. Exposed
/// `pub(crate)` so the standalone unit test can drive it directly against
/// a bare WS connection (no [`ExecSpec`] handshake), proving the byte
/// plumbing independent of the handshake.
pub(crate) fn ws_stream_io<St, Si>(stream: St, sink: Si) -> (ExecReader<St>, ExecWriter<Si>)
where
    St: Stream<Item = Result<Message, WsError>> + Unpin,
    Si: Sink<Message, Error = WsError> + Unpin,
{
    (ExecReader::new(stream), ExecWriter::new(sink))
}

/// `AsyncRead` adapter over a WS message stream (see [`ws_stream_io`]).
pub(crate) struct ExecReader<S> {
    stream: S,
    pending: Vec<u8>,
    eof: bool,
}

impl<S> ExecReader<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            pending: Vec::new(),
            eof: false,
        }
    }
}

impl<S> AsyncRead for ExecReader<S>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if !self.pending.is_empty() {
                let n = std::cmp::min(buf.remaining(), self.pending.len());
                buf.put_slice(&self.pending[..n]);
                self.pending.drain(..n);
                return Poll::Ready(Ok(()));
            }
            if self.eof {
                return Poll::Ready(Ok(())); // 0-byte read == EOF.
            }
            match Pin::new(&mut self.stream).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Binary(bytes)))) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    self.pending = bytes;
                }
                Poll::Ready(Some(Ok(Message::Text(_)))) => {
                    // The ExecExit tail (or any other control frame) —
                    // treated as clean EOF; the caller reads exit details
                    // out of band (progress/turns), not off this stream.
                    self.eof = true;
                }
                Poll::Ready(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                Poll::Ready(Some(Ok(Message::Close(_)))) | Poll::Ready(None) => {
                    self.eof = true;
                }
                Poll::Ready(Some(Ok(Message::Frame(_)))) => continue,
                Poll::Ready(Some(Err(_))) => {
                    self.eof = true;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// `AsyncWrite` adapter over a WS message sink (see [`ws_stream_io`]).
pub(crate) struct ExecWriter<S> {
    sink: S,
    stdin_closed: bool,
}

impl<S> ExecWriter<S> {
    fn new(sink: S) -> Self {
        Self {
            sink,
            stdin_closed: false,
        }
    }
}

fn ws_err_to_io(e: WsError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
}

impl<S> AsyncWrite for ExecWriter<S>
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::new(&mut self.sink).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(ws_err_to_io(e))),
            Poll::Pending => return Poll::Pending,
        }
        let len = buf.len();
        match Pin::new(&mut self.sink).start_send(Message::Binary(buf.to_vec())) {
            Ok(()) => Poll::Ready(Ok(len)),
            Err(e) => Poll::Ready(Err(ws_err_to_io(e))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.sink)
            .poll_flush(cx)
            .map_err(ws_err_to_io)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        // ccteam-exec.v1 half-close: tell the satellite to close the
        // child's stdin (EOF), THEN close the WS sink. Mirrors the local
        // stream-json transport's "writer task drops -> ChildStdin closes"
        // shape (transport.rs doc comment) over the wire.
        if !self.stdin_closed {
            self.stdin_closed = true;
            if let Err(e) =
                Pin::new(&mut self.sink).start_send(Message::Text(r#"{"op":"stdin_close"}"#.into()))
            {
                return Poll::Ready(Err(ws_err_to_io(e)));
            }
        }
        Pin::new(&mut self.sink)
            .poll_close(cx)
            .map_err(ws_err_to_io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn echo_socket(mut socket: WebSocket) {
        while let Some(Ok(msg)) = socket.recv().await {
            match msg {
                AxumMessage::Binary(b) => {
                    if socket.send(AxumMessage::Binary(b)).await.is_err() {
                        break;
                    }
                }
                AxumMessage::Close(_) => break,
                _ => continue,
            }
        }
    }

    async fn echo_upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(echo_socket)
    }

    async fn spawn_echo_server() -> String {
        let app = Router::new().route("/echo", get(echo_upgrade));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("ws://{addr}/echo")
    }

    /// `ws_stream_io` unit test (per the wave brief): drive the adapter
    /// against a REAL in-process axum WS echo server over loopback — bytes
    /// written on the `AsyncWrite` half come back out the `AsyncRead` half,
    /// independent of the `ExecSpec`/`ExecStarted` handshake.
    #[tokio::test]
    async fn ws_stream_io_round_trips_bytes_over_a_real_socket() {
        let url = spawn_echo_server().await;
        let (ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
        let (sink, stream) = ws.split();
        let (mut reader, mut writer) = ws_stream_io(stream, sink);

        writer.write_all(b"hello satellite").await.unwrap();
        writer.flush().await.unwrap();

        let mut buf = [0u8; 32];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello satellite");

        // A second write/read round trip proves the reader/writer are not
        // one-shot.
        writer.write_all(b"again").await.unwrap();
        writer.flush().await.unwrap();
        let n2 = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n2], b"again");
    }

    /// A Text frame in the read direction (what the real satellite sends as
    /// the `ExecExit` tail) must read as clean EOF, not an error.
    #[tokio::test]
    async fn ws_stream_io_text_frame_reads_as_eof() {
        let app = Router::new().route(
            "/exit",
            get(|ws: WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket: WebSocket| async move {
                    let _ = socket.send(AxumMessage::Text(r#"{"exit":0}"#.into())).await;
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{addr}/exit"))
            .await
            .unwrap();
        let (sink, stream) = ws.split();
        let (mut reader, _writer) = ws_stream_io(stream, sink);
        let mut buf = [0u8; 8];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "Text frame must read as EOF (0 bytes), not data");
    }

    #[test]
    fn exec_spec_new_defaults_are_empty() {
        let spec = ExecSpec::new("claude", "demo", "s7", "stream-json");
        assert_eq!(spec.v, EXEC_WIRE_VERSION);
        assert!(spec.args.is_empty());
        assert!(spec.env.is_empty());
        assert!(spec.files.is_empty());
    }

    #[test]
    fn exec_spec_round_trips_json() {
        let mut spec = ExecSpec::new("claude", "demo", "s7", "stream-json");
        spec.args = vec!["--input-format".into(), "stream-json".into()];
        spec.env.insert("CCTEAM_CHAT_SID".into(), "s7".into());
        spec.files.push(ExecFile {
            relpath: ".ccteam/chat/s7/mcp.json".into(),
            content: "{}".into(),
        });
        let json = serde_json::to_string(&spec).unwrap();
        let back: ExecSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn exec_started_ok_and_rejection_shapes() {
        let ok: ExecStarted = serde_json::from_str(r#"{"ok":true,"pid":123}"#).unwrap();
        assert!(ok.ok);
        assert_eq!(ok.pid, Some(123));

        let rej: ExecStarted =
            serde_json::from_str(r#"{"ok":false,"code":"unknown-slug","message":"nope"}"#).unwrap();
        assert!(!rej.ok);
        assert_eq!(rej.code.as_deref(), Some("unknown-slug"));
    }

    #[tokio::test]
    async fn connect_rejects_immediately_when_satellite_declines() {
        let app = Router::new().route(
            "/ws/exec",
            get(|ws: WebSocketUpgrade| async {
                ws.protocols([EXEC_SUBPROTOCOL])
                    .on_upgrade(|mut socket: WebSocket| async move {
                        // Drain the ExecSpec, then reject.
                        let _ = socket.recv().await;
                        let _ = socket
                            .send(AxumMessage::Text(
                                r#"{"ok":false,"code":"unknown-slug","message":"no such project"}"#
                                    .into(),
                            ))
                            .await;
                    })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let target = RemoteExecTarget {
            exec_ws_url: format!("ws://{addr}/ws/exec"),
            agent_token: "tok".into(),
        };
        let spec = ExecSpec::new("claude", "demo", "s7", "stream-json");
        // `.err()` (not `.unwrap_err()`): the Ok side is an opaque
        // `impl AsyncRead/Write` tuple with no `Debug`, which
        // `unwrap_err()`'s bound requires even on the error path.
        let err = connect(&target, spec)
            .await
            .err()
            .expect("connect must fail when the satellite declines")
            .to_string();
        assert!(err.contains("unknown-slug"), "got: {err}");
        assert!(err.contains("no such project"), "got: {err}");
    }

    #[tokio::test]
    async fn connect_bridges_stdio_bytes_after_ok_ack() {
        let app = Router::new().route(
            "/ws/exec",
            get(|ws: WebSocketUpgrade| async {
                ws.protocols([EXEC_SUBPROTOCOL])
                    .on_upgrade(|mut socket: WebSocket| async move {
                        let _ = socket.recv().await; // ExecSpec
                        let _ = socket
                            .send(AxumMessage::Text(r#"{"ok":true,"pid":42}"#.into()))
                            .await;
                        // Echo one Binary frame back (simulating the fake
                        // vendor's stdout), then send the exit tail.
                        if let Some(Ok(AxumMessage::Binary(b))) = socket.recv().await {
                            let _ = socket.send(AxumMessage::Binary(b)).await;
                        }
                        let _ = socket.send(AxumMessage::Text(r#"{"exit":0}"#.into())).await;
                    })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let target = RemoteExecTarget {
            exec_ws_url: format!("ws://{addr}/ws/exec"),
            agent_token: "tok".into(),
        };
        let spec = ExecSpec::new("claude", "demo", "s7", "stream-json");
        let (mut reader, mut writer) = connect(&target, spec).await.unwrap();
        writer.write_all(b"ping").await.unwrap();
        writer.flush().await.unwrap();
        let mut buf = [0u8; 8];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");
        // Exit tail reads as EOF.
        let n2 = reader.read(&mut buf).await.unwrap();
        assert_eq!(n2, 0);
    }
}
