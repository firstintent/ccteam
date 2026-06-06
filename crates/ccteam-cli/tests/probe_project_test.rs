//! V0.6.6 F167 — integration test for `ccteam internal probe-project
//! --json`.
//!
//! The probe heuristic lives in `ccteam_core::templates::project_probe`
//! (covered by unit tests in that module). This binary-surface test
//! pins the CLI wire shape — argv parsing (`--path` / `--json`), exit
//! code, and the JSON output schema — so the `/ccteam-creator` skill
//! Phase 3.6 has a stable contract.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

#[test]
fn probe_project_emits_json_on_fresh_rust_single_repo() {
    let td = tempdir().unwrap();
    write(
        &td.path().join("Cargo.toml"),
        "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
    );
    write(&td.path().join("src/main.rs"), "fn main() {}\n");
    fs::create_dir_all(td.path().join("tests")).unwrap();

    let out = Command::new(ccteam_bin())
        .args(["internal", "probe-project"])
        .arg("--path")
        .arg(td.path())
        .arg("--json")
        .output()
        .expect("spawn ccteam probe-project");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\nstdout:\n{stdout}"));
    assert_eq!(v["kind"], "single-repo");
    let langs = v["languages"].as_array().unwrap();
    assert!(
        langs.iter().any(|l| l == "rust"),
        "expected rust in languages, got {langs:?}"
    );
    let scope = v["probable_scope"].as_array().unwrap();
    let scope_strs: Vec<String> = scope
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert!(scope_strs.contains(&"src".to_string()));
    assert!(scope_strs.contains(&"tests".to_string()));
}

#[test]
fn probe_project_emits_human_readable_text_without_json_flag() {
    let td = tempdir().unwrap();
    write(&td.path().join("README.md"), "# hi\n");
    fs::create_dir_all(td.path().join("docs")).unwrap();

    let out = Command::new(ccteam_bin())
        .args(["internal", "probe-project"])
        .arg("--path")
        .arg(td.path())
        .output()
        .expect("spawn ccteam probe-project");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.starts_with("kind: docs-only\n"), "stdout:\n{stdout}");
    assert!(stdout.contains("probable_scope: [docs]"));
}

#[test]
fn probe_project_detects_monorepo_rust_workspace_with_glob() {
    let td = tempdir().unwrap();
    write(
        &td.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    for c in ["alpha", "beta"] {
        write(
            &td.path().join(format!("crates/{c}/Cargo.toml")),
            "[package]\nname = \"x\"\nversion = \"0.1\"\n",
        );
        let body = match c {
            "alpha" => "fn x() {}\n".repeat(500),
            _ => "fn y() {}\n".repeat(100),
        };
        write(&td.path().join(format!("crates/{c}/src/lib.rs")), &body);
    }
    let out = Command::new(ccteam_bin())
        .args(["internal", "probe-project"])
        .arg("--path")
        .arg(td.path())
        .arg("--json")
        .output()
        .expect("spawn ccteam probe-project");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "monorepo");
    let scope: Vec<String> = v["probable_scope"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert!(
        scope.iter().any(|s| s.starts_with("crates/")),
        "expected crates/ prefix in scope, got {scope:?}"
    );
}

#[test]
fn probe_project_default_path_uses_cwd() {
    // No --path arg — `ccteam internal probe-project --json` should fall back
    // to `std::env::current_dir()`. We run from a fresh tempdir to
    // confirm the cwd hand-off works (would Empty-classify a bare
    // dir).
    let td = tempdir().unwrap();
    let out = Command::new(ccteam_bin())
        .current_dir(td.path())
        .args(["internal", "probe-project"])
        .arg("--json")
        .output()
        .expect("spawn ccteam probe-project");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "empty");
}
