//! V0.6.0 Wave 3 F112 §B — auto-critic vendor decision helper.
//!
//! Pulled out of the `ccteam-creator` skill body so the decision is
//! deterministic + unit-testable (the skill body is markdown +
//! LLM-driven). The skill calls this helper at Phase 3.5 to decide
//! whether to vendor a critic-flavoured persona on Codex.
//!
//! Decision inputs:
//!
//! 1. **Persona id / tags** — is the role critic-flavoured? Today
//!    the trigger set is a small list of canonical critic ids; tags
//!    are evaluated by case-insensitive substring match against the
//!    `critic` / `reviewer` / `second-opinion` markers.
//! 2. **Codex probe result** — `Available` (cli on PATH + `codex
//!    login status` ok) vs `Unavailable(reason)`. The probe itself
//!    runs from the skill body (Bash) so this module stays free of
//!    side effects.
//!
//! Output:
//!
//! - `Vendor::Codex` when the role is critic-flavoured AND codex is
//!   available.
//! - `Vendor::Claude` otherwise. Caller decides whether to surface a
//!   "Codex critic: unavailable" line in PROJECT PLAN.

use serde::{Deserialize, Serialize};

/// Vendor the auto-critic decision selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Claude,
    Codex,
    Grok,
    Opencode,
}

/// Codex probe outcome — what Phase 3.5's `codex --version &&
/// codex login status` returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexProbe {
    /// CLI present + authenticated.
    Available,
    /// One of the probes failed. The `reason` is purely diagnostic
    /// for the PROJECT PLAN's "Codex critic: unavailable" line.
    Unavailable(&'static str),
}

/// Canonical critic-flavoured persona ids. Matches against the
/// persona's `id` field (kebab-case). Extend as the persona library
/// grows; the test suite guards against silent regressions.
pub const CRITIC_PERSONA_IDS: &[&str] = &[
    "code-critic",
    "reviewer",
    "code-reviewer",
    "pr-reviewer",
    "architect",
    "architecture-reviewer",
];

/// Tags (case-insensitive substring) that mark a persona as
/// critic-flavoured even when its id isn't on `CRITIC_PERSONA_IDS`.
pub const CRITIC_PERSONA_TAG_MARKERS: &[&str] = &["critic", "reviewer", "second-opinion"];

/// True iff this persona id or tag list indicates a critic-flavoured
/// role. Tags match case-insensitively against the markers.
pub fn is_critic_persona(persona_id: &str, tags: &[&str]) -> bool {
    if CRITIC_PERSONA_IDS.contains(&persona_id) {
        return true;
    }
    for tag in tags {
        let lower = tag.to_ascii_lowercase();
        if CRITIC_PERSONA_TAG_MARKERS
            .iter()
            .any(|m| lower.contains(*m))
        {
            return true;
        }
    }
    false
}

/// Apply the Phase 3.5 decision table: critic-flavoured + codex
/// available → Codex; everything else → Claude.
pub fn decide_vendor(persona_id: &str, tags: &[&str], probe: &CodexProbe) -> Vendor {
    if !is_critic_persona(persona_id, tags) {
        return Vendor::Claude;
    }
    match probe {
        CodexProbe::Available => Vendor::Codex,
        CodexProbe::Unavailable(_) => Vendor::Claude,
    }
}

/// User-visible PROJECT PLAN annotation for the "Codex critic:" line.
/// Wave 3 keeps the strings stable so skill tests can string-match.
pub fn plan_annotation(persona_id: &str, tags: &[&str], probe: &CodexProbe) -> &'static str {
    if !is_critic_persona(persona_id, tags) {
        return "not applicable";
    }
    match probe {
        CodexProbe::Available => "auto-enabled",
        CodexProbe::Unavailable(_) => "unavailable (codex CLI not installed / not authenticated)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critic_ids_recognised() {
        for id in CRITIC_PERSONA_IDS {
            assert!(
                is_critic_persona(id, &[]),
                "expected id {id} to be flagged critic"
            );
        }
    }

    #[test]
    fn tags_can_promote_non_critic_id() {
        assert!(is_critic_persona("helper", &["second-opinion"]));
        assert!(is_critic_persona("dev", &["Critic"]));
    }

    #[test]
    fn non_critic_persona_returns_claude_even_when_codex_available() {
        let v = decide_vendor("tech-helper", &["chat"], &CodexProbe::Available);
        assert_eq!(v, Vendor::Claude);
    }
}
