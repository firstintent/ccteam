//! V0.8 W2a — `ccteam mux daemon` re-exec runtime.
//!
//! The rmux SDK's `connect_or_start` protocol spawns the daemon binary
//! as `<daemon_binary> --__internal-daemon <socket>`. By pointing
//! `RMUX_SDK_DAEMON_BINARY` at our own executable AND intercepting that
//! argv form before clap parses (see `ccteam-cli::main`), we host the
//! rmux daemon inside the ccteam binary — no separate `rmux` artifact
//! to ship.
//!
//! See `docs/versions/v0-8-rmux/w2-daemon-spawn-protocol.md` and
//! `references/rmux/crates/rmux-sdk/src/handles/rmux/connect.rs` lines
//! 150-180 for the upstream invariant this implementation tracks.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use rmux_server::{DaemonConfig, ServerDaemon};

/// Fixed `--__internal-daemon` flag emitted by `rmux_sdk` when it
/// spawns the hidden daemon. Re-exported here so the main.rs argv
/// dispatch can avoid a cross-crate string duplication (the value is a
/// public protocol with rmux-sdk's `INTERNAL_DAEMON_FLAG` const).
pub const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";

/// Re-export of rmux-sdk's daemon-binary-override env var name
/// (`"RMUX_SDK_DAEMON_BINARY"`). Re-exported so ccteam-cli's `main()`
/// can set it at process entry — before any child fork — without
/// taking a direct rmux-sdk dependency. See
/// `docs/versions/v0-8-rmux/w2-daemon-spawn-protocol.md`.
pub use rmux_sdk::bootstrap::discovery::SDK_DAEMON_BINARY_ENV;

/// Hidden-daemon worker-thread count. Mirrors rmux's upstream
/// `hidden_daemon_worker_threads` heuristic (fixed at 4 here for
/// simplicity — workspace doesn't depend on `num_cpus`).
const HIDDEN_DAEMON_WORKER_THREADS: usize = 4;

/// Run the ccteam-hosted rmux daemon at `socket`. Blocks until the
/// daemon exits (signal, kill-server request, etc.).
///
/// Called from `ccteam-cli::main` when argv matches
/// `["ccteam", "--__internal-daemon", "<socket>"]` — this happens when
/// `RmuxBackend::new()` sets `RMUX_SDK_DAEMON_BINARY=<current_exe>` and
/// then `rmux_sdk::Rmux::builder().connect_or_start()` spawns the
/// daemon child.
pub fn run_internal_daemon(socket: OsString) -> io::Result<()> {
    let socket_path = PathBuf::from(socket);
    let config = DaemonConfig::new(socket_path);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(HIDDEN_DAEMON_WORKER_THREADS)
        .build()?;

    runtime.block_on(async move {
        let server = ServerDaemon::new(config).bind().await?;
        server.wait().await
    })
}
