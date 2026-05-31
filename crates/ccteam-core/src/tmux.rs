//! V0.8 W1 — back-compat re-export over `ccteam-harness::tmux_ops`.
//!
//! The primitive `tmux` CLI wrapper (`TmuxSession`, free fns
//! `capture_pane_*`, `query_pane_dims_*`, `pid_is_alive`,
//! `tmux_available`, …) moved into `ccteam-harness/src/tmux_ops.rs` so the
//! `ProcessBackend` trait + `TmuxBackend` impl can live in the same crate
//! without inducing a `ccteam-harness ↔ ccteam-core` cargo cycle.
//!
//! Everything is re-exported from here so existing callers (production
//! sites + integration tests) keep compiling unchanged. The only item
//! whose body still lives in this file is `session_name_for_project`,
//! which depends on `CcteamPaths` + `ProjectState` (both owned by
//! `ccteam-core`) and would re-introduce the cycle if moved.
//!
//! V0.9 retires this module entirely once the `ProcessBackend` trait has
//! burned in.

pub use ccteam_harness::tmux_ops::{
    capture_pane_tail, capture_pane_tail_from_session, capture_pane_with_ansi,
    capture_pane_with_ansi_from_session, list_sessions, pid_is_alive, query_pane_dims,
    query_pane_dims_from_session, resize_window, session_name_for_slug, tmux_available,
    TmuxSession, SESSION_PREFIX,
};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;

/// Resolve the live tmux session name for a project slug.
///
/// Most projects use the conventional `ccteam-<slug>` name, but
/// meta-agent sessions intentionally use `ccteam-meta-<handle>`.
/// `state.json.tmux_session` is the source of truth for those cases.
/// If the state is missing or malformed we fall back to the
/// conventional name so diagnostic surfaces still degrade cleanly.
///
/// Stays in `ccteam-core` (rather than `ccteam-harness`) because the
/// `CcteamPaths` + `ProjectState` dependency would re-introduce the
/// cargo cycle that the W1 move was designed to break.
pub fn session_name_for_project(paths: &CcteamPaths, slug: &str) -> String {
    let fallback = session_name_for_slug(slug);
    let state_path = paths.project_state(slug);
    match ProjectState::load(&state_path) {
        Ok(state) if !state.tmux_session.trim().is_empty() => state.tmux_session,
        _ => fallback,
    }
}
