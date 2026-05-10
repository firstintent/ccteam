//! Axum routers for the ccteam web layer.
//!
//! M5.0 ships only `/health`. M5.1 will mount `dashboard` / `project`
//! / `assets`; M5.2 mounts `sse` + `screenshot`; M5.3 mounts
//! `actions` + auth middleware.

use axum::Router;

pub mod health;

/// Compose every M5.x sub-router available at the current ship state.
/// Currently returns just the M5.0 health router.
pub fn router() -> Router {
    Router::new().merge(health::router())
}
