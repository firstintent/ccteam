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
async fn register_pattern_compiles_and_stores() {
    // W2b: register_pattern now compiles the regex and stores it in the
    // per-session matcher consulted by subscribe. A valid regex is Ok;
    // an invalid one errors loudly. No tmux required (no subscribe).
    let backend = TmuxBackend::new();
    let id = MuxSessionId::new("any-name");
    backend
        .register_pattern(&id, "claude.idle".into(), r"\[idle\]".into())
        .await
        .expect("valid regex must register");
    let err = backend
        .register_pattern(&id, "bad".into(), r"(unclosed".into())
        .await;
    assert!(err.is_err(), "invalid regex must surface a compile error");
}

#[tokio::test]
async fn register_base_patterns_loads_claude_tier() {
    use ccteam_mux::patterns::PatternVendor;
    // No tmux required — register_base_patterns only touches the
    // in-memory matcher registry.
    let backend = TmuxBackend::new();
    let id = MuxSessionId::new("base-patterns-session");
    backend
        .register_base_patterns(&id, PatternVendor::Claude)
        .await
        .expect("base patterns must register");
}

/// Full subscribe integration: spawn a tmux session, register the
/// Claude base patterns, subscribe, send a line that trips a pattern,
/// and assert both OutputChunk and PatternMatched arrive on the stream.
///
/// Gated `#[ignore]` because it needs a real tmux AND a writable
/// `~/.ccteam/pty` FIFO dir (the relay invokes `tmux pipe-pane "cat >>
/// <fifo>"`). Run with `cargo test -p ccteam-mux -- --ignored`.
#[tokio::test]
#[ignore = "requires tmux on PATH + writable FIFO dir"]
async fn subscribe_streams_output_and_fires_pattern() {
    use ccteam_mux::patterns::PatternVendor;
    use ccteam_mux::MuxEvent;
    use futures::StreamExt;

    if skip_if_no_tmux() {
        return;
    }
    let backend = Arc::new(TmuxBackend::new());
    let session_name = random_session_name("subscribe");
    // Run a loop that re-emits the marker every 300ms so the pane stays
    // alive (no controlling tty under daemon-launch can make a bare
    // interactive shell exit immediately) and the marker fires
    // repeatedly within the collection window.
    let spec = MuxSessionSpec::new(
        &session_name,
        vec![
            "sh".into(),
            "-c".into(),
            "while true; do printf 'CCTEAM-MARKER-OK\\n'; sleep 0.3; done".into(),
        ],
        PathBuf::from("/tmp"),
    );
    let id = backend.spawn(spec).await.expect("spawn");
    // Give the pane a moment to come up before subscribing.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Register a pattern that matches the line the pane emits.
    backend
        .register_base_patterns(&id, PatternVendor::Claude)
        .await
        .unwrap();
    backend
        .register_pattern(&id, "test.marker".into(), r"CCTEAM-MARKER-(\w+)".into())
        .await
        .unwrap();

    let mut stream = backend.subscribe(&id).await.expect("subscribe");

    // Collect events for up to 5s; assert we see a chunk + the marker.
    let mut saw_chunk = false;
    let mut saw_marker = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), stream.next()).await {
            Ok(Some(MuxEvent::OutputChunk(_))) => saw_chunk = true,
            Ok(Some(MuxEvent::PatternMatched { regex_id, captured })) => {
                if regex_id == "test.marker" {
                    saw_marker = true;
                    assert_eq!(captured, "OK");
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {} // timeout tick — keep polling until deadline
        }
        if saw_chunk && saw_marker {
            break;
        }
    }

    // Drop the stream → relay refcount hits 0 → pipe-pane stop + FIFO
    // unlink (best-effort, in a spawned task).
    drop(stream);
    backend.kill(&id).await.unwrap();

    assert!(saw_chunk, "expected at least one OutputChunk");
    assert!(saw_marker, "expected the registered marker pattern to fire");
}
