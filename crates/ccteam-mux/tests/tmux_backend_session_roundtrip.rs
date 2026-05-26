//! V0.8 W1 — TmuxBackend end-to-end smoke test through the trait
//! object. Gated on a real tmux being on PATH (skipped silently
//! otherwise so CI / sandboxed dev machines without tmux still pass).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ccteam_mux::tmux_ops::tmux_available;
use ccteam_mux::{MuxBackend, MuxSessionId, MuxSessionSpec, TmuxBackend};

fn skip_if_no_tmux() -> bool {
    if !tmux_available() {
        eprintln!(
            "tmux_backend_session_roundtrip: skipping — tmux not on PATH (dev / CI \
             without tmux installed)"
        );
        true
    } else {
        false
    }
}

fn random_session_name(base: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ccteam-mux-w1-{base}-{nanos}")
}

#[tokio::test]
async fn spawn_send_capture_kill_through_trait() {
    if skip_if_no_tmux() {
        return;
    }
    let backend: Arc<dyn MuxBackend> = Arc::new(TmuxBackend::new());
    let session_name = random_session_name("roundtrip");
    let spec = MuxSessionSpec::new(
        &session_name,
        vec!["sh".into(), "-c".into(), "sleep 60".into()],
        PathBuf::from("/tmp"),
    );

    let id = backend.spawn(spec).await.expect("spawn must succeed");
    assert_eq!(id.0, session_name);
    assert!(backend.exists(&id).await.unwrap(), "session must exist");

    // pane_pid populated after spawn.
    let pid = backend.pane_pid(&id).await.unwrap();
    assert!(pid.is_some(), "pane_pid must report PID after spawn");

    // is_alive composite (default-method on trait).
    assert!(
        backend.is_alive(&id, pid).await.unwrap(),
        "is_alive must succeed for live session"
    );

    // list_pane_pids → at least one entry.
    let pane_pids = backend.list_pane_pids(&id).await.unwrap();
    assert!(
        !pane_pids.is_empty(),
        "list_pane_pids must report at least one pid"
    );

    // pane_dims → Some (the 200×50 spawn workaround forces it).
    let dims = backend.pane_dims(&id).await.unwrap();
    assert!(dims.is_some(), "pane_dims should be Some after spawn");

    // send_text + send_enter — no panic (the inner sh shell drops
    // the keystrokes; we just verify the tmux call succeeds).
    backend.send_text(&id, "echo hello").await.unwrap();
    backend.send_enter(&id).await.unwrap();

    // Wait a tick so capture has something to show.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let captured = backend.capture(&id, 50, false).await.unwrap();
    // Best-effort assertion: capture may be empty (sh swallows fast)
    // but must not error.
    assert!(captured.len() < 64 * 1024, "capture must bound output");

    backend.kill(&id).await.unwrap();
    assert!(
        !backend.exists(&id).await.unwrap(),
        "session must be gone after kill"
    );
}

#[tokio::test]
async fn kill_is_idempotent_on_missing_session() {
    if skip_if_no_tmux() {
        return;
    }
    let backend: Arc<dyn MuxBackend> = Arc::new(TmuxBackend::new());
    let id = MuxSessionId::new(random_session_name("absent"));
    // Must not error on a session that never existed.
    backend.kill(&id).await.unwrap();
}

#[tokio::test]
async fn subscribe_returns_w2_error() {
    // No tmux required; the W1 stub is a synchronous Err return.
    let backend: Arc<dyn MuxBackend> = Arc::new(TmuxBackend::new());
    let id = MuxSessionId::new("does-not-need-to-exist");
    // `MuxEventStream` is `Pin<Box<dyn Stream>>` which doesn't impl
    // Debug, so the result can't use `unwrap_err()`; match it.
    let err = match backend.subscribe(&id).await {
        Ok(_) => panic!("subscribe should error in W1"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("W2") || msg.contains("PtyRegistry"),
        "subscribe error should point to W2 / PtyRegistry: got `{msg}`"
    );
}

#[tokio::test]
async fn register_pattern_w1_stub_is_ok() {
    // W1 stub returns Ok(()) so adapter code can call it pre-W2.
    let backend: Arc<dyn MuxBackend> = Arc::new(TmuxBackend::new());
    let id = MuxSessionId::new("any-name");
    backend
        .register_pattern(&id, "claude.idle".into(), r"\[idle\]".into())
        .await
        .unwrap();
}
