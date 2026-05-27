//! V0.8 W5 — `backend_kind_from_env` is the sync, side-effect-free
//! resolver the CLI uses to branch interactive attach + non-interactive
//! peek on the configured mux backend WITHOUT constructing a backend
//! (and, for rmux, without lazily connecting a daemon). This guards the
//! parity between its match and `from_env`'s dispatch.
//!
//! Env-mutating tests run in their own process (integration test
//! binary) per the workspace convention, but cargo still parallelizes
//! tests within this binary — serialize `CCTEAM_MUX_BACKEND` writes
//! against an in-crate mutex.

use std::sync::{Mutex, OnceLock};

use ccteam_mux::{backend_kind_from_env, BackendKind};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_backend_env<F: FnOnce()>(value: Option<&str>, body: F) {
    let _guard = match env_lock().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match value {
        Some(v) => std::env::set_var("CCTEAM_MUX_BACKEND", v),
        None => std::env::remove_var("CCTEAM_MUX_BACKEND"),
    }
    body();
    std::env::remove_var("CCTEAM_MUX_BACKEND");
}

#[test]
fn unset_yields_tmux() {
    with_backend_env(None, || {
        assert_eq!(backend_kind_from_env(), BackendKind::Tmux);
    });
}

#[test]
fn empty_yields_tmux() {
    with_backend_env(Some(""), || {
        assert_eq!(backend_kind_from_env(), BackendKind::Tmux);
    });
}

#[test]
fn explicit_tmux_yields_tmux() {
    with_backend_env(Some("tmux"), || {
        assert_eq!(backend_kind_from_env(), BackendKind::Tmux);
    });
}

#[test]
fn rmux_yields_rmux() {
    with_backend_env(Some("rmux"), || {
        assert_eq!(backend_kind_from_env(), BackendKind::Rmux);
    });
}

#[test]
fn inproc_test_yields_inproc() {
    with_backend_env(Some("inproc-test"), || {
        assert_eq!(backend_kind_from_env(), BackendKind::InProc);
    });
}

#[test]
fn unknown_falls_back_to_tmux() {
    // `from_env` errors on an unknown value; `backend_kind_from_env`
    // (used only by sync CLI branches that already error elsewhere via
    // `from_env`) takes the safe default so an attach branch never
    // misroutes a typo'd value into the rmux path.
    with_backend_env(Some("bogus"), || {
        assert_eq!(backend_kind_from_env(), BackendKind::Tmux);
    });
}
