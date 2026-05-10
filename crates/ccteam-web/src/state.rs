//! Shared application state passed to every axum handler. Wraps the
//! resolved [`CcteamPaths`] so handlers don't re-resolve `from_env()`
//! per request (and tests can swap the projects_root root via
//! `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` before constructing
//! [`AppState`]).

use std::sync::Arc;

use ccteam_core::CcteamPaths;

#[derive(Clone)]
pub struct AppState {
    pub paths: Arc<CcteamPaths>,
}

impl AppState {
    pub fn new(paths: CcteamPaths) -> Self {
        Self {
            paths: Arc::new(paths),
        }
    }
}
