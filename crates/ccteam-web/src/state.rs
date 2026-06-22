//! Shared application state passed to every axum handler. Wraps the
//! resolved [`CcteamPaths`] so handlers don't re-resolve `from_env()`
//! per request (and tests can swap the projects_root root via
//! `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` before constructing
//! [`AppState`]).
//!
//! V0.3 M5.2 added the [`EventBus`] field so SSE handlers can
//! subscribe to the single watcher → broadcast pump. The bus is
//! constructed eagerly in [`AppState::new`]; tests that don't care
//! about live events use [`AppState::new_no_bus`] which still hands
//! out a working bus (the watcher just has nothing to watch).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ccteam_core::CcteamPaths;
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::auth::AuthState;
use crate::chat_protocol::{WebChannelMessage, WebSendMessage};
use crate::pty::PtyRegistry;
use crate::watcher::{spawn_watcher, EventBus};

#[derive(Clone)]
pub struct AppState {
    pub paths: Arc<CcteamPaths>,
    /// Live progress event bus. Subscribers go through
    /// `bus.subscribe()`; the producer side is owned by the
    /// dedicated watcher thread spawned in [`AppState::new`].
    pub bus: EventBus,
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
    /// Resolve paths + spawn the progress watcher. If the watcher
    /// fails to start (e.g. progress dir cannot be created — rare),
    /// we log + fall back to an inert bus so the read-only routes
    /// (`/`, `/project/<slug>`) still serve. SSE will simply have no
    /// publisher; clients reconnect harmlessly.
    ///
    /// Auth defaults to disabled — callers that want a token gate
    /// (the `serve()` non-loopback path) construct via
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
        let bus = match spawn_watcher(paths.progress_dir(), paths.harness_dir()) {
            Ok(b) => b,
            Err(err) => {
                tracing::error!(
                    ?err,
                    progress_dir = %paths.progress_dir().display(),
                    harness_dir = %paths.harness_dir().display(),
                    "ccteam-web: progress + harness watchers failed to start; SSE will be inert",
                );
                EventBus::inert()
            }
        };
        let (chat_outbound, _) = broadcast::channel(256);
        Self {
            paths: Arc::new(paths),
            bus,
            auth: Arc::new(auth),
            pty: PtyRegistry::new(),
            chat_inbound: None,
            chat_outbound,
            chat_backlog: Arc::new(Mutex::new(Vec::new())),
            chat_conns: Arc::new(Mutex::new(HashMap::new())),
            gateway: None,
            creds_path: Arc::new(ccteam_im::credentials::default_path()),
            im_poll: Arc::new(Mutex::new(None)),
        }
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
    pub fn with_gateway(mut self, gateway: Arc<Mutex<ccteam_im::gateway::Gateway>>) -> Self {
        self.gateway = Some(gateway);
        self
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
}
