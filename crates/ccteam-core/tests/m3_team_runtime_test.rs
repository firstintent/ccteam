//! M3.3 integration: orchestrator's per-team runtime registry.
//!
//! Asserts that `Orchestrator::new` discovers `<root>/teams/<name>/team.yaml`
//! files, registers a `TeamRuntime` per team with its own DAG, and that
//! `team_runtime(name)` returns the right templates. Covers both the
//! happy path (dev + research both registered) and the legacy
//! fallback (only `phases/` present, dev registered implicitly).
//!
//! V0.2.2 F40: research is canonical; `team_runtime("product-research")`
//! resolves through the alias-aware lookup path.

use std::sync::OnceLock;

use ccteam_core::{
    disable_tool_surface_bootstrap_for_tests, write_all_global_team_templates, CcteamPaths,
    Orchestrator, OrchestratorConfig,
};
use tempfile::TempDir;

static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
fn isolation() {
    DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
}

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

#[test]
fn orchestrator_loads_dev_and_research_after_init() {
    // V0.2.2 F40: renamed from
    // `orchestrator_loads_dev_and_product_research_after_init`. The
    // research team replaces product-research; the alias-aware test
    // below verifies the legacy name still resolves.
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    // `ccteam init` equivalent: write all team bundles to ~/.ccteam/.
    write_all_global_team_templates(&paths.root, false).unwrap();

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();

    let dev = orch
        .team_runtime("dev")
        .expect("dev team should be registered after write_all_global_team_templates");
    assert_eq!(dev.spec.name, "dev");
    assert_eq!(dev.spec.phase_dir, "phases");
    assert!(
        dev.templates.iter().any(|t| t.name == "plan-eng"),
        "dev pipeline should have the plan-eng phase",
    );
    assert!(
        dev.templates.iter().any(|t| t.name == "ship"),
        "dev pipeline should have the ship phase",
    );
    assert_eq!(dev.dag.entry(), "plan-eng", "dev DAG starts at plan-eng");

    let research = orch
        .team_runtime("research")
        .expect("research team should be registered after write_all_global_team_templates");
    assert_eq!(research.spec.name, "research");
    // V0.2 M0.17.2: phase_dir is relative to team dir; default `phases`.
    assert_eq!(research.spec.phase_dir, "phases");
    assert!(
        research.templates.iter().any(|t| t.name == "kickoff"),
        "research should have the kickoff phase",
    );
    assert!(
        research.templates.iter().any(|t| t.name == "verdict"),
        "research should have the verdict phase",
    );
    assert_eq!(
        research.dag.entry(),
        "kickoff",
        "research DAG starts at kickoff",
    );
    // verdict_schema should list `verdict` so M3.4's verdict-emitting
    // phase is recognized as such.
    assert_eq!(
        research.spec.verdict_schema,
        vec!["verdict".to_string()],
        "research declares the verdict phase in verdict_schema",
    );
    // 3 team-specific ESCALATE prefixes registered.
    let prefixes: Vec<&str> = research
        .spec
        .escalate_grammar_extensions
        .iter()
        .map(|e| e.prefix.as_str())
        .collect();
    assert!(prefixes.contains(&"MARKET_DUPLICATE"));
    assert!(prefixes.contains(&"INSUFFICIENT_VALIDATION"));
    assert!(prefixes.contains(&"LOW_DIFFERENTIATION"));
    // V0.2.2 F40: aliases field carries the legacy name.
    assert!(
        research.spec.aliases.iter().any(|a| a == "product-research"),
        "research team yaml must list `product-research` as an alias for soft migration",
    );
}

#[test]
fn orchestrator_team_runtime_resolves_legacy_alias_product_research() {
    // V0.2.2 F40: old projects whose `state.json::team` still carries
    // `product-research` must still find the registered runtime. The
    // alias-aware lookup walks every TeamRuntime's `spec.aliases`
    // when a direct key miss happens.
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_all_global_team_templates(&paths.root, false).unwrap();

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    let canonical = orch.team_runtime("research").expect("canonical present");
    let via_alias = orch
        .team_runtime("product-research")
        .expect("legacy alias `product-research` must resolve to research runtime");
    // Same runtime — alias-aware lookup returns the canonical one.
    assert_eq!(canonical.spec.name, via_alias.spec.name);
    assert_eq!(via_alias.spec.name, "research");
}

#[test]
fn orchestrator_legacy_fallback_registers_dev_when_only_phases_dir_present() {
    // Pre-M3.3 installs only have `~/.ccteam/phases/` populated, no
    // `~/.ccteam/teams/dev/team.yaml`. The orchestrator must still
    // register a dev runtime so legacy projects keep dispatching.
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let phases_dir = paths.phases_dir();
    std::fs::create_dir_all(&phases_dir).unwrap();
    std::fs::write(
        phases_dir.join("01-step.md"),
        "---\nname: step\nparallelism: solo\n---\nbody\n",
    )
    .unwrap();

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();

    let dev = orch
        .team_runtime("dev")
        .expect("legacy dev fallback should register dev");
    assert_eq!(dev.spec.name, "dev");
    assert_eq!(dev.templates.len(), 1);
    assert_eq!(dev.templates[0].name, "step");
}

#[test]
fn orchestrator_inert_when_no_phases_or_teams_present() {
    // Pure empty install. orchestrator stays inert (no panics).
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert!(orch.team_runtime("dev").is_none());
    assert!(orch.team_runtime("research").is_none());
    // V0.2.2 F40: the alias-aware lookup also returns None when no
    // runtime is registered (no source for the alias to redirect to).
    assert!(orch.team_runtime("product-research").is_none());
    assert_eq!(orch.teams().count(), 0);
    // Backwards-compat accessor still safe (returns empty fallback).
    assert!(orch.templates().is_empty());
    assert!(orch.dag().is_empty());
}

#[test]
fn orchestrator_skips_team_with_missing_phase_dir() {
    // A team.yaml that points at a phase_dir not yet on disk should be
    // logged + skipped, not crash the orchestrator. This matches the
    // "user runs `ccteam init` partially, then starts" recovery path.
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let team_dir = paths.root.join("teams").join("ghost-team");
    std::fs::create_dir_all(&team_dir).unwrap();
    // V0.2 M0.17.2: any non-default phase_dir value works for the
    // "phase_dir doesn't exist" path. `ghost` (no `phases-` legacy
    // prefix) avoids triggering the legacy-rewrite logic.
    std::fs::write(
        team_dir.join("team.yaml"),
        "name: ghost-team\nphase_dir: ghost\n",
    )
    .unwrap();

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert!(
        orch.team_runtime("ghost-team").is_none(),
        "ghost-team's phase_dir is missing → not registered",
    );
}

#[test]
fn write_all_global_team_templates_is_idempotent() {
    // Re-running write_all_global_team_templates(force=false) preserves
    // any operator hand-edits and doesn't bump file mtimes. Mirrors the
    // contract write_global_phase_templates already had.
    isolation();
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_all_global_team_templates(&paths.root, false).unwrap();
    // V0.2 M0.17.1: phase markdown lives under teams/<name>/phases/.
    let path = paths
        .root
        .join("teams")
        .join("dev")
        .join("phases")
        .join("02-plan-eng.md");
    std::fs::write(&path, "USER EDIT\n").unwrap();
    write_all_global_team_templates(&paths.root, false).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT\n");
    write_all_global_team_templates(&paths.root, true).unwrap();
    assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDIT\n");
}
