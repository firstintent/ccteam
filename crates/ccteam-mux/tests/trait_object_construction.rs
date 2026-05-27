//! V0.8 W1 — verify each `MuxBackend` impl is constructible as
//! `Arc<dyn MuxBackend>` (dyn-compat / object safety check).
//!
//! The trait is `async fn` heavy; this test catches accidental
//! `Self: Sized` bounds or `&mut self` slips at compile time. It's
//! cheap and a useful guardrail when the trait surface evolves.

use std::sync::{Arc, Mutex, OnceLock};

use ccteam_mux::{from_env, InProcBackend, MuxBackend, RmuxBackend, TmuxBackend};

/// Tests that mutate `CCTEAM_MUX_BACKEND` must serialize against each
/// other — cargo test parallelism otherwise races the env var (one
/// test's `set_var("rmux")` is observable by another test's
/// `from_env()` call in a different thread). One in-crate mutex
/// suffices; no need for the `serial_test` crate here.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn tmux_backend_is_dyn_compat() {
    let _: Arc<dyn MuxBackend> = Arc::new(TmuxBackend::new());
}

#[test]
fn inproc_backend_is_dyn_compat() {
    let _: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());
}

#[test]
fn rmux_backend_is_dyn_compat() {
    // V0.8 W2a — RmuxBackend constructs without contacting the daemon
    // (the SDK `Rmux` handle is lazily initialized on first
    // `connect_or_start`).
    let _: Arc<dyn MuxBackend> = Arc::new(RmuxBackend::new());
}

#[test]
fn from_env_default_yields_rmux() {
    // No env override → expect the rmux default (the bundled backend;
    // `from_env` just constructs it, it doesn't connect a daemon here).
    let _guard = match env_lock().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    std::env::remove_var("CCTEAM_MUX_BACKEND");
    let backend = from_env().expect("from_env default should succeed");
    assert_eq!(backend.backend_kind(), ccteam_mux::BackendKind::Rmux);
}

#[test]
fn from_env_inproc_test_yields_inproc() {
    let _guard = match env_lock().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    std::env::set_var("CCTEAM_MUX_BACKEND", "inproc-test");
    let backend = from_env().expect("inproc-test should succeed");
    // Sanity: list_sessions on a fresh InProcBackend is empty.
    let live = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(backend.list_sessions())
        .unwrap();
    assert!(live.is_empty());
    std::env::remove_var("CCTEAM_MUX_BACKEND");
}

#[test]
fn from_env_rmux_yields_rmux() {
    // V0.8 W2a — `CCTEAM_MUX_BACKEND=rmux` now constructs RmuxBackend
    // (daemon connect is lazy; this asserts the dispatch, not the
    // daemon spawn).
    let _guard = match env_lock().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    std::env::set_var("CCTEAM_MUX_BACKEND", "rmux");
    let _backend = from_env().expect("rmux should construct under W2a");
    std::env::remove_var("CCTEAM_MUX_BACKEND");
}
