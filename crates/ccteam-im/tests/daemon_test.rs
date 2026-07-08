//! Daemon lifecycle + registry round-trip integration tests.

use std::time::Duration;

use ccteam_harness::AgentVendor;
use ccteam_im::{
    daemon::{run_daemon, DaemonArgs},
    list_bots, register_bot, registration_path, unregister_bot,
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
async fn daemon_runs_until_max_runtime() {
    let _g = env_lock();
    let _tmp = isolate_home();
    let args = DaemonArgs {
        credentials: None,
        registry: None,
        max_runtime: Some(Duration::from_millis(150)),
        adapter_factory: None,
        channels_override: None,
        extra_channels: None,
        ..Default::default()
    };
    run_daemon(args).await.unwrap();
}

/// v0.8.5 P1 — at startup the daemon registers the gateway's command menu
/// (`menu_command_specs`) on **every** channel via
/// `Channel::register_commands`. Inject two mock channels, run briefly,
/// and assert each one recorded exactly the in-menu specs. (A real
/// menu-less channel is a no-op; the mock records the call purely so this
/// wiring is observable — Telegram's `setMyCommands` body shape is tested
/// in the telegram provider unit tests.)
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn daemon_registers_command_menu_per_channel() {
    use ccteam_im::gateway::menu_command_specs;
    use ccteam_im::transport::providers::mock::MockChannel;
    use ccteam_im::transport::Channel;
    use std::collections::HashMap;
    use std::sync::Arc;

    let _g = env_lock();
    let _tmp = isolate_home();

    let ch_a = MockChannel::new().with_name("mock-a");
    let ch_b = MockChannel::new().with_name("mock-b");
    let mut override_map: HashMap<String, Arc<dyn Channel + Send + Sync>> = HashMap::new();
    override_map.insert("mock-a".into(), Arc::new(ch_a.clone()));
    override_map.insert("mock-b".into(), Arc::new(ch_b.clone()));

    let args = DaemonArgs {
        credentials: None,
        registry: None,
        max_runtime: Some(Duration::from_millis(150)),
        adapter_factory: None,
        channels_override: Some(override_map),
        extra_channels: None,
        ..Default::default()
    };
    run_daemon(args).await.unwrap();

    let expected = menu_command_specs();
    assert!(!expected.is_empty(), "there are in-menu gateway commands");
    // Only commands flagged `in_menu` in `GATEWAY_COMMANDS` appear —
    // passthrough vendor slashes must never be advertised.
    assert!(expected.iter().any(|c| c.name == "/sessions"));
    assert!(expected.iter().any(|c| c.name == "/help"));
    assert!(
        !expected.iter().any(|c| c.name == "/compact"),
        "passthrough vendor slashes must stay out of the menu"
    );
    // v0.8.23 review §3.2-4 — the multi-session navigation verbs used to be
    // menu-invisible (in_menu:false), hiding the workflow's core verbs behind
    // /help. They're in_menu now, with the arg hint woven into the
    // description so a menu tap still teaches the argument.
    for (name, hint) in [
        ("/use", "<id|@role>"),
        ("/cd", "<project>"),
        ("/role", "<role>"),
        ("/stop", "<id>"),
        ("/interrupt", "[id]"),
        ("/newproject", "<slug> <path>"),
    ] {
        let spec = expected
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} must be in the menu: {expected:?}"));
        assert!(
            spec.description.starts_with(hint),
            "{name} menu description must lead with its arg hint: {:?}",
            spec.description
        );
    }

    for ch in [&ch_a, &ch_b] {
        let calls = ch.registered_commands().await;
        assert_eq!(
            calls.len(),
            1,
            "register_commands called exactly once per channel at startup"
        );
        assert_eq!(calls[0], expected, "registered the in-menu gateway specs");
    }
}

/// Serialize env-mutating tests so concurrent `HOME` swaps in this
/// binary don't race with each other (cargo runs integration tests
/// within one file in parallel by default).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn registration_round_trip() {
    let _g = env_lock();
    let _tmp = isolate_home();
    let path = register_bot("dev-foo", "lead", AgentVendor::Claude, "telegram", "12345").unwrap();
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
