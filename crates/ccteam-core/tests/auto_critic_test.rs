//! V0.6.0 Wave 3 F112 §B — auto-critic vendor decision tests.
//! Drives `ccteam_core::auto_critic::decide_vendor` directly so the
//! Phase 3.5 logic in `skills/ccteam-creator/SKILL.md` has a
//! programmatic guard that mirrors what the skill body says.

use ccteam_core::auto_critic::{
    decide_vendor, is_critic_persona, plan_annotation, CodexProbe, Vendor, CRITIC_PERSONA_IDS,
};

#[test]
fn critic_persona_and_codex_available_returns_codex() {
    for id in CRITIC_PERSONA_IDS {
        let v = decide_vendor(id, &[], &CodexProbe::Available);
        assert_eq!(v, Vendor::Codex, "expected codex for persona id {id}");
    }
}

#[test]
fn critic_persona_but_codex_missing_falls_back_to_claude() {
    let v = decide_vendor(
        "code-critic",
        &[],
        &CodexProbe::Unavailable("codex CLI not on PATH"),
    );
    assert_eq!(v, Vendor::Claude);
}

#[test]
fn non_critic_persona_stays_claude_even_with_codex_available() {
    // Even if the user has codex installed + authed, a non-critic
    // persona (e.g. a chat assistant) should never be silently
    // vendored on codex.
    let v = decide_vendor("tech-helper", &["chat"], &CodexProbe::Available);
    assert_eq!(v, Vendor::Claude);
}

#[test]
fn tag_promotion_picks_up_user_personas() {
    // A user-defined persona id ccteam doesn't ship can still be
    // recognised as a critic via tags. Future V0.7 user-persona flow
    // depends on this.
    assert!(is_critic_persona("my-pet-critic", &["second-opinion"]));
    let v = decide_vendor("my-pet-critic", &["Second-Opinion"], &CodexProbe::Available);
    assert_eq!(v, Vendor::Codex);
}

#[test]
fn plan_annotation_strings_match_skill_body() {
    // The ccteam-creator SKILL.md Phase 4 PROJECT PLAN includes
    // strings like "Codex critic: auto-enabled". These tests pin the
    // exact text so the skill markdown stays in sync with the
    // helper output — change one, change both.
    assert_eq!(
        plan_annotation("code-critic", &[], &CodexProbe::Available),
        "auto-enabled"
    );
    assert_eq!(
        plan_annotation("code-critic", &[], &CodexProbe::Unavailable("missing")),
        "unavailable (codex CLI not installed / not authenticated)"
    );
    assert_eq!(
        plan_annotation("tech-helper", &[], &CodexProbe::Available),
        "not applicable"
    );
}
