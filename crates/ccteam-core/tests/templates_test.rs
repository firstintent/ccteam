//! Integration test: `write_project_settings` lays down a valid
//! `.claude/settings.json` under a tempdir-rooted project.

use ccteam_core::{
    write_global_helper_templates, write_project_settings, HELPER_TEMPLATES,
};
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

// -------------- M2.4: helper template global writer --------------

#[test]
fn write_global_helper_templates_lays_down_both_helpers() {
    let tmp = TempDir::new().unwrap();
    write_global_helper_templates(tmp.path(), false).unwrap();
    let dir = tmp.path().join("templates");
    for (name, _) in HELPER_TEMPLATES {
        assert!(
            dir.join(name).is_file(),
            "helper template missing: {}",
            dir.join(name).display(),
        );
    }
    assert_eq!(HELPER_TEMPLATES.len(), 2, "M2.4 ships exactly 2 helpers");
}

#[test]
fn write_global_helper_templates_preserves_user_edits_without_force() {
    let tmp = TempDir::new().unwrap();
    write_global_helper_templates(tmp.path(), false).unwrap();
    let path = tmp.path().join("templates/review-with-user-loop.md");
    std::fs::write(&path, "USER EDIT\n").unwrap();
    write_global_helper_templates(tmp.path(), false).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT\n");
    write_global_helper_templates(tmp.path(), true).unwrap();
    assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDIT\n");
}

#[test]
fn helper_template_filenames_use_hyphens_for_at_reference() {
    // The on-disk filename is what `@~/.ccteam/templates/<name>` cites.
    // Underscore filenames would force phase markdown to write the
    // Rust source name; hyphens match the rest of the @-reference
    // convention in phase markdown.
    for (name, _) in HELPER_TEMPLATES {
        assert!(
            !name.contains('_'),
            "helper template filename must not contain underscores: {name}",
        );
        assert!(
            name.ends_with(".md"),
            "helper template must be .md: {name}",
        );
    }
}

#[test]
fn plan_eng_phase_embeds_review_with_user_loop_helper() {
    // M2.4 acceptance: dev's plan-eng phase must reference the helper
    // template via `@~/.ccteam/templates/review-with-user-loop.md` so
    // the helper actually lands in a phase prompt — otherwise we have
    // a dead template nobody calls.
    let body = include_str!("../../../teams/dev/phases/02-plan-eng.md");
    assert!(
        body.contains("@~/.ccteam/templates/review-with-user-loop.md"),
        "plan-eng phase markdown must inline @~/.ccteam/templates/review-with-user-loop.md",
    );
}
