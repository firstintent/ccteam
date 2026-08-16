//! Shared application state passed to every axum handler. Wraps the
//! resolved [`CcteamPaths`] so handlers don't re-resolve `from_env()`
//! per request (and tests can swap the projects_root root via
//! `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` before constructing
//! [`AppState`]).
//!
//! V0.3 M5.2 added a progress-file-watcher `EventBus` field for the (now
//! removed, v0.9.0 W4) `routes::sse`/`harness_sse`; every live event source
//! today is the gateway's own broadcast (`Gateway::subscribe_events`, see
//! `crate::ring`), so this state carries no file-watcher bus anymore.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ccteam_core::CcteamPaths;
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::auth::AuthState;
use crate::chat_protocol::{WebChannelMessage, WebSendMessage};
use crate::pty::PtyRegistry;

#[derive(Clone)]
pub struct AppState {
    pub paths: Arc<CcteamPaths>,
    /// V0.3 M5.3 — auth gate state. Cloned per request, so the inner
    /// `Arc<AuthState>` keeps the token allocation shared. When
    /// `enabled = false` (loopback bind, or `--no-auth` opt-out) the
    /// `auth_layer` middleware short-circuits to pass-through.
    pub auth: Arc<AuthState>,
    /// V0.3.2 F56 — refcounted `tmux pipe-pane` registry shared by
    /// all WS PTY subscribers. The first subscriber to a given
    /// `<slug>` (or `<slug>/<sid>`) creates the FIFO + `pipe-pane`;
    /// the last drop tears them down.
    pub pty: PtyRegistry,
    /// Browser chat inbound bridge. `ccteam-web` owns only the neutral
    /// JSON shape; `ccteam-cli` translates this into the IM gateway.
    pub chat_inbound: Option<mpsc::Sender<WebChannelMessage>>,
    /// Browser chat outbound fan-out, fed by the CLI bridge.
    pub chat_outbound: broadcast::Sender<WebSendMessage>,
    /// Browser chat outbound backlog for messages emitted while a
    /// matching web socket is disconnected. Bounded by
    /// [`CHAT_BACKLOG_CAP`] (oldest dropped first) — combined with the
    /// connection registry below, entries only accrue while a recipient
    /// has zero live sockets.
    pub chat_backlog: Arc<Mutex<Vec<WebSendMessage>>>,
    /// Per-recipient (`chat_id`) live web-chat socket count. Shared with
    /// the CLI `web_chat_bridge` so the send path can decide whether an
    /// outbound message rides the live broadcast (≥1 socket) or must be
    /// parked in `chat_backlog` (0 sockets). The WS edge bumps this on
    /// connect and decrements on disconnect.
    pub chat_conns: ChatConns,
    /// V0.8.6 W5b — handle to the live IM gateway, shared with the daemon
    /// that owns the session map (the web server runs in the same daemon
    /// process). `Some` when `ccteam start` runs the gateway alongside web;
    /// the resource-API session endpoints compose
    /// `Gateway::{session_views,create_session_api,submit_to_sid,
    /// stop_session}` through it. `None` for the standalone "internal web"
    /// path (no daemon gateway) — session endpoints then return 503. The
    /// coupling is a direct crate dep (`ccteam-web -> ccteam-im`), acyclic
    /// because `ccteam-im` does not depend on `ccteam-web`.
    pub gateway: Option<Arc<Mutex<ccteam_im::gateway::Gateway>>>,
    /// The `(sid, secret)` registry, held DIRECTLY rather than reached through
    /// `gateway`: verifying a managed session's principal must not queue behind
    /// whatever else holds the gateway lock — including a spawn that is at that
    /// very moment waiting for the vendor to finish this call.
    pub session_principals: Option<Arc<ccteam_im::principals::SessionPrincipals>>,
    /// `Mcp-Session-Id` bindings — one identity per hand-started vendor PROCESS
    /// (see `crate::routes::mcp`). Always present, like the rings below: it is
    /// pure in-memory state the `/mcp` front door owns, and a daemon restart has
    /// already ended every conversation it described. The idle sweep that reaps
    /// it is spawned alongside the gateway in [`Self::with_gateway`], because
    /// closing a node needs one.
    pub native_bindings: Arc<ccteam_im::native_bindings::NativeBindings>,
    /// v0.8.8 F4 — IM credentials file path the `config/im/*` handlers
    /// read + write. Defaults to `ccteam_im::credentials::default_path()`
    /// (`~/.ccteam/im/credentials.json`); integration tests override it via
    /// [`AppState::with_creds_path`] to a tempdir so they never touch the
    /// real user creds (CLAUDE.md test-isolation discipline).
    pub creds_path: Arc<PathBuf>,
    /// v0.8.8 F4 — single-slot status for the async Telegram `chat_id`
    /// capture (`POST .../chat-id/start` spawns a background poll; the
    /// `GET .../chat-id` poller reads this). `None` = no capture has been
    /// started this process. Single slot is enough: the web config flow is
    /// one operator binding one chat at a time.
    pub im_poll: Arc<Mutex<Option<TelegramChatIdPoll>>>,
    /// v0.8.22 P1 (review §3.1-3) — per-session SSE replay ring + live tap
    /// (see `crate::ring`'s module doc). Always present (even with no
    /// gateway — it just never gets fed), so the SSE handler doesn't need a
    /// separate `Option`. The feeder task is spawned once, alongside the
    /// gateway, in [`Self::with_gateway`].
    pub(crate) session_ring: Arc<crate::ring::SessionEventRing>,
    /// v0.9.0 W4 (F4) — the team view's cross-session replay ring + live tap
    /// (`GET /api/v1/agents/events`, see `crate::ring::GlobalEventRing`'s
    /// module doc). Same always-present / feeder-spawned-once discipline as
    /// `session_ring` above.
    pub(crate) global_ring: Arc<crate::ring::GlobalEventRing>,
    /// v0.9 T4 — MCP HTTP (`POST /mcp`) dispatch pieces. Built into a
    /// [`ccteam_im::mcp::McpDispatch`] per request via [`Self::mcp_dispatch`].
    /// `sink` / `pending` are `Some` when the daemon composition root hands
    /// them in (`ccteam start` with IM on); standalone `ccteam web` leaves
    /// them `None` so stateful tools return MCP `isError` (mirrors session
    /// REST 503 when `gateway` is `None`).
    pub mcp_sink: Option<ccteam_im::mcp::GatewayEventSink>,
    /// Shared pending-interaction registry for MCP `interaction/ask` /
    /// `permission/ask` (same Arc the gateway + mcp.sock hold).
    pub mcp_pending: Option<ccteam_im::mcp::PendingRegistry>,
    /// v0.9.0 reverse-connection — live satellite control channels + exec
    /// dial-back rendezvous (`GET /api/v1/hosts/channel` / `…/hosts/exec/{nonce}`
    /// register into this). `ccteam start` hands in the SAME hub it wires
    /// into the gateway's `HubRemoteHostProxy` ([`Self::with_host_hub`]);
    /// a standalone/default state gets its own (empty) hub — handlers work,
    /// spawns simply find no connected hosts.
    pub host_hub: Arc<ccteam_harness::HostChannelHub>,
    /// v0.9.15 DSHWEB — per-identity DSH web instance supervisor plus the
    /// reqwest client used by the companion-port byte proxy. These are not
    /// agent sessions and never enter the gateway live map.
    pub dsh_web: Arc<crate::dsh_web::DshWebSupervisor>,
    pub dsh_proxy_client: reqwest::Client,
    /// Coalesces concurrent daemon-wide status aggregations. The completed
    /// result is never retained after its in-flight computation finishes.
    pub(crate) status_singleflight: crate::routes::status::StatusSingleflight,
}

/// v0.8.8 F4 — state of an in-flight Telegram `chat_id` long-poll capture
/// (the async `POST .../chat-id/start` → `GET .../chat-id` flow).
#[derive(Debug, Clone, PartialEq)]
pub enum TelegramChatIdPoll {
    /// A background poll is running; the owner hasn't DMed the bot yet.
    Pending,
    /// Captured the owner's `chat_id` (persisted into
    /// `credentials.telegram.allowed_chat_ids` by the GET poller).
    Captured(i64),
    /// The poll window elapsed with no incoming message.
    Timeout,
    /// The poll failed (HTTP / API error); carries a human reason.
    Error(String),
}

/// Shared map of `chat_id` → live web-chat socket count.
pub type ChatConns = Arc<Mutex<HashMap<String, usize>>>;

/// Hard cap on parked outbound messages. With the connection registry
/// gating inserts, the backlog only fills while a recipient is offline;
/// the cap is a safety valve against an unbounded offline window.
pub const CHAT_BACKLOG_CAP: usize = 1024;

impl AppState {
    /// Resolve paths. Auth defaults to disabled — callers that want a token
    /// gate (the `serve()` non-loopback path) construct via
    /// [`AppState::with_auth`].
    pub fn new(paths: CcteamPaths) -> Self {
        Self::build(paths, AuthState::disabled())
    }

    /// Construct an `AppState` with an explicit auth state. Used by
    /// `serve()` once it has decided enabled / token from the bind
    /// heuristic + token-file path.
    pub fn with_auth(paths: CcteamPaths, auth: AuthState) -> Self {
        Self::build(paths, auth)
    }

    fn build(paths: CcteamPaths, auth: AuthState) -> Self {
        let (chat_outbound, _) = broadcast::channel(256);
        Self {
            paths: Arc::new(paths),
            auth: Arc::new(auth),
            pty: PtyRegistry::new(),
            chat_inbound: None,
            chat_outbound,
            chat_backlog: Arc::new(Mutex::new(Vec::new())),
            chat_conns: Arc::new(Mutex::new(HashMap::new())),
            gateway: None,
            session_principals: None,
            native_bindings: Arc::new(ccteam_im::native_bindings::NativeBindings::new()),
            creds_path: Arc::new(ccteam_im::credentials::default_path()),
            im_poll: Arc::new(Mutex::new(None)),
            session_ring: Arc::new(crate::ring::SessionEventRing::new()),
            global_ring: Arc::new(crate::ring::GlobalEventRing::new()),
            mcp_sink: None,
            mcp_pending: None,
            host_hub: Arc::new(ccteam_harness::HostChannelHub::default()),
            dsh_web: Arc::new(crate::dsh_web::DshWebSupervisor::disabled()),
            dsh_proxy_client: reqwest::Client::new(),
            status_singleflight: crate::routes::status::StatusSingleflight::default(),
        }
    }

    pub fn with_dsh_web(mut self, supervisor: Arc<crate::dsh_web::DshWebSupervisor>) -> Self {
        self.dsh_web = supervisor;
        self
    }

    /// v0.9.0 reverse-connection — share the daemon's host-channel hub (the
    /// same instance the gateway's `HubRemoteHostProxy` opens execs
    /// through) so satellite WS registrations and spawn-path lookups meet.
    pub fn with_host_hub(mut self, hub: Arc<ccteam_harness::HostChannelHub>) -> Self {
        self.host_hub = hub;
        self
    }

    pub fn with_chat_bridge(
        mut self,
        inbound: mpsc::Sender<WebChannelMessage>,
        outbound: broadcast::Sender<WebSendMessage>,
        backlog: Arc<Mutex<Vec<WebSendMessage>>>,
        conns: ChatConns,
    ) -> Self {
        self.chat_inbound = Some(inbound);
        self.chat_outbound = outbound;
        self.chat_backlog = backlog;
        self.chat_conns = conns;
        self
    }

    /// V0.8.6 W5b — attach the live IM gateway handle. `ccteam start`
    /// builds the `Arc<Mutex<Gateway>>` once (composition root) and clones
    /// it into the web state factory here and into the daemon, so both
    /// drive the *same* in-memory session map. The standalone "internal
    /// web" path never calls this, leaving `gateway = None` so session
    /// endpoints return 503.
    ///
    /// v0.8.22 P1 (review §3.1-3) — also spawns the ONE persistent
    /// [`crate::ring::spawn_ring_feeder`] task for this gateway, so the SSE
    /// replay ring stays populated for as long as the daemon runs,
    /// independent of whether any per-session SSE client is connected. This
    /// is a composition-root call (mirrors the gateway attach itself): call
    /// it more than once and you get one feeder task per call, each
    /// independently recording the same events into the ring under
    /// different seqs — harmless in practice (nothing production does this)
    /// but not something to do casually.
    pub fn with_gateway(
        mut self,
        gateway: Arc<Mutex<ccteam_im::gateway::Gateway>>,
        principals: Arc<ccteam_im::principals::SessionPrincipals>,
    ) -> Self {
        self.session_principals = Some(Arc::clone(&principals));
        // v0.10 — reap the `Mcp-Session-Id` bindings of hand-started clients that
        // went away without saying so (most of them: only codex and grok were
        // observed sending `DELETE`). Same one-task-per-`with_gateway` discipline
        // as the feeders below.
        crate::routes::mcp::spawn_binding_reaper(
            Arc::clone(&gateway),
            principals,
            Arc::clone(&self.native_bindings),
        );
        crate::ring::spawn_ring_feeder(Arc::clone(&gateway), Arc::clone(&self.session_ring));
        // v0.9.0 W4 — the team view's global feeder, spawned alongside the
        // per-sid one (same composition-root call, same "one feeder per
        // `with_gateway` call" caveat documented above).
        crate::ring::spawn_global_ring_feeder(Arc::clone(&gateway), Arc::clone(&self.global_ring));
        self.gateway = Some(gateway);
        self
    }

    /// [`Self::with_gateway`] for a caller that still OWNS the gateway — it
    /// takes the principal registry off it before wrapping, so the two can
    /// never be wired from different gateways.
    pub fn with_gateway_owned(self, gateway: ccteam_im::gateway::Gateway) -> Self {
        let principals = gateway.principals();
        self.with_gateway(Arc::new(Mutex::new(gateway)), principals)
    }

    /// v0.8.8 F4 — point the `config/im/*` handlers at a non-default
    /// credentials file. Integration tests pass a tempdir path so reading
    /// and writing IM creds never touches the real
    /// `~/.ccteam/im/credentials.json` (CLAUDE.md test-isolation rule). Not
    /// `#[cfg(test)]` because `ccteam-web` tests live in a separate crate
    /// (own compilation unit) and can't see `cfg(test)` items.
    pub fn with_creds_path(mut self, path: PathBuf) -> Self {
        self.creds_path = Arc::new(path);
        self
    }

    /// v0.9 T4 — attach the MCP dispatch pieces the daemon composition root
    /// already owns for `mcp.sock`. `ccteam start` clones the same sink /
    /// pending into web (gateway is attached separately via
    /// [`Self::with_gateway`]) so `POST /mcp` drives the live session map.
    /// Standalone `ccteam web` never calls this — protocol-core tools
    /// (`status` / `tools/list`) still work; gateway-backed
    /// tools return MCP `isError`.
    pub fn with_mcp(
        mut self,
        sink: Option<ccteam_im::mcp::GatewayEventSink>,
        pending: Option<ccteam_im::mcp::PendingRegistry>,
    ) -> Self {
        self.mcp_sink = sink;
        self.mcp_pending = pending;
        self
    }

    /// Build a per-request [`ccteam_im::mcp::McpDispatch`] from the pieces
    /// stored on this state. Cheap (clones Arcs / Option senders).
    pub fn mcp_dispatch(&self) -> ccteam_im::mcp::McpDispatch {
        ccteam_im::mcp::McpDispatch {
            paths: (*self.paths).clone(),
            sink: self.mcp_sink.clone(),
            pending: self.mcp_pending.clone(),
            gateway: self.gateway.clone(),
        }
    }
}
