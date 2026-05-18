//! V0.6.0 Wave 3 F112 §C — quota fallback decision tests.
//!
//! Drives `ccteam_core::preferences::quota_fallback_decision`
//! directly. The orchestrator hook in `try_spawn_with_prompt` is a
//! thin wrapper over this helper + an `adapter_for(Executor::Codex)`
//! liveness check; testing the pure decision here keeps the matrix
//! readable without bringing up the full orchestrator + adapter map.

use ccteam_core::preferences::{
    quota_fallback_decision, OnClaudeQuota, Preferences, QuotaFallbackDecision,
};

fn prefs_on() -> Preferences {
    let mut p = Preferences::default();
    p.fallback.on_claude_quota = OnClaudeQuota::Codex;
    p
}

fn prefs_off() -> Preferences {
    Preferences::default()
}

#[test]
fn quota_not_tripped_returns_proceed_regardless_of_prefs() {
    assert_eq!(
        quota_fallback_decision(false, true, "main", &prefs_on()),
        QuotaFallbackDecision::Proceed
    );
    assert_eq!(
        quota_fallback_decision(false, false, "main", &prefs_off()),
        QuotaFallbackDecision::Proceed
    );
}

#[test]
fn quota_tripped_with_prefs_on_swaps_to_codex() {
    let p = prefs_on();
    assert_eq!(
        quota_fallback_decision(true, true, "main", &p),
        QuotaFallbackDecision::SwapToCodex
    );
    assert_eq!(
        quota_fallback_decision(true, true, "any-role", &p),
        QuotaFallbackDecision::SwapToCodex
    );
}

#[test]
fn quota_tripped_with_prefs_off_hard_stops() {
    assert_eq!(
        quota_fallback_decision(true, true, "main", &prefs_off()),
        QuotaFallbackDecision::HardStop
    );
}

#[test]
fn quota_tripped_on_codex_executor_hard_stops_even_with_prefs_on() {
    // Even when fallback is enabled, a budget trip on a Codex
    // executor has no Claude→Codex swap available, so it falls
    // through to hard-stop.
    assert_eq!(
        quota_fallback_decision(true, false, "main", &prefs_on()),
        QuotaFallbackDecision::HardStop
    );
}

#[test]
fn quota_tripped_with_role_eligibility_list_filters_swap() {
    let mut p = prefs_on();
    p.fallback.codex.enabled_for_roles = vec!["critic".into()];

    // Listed role → swap.
    assert_eq!(
        quota_fallback_decision(true, true, "critic", &p),
        QuotaFallbackDecision::SwapToCodex
    );
    // Unlisted role → hard-stop (user explicitly restricted scope).
    assert_eq!(
        quota_fallback_decision(true, true, "main", &p),
        QuotaFallbackDecision::HardStop
    );
}
