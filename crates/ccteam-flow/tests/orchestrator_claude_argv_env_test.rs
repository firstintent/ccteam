//! F29 — `OrchestratorConfig::default()` reads `CCTEAM_CLAUDE_ARGV`
//! when set so e2e harnesses can inject a stub claude without
//! rebuilding the binary.
//!
//! Env-mutating tests live in their own integration binary per
//! CLAUDE.md §六 ("env-mutating tests放 `crates/*/tests/*.rs`
//! integration (各独立进程)") so they don't race against unit-test
//! readers of the same env var inside the lib binary.

use ccteam_flow::OrchestratorConfig;

/// Each test mutates the same env var — serialize them within this
/// binary using a static mutex so the precedence checks don't race.
fn env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[test]
fn default_reads_ccteam_claude_argv_when_set() {
    let _g = env_lock().lock().unwrap();
    std::env::set_var("CCTEAM_CLAUDE_ARGV", "sh -c echo-stub");
    let cfg = OrchestratorConfig::default();
    assert_eq!(
        cfg.claude_argv,
        vec!["sh".to_string(), "-c".to_string(), "echo-stub".to_string()],
    );
    std::env::remove_var("CCTEAM_CLAUDE_ARGV");
}

#[test]
fn default_falls_back_to_claude_when_env_missing() {
    let _g = env_lock().lock().unwrap();
    std::env::remove_var("CCTEAM_CLAUDE_ARGV");
    let cfg = OrchestratorConfig::default();
    assert_eq!(cfg.claude_argv.first().map(String::as_str), Some("claude"));
    assert!(
        cfg.claude_argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"),
        "default argv should carry --dangerously-skip-permissions; got {:?}",
        cfg.claude_argv,
    );
}

#[test]
fn default_treats_empty_env_as_unset() {
    let _g = env_lock().lock().unwrap();
    std::env::set_var("CCTEAM_CLAUDE_ARGV", "   ");
    let cfg = OrchestratorConfig::default();
    // All-whitespace env collapses to no parts → fallback to default.
    assert_eq!(cfg.claude_argv.first().map(String::as_str), Some("claude"));
    std::env::remove_var("CCTEAM_CLAUDE_ARGV");
}
