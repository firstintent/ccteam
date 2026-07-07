//! Regression: `ccteam init --slug <name>` installs in the *current
//! working directory*, not under `<projects_root>/<name>`.
//!
//! Before this fix, passing `--slug` (without `--in`) silently
//! relocated the install to `<projects_root>/<slug>/` — so a user
//! standing in an existing repo who only wanted to *name* their project
//! got an empty skeleton created elsewhere and none of their code.
//! `--slug` is now a pure name override; the install target is the cwd
//! (or `--in <path>`). To create a fresh central project use
//! `ccteam project new <slug>`.
//!
//! Runs the real binary via `CARGO_BIN_EXE_ccteam` with a child cwd
//! (`Command::current_dir`) so the chdir is process-local and can't race
//! other tests (cwd is process-global — see CLAUDE.md §六).

use std::process::Command;

use tempfile::TempDir;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

#[test]
fn init_with_slug_targets_cwd_not_projects_root() {
    let tmp = TempDir::new().unwrap();
    let ccteam_home = tmp.path().join("home");
    let projects_root = tmp.path().join("projects");
    // The user's existing repo — an arbitrary path *outside* projects_root.
    let repo = tmp.path().join("nasworkspace").join("AgentServe");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();
    // Canonicalize so the child's `current_dir()` (which resolves
    // symlinks, e.g. a symlinked $TMPDIR) matches our `repo.join(..)`
    // assertions.
    let repo = std::fs::canonicalize(&repo).unwrap();

    let out = Command::new(ccteam_bin())
        .args(["init", "--slug", "agentserver"])
        .current_dir(&repo)
        .env("CCTEAM_HOME", &ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", &projects_root)
        .output()
        .expect("spawn ccteam init");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "init should succeed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Installed into the cwd (the repo) ...
    assert!(
        repo.join(".ccteam").join("state.json").is_file(),
        "expected .ccteam/state.json under the cwd {}, but it's missing.\nstdout: {stdout}",
        repo.display()
    );
    // ... and NOT into <projects_root>/agentserver (the old relocate bug).
    assert!(
        !projects_root.join("agentserver").exists(),
        "init --slug must NOT create <projects_root>/agentserver; the slug is a name, \
         not a location.\nstdout: {stdout}"
    );
    // The reported target dir is the cwd.
    assert!(
        stdout.contains(&repo.display().to_string()),
        "stdout should report the cwd ({}) as the target dir.\nstdout: {stdout}",
        repo.display()
    );
}
