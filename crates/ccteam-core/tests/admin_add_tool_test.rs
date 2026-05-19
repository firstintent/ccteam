//! V0.6.1 F128 — integration test for `admin_actions::add_tool`.
//!
//! Covers the four canonical states the helper must handle:
//! 1. Frontmatter already lists `tools: A, B` → append produces
//!    `A, B, C` (CSV preserved).
//! 2. Frontmatter has no `tools:` line → a new line is inserted.
//! 3. Tool already present → idempotent no-op (file rewritten with
//!    the same CSV; `already_present: true`).
//! 4. Missing persona file → clean error (no silent stub creation).

use std::fs;
use std::path::Path;

use ccteam_core::admin_actions;
use tempfile::TempDir;

fn seed_project(root: &Path, slug: &str, bot: &str, body: &str) -> std::path::PathBuf {
    let project_dir = root.join("projects").join(slug);
    let agents_dir = project_dir.join(".claude").join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    fs::write(agents_dir.join(format!("{bot}.md")), body).unwrap();
    project_dir
}

#[test]
fn add_tool_appends_to_existing_csv() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\ndescription: helper bot\ntools: Read, Grep\nmodel: sonnet\n---\n\nbot body\n",
    );
    let res = admin_actions::add_tool(&project_dir, "alice", "WebFetch").unwrap();
    assert!(!res.already_present);
    assert_eq!(res.added, "WebFetch");
    assert_eq!(res.new_tools_csv, "Read, Grep, WebFetch");

    let on_disk = fs::read_to_string(&res.path).unwrap();
    assert!(on_disk.contains("tools: Read, Grep, WebFetch"));
    // Other frontmatter fields preserved.
    assert!(on_disk.contains("model: sonnet"));
    assert!(on_disk.contains("description: helper bot"));
    // Body preserved.
    assert!(on_disk.contains("bot body"));
    // Frontmatter still closes properly.
    let parts: Vec<&str> = on_disk.split("---").collect();
    assert!(
        parts.len() >= 3,
        "frontmatter must still have open + close ---"
    );
}

#[test]
fn add_tool_inserts_line_when_frontmatter_has_no_tools_key() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\ndescription: helper\nmodel: sonnet\n---\n\nbot body\n",
    );
    let res = admin_actions::add_tool(&project_dir, "alice", "Bash").unwrap();
    assert!(!res.already_present);
    assert_eq!(res.new_tools_csv, "Bash");

    let on_disk = fs::read_to_string(&res.path).unwrap();
    assert!(on_disk.contains("tools: Bash"));
    assert!(on_disk.contains("model: sonnet"));
    assert!(on_disk.contains("bot body"));
}

#[test]
fn add_tool_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\ntools: Read, WebFetch\n---\nbody\n",
    );
    let res = admin_actions::add_tool(&project_dir, "alice", "WebFetch").unwrap();
    assert!(res.already_present);
    assert_eq!(res.new_tools_csv, "Read, WebFetch");

    let on_disk = fs::read_to_string(&res.path).unwrap();
    // The CSV is unchanged + the tool list still has exactly one
    // `WebFetch` entry (no duplicates).
    let count = on_disk.matches("WebFetch").count();
    assert_eq!(count, 1, "WebFetch must not duplicate; got: {on_disk}");
}

#[test]
fn add_tool_refuses_missing_persona() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("projects").join("dev-empty");
    fs::create_dir_all(project_dir.join(".claude/agents")).unwrap();
    let err = admin_actions::add_tool(&project_dir, "ghost", "Read")
        .expect_err("phantom bot must error, not auto-create");
    assert!(err.to_string().contains("no persona file"));
}

#[test]
fn add_tool_event_shape_round_trips() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\ntools: Read\n---\nbody\n",
    );
    let res = admin_actions::add_tool(&project_dir, "alice", "WebFetch").unwrap();
    let ev = admin_actions::build_tool_added_event(
        "alice",
        &res.path,
        &res.added,
        &res.new_tools_csv,
        res.already_present,
    );
    assert_eq!(ev["event"], "tool_added");
    assert_eq!(ev["role"], "alice");
    assert_eq!(ev["tool"], "WebFetch");
    assert_eq!(ev["tools"], "Read, WebFetch");
    assert_eq!(ev["already_present"], false);
    assert!(ev["ts"].as_str().unwrap().contains('T'));
}

#[test]
fn add_tool_rejects_empty_descriptor() {
    let tmp = TempDir::new().unwrap();
    let project_dir = seed_project(
        tmp.path(),
        "dev-helper",
        "alice",
        "---\nname: alice\n---\nbody\n",
    );
    let err = admin_actions::add_tool(&project_dir, "alice", "   ")
        .expect_err("empty descriptor must fail");
    assert!(err.to_string().contains("empty"));
}

#[test]
fn add_tool_preserves_other_frontmatter_keys_exactly() {
    let tmp = TempDir::new().unwrap();
    let original = concat!(
        "---\n",
        "name: alice\n",
        "description: helper that summarises issues\n",
        "tools: Read, Grep\n",
        "model: claude-sonnet-4-5\n",
        "---\n",
        "\n",
        "## persona\n",
        "you are alice, helpful + concise\n",
    );
    let project_dir = seed_project(tmp.path(), "dev-helper", "alice", original);
    admin_actions::add_tool(&project_dir, "alice", "WebFetch").unwrap();
    let on_disk = fs::read_to_string(project_dir.join(".claude/agents/alice.md")).unwrap();
    // Every original frontmatter key still present + body intact.
    assert!(on_disk.contains("name: alice"));
    assert!(on_disk.contains("description: helper that summarises issues"));
    assert!(on_disk.contains("model: claude-sonnet-4-5"));
    assert!(on_disk.contains("you are alice, helpful + concise"));
    assert!(on_disk.contains("## persona"));
}

#[test]
fn add_tool_rejects_invalid_bot_name() {
    let tmp = TempDir::new().unwrap();
    let err = admin_actions::add_tool(tmp.path(), "Helper Bot", "Read")
        .expect_err("bot with space must fail");
    assert!(err.to_string().contains("not allowed"));
}
