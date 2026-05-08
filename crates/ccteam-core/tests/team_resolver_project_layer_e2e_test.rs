//! F28 — project-layer team override (`<project_dir>/.ccteam/team/team.yaml`)
//! must win over the user / repo cached runtime when the orchestrator
//! resolves a team for a specific project. Pre-V0.2.1 the production
//! callers passed `for_orchestrator(...)` everywhere, so the Project
//! source was dead code (M0.17 ship description claimed first-source-
//! wins; e2e Suite B5 caught it).
//!
//! This test goes through the public `team_runtime_for_state` helper,
//! which is the single seam the dispatch path uses post-V0.2.1.

use std::sync::atomic::{AtomicU64, Ordering};

use ccteam_core::{
    disable_tool_surface_bootstrap_for_tests, write_all_global_team_templates, CcteamPaths,
    Orchestrator, OrchestratorConfig, Parallelism, PhaseState, ProjectState,
};
use tempfile::TempDir;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("f28-proj-{pid}-{n}")
}

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

fn fresh_state(slug: &str, team: &str) -> ProjectState {
    let mut s = ProjectState::initial_for_team(slug.to_string(), team.to_string());
    s.tmux_session = format!("ccteam-{slug}");
    s.current_phase = "plan-eng".into();
    s.phase_state = PhaseState::Idle;
    s
}

#[test]
fn project_layer_team_override_wins_over_repo_cached_runtime() {
    disable_tool_surface_bootstrap_for_tests();

    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    // Seed shipped teams in repo layer (`<root>/teams/dev/...`) so
    // the orchestrator's startup walk caches a TeamRuntime keyed `dev`.
    write_all_global_team_templates(&paths.root, false).unwrap();

    // Build the orchestrator with the cache populated.
    let config = OrchestratorConfig {
        skip_tool_check: true,
        ..OrchestratorConfig::default()
    };
    let orch = Orchestrator::new(paths.clone(), config).unwrap();

    let slug = unique_slug();
    let project_dir = paths.project_dir(&slug);
    std::fs::create_dir_all(&project_dir).unwrap();

    // Sanity: with no override the cached repo runtime drives.
    let state = fresh_state(&slug, "dev");
    let baseline = orch.team_runtime_for_state(&state).expect("dev runtime");
    let baseline_desc = baseline.spec.description.clone();
    drop(baseline);

    // Now drop a project-layer override that mutates a spec field
    // (description) so we can fingerprint which layer drove the
    // resolver. The override copies the repo phase_dir convention so
    // template loading keeps working without per-project markdowns.
    let override_dir = project_dir.join(".ccteam").join("team");
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(
        override_dir.join("team.yaml"),
        "name: dev\n\
         description: F28-project-override\n",
    )
    .unwrap();

    let after = orch.team_runtime_for_state(&state).expect("override runtime");
    assert_eq!(
        after.spec.description, "F28-project-override",
        "project layer override should win; baseline desc was {baseline_desc:?}; \
         got after.spec.description = {:?}",
        after.spec.description,
    );
    // The DAG must still load (override carried only team.yaml; phase
    // markdowns came from user/repo fallback).
    assert!(!after.dag.is_empty(), "phase DAG should load via fallback");
    assert!(
        !after.templates.is_empty(),
        "phase templates should load via fallback",
    );
}

#[test]
fn project_layer_no_override_returns_cached_repo_runtime() {
    disable_tool_surface_bootstrap_for_tests();

    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_all_global_team_templates(&paths.root, false).unwrap();

    let config = OrchestratorConfig {
        skip_tool_check: true,
        ..OrchestratorConfig::default()
    };
    let orch = Orchestrator::new(paths.clone(), config).unwrap();
    let slug = unique_slug();
    std::fs::create_dir_all(paths.project_dir(&slug)).unwrap();

    let state = fresh_state(&slug, "dev");
    let runtime = orch.team_runtime_for_state(&state).expect("dev runtime");
    // No project override → matches Borrowed (the cached one)
    assert!(matches!(runtime, std::borrow::Cow::Borrowed(_)));
}

// `Parallelism` is re-exported but not used directly; pull it in as a
// compile-time check that the test fixture path doesn't drift away
// from the public surface.
#[allow(dead_code)]
fn _parallelism_check() -> Parallelism {
    Parallelism::Solo
}
