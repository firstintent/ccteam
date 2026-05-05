//! M2.1 integration tests: sub-skill scheduling + auto @-attachment of
//! prior-phase outputs into the next phase's prompt.
//!
//! Pattern: configure `OrchestratorConfig::subskill_argv` to a shell
//! stub that captures stdin and writes a known body to stdout. That
//! exercises the orchestrator's plumbing (path resolution, output
//! persistence, progress.jsonl events, attachment collection) without
//! requiring a real `claude` on the PATH.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use ccteam_core::{
    bootstrap_project, disable_tool_surface_bootstrap_for_tests, write_global_phase_templates,
    CcteamPaths, Orchestrator, OrchestratorConfig, PhaseHistoryEntry, PhaseState, ProjectState,
    SubSkillTrigger,
};

static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();

fn ensure_isolation() {
    DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
}

fn fresh_paths(tmp: &tempfile::TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    }
}

/// Replace the shipped phase markdown under `~/.ccteam/phases/` with
/// a single template that names a `local:` sub-skill so the test can
/// stage the agent body next to the project. Returns the slug that
/// gets bootstrapped.
fn setup_subskill_project(
    paths: &CcteamPaths,
    template_body: &str,
    agent_body: &str,
) -> String {
    write_global_phase_templates(&paths.root, true).unwrap();
    let phases = paths.phases_dir();
    // Wipe shipped templates so only ours remains.
    for entry in std::fs::read_dir(&phases).unwrap() {
        let p = entry.unwrap().path();
        std::fs::remove_file(&p).unwrap();
    }
    std::fs::write(phases.join("01-implement.md"), template_body).unwrap();

    let slug = "subskill-demo";
    bootstrap_project(paths, slug, "demo request", "dev").unwrap();
    let project = paths.project_dir(slug);
    std::fs::write(project.join("agent.md"), agent_body).unwrap();
    slug.to_string()
}

#[test]
fn run_phase_sub_skills_phase_done_writes_output_via_stub_runner() {
    ensure_isolation();
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);

    // Phase 'implement' with one phase_done sub_skill pointed at a
    // local agent file. output_to lands under .ccteam/.
    let template_body = concat!(
        "---\n",
        "name: implement\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: local:agent.md\n",
        "    trigger: phase_done\n",
        "    output_to: .ccteam/code-review.md\n",
        "---\n",
        "body\n",
    );
    let slug = setup_subskill_project(&paths, template_body, "# stub agent\n");
    let project_dir = paths.project_dir(&slug);

    let cfg = OrchestratorConfig {
        skip_tool_check: true,
        // Stub: read stdin (the prompt), discard it, write a fixed
        // body to stdout. Production claude -p does the same shape
        // but actually invokes the model.
        subskill_argv: Some(vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null && printf 'REVIEW BODY\\n'".into(),
        ]),
        ..OrchestratorConfig::default()
    };
    let orch = Orchestrator::new(paths.clone(), cfg).unwrap();
    let template = orch
        .templates()
        .iter()
        .find(|t| t.name == "implement")
        .unwrap()
        .clone();

    orch.run_phase_sub_skills(&slug, &template, SubSkillTrigger::PhaseDone);

    let out = project_dir.join(".ccteam/code-review.md");
    assert!(out.is_file(), "sub-skill output file should exist: {}", out.display());
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("REVIEW BODY"), "got: {body:?}");

    // progress.jsonl should record subskill_started + subskill_done.
    let pj = std::fs::read_to_string(paths.progress_jsonl(&slug)).unwrap();
    assert!(pj.contains("subskill_started"), "got: {pj}");
    assert!(pj.contains("subskill_done"), "got: {pj}");
}

#[test]
fn run_phase_sub_skills_records_failed_when_runner_exits_nonzero() {
    ensure_isolation();
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);

    let template_body = concat!(
        "---\n",
        "name: implement\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: local:agent.md\n",
        "    trigger: phase_done\n",
        "    output_to: .ccteam/x.md\n",
        "---\n",
    );
    let slug = setup_subskill_project(&paths, template_body, "x");

    let cfg = OrchestratorConfig {
        skip_tool_check: true,
        subskill_argv: Some(vec!["sh".into(), "-c".into(), "exit 17".into()]),
        ..OrchestratorConfig::default()
    };
    let orch = Orchestrator::new(paths.clone(), cfg).unwrap();
    let template = orch.templates()[0].clone();

    orch.run_phase_sub_skills(&slug, &template, SubSkillTrigger::PhaseDone);

    let out = paths.project_dir(&slug).join(".ccteam/x.md");
    assert!(!out.exists(), "failed runner must not leave output behind");
    let pj = std::fs::read_to_string(paths.progress_jsonl(&slug)).unwrap();
    assert!(pj.contains("subskill_failed"), "got: {pj}");
}

#[test]
fn attachments_for_next_phase_picks_up_phase_done_outputs() {
    ensure_isolation();
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);

    // Two phases: 01-implement (with phase_done sub_skill) → 02-review
    // (no sub_skill). After implement runs and 'advances', the review
    // dispatch should @-attach implement's output_to.
    let implement_body = concat!(
        "---\n",
        "name: implement\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: local:agent.md\n",
        "    trigger: phase_done\n",
        "    output_to: .ccteam/code-review.md\n",
        "---\n",
    );
    let review_body = concat!(
        "---\n",
        "name: review\n",
        "parallelism: solo\n",
        "---\n",
    );
    write_global_phase_templates(&paths.root, true).unwrap();
    let phases = paths.phases_dir();
    for entry in std::fs::read_dir(&phases).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    std::fs::write(phases.join("01-implement.md"), implement_body).unwrap();
    std::fs::write(phases.join("02-review.md"), review_body).unwrap();

    let slug = "att-demo";
    bootstrap_project(&paths, slug, "demo request", "dev").unwrap();
    let project = paths.project_dir(slug);
    // Pretend implement's sub-skill already ran:
    std::fs::create_dir_all(project.join(".ccteam")).unwrap();
    std::fs::write(project.join(".ccteam/code-review.md"), "REVIEW\n").unwrap();

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    // Pretend implement just finished.
    let mut state = ProjectState::initial(slug.to_string());
    state.phase_history.push(PhaseHistoryEntry {
        phase: "implement".into(),
        status: "passed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    state.phase_state = PhaseState::Idle;
    state.current_phase = "review".into();

    let attachments = orch.attachments_for_next_phase(slug, &state);
    assert_eq!(attachments, vec![".ccteam/code-review.md".to_string()]);
}

#[test]
fn attachments_for_next_phase_returns_empty_when_output_missing() {
    ensure_isolation();
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);

    let template_body = concat!(
        "---\n",
        "name: implement\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: local:agent.md\n",
        "    trigger: phase_done\n",
        "    output_to: .ccteam/never-written.md\n",
        "---\n",
    );
    let slug = setup_subskill_project(&paths, template_body, "x");

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    let mut state = ProjectState::initial(slug.to_string());
    state.phase_history.push(PhaseHistoryEntry {
        phase: "implement".into(),
        status: "passed".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });

    // The sub-skill never ran, so the output file is absent →
    // attachment list must be empty (no broken @-references for the
    // next phase).
    assert!(orch.attachments_for_next_phase(&slug, &state).is_empty());
    let _ = Path::new(".");  // silence unused import on some platforms
}
