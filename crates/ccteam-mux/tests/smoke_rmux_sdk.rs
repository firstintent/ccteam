//! V0.8 W0 spike — end-to-end smoke test against the real rmux daemon.
//!
//! Two tests live here:
//!
//! - `rmux_types_compile_link` (NOT `#[ignore]`) instantiates the
//!   public rmux-sdk types ccteam will rely on. Pure compile-link;
//!   does not touch the daemon. This is the semver-drift canary —
//!   every `cargo test` run flags rmux API changes the day they
//!   land in `Cargo.lock`.
//!
//! - `smoke_rmux_sdk_echo` (`#[ignore]`) spawns a real `rmux` daemon,
//!   opens a detached session that runs `echo hello`, waits for the
//!   pane snapshot to contain `hello`, then shuts the daemon down.
//!   Requires the `rmux` binary on `PATH`. Run via
//!   `cargo test -p ccteam-mux smoke_rmux_sdk -- --ignored --nocapture`.
//!
//! The smoke test does **not** ride on `connect_or_start_with_launcher`
//! — that API does not exist in `rmux-sdk` 0.3.1. The published SDK
//! auto-spawns the hidden daemon by invoking `rmux_os::daemon::daemon_binary()`
//! against the `rmux` binary on `PATH`. ccteam will need a launcher
//! escape hatch when `ccteam mux daemon` lands in W2 (either upstream
//! a `with_launcher` builder method or bundle/install our own `rmux`
//! binary path); recorded for W2 doc-first.

use std::time::Duration;

use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, ProcessSpec, Rmux, SessionName, TerminalSizeSpec,
};

/// Pure compile-link semver-drift canary. Runs on every `cargo test`.
///
/// If any of these types vanish or change shape across an rmux minor
/// bump, this test stops compiling — which is exactly the signal we
/// want before the bumped lockfile lands in `main`.
#[test]
fn rmux_types_compile_link() {
    // `SessionName` constructor — used by every `ensure_session` call.
    let name = SessionName::new("ccteam-spike-canary").expect("session-name validates");
    assert_eq!(name.as_str(), "ccteam-spike-canary");

    // `TerminalSizeSpec::new` — pty cols/rows; ccteam wraps tmux's 200x50.
    let size = TerminalSizeSpec::new(200, 50);
    let _ = size;

    // `EnsureSessionPolicy` enum — the CreateOrReuse variant is the
    // shape ccteam will rely on in mode 3 chat (reuse if present, create
    // fresh otherwise).
    let policy = EnsureSessionPolicy::CreateOrReuse;
    let _ = policy;

    // `ProcessSpec::shell` and `ProcessSpec::argv` — the two argv-shaping
    // forms ccteam will use depending on whether the vendor binary needs
    // a `$SHELL -c` wrapper.
    let _shell = ProcessSpec::shell("echo hello");
    let _argv: ProcessSpec = ProcessSpec::argv(["echo", "hello"]);

    // `EnsureSession::named(...).policy(...).detached(...).size(...).process(...)`
    // is the canonical builder shape from the rmux-sdk crate docs.
    let _spec = EnsureSession::named(name)
        .policy(EnsureSessionPolicy::CreateOrReuse)
        .detached(true)
        .size(TerminalSizeSpec::new(200, 50))
        .process(ProcessSpec::shell("echo hello"));
}

/// End-to-end smoke against a real rmux daemon. `#[ignore]` because it
/// needs the `rmux` system binary on `PATH`.
///
/// Behaviour:
/// 1. probe `rmux --version`; on failure print + return Ok (skip)
/// 2. `Rmux::builder().connect_or_start()` — auto-spawns hidden daemon
/// 3. `EnsureSession::named(...).create_or_reuse().detached(true)
///    .process(ProcessSpec::shell("echo hello && sleep 1"))`
/// 4. `session.pane(0, 0).wait_for_text("hello")` (bounded by SDK
///    default-timeout set on the builder)
/// 5. `session.pane(0, 0).snapshot().visible_text()` must contain `hello`
/// 6. `rmux.shutdown()` — clean teardown of the daemon we just spawned
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn smoke_rmux_sdk_echo() {
    // Step 1 — probe for `rmux` binary. The rmux-sdk hidden-daemon
    // launcher uses `rmux_os::daemon::daemon_binary()` which falls back
    // to `rmux` on PATH; without it `connect_or_start` hangs for the
    // full startup timeout. Short-circuit early with a clear skip.
    let probe = std::process::Command::new("rmux").arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => {
            eprintln!(
                "rmux probe: {}",
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
        Ok(out) => {
            eprintln!(
                "SKIP: `rmux --version` exited {} — stderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return;
        }
        Err(err) => {
            eprintln!("SKIP: `rmux` binary not on PATH ({err}); install rmux to run this test");
            return;
        }
    }

    // Step 2 — connect or start. Default timeout bounds every
    // subsequent SDK call (including `wait_for_text`).
    let rmux = Rmux::builder()
        .default_timeout(Duration::from_secs(5))
        .connect_or_start()
        .await
        .expect("connect_or_start succeeds when rmux is on PATH");

    // Step 3 — ensure detached session running `echo hello && sleep 1`.
    // `sleep 1` keeps the pane alive long enough for snapshot capture
    // — without it the pane can exit between `wait_for_text` and
    // `snapshot()` and lose the rendered output on some terminals.
    let session_name = SessionName::new("ccteam-w0-smoke").expect("smoke session name validates");
    let session = rmux
        .ensure_session(
            EnsureSession::named(session_name.clone())
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true)
                .size(TerminalSizeSpec::new(80, 24))
                .process(ProcessSpec::shell("echo hello && sleep 1")),
        )
        .await
        .expect("ensure_session succeeds against fresh daemon");

    // Step 4 — wait for "hello" to render. Bounded by the SDK's
    // default_timeout (5s above); blows past that → test fails.
    let pane = session.pane(0, 0);
    pane.wait_for_text("hello")
        .await
        .expect("pane renders `hello` within default timeout");

    // Step 5 — snapshot the pane and confirm `hello` is in visible text.
    let snapshot = pane.snapshot().await.expect("snapshot succeeds");
    let visible = snapshot.visible_text();
    eprintln!("pane visible_text: {visible:?}");
    assert!(
        visible.contains("hello"),
        "pane snapshot must contain `hello`; got {visible:?}"
    );

    // Step 6 — clean shutdown of the daemon (consumes `rmux`).
    rmux.shutdown()
        .await
        .expect("daemon accepts SDK-initiated shutdown");
}
