//! v0.8.7 W3 (DC.3) — `ccteam role <search|list>` CLI surface tests.
//!
//! `search` is offline (the bundled manifest, no network) and `list` reads
//! a project's `.claude/agents/` — both run via the real binary against a
//! `TempDir` `CCTEAM_HOME` / cwd so nothing touches the real `~/.ccteam` or
//! `~/.claude.json`. (`role add` hits the network, so it is covered by the
//! deterministic `ccteam-im` mock-server test, not here.)

use std::process::Command;

use tempfile::TempDir;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

#[test]
fn role_search_lists_catalog_offline() {
    let tmp = TempDir::new().unwrap();
    let out = Command::new(ccteam_bin())
        .args(["role", "search", "backend"])
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "role search should exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The catalog has backend roles; the output should name at least one and
    // print the add hint.
    assert!(
        stdout.contains("backend"),
        "search `backend` should list backend roles; got: {stdout}"
    );
    assert!(
        stdout.contains("ccteam role add"),
        "search output should hint at `ccteam role add`; got: {stdout}"
    );
}

#[test]
fn role_search_json_is_parseable_array() {
    let tmp = TempDir::new().unwrap();
    let out = Command::new(ccteam_bin())
        .args(["role", "search", "backend", "--format", "json"])
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role search --format json");
    assert!(
        out.status.success(),
        "role search --format json should exit 0"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("role search --format json must emit valid JSON");
    let arr = v.as_array().expect("search json is an array");
    assert!(!arr.is_empty(), "backend search should return entries");
    // Each entry carries the id used by `role add`.
    assert!(
        arr.iter()
            .all(|e| e.get("id").and_then(|i| i.as_str()).is_some()),
        "every catalog entry must carry an `id`"
    );
}

#[test]
fn role_search_no_match_is_clean() {
    let tmp = TempDir::new().unwrap();
    let out = Command::new(ccteam_bin())
        .args(["role", "search", "zzz-no-such-role-zzz"])
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role search");
    assert!(
        out.status.success(),
        "an empty search result is not an error"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no catalog roles match"),
        "no-match should print a friendly message; got: {stdout}"
    );
}

#[test]
fn role_list_empty_project_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    // An uninitialized cwd: no .claude/agents/ dir at all.
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let out = Command::new(ccteam_bin())
        .args(["role", "list"])
        .current_dir(&repo)
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "role list on an uninitialized project must exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("no roles installed"),
        "empty project should print a friendly message; got: {stdout}"
    );
}

#[test]
fn role_list_reports_installed_roles() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let agents = repo.join(".claude").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Reviews diffs\n---\nYou review.\n",
    )
    .unwrap();

    let out = Command::new(ccteam_bin())
        .args(["role", "list"])
        .current_dir(&repo)
        .env("CCTEAM_HOME", tmp.path().join("home"))
        .env("CCTEAM_PROJECTS_ROOT", tmp.path().join("projects"))
        .output()
        .expect("spawn ccteam role list");
    assert!(out.status.success(), "role list should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("reviewer"),
        "list should report the installed `reviewer` role; got: {stdout}"
    );
}
