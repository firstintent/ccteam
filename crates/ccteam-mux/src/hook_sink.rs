//! V0.8 rmux W6 — ccteam-owned hook sink (Option C).
//!
//! A Unix-domain-socket sink that lets Claude Code hook subprocesses
//! forward their lifecycle events to the ccteam orchestrator over a
//! ccteam-owned socket (`~/.ccteam/run/hook.sock`), separate from the
//! rmux daemon's own `mux.sock`. The orchestrator consumes the events
//! and becomes the *single writer* of `progress.jsonl`, closing the
//! two-writer race that the legacy "hook subprocess writes the file
//! directly" path carried (V0.6.4 `OutboundCursor` race was symptomatic).
//!
//! This is **Option C** from `docs/versions/v0-8-rmux/w6-hook-reroute-design.md`:
//! it requires NO upstream rmux daemon changes — ccteam runs its own
//! sidecar listener. The whole path is flag-gated on
//! `CCTEAM_HOOK_VIA_DAEMON=1` (see `ccteam_core::hook_routing`); when the
//! flag is unset, nothing in this module is exercised and hook
//! subprocesses keep writing `progress.jsonl` through the legacy
//! `hook.sh chat-progress <arg>` dispatch.
//!
//! ## Wire format
//!
//! Each client connection carries exactly one event, framed as a
//! 4-byte big-endian length prefix followed by that many bytes of UTF-8
//! JSON ([`HookEvent`]), then the connection closes. No multiplexing,
//! no keep-alive — a hook subprocess is short-lived and fires once.
//!
//! ## Red-line compliance
//!
//! The event payload is the verbatim Claude Code hook JSON
//! (`payload_json`) plus the dispatch `kind` / `action` the legacy CLI
//! path already used. The sink does no byte-stream / pane scraping; it
//! is a typed RPC receiver. The orchestrator deserializes the payload
//! into the existing typed hook handlers once.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// Default UDS path for the ccteam-owned hook sink. Resolves to
/// `$HOME/.ccteam/run/hook.sock` on Unix, alongside the rmux daemon's
/// `mux.sock`. Kept deliberately separate from
/// [`crate::default_ccteam_mux_socket_path`] so the two sockets never
/// alias.
pub fn default_ccteam_hook_socket_path() -> PathBuf {
    let home = if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
    } else if let Some(dir) = std::env::var_os("TMPDIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from("/tmp")
    };
    home.join(".ccteam").join("run").join("hook.sock")
}

/// One Claude Code hook firing, forwarded from the hook subprocess to
/// the orchestrator.
///
/// `kind` / `action` mirror the `ccteam_hooks::dispatch(kind, action,
/// stdin)` arity so the orchestrator can translate without inventing a
/// new schema — e.g. `kind = "chat-progress"`, `action =
/// Some("session-start")`. `payload_json` is the raw Claude Code hook
/// stdin payload, passed through verbatim (lossless).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEvent {
    /// The bot's session identity (`<slug>-<role>`); informational —
    /// the actual progress.jsonl routing is derived by the hook handler
    /// from `payload_json` + env the same way the legacy path did.
    pub session_id: String,
    /// Dispatch kind, e.g. `"chat-progress"` / `"progress-append"`.
    pub kind: String,
    /// Dispatch action (the event arg), e.g. `Some("session-start")`.
    /// `None` for kinds that take no positional arg.
    pub action: Option<String>,
    /// Verbatim Claude Code hook stdin JSON, serialized as a string so
    /// the framing stays a flat envelope. Empty string when the hook
    /// fired with no stdin payload.
    pub payload_json: String,
}

/// Maximum framed event size (1 MiB). A Claude Code hook payload is a
/// few KiB at most; the cap rejects a corrupt / hostile length prefix
/// before allocating.
const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Server side of the hook sink: a bound UDS listener that yields each
/// received [`HookEvent`] on an `mpsc::Receiver`.
///
/// The accept loop runs on a spawned task; dropping the `HookSink`
/// (and thus the receiver) terminates it. Errors on individual
/// connections are logged and swallowed — one malformed hook fire must
/// never take down the sink.
pub struct HookSink {
    rx: mpsc::Receiver<HookEvent>,
    socket_path: PathBuf,
    _accept_task: tokio::task::JoinHandle<()>,
}

impl HookSink {
    /// Bind a sink at `socket_path`, removing any stale socket file
    /// first. Spawns the accept loop and returns a handle whose
    /// [`HookSink::recv`] / [`HookSink::into_receiver`] surface the
    /// events.
    pub fn bind(socket_path: impl AsRef<Path>) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        if let Some(parent) = socket_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create hook.sock parent {}", parent.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o700);
                    let _ = std::fs::set_permissions(parent, perms);
                }
            }
        }
        // A stale socket file from a previous (crashed) daemon would make
        // `bind` fail with EADDRINUSE; remove it best-effort first.
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("bind hook.sock at {}", socket_path.display()))?;

        let (tx, rx) = mpsc::channel::<HookEvent>(256);
        let accept_task = tokio::spawn(accept_loop(listener, tx));
        Ok(Self {
            rx,
            socket_path,
            _accept_task: accept_task,
        })
    }

    /// Receive the next hook event, or `None` once the accept loop has
    /// terminated and all senders are dropped.
    pub async fn recv(&mut self) -> Option<HookEvent> {
        self.rx.recv().await
    }

    /// The path this sink is bound at.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for HookSink {
    fn drop(&mut self) {
        // Best-effort: abort the accept loop + clean the socket file so a
        // subsequent bind at the same path doesn't hit a stale file.
        self._accept_task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn accept_loop(listener: UnixListener, tx: mpsc::Sender<HookEvent>) {
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(error = %err, "hook-sink: accept failed; sink stopping");
                return;
            }
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            match read_one_event(stream).await {
                Ok(event) => {
                    // If the receiver is gone the sink is being torn
                    // down; drop silently.
                    let _ = tx.send(event).await;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "hook-sink: malformed connection; dropped");
                }
            }
        });
    }
}

/// Read exactly one length-prefixed JSON [`HookEvent`] from a stream.
async fn read_one_event(mut stream: UnixStream) -> Result<HookEvent> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("read hook frame length prefix")?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 || len > MAX_FRAME_BYTES {
        anyhow::bail!("hook frame length {len} out of range (max {MAX_FRAME_BYTES})");
    }
    let mut body = vec![0u8; len as usize];
    stream
        .read_exact(&mut body)
        .await
        .context("read hook frame body")?;
    let event: HookEvent = serde_json::from_slice(&body).context("parse hook frame JSON")?;
    Ok(event)
}

/// Client side of the hook sink, used by `ccteam mux hook-emit`.
pub struct HookSinkClient;

impl HookSinkClient {
    /// Connect to the sink at `socket_path`, send exactly one framed
    /// [`HookEvent`], and close. Returns `Err` when the sink isn't
    /// listening (e.g. orchestrator down) so the caller can exit
    /// non-zero *quietly* — a stray hook fire must not error-spam
    /// Claude Code's UI.
    pub async fn emit(socket_path: impl AsRef<Path>, event: &HookEvent) -> Result<()> {
        let socket_path = socket_path.as_ref();
        let mut stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connect hook.sock at {}", socket_path.display()))?;
        let body = serde_json::to_vec(event).context("serialize hook event")?;
        let len: u32 = body
            .len()
            .try_into()
            .ok()
            .filter(|n| *n <= MAX_FRAME_BYTES)
            .context("hook event too large to frame")?;
        stream
            .write_all(&len.to_be_bytes())
            .await
            .context("write hook frame length")?;
        stream
            .write_all(&body)
            .await
            .context("write hook frame body")?;
        stream.flush().await.context("flush hook frame")?;
        // Drop closes the connection; the server reads to EOF after the
        // framed body, which it already has.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(i: usize) -> HookEvent {
        HookEvent {
            session_id: "dev-foo-alice".to_string(),
            kind: "chat-progress".to_string(),
            action: Some("user-prompt".to_string()),
            payload_json: format!("{{\"seq\":{i}}}"),
        }
    }

    #[tokio::test]
    async fn roundtrip_100_events_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("hook.sock");
        let mut sink = HookSink::bind(&sock).unwrap();

        // Emit 100 events sequentially through the client. Each emit is
        // its own connection (matches the real one-shot hook subprocess).
        for i in 0..100 {
            HookSinkClient::emit(&sock, &ev(i)).await.unwrap();
        }

        // All 100 must arrive. Because each emit completes (flush) before
        // the next connects, and accept ordering on a single listener is
        // FIFO, they arrive in order.
        let mut got = Vec::new();
        for _ in 0..100 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), sink.recv())
                .await
                .expect("recv timed out")
                .expect("sink closed early");
            got.push(event);
        }
        assert_eq!(got.len(), 100);
        for (i, event) in got.iter().enumerate() {
            assert_eq!(event, &ev(i), "event {i} out of order or corrupted");
        }
    }

    #[tokio::test]
    async fn emit_errors_when_sink_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("nope.sock");
        // No sink bound → connect should fail (quiet error path).
        let res = HookSinkClient::emit(&sock, &ev(0)).await;
        assert!(res.is_err(), "emit must error when no sink is listening");
    }
}
