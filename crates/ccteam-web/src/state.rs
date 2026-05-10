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

use std::sync::Arc;

use ccteam_core::CcteamPaths;

use crate::auth::AuthState;
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
}

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
        Self {
            paths: Arc::new(paths),
            bus,
            auth: Arc::new(auth),
        }
    }

    /// Construct an `AppState` with a pre-built bus. Used by tests
    /// that want to publish events directly via
    /// [`EventBus::publish_for_test`] without spinning a watcher.
    #[cfg(test)]
    pub fn with_bus(paths: CcteamPaths, bus: EventBus) -> Self {
        Self {
            paths: Arc::new(paths),
            bus,
            auth: Arc::new(AuthState::disabled()),
        }
    }
}
