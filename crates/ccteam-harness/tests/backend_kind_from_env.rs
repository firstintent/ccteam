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

use ccteam_harness::{backend_kind_from_env, BackendKind};

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
fn unset_yields_rmux() {
    with_backend_env(None, || {
        assert_eq!(backend_kind_from_env(), BackendKind::Rmux);
    });
}

#[test]
fn empty_yields_rmux() {
    with_backend_env(Some(""), || {
        assert_eq!(backend_kind_from_env(), BackendKind::Rmux);
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
fn unknown_falls_back_to_rmux() {
    // `from_env` errors on an unknown value; `backend_kind_from_env`
    // (used only by sync CLI branches that already error elsewhere via
    // `from_env`) degrades to rmux — the bundled always-available
    // backend — so a typo'd value never misroutes onto a possibly-absent
    // tmux. Only an explicit `tmux` opts out.
    with_backend_env(Some("bogus"), || {
        assert_eq!(backend_kind_from_env(), BackendKind::Rmux);
    });
}

// V0.8 W5-fix regression guard: `default_backend()` must HONOR
// CCTEAM_MUX_BACKEND, not hardcode tmux. Before the fix the W2c-migrated
// claude_tui / codex_exec adapters (which call default_backend()) stayed
// on tmux even under CCTEAM_MUX_BACKEND=rmux, so the feature flag had no
// effect on production mode-3 spawns. These assert the routing reaches
// the backend the trait object actually is, via backend_kind().

#[test]
fn default_backend_honors_rmux_env() {
    with_backend_env(Some("rmux"), || {
        assert_eq!(
            ccteam_harness::default_backend().backend_kind(),
            BackendKind::Rmux,
            "default_backend() must route to rmux under CCTEAM_MUX_BACKEND=rmux"
        );
    });
}

#[test]
fn default_backend_defaults_to_rmux() {
    with_backend_env(None, || {
        assert_eq!(
            ccteam_harness::default_backend().backend_kind(),
            BackendKind::Rmux,
            "default_backend() default (env unset) is rmux — the bundled backend"
        );
    });
}

#[test]
fn default_backend_unknown_value_falls_back_to_rmux() {
    with_backend_env(Some("bogus"), || {
        assert_eq!(
            ccteam_harness::default_backend().backend_kind(),
            BackendKind::Rmux,
            "default_backend() degrades a typo'd value to rmux (the bundled always-available backend)"
        );
    });
}
