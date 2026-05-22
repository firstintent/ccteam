//! Axum routers for the ccteam web layer.
//!
//! M5.0 shipped `/health`. M5.1 added `dashboard` / `project` /
//! `assets`. M5.2 added `sse` (`/sse/all` + `/sse/project/<slug>`) +
//! `screenshot` (`/screenshot/<slug>.png`). The pane snapshot route
//! adds raw ANSI bytes for browser-side xterm.js rendering. **M5.3
//! (this PR)** mounts `actions` (`POST
//! /api/<slug>/{btw,inject_decision,pause,resume}`) and the
//! `auth_layer` middleware that gates the entire stateful router when
//! token auth is enabled.

use axum::Router;

use crate::state::AppState;

pub mod actions;
pub mod api_v1;
pub mod assets;
pub mod dashboard;
pub mod harness_sse;
pub mod health;
// V0.6.1 F139 — `POST /internal/hook/:kind[/:action]` daemon-side hook
// dispatcher (replaces per-hook `ccteam internal hook ...` cold spawn).
pub mod internal_hook;
pub mod pane_snapshot;
pub mod project;
pub mod pty_ws;
pub mod screenshot;
pub mod session;
pub mod sse;
// V0.5.0 F96 — Agent Teams JSON API + SSE channel.
pub mod teams_api;
pub mod teams_sse;
// V0.6.3 F141 — `POST /webhook/{project}/{token}` HTTP→file ingress.
pub mod webhook;

/// Compose every M5.x sub-router available at the current ship state.
/// `health` is state-less (M5.0 contract) so it merges in without an
/// `AppState`; the M5.1 / M5.2 / M5.3 routers are stateful and need
/// the same `AppState` so the call site builds them via
/// `.with_state(...)` in `lib::router`.
pub fn stateful_router() -> Router<AppState> {
    Router::new()
        .merge(dashboard::router())
        .merge(project::router())
        .merge(session::router())
        .merge(assets::router())
        .merge(sse::router())
        .merge(harness_sse::router())
        .merge(pane_snapshot::router())
        .merge(screenshot::router())
        .merge(actions::router())
        .merge(internal_hook::router())
        .merge(api_v1::router())
        .merge(pty_ws::router())
        // V0.5.0 F96 — Agent Teams surface.
        .merge(teams_api::router())
        .merge(teams_sse::router())
}

/// Stateless routers (currently just `/health`). M5.3 keeps `/health`
/// outside the auth gate so ops monitoring works without baking in
/// the secret token.
pub fn stateless_router() -> Router {
    Router::new().merge(health::router())
}

/// V0.6.3 F141 — webhook ingress router. It is stateful (needs
/// `AppState` to resolve project paths) but mounted **outside** the
/// `auth_layer` bearer gate: the `POST /webhook/{project}/{token}`
/// route carries its own per-project token in the URL path. The
/// caller (`lib::router_with_state`) attaches `AppState` via
/// `.with_state(...)` before merging it at the top level.
pub fn webhook_router() -> Router<AppState> {
    webhook::router()
}
