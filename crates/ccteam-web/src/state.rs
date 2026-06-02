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
    /// V0.5.0 F96 — Anthropic `~/.claude/` root for Agent Teams.
    /// Resolved at AppState construction via
    /// `crate::teams::claude_home()` (env override
    /// `CCTEAM_CLAUDE_HOME` honored). Read-only data path; the
    /// orchestrator never writes here.
    pub claude_home: Arc<PathBuf>,
    /// V0.5.0 F96 — path to the global teams progress jsonl
    /// (`~/.ccteam/teams-progress.jsonl`). Distinct from per-project
    /// `~/.ccteam/progress/<slug>.jsonl`. The teams SSE channel tails
    /// this file. Tests override via `with_teams_progress_path`.
    ///
    /// F95 added `CcteamPaths::teams_progress_jsonl()` as the
    /// canonical resolver. We construct the same string here
    /// (`paths.root.join("teams-progress.jsonl")`) — when F95 lands in
    /// this worktree the line below becomes
    /// `paths.teams_progress_jsonl()` with no behaviour change.
    pub teams_progress_path: Arc<PathBuf>,
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
        let claude_home = crate::teams::claude_home().unwrap_or_else(|err| {
            tracing::warn!(
                ?err,
                "ccteam-web: claude_home() resolution failed; defaulting to /tmp/.claude"
            );
            PathBuf::from("/tmp/.claude")
        });
        // F95 canonicalised the path; switch over now that it's
        // available (was `paths.root.join("teams-progress.jsonl")` pre-F95).
        let teams_progress_path = paths.teams_progress_jsonl();
        let (chat_outbound, _) = broadcast::channel(256);
        Self {
            paths: Arc::new(paths),
            bus,
            auth: Arc::new(auth),
            pty: PtyRegistry::new(),
            claude_home: Arc::new(claude_home),
            teams_progress_path: Arc::new(teams_progress_path),
            chat_inbound: None,
            chat_outbound,
            chat_backlog: Arc::new(Mutex::new(Vec::new())),
            chat_conns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Construct an `AppState` with a pre-built bus. Used by tests
    /// that want to publish events directly via
    /// [`EventBus::publish_for_test`] without spinning a watcher.
    #[cfg(test)]
    pub fn with_bus(paths: CcteamPaths, bus: EventBus) -> Self {
        let claude_home =
            crate::teams::claude_home().unwrap_or_else(|_| PathBuf::from("/tmp/.claude"));
        let teams_progress_path = paths.teams_progress_jsonl();
        let (chat_outbound, _) = broadcast::channel(256);
        Self {
            paths: Arc::new(paths),
            bus,
            auth: Arc::new(AuthState::disabled()),
            pty: PtyRegistry::new(),
            claude_home: Arc::new(claude_home),
            teams_progress_path: Arc::new(teams_progress_path),
            chat_inbound: None,
            chat_outbound,
            chat_backlog: Arc::new(Mutex::new(Vec::new())),
            chat_conns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// V0.5.0 F96 — replace the Anthropic teams root for tests that
    /// stage `<tmp>/.claude/teams/<>/` without touching the real
    /// `$HOME/.claude`. Returns the modified state by value so
    /// callers can chain on `AppState::new(...)`.
    pub fn with_claude_home(mut self, claude_home: PathBuf) -> Self {
        self.claude_home = Arc::new(claude_home);
        self
    }

    /// V0.5.0 F96 — override the teams progress jsonl path. Tests
    /// point this at a tempdir file the test seeds + appends to.
    pub fn with_teams_progress_path(mut self, path: PathBuf) -> Self {
        self.teams_progress_path = Arc::new(path);
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
}
