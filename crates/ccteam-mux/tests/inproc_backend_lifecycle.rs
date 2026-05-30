//! V0.8 W1 — InProcBackend lifecycle smoke test exercised through
//! the trait object (not the concrete impl) to catch any trait-vs-impl
//! signature drift.

use std::path::PathBuf;
use std::sync::Arc;

use ccteam_mux::{InProcBackend, MuxBackend, MuxSessionSpec};

fn make_spec(name: &str) -> MuxSessionSpec {
    MuxSessionSpec::new(name, vec!["true".into()], PathBuf::from("/tmp"))
}

#[tokio::test]
async fn spawn_exists_kill_through_trait_object() {
    let backend: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());

    let id = backend.spawn(make_spec("lifecycle-1")).await.unwrap();
    assert!(backend.exists(&id).await.unwrap());

    // `is_alive` defaults to `exists` when `expected_pid` is None.
    assert!(backend.is_alive(&id, None).await.unwrap());

    backend.kill(&id).await.unwrap();
    assert!(!backend.exists(&id).await.unwrap());
}

#[tokio::test]
async fn send_text_errors_with_not_applicable() {
    let backend: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());
    let id = backend.spawn(make_spec("send-target")).await.unwrap();
    let err = backend.send_text(&id, "hi").await.unwrap_err();
    assert!(err.to_string().contains("not applicable"));
}

#[tokio::test]
async fn subscribe_returns_empty_stream() {
    use futures::StreamExt;
    let backend: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());
    let id = backend.spawn(make_spec("sub-target")).await.unwrap();
    let mut stream = backend.subscribe(&id).await.unwrap();
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn kill_is_idempotent() {
    let backend: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());
    let id = backend.spawn(make_spec("k-1")).await.unwrap();
    backend.kill(&id).await.unwrap();
    // Second kill on a gone session — must not error.
    backend.kill(&id).await.unwrap();
}
