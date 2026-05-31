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
use std::path::{Path, PathBuf};

use rmux_server::{DaemonConfig, ServerDaemon};

/// ccteam-owned rmux daemon config. The ONLY directive that matters:
/// `set -g exit-empty off`.
///
/// **Production-killer this defends against (W-verify G-D)**: rmux's
/// `exit-empty` server option defaults to `on`
/// (`rmux-server/.../options/table.rs`), so a stock daemon
/// self-terminates the moment its last session is killed. ccteam hosts
/// the daemon as a sibling process for 24/7 mode-3 chat — if it died
/// whenever the final bot stopped, `RmuxBackend`'s cached handle would
/// then point at a dead daemon. Disabling exit-empty keeps the daemon
/// resident across "all bots stopped" windows.
const CCTEAM_RMUX_CONF: &str = "# ccteam-managed rmux daemon config — do not edit.\n\
                                # Keeps the daemon resident when its last session is killed\n\
                                # (mode-3 chat is 24/7; see daemon.rs CCTEAM_RMUX_CONF).\n\
                                set -g exit-empty off\n";

/// Write [`CCTEAM_RMUX_CONF`] next to the daemon socket and return its
/// path, for passing to `DaemonConfig::with_config_files`. Best-effort:
/// returns `None` if the parent dir is unavailable or the write fails,
/// in which case the daemon falls back to stock defaults (exit-empty on)
/// — degraded but not broken.
fn write_ccteam_rmux_conf(socket_path: &Path) -> Option<PathBuf> {
    let dir = socket_path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let conf = dir.join("ccteam-rmux.conf");
    std::fs::write(&conf, CCTEAM_RMUX_CONF).ok()?;
    Some(conf)
}

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

    // Disable rmux's exit-empty self-termination (W-verify G-D). Without
    // this the daemon dies when its last session is killed, stranding the
    // 24/7 mode-3 chat use case. Best-effort: a write failure degrades to
    // stock defaults rather than aborting daemon startup.
    let config = match write_ccteam_rmux_conf(&socket_path) {
        Some(conf) => DaemonConfig::new(socket_path).with_config_files(vec![conf], true, None),
        None => DaemonConfig::new(socket_path),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(HIDDEN_DAEMON_WORKER_THREADS)
        .build()?;

    runtime.block_on(async move {
        let server = ServerDaemon::new(config).bind().await?;
        server.wait().await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_writer_disables_exit_empty() {
        let dir =
            std::env::temp_dir().join(format!("ccteam-harness-conf-test-{}", std::process::id()));
        let socket = dir.join("mux.sock");
        let conf = write_ccteam_rmux_conf(&socket).expect("write conf");
        let body = std::fs::read_to_string(&conf).expect("read conf");
        assert!(
            body.contains("set -g exit-empty off"),
            "ccteam rmux conf must disable exit-empty, got:\n{body}"
        );
        assert_eq!(conf.file_name().unwrap(), "ccteam-rmux.conf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conf_writer_returns_none_for_rootless_path() {
        // "/" has no parent component → None, so run_internal_daemon
        // degrades to stock defaults rather than aborting startup.
        assert!(write_ccteam_rmux_conf(Path::new("/")).is_none());
    }
}
