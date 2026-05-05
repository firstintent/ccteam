//! Integration test: load every phase template shipped under `phases/`
//! and verify it parses + satisfies M0 invariants (`parallelism: solo`,
//! empty `sub_skills`).

use std::path::PathBuf;

use ccteam_core::{Parallelism, PhaseTemplate};

fn phases_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("../../phases")
}

const M0_TEMPLATES: &[(&str, &str)] = &[
    ("02-plan-eng.md", "plan-eng"),
    ("03-implement.md", "implement"),
    ("04-test-author.md", "test-author"),
    ("05-test-run.md", "test-run"),
    ("06-fix.md", "fix"),
    ("09-ship.md", "ship"),
];

#[test]
fn every_m0_template_parses_and_validates() {
    let dir = phases_dir();
    for (file, expected_name) in M0_TEMPLATES {
        let path = dir.join(file);
        let template = PhaseTemplate::load(&path)
            .unwrap_or_else(|e| panic!("load {}: {e:#}", path.display()));

        assert_eq!(
            template.name, *expected_name,
            "{file}: front-matter `name` must equal phase id (without numeric prefix)",
        );
        assert_eq!(
            template.parallelism,
            Parallelism::Solo,
            "{file}: M0 only supports parallelism: solo",
        );
        assert!(
            template.sub_skills.is_empty(),
            "{file}: M0 sub_skills must be empty (M2 starts scheduling)",
        );

        template
            .validate_m0()
            .unwrap_or_else(|e| panic!("{file} fails M0 validation: {e:#}"));
    }
}

#[test]
fn every_m0_template_body_ends_with_phase_done_or_escalate() {
    let dir = phases_dir();
    for (file, expected_name) in M0_TEMPLATES {
        let path = dir.join(file);
        let body = std::fs::read_to_string(&path).unwrap();
        let phase_done = format!("PHASE_DONE: {expected_name}");
        assert!(
            body.contains(&phase_done),
            "{file}: must mention `{phase_done}` so Stop hook can detect terminal state",
        );
        assert!(
            body.contains("ESCALATE"),
            "{file}: must mention `ESCALATE:` as the failure signal",
        );
    }
}
