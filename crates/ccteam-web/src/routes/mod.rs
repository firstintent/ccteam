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
// v0.8.6 W5b ResDisk — resource API: capabilities probe.
pub mod capabilities;
pub mod chat_ws;
pub mod dashboard;
pub mod harness_sse;
pub mod health;
// V0.6.1 F139 — `POST /internal/hook/:kind[/:action]` daemon-side hook
// dispatcher (replaces per-hook `ccteam internal hook ...` cold spawn).
pub mod internal_hook;
// v0.8.7 W5 (Item E) — OpenAPI auto-docs. Aggregates every `/api/v1`
// handler into one `OpenApiRouter` (single source with the route table)
// + serves the spec (`/api/v1/openapi.json`) and Scalar UI (`/api/docs`).
pub mod openapi;
pub mod pane_snapshot;
pub mod project;
// v0.8.6 W5b ResDisk — resource API: project lifecycle (POST/DELETE) +
// project-scoped roles (GET/PUT). `project` (singular) is the legacy
// redirect handler; `projects` / `roles` are the new resource routers.
pub mod projects;
pub mod pty_ws;
pub mod roles;
pub mod screenshot;
pub mod session;
// v0.8.6 W5b ResSessions — session resource API over the gateway spine.
pub mod sessions_api;
pub mod sse;
// V0.5.0 F96 — Agent Teams JSON API + SSE channel.
pub mod teams_api;
pub mod teams_sse;

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
        // v0.8.7 W5 (Item E) — the ENTIRE `/api/v1` resource surface
        // (capabilities · projects GET/POST/DELETE · roles GET/PUT ·
        // sessions GET/POST + {sid}/{turn,events,stop} · workflow panels ·
        // teams + SSE) is now aggregated by `openapi::api_v1_router()` into
        // one `OpenApiRouter` so the spec is generated from the same route
        // registrations (single source, anti-drift). It also mounts the
        // spec at `/api/v1/openapi.json` and the Scalar UI at `/api/docs`,
        // both inside this auth-gated stateful router.
        .merge(openapi::api_v1_router())
        .merge(chat_ws::router())
        .merge(pty_ws::router())
}

/// Stateless routers (currently just `/health`). M5.3 keeps `/health`
/// outside the auth gate so ops monitoring works without baking in
/// the secret token.
pub fn stateless_router() -> Router {
    Router::new().merge(health::router())
}
