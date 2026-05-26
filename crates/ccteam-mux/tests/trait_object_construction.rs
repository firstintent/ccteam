//! V0.8 W1 — verify each `MuxBackend` impl is constructible as
//! `Arc<dyn MuxBackend>` (dyn-compat / object safety check).
//!
//! The trait is `async fn` heavy; this test catches accidental
//! `Self: Sized` bounds or `&mut self` slips at compile time. It's
//! cheap and a useful guardrail when the trait surface evolves.

use std::sync::Arc;

use ccteam_mux::{from_env, InProcBackend, MuxBackend, TmuxBackend};

#[test]
fn tmux_backend_is_dyn_compat() {
    let _: Arc<dyn MuxBackend> = Arc::new(TmuxBackend::new());
}

#[test]
fn inproc_backend_is_dyn_compat() {
    let _: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());
}

#[test]
fn from_env_default_yields_tmux() {
    // No env override → expect the tmux default (regardless of
    // whether tmux is actually installed; `from_env` just constructs
    // the backend, it doesn't probe).
    std::env::remove_var("CCTEAM_MUX_BACKEND");
    let _backend = from_env().expect("from_env default should succeed");
}

#[test]
fn from_env_inproc_test_yields_inproc() {
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
fn from_env_rmux_errors_until_w2() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "rmux");
    let err = match from_env() {
        Ok(_) => panic!("rmux should not yet construct"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("W2"));
    std::env::remove_var("CCTEAM_MUX_BACKEND");
}
