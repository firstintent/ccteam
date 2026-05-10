//! Axum routers for the ccteam web layer.
//!
//! M5.0 shipped `/health`. M5.1 adds `dashboard` / `project` /
//! `assets`. M5.2 will mount `sse` + `screenshot`; M5.3 mounts
//! `actions` + auth middleware.

use axum::Router;

use crate::state::AppState;

pub mod assets;
pub mod dashboard;
pub mod health;
pub mod project;

/// Compose every M5.x sub-router available at the current ship state.
/// `health` is state-less (M5.0 contract) so it merges in without an
/// `AppState`; the M5.1 routers are stateful and need the same
/// `AppState` so the call site builds them via `.with_state(...)` in
/// `lib::router`.
pub fn stateful_router() -> Router<AppState> {
    Router::new()
        .merge(dashboard::router())
        .merge(project::router())
        .merge(assets::router())
}

/// Stateless routers (currently just `/health`).
pub fn stateless_router() -> Router {
    Router::new().merge(health::router())
}
