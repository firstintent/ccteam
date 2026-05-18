//! V0.6.0 Wave 3 F112 §C — `~/.ccteam/preferences.toml` integration
//! tests. Mirrors the in-crate unit tests on `preferences.rs` but
//! exercises the public API through the integration boundary so any
//! visibility regression (pub vs pub(crate)) surfaces in CI.

use std::fs;

use ccteam_core::preferences::{
    self, load, load_or_default, preferences_path, save, OnClaudeQuota, Preferences,
};
use tempfile::TempDir;

#[test]
fn load_default_when_file_absent() {
    let tmp = TempDir::new().unwrap();
    let prefs = load(tmp.path()).expect("load missing file should not error");
    assert_eq!(prefs, Preferences::default());
    assert_eq!(prefs.fallback.on_claude_quota, OnClaudeQuota::Off);
}

#[test]
fn save_round_trip_preserves_values() {
    let tmp = TempDir::new().unwrap();
    let mut prefs = Preferences::default();
    prefs.fallback.on_claude_quota = OnClaudeQuota::Codex;
    prefs.fallback.codex.enabled_for_roles = vec!["critic".into(), "reviewer".into()];

    save(tmp.path(), &prefs).expect("save round-trip");
    let back = load(tmp.path()).expect("re-load round-trip");
    assert_eq!(back, prefs);

    // File on disk has the expected TOML shape.
    let raw = fs::read_to_string(preferences_path(tmp.path())).unwrap();
    assert!(
        raw.contains("on_claude_quota = \"codex\""),
        "TOML body missing on_claude_quota: {raw}"
    );
    assert!(
        raw.contains("enabled_for_roles"),
        "TOML body missing enabled_for_roles: {raw}"
    );
}

#[test]
fn missing_file_graceful_via_load_or_default() {
    let tmp = TempDir::new().unwrap();
    let prefs = load_or_default(tmp.path());
    assert_eq!(prefs, Preferences::default());
}

#[test]
fn invalid_toml_graceful_via_load_or_default() {
    let tmp = TempDir::new().unwrap();
    let path = preferences_path(tmp.path());
    fs::write(&path, b"this is = = not = valid").unwrap();
    // Hard load surfaces the error.
    assert!(load(tmp.path()).is_err());
    // Production entry returns defaults instead of failing.
    let prefs = load_or_default(tmp.path());
    assert_eq!(prefs, Preferences::default());
}

#[test]
fn role_eligibility_gates_codex_fallback() {
    let mut prefs = Preferences::default();
    prefs.fallback.on_claude_quota = OnClaudeQuota::Codex;

    // Empty list = all roles eligible.
    assert!(prefs.codex_fallback_enabled_for("main"));
    assert!(prefs.codex_fallback_enabled_for("fixer"));

    // Restricted list = only listed roles.
    prefs.fallback.codex.enabled_for_roles = vec!["critic".into()];
    assert!(prefs.codex_fallback_enabled_for("critic"));
    assert!(!prefs.codex_fallback_enabled_for("main"));

    // off + any role list → never enabled.
    prefs.fallback.on_claude_quota = OnClaudeQuota::Off;
    assert!(!prefs.codex_fallback_enabled_for("critic"));
}

#[test]
fn preferences_path_lives_at_root_preferences_toml() {
    let tmp = TempDir::new().unwrap();
    let path = preferences::preferences_path(tmp.path());
    assert_eq!(path, tmp.path().join("preferences.toml"));
}
