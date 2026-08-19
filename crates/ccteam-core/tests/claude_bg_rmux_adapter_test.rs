//! V0.8 rmux Step 3 — positive adapter-layer coverage through rmux.
//!
//! The tmux-fixture adapter tests prove the adapters build correct
//! session specs; `rmux_backend_session_roundtrip` proves the rmux trait
//! executes a spec end-to-end; the `default_backend_defaults_to_rmux`
//! unit test proves `default_backend()` resolves to rmux when unset. This
//! test closes the remaining seam: it drives the REAL production spawn
//! path (`ClaudeBgAdapter` → `default_backend()` → rmux daemon → live
//! session) so the composition is covered, not just each link.
//!
//! `#[ignore]` because it spawns a real rmux daemon by re-execing the
//! ccteam binary (`--__internal-daemon`). Run with:
//!
//! ```sh
//! cargo build --bin ccteam
//! cargo test -p ccteam-core --test claude_bg_rmux_adapter_test -- \
//!     --ignored --nocapture
//! ```

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ClaudeBgAdapter, ExecutionMode, HarnessAdapter, SpawnCtx,
    CLAUDE_BIN_ENV,
};

/// Drop guard that restores a `$ENV` var to its pre-test value.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Locate the ccteam binary in the workspace target dir; the rmux SDK
/// re-execs it with `--__internal-daemon <socket>` to host the daemon.
fn locate_ccteam_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CCTEAM_TEST_BIN") {
        return Some(PathBuf::from(path));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent()?.parent()?;
    for rel in ["target/debug/ccteam", "target/release/ccteam"] {
        let candidate = workspace_root.join(rel);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
#[ignore]
async fn via_mux_bg_spawn_routes_through_rmux_daemon() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!(
            "SKIP: ccteam binary not found in target/{{debug,release}}/ccteam; build with \
             `cargo build --bin ccteam` (or set CCTEAM_TEST_BIN=...) and rerun."
        );
        return;
    };

    let tmp = tempfile::tempdir().unwrap();

    // Isolate the rmux socket + ccteam paths under a per-test HOME so the
    // adapter's `default_backend()` (which resolves `RmuxBackend::new()`
    // → `default_ccteam_harness_socket_path()` from HOME) lands on a private
    // socket instead of the shared `~/.ccteam/run/mux.sock`.
    let _home = EnvGuard::set("HOME", tmp.path().to_str().unwrap());
    // Re-exec target for the daemon. RmuxBackend::new defaults this to
    // current_exe(), which in a test would be the TEST binary (no
    // --__internal-daemon handler) — pin it to the real ccteam binary.
    let _daemon_bin = EnvGuard::set("RMUX_SDK_DAEMON_BINARY", bin.to_str().unwrap());
    let _backend_env = EnvGuard::set("CCTEAM_MUX_BACKEND", "rmux");
    let _via_mux = EnvGuard::set("CCTEAM_CLAUDE_BG_VIA_MUX", "1");

    // Fake `claude` that sleeps so the rmux pane child stays alive for the
    // exists() assert before close_thread reaps it.
    let fake_claude = tmp.path().join("fake-claude");
    std::fs::write(&fake_claude, "#!/bin/sh\nsleep 30\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _bin_env = EnvGuard::set(CLAUDE_BIN_ENV, fake_claude.to_str().unwrap());

    let brief = AgentSpecBrief {
        role: "tester".into(),
    };
    let ctx = SpawnCtx {
        mode: None,
        slug: "rmuxbg".into(),
        sid: "claude-rmux-1".into(),
        owner: "user:web-api".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec!["do the thing".into()],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    };

    let adapter = ClaudeBgAdapter::new();
    let handle = adapter
        .start_thread(&brief, &ctx)
        .await
        .expect("via-mux start_thread must spawn an rmux-backed session");

    assert_eq!(handle.vendor, AgentVendor::Claude);
    assert_eq!(handle.mode, ExecutionMode::Bg);
    assert_eq!(handle.identity, "ccteam-bg-rmuxbg-claude-rmux-1");
    assert_eq!(
        handle.raw_extras.get("via_mux").and_then(|v| v.as_bool()),
        Some(true),
        "handle must be flagged via_mux"
    );

    // The production backend selector resolves to rmux on the same socket;
    // the session the adapter spawned must be live in the rmux daemon.
    let backend = ccteam_harness::default_backend();
    assert_eq!(
        backend.backend_kind(),
        ccteam_harness::BackendKind::Rmux,
        "default_backend() must be rmux under CCTEAM_MUX_BACKEND=rmux"
    );
    let id = ccteam_harness::MuxSessionId::new(handle.identity.clone());

    let mut alive = false;
    for _ in 0..30 {
        if backend.exists(&id).await.unwrap_or(false) {
            alive = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        alive,
        "rmux session must exist after via-mux spawn through default_backend()"
    );

    // exists() can report Ok(true) the moment the daemon registers the
    // session — confirm the pane child actually execed before we reap it,
    // so close_thread races a live child, not a still-launching one.
    let mut pid = None;
    for _ in 0..30 {
        pid = backend.pane_pid(&id).await.unwrap_or(None);
        if pid.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        pid.is_some(),
        "rmux pane child PID must be up before teardown"
    );

    // close_thread routes teardown through ProcessBackend::kill on the rmux
    // daemon. RmuxBackend pins `exit-empty off`, so the daemon stays up
    // and exists() returns a clean Ok(false); tolerate a transport-closed
    // Err too in case the daemon races a shutdown.
    adapter
        .close_thread(&handle)
        .await
        .expect("close_thread must reap the rmux session");

    let mut gone = false;
    for _ in 0..30 {
        match backend.exists(&id).await {
            Ok(false) => {
                gone = true;
                break;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("transport") || msg.contains("closed") {
                    gone = true;
                    break;
                }
            }
            Ok(true) => {}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "rmux session must be gone after close_thread");
}
