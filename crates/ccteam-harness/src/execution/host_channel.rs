//! `ccteam-host.v1` — the reverse-connection host control-channel hub.
//!
//! v0.9.0 network inversion: satellites no longer listen on any port. Each
//! satellite dials OUT to the main daemon (`GET /api/v1/hosts/channel`,
//! bearer = its agent token) and keeps one long-lived control WebSocket up
//! (reconnect with jittered exponential backoff). The daemon registers that
//! connection here, keyed by host id. Presence == a live registration; the
//! 90s heartbeat-TTL registry gate stays as a coarse persistence layer fed
//! by `report` frames riding this channel.
//!
//! Remote exec becomes a **dial-back rendezvous**: [`HostChannelHub::open_exec`]
//! mints a single-use nonce, pushes `exec_open{nonce}` down the control
//! channel, and awaits the satellite's fresh outbound WS to
//! `GET /api/v1/hosts/exec/{nonce}`. The daemon-side WS handler claims the
//! nonce ([`HostChannelHub::claim_exec`], host-bound + single-use) and hands
//! the paired [`ExecBridge`] half back to the awaiting spawn. From there the
//! frames are exactly `ccteam-exec.v1` (`remote_exec`): who dialed the TCP
//! connection is irrelevant to the protocol roles.
//!
//! The hub itself is transport-blind: it never touches a socket. The web
//! layer pumps WS frames ↔ [`ExecBridge`] channels; in-process tests speak
//! the protocol over the bridge halves directly (no WS at all).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures::{Sink, Stream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_util::sync::{CancellationToken, PollSender};

/// WS subprotocol of the satellite→daemon control channel.
pub const HOST_CHANNEL_SUBPROTOCOL: &str = "ccteam-host.v1";

/// How long [`HostChannelHub::open_exec`] waits for the satellite's
/// dial-back after pushing `exec_open` (WAN-generous; a LAN dial-back is
/// milliseconds).
pub const EXEC_DIALBACK_TIMEOUT: Duration = Duration::from_secs(15);

/// Link keepalive: the daemon pings each control/exec socket this often.
/// tokio-tungstenite / axum auto-answer pongs, so any healthy link shows
/// traffic at least this often in both directions.
pub const KEEPALIVE_PERIOD: Duration = Duration::from_secs(20);

/// A link with no inbound frame for this long is treated as half-open and
/// torn down (production-stability contract: a silently dead WAN link must
/// become a readable failure / reconnect, never a hang). 3× ping period +
/// grace.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(75);

/// How often a satellite pushes a `report` frame (agents/projects/version)
/// up the control channel. Keeps the registry TTL (90s) comfortably fresh
/// and doubles as application-level keepalive.
pub const REPORT_PERIOD: Duration = Duration::from_secs(25);

/// Control-channel message buffer (daemon → satellite direction).
const CTRL_BUFFER: usize = 32;

/// Per-exec frame buffer in each direction. Bounded so a stalled WS
/// applies backpressure to the producing side instead of ballooning.
const EXEC_FRAME_BUFFER: usize = 64;

/// Daemon → satellite control messages (serialized to wire frames by the
/// web layer; the hub is wire-agnostic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubCtrlMsg {
    /// Ask the satellite to dial back a fresh exec WS for `nonce`.
    ExecOpen { nonce: String, sid: String },
}

/// One half of an in-memory duplex frame pipe. [`ExecBridge::pair`] returns
/// two mirrored halves: what one half `tx`-sends, the other half `rx`-receives.
/// The daemon-side spawn holds one half (via [`HostChannelHub::open_exec`]);
/// the web WS pump (or an in-process test satellite) holds the other.
pub struct ExecBridge {
    pub tx: mpsc::Sender<Message>,
    pub rx: mpsc::Receiver<Message>,
}

impl ExecBridge {
    /// Build a mirrored bridge pair.
    pub fn pair() -> (ExecBridge, ExecBridge) {
        let (a_tx, a_rx) = mpsc::channel(EXEC_FRAME_BUFFER);
        let (b_tx, b_rx) = mpsc::channel(EXEC_FRAME_BUFFER);
        (
            ExecBridge { tx: a_tx, rx: b_rx },
            ExecBridge { tx: b_tx, rx: a_rx },
        )
    }

    /// Adapt this half to the `Stream`/`Sink` shape the `ccteam-exec.v1`
    /// plumbing (`remote_exec::ws_stream_io` etc.) expects — the same
    /// bounds a split tungstenite socket satisfies, so bridge and real WS
    /// are interchangeable at every protocol call site.
    pub fn into_io(self) -> (BridgeStream, BridgeSink) {
        (BridgeStream(self.rx), BridgeSink(PollSender::new(self.tx)))
    }
}

impl std::fmt::Debug for ExecBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExecBridge")
    }
}

/// `Stream<Item = Result<Message, WsError>>` over a bridge receiver. Channel
/// closed (peer half dropped) yields `None` — clean EOF, mirroring a WS
/// close.
pub struct BridgeStream(mpsc::Receiver<Message>);

impl Stream for BridgeStream {
    type Item = Result<Message, WsError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

/// `Sink<Message, Error = WsError>` over a bridge sender. A closed peer maps
/// to `WsError::AlreadyClosed` (→ `BrokenPipe` at the byte layer).
pub struct BridgeSink(PollSender<Message>);

impl Sink<Message> for BridgeSink {
    type Error = WsError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), WsError>> {
        Pin::new(&mut self.0)
            .poll_ready(cx)
            .map_err(|_| WsError::AlreadyClosed)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), WsError> {
        Pin::new(&mut self.0)
            .start_send(item)
            .map_err(|_| WsError::AlreadyClosed)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), WsError>> {
        Pin::new(&mut self.0)
            .poll_flush(cx)
            .map_err(|_| WsError::AlreadyClosed)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), WsError>> {
        Pin::new(&mut self.0)
            .poll_close(cx)
            .map_err(|_| WsError::AlreadyClosed)
    }
}

/// What [`HostChannelHub::register`] hands the control-channel handler task.
pub struct HostChannelRegistration {
    pub host_id: String,
    /// Monotonic registration generation — pass back to
    /// [`HostChannelHub::unregister`] so a stale handler can never evict its
    /// own replacement.
    pub generation: u64,
    /// Control messages to serialize down the wire (`exec_open`, …).
    pub ctrl_rx: mpsc::Receiver<HubCtrlMsg>,
    /// Cancelled when a NEWER connection for the same host registers — the
    /// old handler must close its socket and exit (satellite reconnected
    /// through a NAT rebind while the stale TCP half was still "open").
    /// Level-triggered (`CancellationToken`), so a kick that lands between
    /// two `select!` polls is never lost.
    pub kicked: CancellationToken,
}

struct HostHandle {
    ctrl_tx: mpsc::Sender<HubCtrlMsg>,
    generation: u64,
    kicked: CancellationToken,
}

struct PendingExec {
    host_id: String,
    slot: oneshot::Sender<ExecBridge>,
}

/// Live satellite control channels + pending exec dial-backs, keyed by host
/// id. One instance per daemon process, shared by the web WS handlers (which
/// register connections / claim dial-backs) and the spawn path (which opens
/// execs through `SpawnCtx::remote`).
#[derive(Default)]
pub struct HostChannelHub {
    hosts: Mutex<HashMap<String, HostHandle>>,
    pending: Mutex<HashMap<String, PendingExec>>,
    generation: AtomicU64,
}

impl std::fmt::Debug for HostChannelHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hosts = self.hosts.lock().map(|h| h.len()).unwrap_or(0);
        let pending = self.pending.lock().map(|p| p.len()).unwrap_or(0);
        write!(f, "HostChannelHub{{hosts:{hosts}, pending:{pending}}}")
    }
}

impl HostChannelHub {
    /// Register a live control channel for `host_id`, kicking (and
    /// replacing) any existing registration — the newest connection always
    /// wins, so a satellite reconnect never fights its own half-open ghost.
    pub fn register(&self, host_id: &str) -> HostChannelRegistration {
        let (ctrl_tx, ctrl_rx) = mpsc::channel(CTRL_BUFFER);
        let kicked = CancellationToken::new();
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = HostHandle {
            ctrl_tx,
            generation,
            kicked: kicked.clone(),
        };
        let old = self
            .hosts
            .lock()
            .expect("host hub lock")
            .insert(host_id.to_string(), handle);
        if let Some(old) = old {
            old.kicked.cancel();
        }
        HostChannelRegistration {
            host_id: host_id.to_string(),
            generation,
            ctrl_rx,
            kicked,
        }
    }

    /// Drop the registration for `host_id` iff `generation` still matches
    /// (a replaced handler unregistering late must not evict its successor).
    pub fn unregister(&self, host_id: &str, generation: u64) {
        let mut hosts = self.hosts.lock().expect("host hub lock");
        if hosts.get(host_id).map(|h| h.generation) == Some(generation) {
            hosts.remove(host_id);
        }
    }

    /// Whether `host_id` has a live control channel right now.
    pub fn is_connected(&self, host_id: &str) -> bool {
        self.hosts
            .lock()
            .expect("host hub lock")
            .contains_key(host_id)
    }

    /// Host ids with a live control channel.
    pub fn connected_hosts(&self) -> Vec<String> {
        self.hosts
            .lock()
            .expect("host hub lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Open an exec dial-back to `host_id`: mint a single-use nonce, push
    /// `exec_open` down its control channel, await the paired bridge from
    /// the dial-back WS handler. Every failure is a readable `Err` (no live
    /// channel / satellite never dialed back within
    /// [`EXEC_DIALBACK_TIMEOUT`]) — the caller must not fall back to a
    /// local spawn.
    pub async fn open_exec(&self, host_id: &str, sid: &str) -> Result<ExecBridge> {
        self.open_exec_with_timeout(host_id, sid, EXEC_DIALBACK_TIMEOUT)
            .await
    }

    pub async fn open_exec_with_timeout(
        &self,
        host_id: &str,
        sid: &str,
        timeout: Duration,
    ) -> Result<ExecBridge> {
        let ctrl_tx = {
            let hosts = self.hosts.lock().expect("host hub lock");
            let Some(handle) = hosts.get(host_id) else {
                bail!(
                    "host `{host_id}` has no live control channel (satellite not \
                     connected to this daemon); session was not created"
                );
            };
            handle.ctrl_tx.clone()
        };
        let nonce = mint_nonce();
        let (slot_tx, slot_rx) = oneshot::channel();
        self.pending.lock().expect("pending lock").insert(
            nonce.clone(),
            PendingExec {
                host_id: host_id.to_string(),
                slot: slot_tx,
            },
        );
        let msg = HubCtrlMsg::ExecOpen {
            nonce: nonce.clone(),
            sid: sid.to_string(),
        };
        if ctrl_tx.send(msg).await.is_err() {
            self.pending.lock().expect("pending lock").remove(&nonce);
            bail!(
                "host `{host_id}` control channel closed while requesting exec; \
                 session was not created"
            );
        }
        match tokio::time::timeout(timeout, slot_rx).await {
            Ok(Ok(bridge)) => Ok(bridge),
            Ok(Err(_)) => {
                self.pending.lock().expect("pending lock").remove(&nonce);
                Err(anyhow!(
                    "host `{host_id}` exec dial-back was dropped before pairing; \
                     session was not created"
                ))
            }
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&nonce);
                Err(anyhow!(
                    "host `{host_id}` did not dial back the exec channel within \
                     {}s; session was not created",
                    timeout.as_secs()
                ))
            }
        }
    }

    /// Claim a pending exec dial-back. Single-use: the entry is removed on
    /// success. `host_id` must match the host the nonce was minted for — a
    /// different (even validly authenticated) host presenting the nonce is
    /// rejected and the pending entry stays intact for the right one.
    pub fn claim_exec(&self, nonce: &str, host_id: &str) -> Result<oneshot::Sender<ExecBridge>> {
        let mut pending = self.pending.lock().expect("pending lock");
        match pending.get(nonce) {
            None => bail!("unknown or expired exec nonce"),
            Some(p) if p.host_id != host_id => {
                bail!("exec nonce was not minted for host `{host_id}`")
            }
            Some(_) => {}
        }
        let p = pending.remove(nonce).expect("checked above");
        Ok(p.slot)
    }
}

/// Mint an unguessable dial-back nonce (16 CSPRNG bytes, lowercase hex).
/// Defense-in-depth on top of the agent-token bearer + host binding —
/// mirrors `ccteam_core::session_secret::mint` (which harness cannot call:
/// core depends on harness).
fn mint_nonce() -> String {
    let mut buf = [0u8; 16];
    if getrandom::getrandom(&mut buf).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let mixed = nanos ^ (pid << 64) ^ pid.rotate_left(32);
        buf.copy_from_slice(&mixed.to_le_bytes());
    }
    let mut out = String::with_capacity(32);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mint_nonce_is_32_hex_and_unique() {
        let a = mint_nonce();
        let b = mint_nonce();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn open_exec_without_channel_errors_readable() {
        let hub = HostChannelHub::default();
        let err = hub.open_exec("sat", "s7").await.unwrap_err().to_string();
        assert!(err.contains("no live control channel"), "got: {err}");
    }

    #[tokio::test]
    async fn open_exec_pairs_with_dialback_claim() {
        let hub = Arc::new(HostChannelHub::default());
        let mut reg = hub.register("sat");
        assert!(hub.is_connected("sat"));

        let hub2 = hub.clone();
        let satellite = tokio::spawn(async move {
            let Some(HubCtrlMsg::ExecOpen { nonce, sid }) = reg.ctrl_rx.recv().await else {
                panic!("expected ExecOpen");
            };
            assert_eq!(sid, "s7");
            let slot = hub2.claim_exec(&nonce, "sat").unwrap();
            let (mine, theirs) = ExecBridge::pair();
            slot.send(theirs).ok().unwrap();
            mine
        });

        let bridge = hub.open_exec("sat", "s7").await.unwrap();
        let mut sat = satellite.await.unwrap();

        // Frames flow both ways across the paired halves.
        bridge
            .tx
            .send(Message::Binary(b"in".to_vec()))
            .await
            .unwrap();
        assert_eq!(sat.rx.recv().await, Some(Message::Binary(b"in".to_vec())));
        sat.tx.send(Message::Binary(b"out".to_vec())).await.unwrap();
        let mut rx = bridge.rx;
        assert_eq!(rx.recv().await, Some(Message::Binary(b"out".to_vec())));
    }

    #[tokio::test]
    async fn open_exec_times_out_when_satellite_never_dials_back() {
        let hub = HostChannelHub::default();
        let _reg = hub.register("sat");
        let err = hub
            .open_exec_with_timeout("sat", "s7", Duration::from_millis(50))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not dial back"), "got: {err}");
        // The pending entry is cleaned up: a late claim finds nothing.
        assert!(hub.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn claim_exec_is_single_use_and_host_bound() {
        let hub = Arc::new(HostChannelHub::default());
        let mut reg = hub.register("sat");
        let hub2 = hub.clone();
        let opener = tokio::spawn(async move {
            hub2.open_exec_with_timeout("sat", "s7", Duration::from_secs(2))
                .await
        });
        let Some(HubCtrlMsg::ExecOpen { nonce, .. }) = reg.ctrl_rx.recv().await else {
            panic!("expected ExecOpen");
        };
        // Wrong host cannot claim; the pending entry survives.
        let err = hub.claim_exec(&nonce, "other").unwrap_err().to_string();
        assert!(err.contains("not minted for host"), "got: {err}");
        // Right host claims once …
        let slot = hub.claim_exec(&nonce, "sat").unwrap();
        // … and the nonce is spent.
        let err2 = hub.claim_exec(&nonce, "sat").unwrap_err().to_string();
        assert!(err2.contains("unknown or expired"), "got: {err2}");
        let (_mine, theirs) = ExecBridge::pair();
        slot.send(theirs).ok().unwrap();
        opener.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn register_replacement_kicks_the_old_connection() {
        let hub = HostChannelHub::default();
        let reg1 = hub.register("sat");
        let _reg2 = hub.register("sat");
        // Level-triggered: even though nobody was awaiting at kick time,
        // the cancellation is observable after the fact (the property the
        // reconnect path depends on).
        tokio::time::timeout(Duration::from_secs(1), reg1.kicked.cancelled())
            .await
            .expect("old registration must be kicked");
        // Old generation unregistering late must NOT evict the replacement.
        hub.unregister("sat", reg1.generation);
        assert!(hub.is_connected("sat"));
    }

    #[tokio::test]
    async fn bridge_io_adapters_round_trip_and_eof() {
        use futures::{SinkExt, StreamExt};
        let (a, b) = ExecBridge::pair();
        let (mut a_stream, mut a_sink) = a.into_io();
        let (mut b_stream, mut b_sink) = b.into_io();
        a_sink.send(Message::Binary(b"x".to_vec())).await.unwrap();
        assert_eq!(
            b_stream.next().await.unwrap().unwrap(),
            Message::Binary(b"x".to_vec())
        );
        b_sink.send(Message::Text("t".into())).await.unwrap();
        assert_eq!(
            a_stream.next().await.unwrap().unwrap(),
            Message::Text("t".into())
        );
        // Dropping one side's sink+stream closes the peer's stream (EOF).
        drop(a_sink);
        drop(a_stream);
        assert!(b_stream.next().await.is_none());
    }
}
