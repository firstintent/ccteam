//! V0.8 audit G-D part 2 — `RmuxBackend` recovers after the rmux daemon
//! dies.
//!
//! `#[ignore]` because it spawns a real rmux daemon by re-execing the
//! ccteam binary via the `--__internal-daemon` argv form, then kills that
//! daemon mid-flight to prove the backend reconnects. Run with:
//!
//! ```sh
//! cargo build --bin ccteam
//! cargo test -p ccteam-mux --test rmux_backend_reconnect -- \
//!     --ignored --nocapture
//! ```
//!
//! The non-ignored coverage of the reconnect *logic* (cache
//! invalidation + `ptr_eq` convergence + dead-transport classification)
//! lives in the `rmux_backend` lib unit tests, which need no daemon.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ccteam_mux::{MuxBackend, MuxSessionSpec, RmuxBackend};

fn random_session_name(base: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ccteam-mux-reconnect-{base}-{nanos}")
}

fn locate_ccteam_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CCTEAM_TEST_BIN") {
        return Some(PathBuf::from(path));
    }
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

/// Kill whatever daemon is listening on `socket` by routing a
/// `kill-server` through a throwaway SDK handle. This simulates the
/// daemon crash / reboot case: after this returns the cached handle held
/// by the backend points at a dead transport.
async fn kill_daemon_at(socket: &std::path::Path) {
    use rmux_sdk::{Rmux, RmuxEndpoint};
    let rmux = Rmux::builder()
        .endpoint(RmuxEndpoint::UnixSocket(socket.to_path_buf()))
        .default_timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("connect to running daemon for kill");
    // `shutdown` negotiates kill-server then waits for the transport to
    // close; a clean close is folded into Ok by the SDK.
    rmux.shutdown().await.expect("daemon shutdown");
}

/// Spawn a session, kill the daemon out from under the backend, then
/// drive another operation. With the old non-reconnectable `OnceCell`
/// this op would fail forever; with the reconnect-capable cache the
/// backend transparently re-spawns the daemon and recovers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn backend_recovers_after_daemon_death() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!(
            "SKIP: ccteam binary not found in target/{{debug,release}}/ccteam; build with \
             `cargo build --bin ccteam` (or set CCTEAM_TEST_BIN=...) and rerun."
        );
        return;
    };
    // SAFETY: test process, no other threads racing this env var.
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &bin);

    let tmpdir = tempfile::tempdir().expect("create tempdir for socket");
    let socket_path = tmpdir.path().join("mux.sock");
    let backend: Arc<dyn MuxBackend> = Arc::new(RmuxBackend::with_socket_path(socket_path.clone()));

    // First op spawns the daemon + caches the handle.
    let session_name = random_session_name("survivor");
    let spec = MuxSessionSpec::new(
        &session_name,
        vec!["sh".into(), "-c".into(), "sleep 60".into()],
        PathBuf::from("/tmp"),
    );
    let id = backend.spawn(spec).await.expect("initial spawn");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        backend.exists(&id).await.unwrap(),
        "session exists before daemon death"
    );

    // Kill the daemon. The backend's cached handle is now dead.
    kill_daemon_at(&socket_path).await;
    // Give the OS a beat to tear the socket down.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // This op must transparently reconnect (re-spawn a fresh daemon via
    // connect_or_start) rather than fail permanently. The freshly
    // spawned daemon has no sessions, so `exists` is `false` — the point
    // is that the call *succeeds* instead of erroring on a dead transport.
    let mut recovered = false;
    for _ in 0..20 {
        match backend.exists(&id).await {
            Ok(_) => {
                recovered = true;
                break;
            }
            Err(e) => {
                eprintln!("exists still failing post-death: {e}");
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
    }
    assert!(
        recovered,
        "backend must recover (reconnect) after the daemon dies"
    );

    // And the recovered backend is fully usable: spawn a new session on
    // the fresh daemon.
    let name2 = random_session_name("after");
    let spec2 = MuxSessionSpec::new(
        &name2,
        vec!["sh".into(), "-c".into(), "sleep 60".into()],
        PathBuf::from("/tmp"),
    );
    let id2 = backend
        .spawn(spec2)
        .await
        .expect("spawn on reconnected daemon must succeed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        backend.exists(&id2).await.unwrap(),
        "new session exists on the reconnected daemon"
    );
    backend.kill(&id2).await.ok();
}
