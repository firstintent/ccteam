//! V0.8 W2a — RmuxBackend end-to-end smoke through the trait object.
//!
//! `#[ignore]` because the test spawns a real rmux daemon by re-execing
//! the ccteam binary via the `--__internal-daemon` argv form. Run with:
//!
//! ```sh
//! cargo build --bin ccteam
//! cargo test -p ccteam-harness --test rmux_backend_session_roundtrip -- \
//!     --ignored --nocapture
//! ```
//!
//! Mirrors `tmux_backend_session_roundtrip.rs` shape — same trait, same
//! flow, just routed through the rmux SDK daemon instead of `tmux`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ccteam_harness::{MuxEvent, MuxSessionId, MuxSessionSpec, PaneBackend, RmuxBackend};
use futures::StreamExt;

fn random_session_name(base: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ccteam-harness-w2a-{base}-{nanos}")
}

/// Locate the ccteam binary built in the workspace's target dir. The
/// rmux SDK spawns this binary with `--__internal-daemon <socket>` to
/// host the daemon. We honor `CARGO_BIN_EXE_ccteam` if cargo sets it
/// for this test (only happens when `ccteam-harness` declares ccteam-cli
/// as a `[[bin]]` reference, which it doesn't); otherwise fall back to
/// `target/debug/ccteam` relative to the workspace root.
fn locate_ccteam_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CCTEAM_TEST_BIN") {
        return Some(PathBuf::from(path));
    }
    // CARGO_MANIFEST_DIR for ccteam-harness = .../crates/ccteam-harness; the
    // workspace root is two levels up.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent()?.parent()?;
    let candidate = workspace_root.join("target/debug/ccteam");
    if candidate.exists() {
        return Some(candidate);
    }
    let release = workspace_root.join("target/release/ccteam");
    if release.exists() {
        return Some(release);
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn spawn_send_capture_kill_through_trait() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!(
            "SKIP: ccteam binary not found in target/{{debug,release}}/ccteam; build with \
             `cargo build --bin ccteam` (or set CCTEAM_TEST_BIN=...) and rerun."
        );
        return;
    };
    eprintln!("ccteam binary: {}", bin.display());

    // Point the rmux SDK's daemon spawn at our binary. RmuxBackend::new
    // does this defensively too, but we set it here so the test is
    // independent of the constructor's "only if unset" guard.
    // SAFETY: this is a test process; no other threads are racing on
    // this env var.
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &bin);

    // Route through a per-test tempdir UDS so concurrent test runs
    // don't fight over `~/.ccteam/run/mux.sock`.
    let tmpdir = tempfile::tempdir().expect("create tempdir for socket");
    let socket_path = tmpdir.path().join("mux.sock");
    let backend: Arc<dyn PaneBackend> =
        Arc::new(RmuxBackend::with_socket_path(socket_path.clone()));
    eprintln!("socket: {}", socket_path.display());

    let session_name = random_session_name("roundtrip");
    let spec = MuxSessionSpec::new(
        &session_name,
        vec!["sh".into(), "-c".into(), "echo hello && sleep 30".into()],
        PathBuf::from("/tmp"),
    );

    let id = backend.spawn(spec).await.expect("spawn must succeed");
    assert_eq!(id.0, session_name);

    // Give the daemon a tick to register the new session.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(backend.exists(&id).await.unwrap(), "session must exist");

    // pane_pid populated once the child process is up.
    let pid = backend.pane_pid(&id).await.unwrap();
    eprintln!("pane_pid: {pid:?}");
    assert!(pid.is_some(), "pane_pid must report PID after spawn");

    // is_alive default-method composite.
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

    // pane_dims → Some after spawn.
    let dims = backend.pane_dims(&id).await.unwrap();
    assert!(dims.is_some(), "pane_dims should be Some after spawn");

    // send_text + send_enter — no panic.
    backend.send_text(&id, "echo world").await.unwrap();
    backend.send_enter(&id).await.unwrap();

    // Wait for capture to render.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let captured = backend.capture(&id, 50, false).await.unwrap();
    eprintln!(
        "captured ({} bytes): {}",
        captured.len(),
        String::from_utf8_lossy(&captured).trim()
    );
    let s = String::from_utf8_lossy(&captured);
    assert!(
        s.contains("hello"),
        "capture must contain `hello` (the initial echo); got: {s}"
    );

    backend.kill(&id).await.unwrap();

    // Daemon kill propagation can be async; poll briefly. The rmux
    // daemon also self-terminates when its last session is killed
    // (`PendingShutdownReason::ExitEmpty`), which surfaces as a closed
    // transport on subsequent calls. Treat both `Ok(false)` and
    // transport-closed Err as proof the session is gone.
    let mut gone = false;
    for _ in 0..20 {
        match backend.exists(&id).await {
            Ok(false) => {
                gone = true;
                break;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("closed the transport") || msg.contains("transport error") {
                    gone = true;
                    break;
                }
            }
            Ok(true) => {}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "session must be gone after kill");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn kill_is_idempotent_on_missing_session() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!("SKIP: ccteam binary not found; build with `cargo build --bin ccteam` first.");
        return;
    };
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &bin);

    let tmpdir = tempfile::tempdir().expect("create tempdir for socket");
    let socket_path = tmpdir.path().join("mux.sock");
    let backend: Arc<dyn PaneBackend> = Arc::new(RmuxBackend::with_socket_path(socket_path));
    let id = MuxSessionId::new(random_session_name("absent"));
    backend.kill(&id).await.unwrap();
}

/// Byte-faithfulness smoke (Phase 3): the rmux backend's `subscribe`
/// must surface RAW pane bytes (ANSI escapes intact, no `from_utf8_lossy`
/// mangling, no `\n` re-append) and `capture` must return raw backlog
/// bytes — NOT a rendered grid. `#[ignore]` (needs a real rmux daemon);
/// run with `cargo test -p ccteam-harness --test
/// rmux_backend_session_roundtrip -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn subscribe_and_capture_are_byte_faithful() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!("SKIP: ccteam binary not found; build with `cargo build --bin ccteam` first.");
        return;
    };
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &bin);

    let tmpdir = tempfile::tempdir().expect("create tempdir for socket");
    let socket_path = tmpdir.path().join("mux.sock");
    let backend: Arc<dyn PaneBackend> =
        Arc::new(RmuxBackend::with_socket_path(socket_path.clone()));

    let session_name = random_session_name("byte-faithful");
    // `printf` emits a literal ESC (\033) + SGR color sequence + text.
    // A rendered/line-stream path would strip the raw ESC bytes; the
    // byte-faithful path keeps them.
    let spec = MuxSessionSpec::new(
        &session_name,
        vec![
            "sh".into(),
            "-c".into(),
            // \033[31m = red, \033[0m = reset. Loop so capture's backlog
            // and the live subscribe both have raw ESC bytes to observe.
            "printf '\\033[31mRED\\033[0m\\n'; sleep 30".into(),
        ],
        PathBuf::from("/tmp"),
    );

    let id = backend.spawn(spec).await.expect("spawn must succeed");
    tokio::time::sleep(Duration::from_millis(400)).await;

    // capture → raw backlog bytes must contain the ESC byte (0x1b).
    let captured = backend.capture(&id, 50, true).await.expect("capture ok");
    eprintln!(
        "captured ({} bytes): {:?}",
        captured.len(),
        String::from_utf8_lossy(&captured)
    );
    assert!(
        captured.contains(&0x1b),
        "capture must return RAW ANSI bytes (ESC 0x1b present); got {captured:?}"
    );

    // subscribe → drive the pane to emit fresh bytes, assert a raw
    // OutputChunk carries the ESC byte verbatim.
    let mut stream = backend.subscribe(&id).await.expect("subscribe ok");
    // Emit a fresh colored line on the live tail.
    backend
        .send_text(&id, "printf '\\033[32mGREEN\\033[0m\\n'")
        .await
        .unwrap();
    backend.send_enter(&id).await.unwrap();

    let mut saw_raw_esc = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(MuxEvent::OutputChunk(bytes))) => {
                if bytes.contains(&0x1b) {
                    saw_raw_esc = true;
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {} // per-poll timeout — keep waiting until deadline
        }
    }
    assert!(
        saw_raw_esc,
        "subscribe must yield a byte-verbatim OutputChunk containing ESC 0x1b"
    );

    backend.kill(&id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn register_pattern_w2a_stub_is_ok() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!("SKIP: ccteam binary not found.");
        return;
    };
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &bin);

    let tmpdir = tempfile::tempdir().expect("create tempdir for socket");
    let socket_path = tmpdir.path().join("mux.sock");
    let backend: Arc<dyn PaneBackend> = Arc::new(RmuxBackend::with_socket_path(socket_path));
    let id = MuxSessionId::new("any-name");
    backend
        .register_pattern(&id, "claude.idle".into(), r"\[idle\]".into())
        .await
        .unwrap();
}
