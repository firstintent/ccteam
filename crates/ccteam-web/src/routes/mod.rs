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
// v0.8.18 柱1 — host-keyed agent report (`GET /api/v1/hosts` + `/{host}` +
// the only writable endpoint `POST .../register-mcp`). The host-first
// successor to the flat `capabilities` probe; shares `hosts::probe_bin`.
pub mod chat_ws;
pub mod dashboard;
pub mod harness_sse;
pub mod health;
pub mod hosts;
// v0.8.8 F4 — web IM credential configuration (Telegram + Lark; masked
// read that never echoes secrets; validate-before-persist; restart-required).
pub mod im_config;
// V0.6.1 F139 — `POST /internal/hook/:kind[/:action]` daemon-side hook
// dispatcher (replaces per-hook `ccteam internal hook ...` cold spawn).
pub mod internal_hook;
// v0.8.9 Phase 2 — ccteam-hub plugin marketplace REST surface (global +
// per-project catalog, body preview, install). The network face of
// `ccteam_im::hub`; merged into the `/api/v1` OpenApiRouter (auto auth-gated).
pub mod marketplace;
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
// v0.8.9 Phase 4 — daemon-wide status aggregate (`GET /api/v1/status`) for the
// unified-shell cost pill + Status view. Merged into the `/api/v1`
// OpenApiRouter (auto auth-gated).
pub mod status;
// v0.8.8 B5 — 共享「sid → per-session pane 名」解析(pty_ws + pane_snapshot
// 共用,避免 vendor 分支两份漂移)。
pub mod session_pane;
pub mod sse;
// v0.8.18 档1 — per-user web tenant management (web-first user CRUD; admin-gated).
pub mod users;

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
