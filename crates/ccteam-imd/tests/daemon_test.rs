//! Daemon lifecycle + registry round-trip integration tests.

use std::time::Duration;

use ccteam_core::harness::AgentVendor;
use ccteam_imd::{
    daemon::{run_daemon, DaemonArgs},
    imd_heartbeat_path, list_bots, register_bot, registration_path, unregister_bot,
};
use tempfile::TempDir;

/// Make tests hermetic by pointing HOME at a tempdir before any
/// `dirs::home_dir()` call.
fn isolate_home() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("HOME", tmp.path());
    tmp
}

// Holding a std::sync::Mutex guard across `.await` is fine on the
// single-thread `current_thread` runtime we use here (the task cannot
// migrate); the lint is a generic safety net for multi-thread
// runtimes that don't apply.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn daemon_runs_and_writes_heartbeat() {
    let _g = env_lock();
    let _tmp = isolate_home();
    let args = DaemonArgs {
        credentials: None,
        registry: None,
        tick: Duration::from_millis(40),
        max_runtime: Some(Duration::from_millis(150)),
        adapter_factory: None,
    };
    run_daemon(args).await.unwrap();
    let hb = imd_heartbeat_path();
    assert!(hb.exists(), "heartbeat should exist at {}", hb.display());
}

/// Serialize env-mutating tests so concurrent `HOME` swaps in this
/// binary don't race with each other (cargo runs integration tests
/// within one file in parallel by default).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn registration_round_trip() {
    let _g = env_lock();
    let _tmp = isolate_home();
    let path = register_bot(
        "dev-foo",
        "lead",
        AgentVendor::Claude,
        "telegram",
        "12345",
    )
    .unwrap();
    // Recompute path under the *same* HOME (no race because we hold env_lock).
    let expected = registration_path("dev-foo", "lead");
    assert_eq!(path, expected);
    let listed = list_bots().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].workflow_slug, "dev-foo");
    assert_eq!(listed[0].role, "lead");
    assert_eq!(listed[0].im_platform, "telegram");

    unregister_bot("dev-foo", "lead").unwrap();
    assert!(list_bots().unwrap().is_empty());
}

#[test]
fn unregister_missing_is_ok() {
    let _g = env_lock();
    let _tmp = isolate_home();
    // Should not error even though there's nothing to remove.
    unregister_bot("dev-foo", "lead").unwrap();
}

#[test]
fn register_overwrites_existing() {
    let _g = env_lock();
    let _tmp = isolate_home();
    register_bot("dev-foo", "lead", AgentVendor::Claude, "telegram", "1").unwrap();
    register_bot("dev-foo", "lead", AgentVendor::Codex, "slack", "C2").unwrap();
    let listed = list_bots().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].im_platform, "slack");
    assert!(matches!(listed[0].vendor, AgentVendor::Codex));
}
