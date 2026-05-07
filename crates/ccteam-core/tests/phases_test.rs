//! Integration test: load every phase template shipped under `phases/`
//! and verify it parses + satisfies M0 invariants (`parallelism: solo`,
//! empty `sub_skills`).

use std::path::PathBuf;

use ccteam_core::{Parallelism, PhaseTemplate};

fn phases_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // V0.2 M0.17.1: phases live under teams/<name>/phases/ in the
    // unified team layout (was repo-root `phases/`).
    manifest.join("../../teams/dev/phases")
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
    // M2.1+ note: sub_skills is no longer required to be empty —
    // implement.md now declares the code-reviewer auto-trigger so the
    // orchestrator runs it on phase_done. The validate path is the
    // contract; the empty-list assertion was an M0-era invariant.
    let dir = phases_dir();
    for (file, expected_name) in M0_TEMPLATES {
        let path = dir.join(file);
        let template = PhaseTemplate::load(&path)
            .unwrap_or_else(|e| panic!("load {}: {e:#}", path.display()));

        assert_eq!(
            template.name, *expected_name,
            "{file}: front-matter `name` must equal phase id (without numeric prefix)",
        );
        // Dev's shipped phases are still all `solo` — M2.2 lifts this
        // gate but the `agent_team` enablement lives in a follow-up
        // commit (post-spike), not in the static templates.
        assert_eq!(
            template.parallelism,
            Parallelism::Solo,
            "{file}: dev phases stay solo until M2.2 enables agent_team",
        );

        template
            .validate_m0()
            .unwrap_or_else(|e| panic!("{file} fails validation: {e:#}"));
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

#[test]
fn fix_phase_completion_signal_matches_body_phase_done_sigil() {
    // E2E 2026-05-06 F6 regression: the fix phase declared
    // `completion_signal: TESTS_GREEN` in YAML while the body told
    // Claude to output `PHASE_DONE: fix`. The fix-loop never observed
    // the signal, looped 3x, and falsely escalated despite all tests
    // passing. Auto-loop phases must declare the signal Claude is
    // actually instructed to emit.
    let dir = phases_dir();
    let template = PhaseTemplate::load(&dir.join("06-fix.md")).unwrap();
    assert!(template.auto_loop, "fix phase is auto-loop driven");
    assert_eq!(template.completion_signal, "PHASE_DONE: fix");
    let body = std::fs::read_to_string(dir.join("06-fix.md")).unwrap();
    assert!(
        body.contains(&template.completion_signal),
        "fix body must contain its declared completion_signal `{}`",
        template.completion_signal,
    );
}
