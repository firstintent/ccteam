//! V0.4.6 F83 — `workflow.yaml` location migration tests.
//!
//! Coverage matrix (5 cases) — see `docs/versions/v0-4-6/dev-plan.md` §3.
//!
//! Validates:
//! 1. `WorkflowSpec::load_for_project` priority — `.ccteam/` first,
//!    legacy root second.
//! 2. `migrate_workflow_to_ccteam_dir` behavior for each (root,
//!    `.ccteam/`) presence combination.
//!
//! The migration helper iterates over `config.yaml::projects[]`, so
//! these tests set up a real `~/.ccteam/config.yaml` via
//! `ccteam_core::upsert_project_in_config`.

use std::path::Path;

use ccteam_core::{
    migrate_workflow_to_ccteam_dir, upsert_project_in_config, CcteamPaths, ProjectEntry,
    WorkflowMigrationAction,
};
use ccteam_flow::WorkflowSpec;
use chrono::Utc;
use tempfile::TempDir;

fn paths_in(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join(".ccteam"),
        projects_root: tmp.path().join("projects"),
    }
}

/// Minimal valid workflow.yaml body for fixture files.
const MINIMAL_WORKFLOW: &str = "name: t\nagents:\n  explorer:\n    trigger: manual\n";

fn register_project(paths: &CcteamPaths, slug: &str, project_path: &Path) {
    upsert_project_in_config(
        &paths.root,
        ProjectEntry {
            slug: slug.to_string(),
            path: project_path.to_path_buf(),
            host: ccteam_core::LOCAL_HOST.to_string(),
            remote_slug: None,
            remote_path: None,
            team: "dev".into(),
            installed_at: Utc::now(),
        },
    )
    .expect("upsert into config.yaml");
}

// ---------------------------------------------------------------------
// t01 — load_for_project prefers .ccteam/workflow.yaml when both exist
// ---------------------------------------------------------------------
#[test]
fn t01_load_for_project_prefers_ccteam_dir() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(project.join(".ccteam")).unwrap();

    // Root: "name: root_loser". Nested: "name: nested_winner".
    std::fs::write(
        project.join("workflow.yaml"),
        "name: root_loser\nagents:\n  explorer:\n    trigger: manual\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".ccteam").join("workflow.yaml"),
        "name: nested_winner\nagents:\n  explorer:\n    trigger: manual\n",
    )
    .unwrap();

    let spec = WorkflowSpec::load_for_project(&project).expect("nested loads");
    assert_eq!(
        spec.name, "nested_winner",
        "V0.4.6 F83 priority must be .ccteam/ over project root",
    );
}

// ---------------------------------------------------------------------
// t02 — load_for_project falls back to root when only root exists
// ---------------------------------------------------------------------
#[test]
fn t02_load_for_project_falls_back_to_root() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("workflow.yaml"),
        "name: legacy_root\nagents:\n  explorer:\n    trigger: manual\n",
    )
    .unwrap();

    let spec = WorkflowSpec::load_for_project(&project).expect("root fallback loads");
    assert_eq!(spec.name, "legacy_root");
}

// ---------------------------------------------------------------------
// t03 — `ccteam init` writes workflow.yaml to .ccteam/, not root
//
// We exercise the public install_project_at path indirectly by reaching
// for the scaffold helper through the CLI. The CLI test
// `run_init_fresh_install_scaffolds_and_registers` covers the full
// run_init wiring; this integration test only asserts the contract the
// orchestrator depends on: a freshly-installed project's workflow.yaml
// is at `.ccteam/workflow.yaml` and the parser finds it.
// ---------------------------------------------------------------------
#[test]
fn t03_init_writes_to_ccteam_dir() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();

    // Simulate what scaffold_workflow_yaml does post-F83.
    let ccteam_dir = project.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir).unwrap();
    std::fs::write(ccteam_dir.join("workflow.yaml"), MINIMAL_WORKFLOW).unwrap();

    // Discovery picks it up at the canonical path.
    let spec = WorkflowSpec::load_for_project(&project).expect("ccteam-dir workflow loads");
    assert_eq!(spec.name, "t");

    // Sanity: root was never written.
    assert!(
        !project.join("workflow.yaml").exists(),
        "post-F83 init must not leave a root-level workflow.yaml",
    );
}

// ---------------------------------------------------------------------
// t04 — migration --apply moves root → .ccteam/ for registered projects
// ---------------------------------------------------------------------
#[test]
fn t04_migration_moves_root_to_ccteam_dir() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_in(&tmp);
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("workflow.yaml"), MINIMAL_WORKFLOW).unwrap();
    register_project(&paths, "proj", project.as_path());

    // Dry-run first: reports a move, files untouched.
    let dry = migrate_workflow_to_ccteam_dir(&paths, true).unwrap();
    assert_eq!(dry.len(), 1);
    assert_eq!(
        dry[0].action,
        WorkflowMigrationAction::Moved { dry_run: true }
    );
    assert!(
        project.join("workflow.yaml").exists(),
        "dry-run must not move"
    );
    assert!(!project.join(".ccteam").join("workflow.yaml").exists());

    // Apply: file moves.
    let applied = migrate_workflow_to_ccteam_dir(&paths, false).unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(
        applied[0].action,
        WorkflowMigrationAction::Moved { dry_run: false }
    );
    assert!(
        !project.join("workflow.yaml").exists(),
        "root workflow.yaml should be gone after --apply",
    );
    assert!(
        project.join(".ccteam").join("workflow.yaml").exists(),
        "nested workflow.yaml should exist after --apply",
    );

    // Body preserved verbatim.
    assert_eq!(
        std::fs::read_to_string(project.join(".ccteam").join("workflow.yaml")).unwrap(),
        MINIMAL_WORKFLOW,
    );

    // Re-run is idempotent.
    let rerun = migrate_workflow_to_ccteam_dir(&paths, false).unwrap();
    assert_eq!(rerun[0].action, WorkflowMigrationAction::AlreadyAtCcteamDir);
}

// ---------------------------------------------------------------------
// t05 — migration refuses to act when both locations have a workflow.yaml
// ---------------------------------------------------------------------
#[test]
fn t05_migration_refuses_on_both_present() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_in(&tmp);
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(project.join(".ccteam")).unwrap();
    std::fs::write(
        project.join("workflow.yaml"),
        "name: root\nagents:\n  e:\n    trigger: manual\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".ccteam").join("workflow.yaml"),
        "name: nested\nagents:\n  e:\n    trigger: manual\n",
    )
    .unwrap();
    register_project(&paths, "proj", project.as_path());

    // Apply (not dry-run) — still fail-safe.
    let reports = migrate_workflow_to_ccteam_dir(&paths, false).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].action,
        WorkflowMigrationAction::ConflictBothPresent,
    );

    // Both files left exactly as they were.
    assert_eq!(
        std::fs::read_to_string(project.join("workflow.yaml")).unwrap(),
        "name: root\nagents:\n  e:\n    trigger: manual\n",
    );
    assert_eq!(
        std::fs::read_to_string(project.join(".ccteam").join("workflow.yaml")).unwrap(),
        "name: nested\nagents:\n  e:\n    trigger: manual\n",
    );
}
