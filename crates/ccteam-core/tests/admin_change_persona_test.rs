//! V0.6.1 F128 — integration test for `admin_actions::change_persona`.
//!
//! Covers the happy path (full body replace), the error path
//! (missing persona file), and the round-trip with the
//! `persona_changed` event builder so callers can rely on the JSON
//! shape that lands in `progress.jsonl`.

use std::fs;
use std::path::Path;

use ccteam_core::admin_actions;
use tempfile::TempDir;

fn seed_project(root: &Path, slug: &str, bot: &str, body: &str) -> std::path::PathBuf {
    let project_dir = root.join("projects").join(slug);
    let agents_dir = project_dir.join(".claude").join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    let bot_md = agents_dir.join(format!("{bot}.md"));
    fs::write(&bot_md, body).unwrap();
    project_dir
}

#[test]
fn change_persona_replaces_full_body_atomically() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\ndescription: original\ntools: Read\n---\n\noriginal body\n",
    );
    let new_md = "---\nname: alice\ndescription: revised\ntools: Read, WebFetch\nmodel: sonnet\n---\n\nyou are alice, English + humorous\n";
    let written = admin_actions::change_persona(&project_dir, "alice", new_md).unwrap();
    assert_eq!(written, project_dir.join(".claude/agents/alice.md"));
    let on_disk = fs::read_to_string(&written).unwrap();
    assert_eq!(on_disk, new_md);
    assert!(on_disk.contains("English + humorous"));
}

#[test]
fn change_persona_refuses_missing_bot() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("projects").join("dev-empty");
    fs::create_dir_all(project_dir.join(".claude/agents")).unwrap();
    let err = admin_actions::change_persona(&project_dir, "ghost", "---\nname: x\n---\n")
        .expect_err("must fail when persona file missing");
    let msg = err.to_string();
    assert!(msg.contains("no persona file"), "got: {msg}");
}

#[test]
fn change_persona_rejects_invalid_bot_name() {
    let tmp = TempDir::new().unwrap();
    let err = admin_actions::change_persona(tmp.path(), "Alice", "---\nname: alice\n---\n")
        .expect_err("uppercase bot name should fail");
    assert!(err.to_string().contains("not allowed"));
}

#[test]
fn change_persona_event_shape_round_trips() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\n---\nbody\n",
    );
    let body = "---\nname: alice\ndescription: tweak\n---\nrev\n";
    let written = admin_actions::change_persona(&project_dir, "alice", body).unwrap();
    let ev = admin_actions::build_persona_changed_event("alice", &written, body.len());
    assert_eq!(ev["event"], "persona_changed");
    assert_eq!(ev["role"], "alice");
    assert_eq!(ev["bytes_written"], body.len() as i64);
    let path = ev["path"].as_str().expect("path must be string");
    assert!(path.ends_with("alice.md"), "path field: {path}");
    assert!(ev["ts"].as_str().unwrap().contains('T'));
}

#[test]
fn change_persona_rejects_empty_body() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\n---\nbody\n",
    );
    let err = admin_actions::change_persona(&project_dir, "alice", "   \n\n")
        .expect_err("empty body must fail");
    assert!(err.to_string().contains("empty"));
}

#[test]
fn change_persona_is_atomic_no_partial_file_on_concurrent_read() {
    // Even though we can't deterministically race the rename in a
    // single-threaded test, we verify the temp-file convention: the
    // post-condition is exactly the new content with no `.tmp`
    // siblings dangling.
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\n---\noriginal\n",
    );
    let new_md = "---\nname: alice\ntools: Read\n---\nrevised\n";
    admin_actions::change_persona(&project_dir, "alice", new_md).unwrap();
    let agents_dir = project_dir.join(".claude/agents");
    let mut tmp_files = 0;
    for entry in fs::read_dir(&agents_dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) == Some("tmp")
            || p.to_string_lossy().contains(".md.tmp")
        {
            tmp_files += 1;
        }
    }
    assert_eq!(
        tmp_files, 0,
        "no stale .md.tmp should remain after change_persona"
    );
}
