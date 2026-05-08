//! Integration test: load every phase template shipped under
//! `teams/<team>/phases/` and verify it parses + satisfies M0 invariants
//! (`parallelism: solo`).
//!
//! V0.2 M0.18: phase markdown bodies no longer contain protocol
//! literals (`PHASE_DONE: <name>` / `ESCALATE:`). The orchestrator's
//! per-phase inject prompt carries those — the body stays domain-only.
//! See `docs/v0-2/phase-prompt-architecture.md` §8 for the invariant.

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
fn every_m0_template_body_omits_protocol_literals() {
    // V0.2 M0.18: protocol keywords (`PHASE_DONE: <name>` / `ESCALATE:`)
    // belong in the orchestrator's inject prompt, not in the phase
    // markdown body. A phase body that drifts back to spelling the
    // protocol keyword should fail this guard so reviews catch it
    // before merge. See `docs/v0-2/phase-prompt-architecture.md` §8.
    let dir = phases_dir();
    for (file, expected_name) in M0_TEMPLATES {
        let path = dir.join(file);
        let body = std::fs::read_to_string(&path).unwrap();
        // Strip frontmatter — `completion_signal:` may legitimately
        // declare the literal there, but the body must be clean.
        let body_only = strip_frontmatter(&body);
        let banned = format!("PHASE_DONE: {expected_name}");
        assert!(
            !body_only.contains(&banned),
            "{file} body still contains protocol literal `{banned}` — \
             V0.2 M0.18 expects the inject prompt to carry it",
        );
        assert!(
            !body_only.contains("ESCALATE:"),
            "{file} body still contains `ESCALATE:` literal — \
             V0.2 M0.18 expects the inject prompt to carry it",
        );
    }
}

#[test]
fn fix_phase_effective_completion_signal_resolves_to_phase_done_sigil() {
    // V0.2 M0.18: `06-fix.md` no longer declares `completion_signal`
    // explicitly — `effective_completion_signal()` synthesizes
    // `PHASE_DONE: fix` from the phase name. The auto-loop bootstrap
    // and the inject-prompt builder both consume the synthesized
    // value, so the body / frontmatter stay free of the protocol
    // literal.
    let dir = phases_dir();
    let template = PhaseTemplate::load(&dir.join("06-fix.md")).unwrap();
    assert!(template.auto_loop, "fix phase is auto-loop driven");
    assert_eq!(template.effective_completion_signal(), "PHASE_DONE: fix");
}

fn strip_frontmatter(body: &str) -> &str {
    if let Some(rest) = body.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return &rest[end + 5..];
        }
    }
    body
}
