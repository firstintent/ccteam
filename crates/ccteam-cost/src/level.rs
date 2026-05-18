//! Cost-threshold classification (tech-design §6.8).
//!
//! Migrated from `ccteam-core::cost` in V0.6.0 Wave 1. The signature
//! switched from `classify(&ProjectState)` to
//! `classify(cost, soft_warn, hard_kill)` so this crate has zero
//! `ccteam-core` deps — the orchestrator's tick reads the three
//! primitives off `ProjectState` and forwards them.
//!
//! V0.4.6 F91 historically read the now-deprecated
//! `state.cost_used_usd`; F84 introduces a `WorkflowSpec::budget`-
//! driven replacement that consumes `cost_summary().cost_24h_usd`.
//! Either path can call `classify` — it's pure arithmetic.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostLevel {
    Ok,
    /// `> soft_warn` (default $20).
    SoftWarn,
    /// `>= COST_MID_WARN_USD` ($50).
    MidWarn,
    /// `> hard_kill` (default $200) — terminal: orchestrator kills
    /// the tmux session and escalates.
    HardKill,
}

pub const COST_MID_WARN_USD: f64 = 50.0;

/// Classify `cost` against the three buckets defined by `soft_warn` /
/// `COST_MID_WARN_USD` / `hard_kill`. Ordering matches the V0.4.6
/// implementation exactly (HardKill > MidWarn > SoftWarn > Ok).
pub fn classify(cost: f64, soft_warn: f64, hard_kill: f64) -> CostLevel {
    if cost > hard_kill {
        CostLevel::HardKill
    } else if cost >= COST_MID_WARN_USD {
        CostLevel::MidWarn
    } else if cost > soft_warn {
        CostLevel::SoftWarn
    } else {
        CostLevel::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_walks_thresholds() {
        // Mirrors V0.4.6 `ccteam-core::cost::tests::classify_walks_thresholds`
        // with soft_warn = $20 / hard_kill = $200 (the orchestrator's defaults).
        assert_eq!(classify(0.0, 20.0, 200.0), CostLevel::Ok);
        assert_eq!(classify(20.0, 20.0, 200.0), CostLevel::Ok); // not strictly greater
        assert_eq!(classify(20.01, 20.0, 200.0), CostLevel::SoftWarn);
        assert_eq!(classify(49.99, 20.0, 200.0), CostLevel::SoftWarn);
        assert_eq!(classify(50.0, 20.0, 200.0), CostLevel::MidWarn);
        assert_eq!(classify(199.99, 20.0, 200.0), CostLevel::MidWarn);
        assert_eq!(classify(200.0, 20.0, 200.0), CostLevel::MidWarn);
        assert_eq!(classify(200.01, 20.0, 200.0), CostLevel::HardKill);
    }

    #[test]
    fn classify_respects_custom_thresholds() {
        // A workflow-tuned $5 soft / $50 hard pair → $5.01 should
        // already be SoftWarn (not Ok), and $50.01 should HardKill.
        assert_eq!(classify(4.99, 5.0, 50.0), CostLevel::Ok);
        assert_eq!(classify(5.01, 5.0, 50.0), CostLevel::SoftWarn);
        assert_eq!(classify(49.99, 5.0, 50.0), CostLevel::SoftWarn);
        assert_eq!(classify(50.0, 5.0, 50.0), CostLevel::MidWarn);
        assert_eq!(classify(50.01, 5.0, 50.0), CostLevel::HardKill);
    }
}
