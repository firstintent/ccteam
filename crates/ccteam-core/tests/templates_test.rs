//! Integration test: `write_project_settings` lays down a valid
//! `.claude/settings.json` under a tempdir-rooted project.

use ccteam_core::write_project_settings;
use tempfile::TempDir;

#[test]
fn write_project_settings_creates_dot_claude_dir_and_valid_json() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("acme");

    write_project_settings(&project).unwrap();

    let path = project.join(".claude").join("settings.json");
    assert!(path.exists(), "settings.json should exist after rendering");

    let bytes = std::fs::read(&path).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["hooks"]["SessionStart"].is_array());
}

#[test]
fn write_project_settings_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("acme");

    write_project_settings(&project).unwrap();
    let first = std::fs::read(project.join(".claude/settings.json")).unwrap();

    write_project_settings(&project).unwrap();
    let second = std::fs::read(project.join(".claude/settings.json")).unwrap();

    assert_eq!(first, second, "rerun must produce identical bytes");
}
