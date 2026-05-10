//! Cost-threshold classification (tech-design §6.8). Hook side
//! computes total cost (`crates/ccteam-hooks/src/cost.rs`); this
//! module only classifies a known total against three buckets so the
//! orchestrator's tick can act.

use crate::state::ProjectState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostLevel {
    Ok,
    /// `> state.soft_warn_threshold_usd` (default $20).
    SoftWarn,
    /// `>= COST_MID_WARN_USD` ($50).
    MidWarn,
    /// `> state.hard_kill_threshold_usd` (default $200) — terminal:
    /// orchestrator kills the tmux session and escalates.
    HardKill,
}

pub const COST_MID_WARN_USD: f64 = 50.0;

pub fn classify(state: &ProjectState) -> CostLevel {
    let c = state.cost_used_usd;
    if c > state.hard_kill_threshold_usd {
        CostLevel::HardKill
    } else if c >= COST_MID_WARN_USD {
        CostLevel::MidWarn
    } else if c > state.soft_warn_threshold_usd {
        CostLevel::SoftWarn
    } else {
        CostLevel::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Parallelism, PhaseState};
    use crate::team::TeamKind;
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn st(cost: f64) -> ProjectState {
        let now = Utc::now();
        ProjectState {
            slug: "demo".into(),
            team: "dev".into(),
            team_kind: TeamKind::Workflow,
            created_at: now,
            tmux_session: "ccteam-demo".into(),
            claude_session_id: None,
            claude_pid: None,
            phase_state: PhaseState::Idle,
            current_phase: String::new(),
            parallelism: Parallelism::Solo,
            phase_history: Vec::new(),
            auto_loop_cycle_count: 0,
            cost_used_usd: cost,
            soft_warn_threshold_usd: 20.0,
            hard_kill_threshold_usd: 200.0,
            context_tokens_used: 0,
            context_reset_threshold_tokens: 600_000,
            context_reset_count: 0,
            last_progress_event_at: None,
            last_event_type: None,
            last_user_interaction_at: now,
            user_attached: false,
            user_pause_pending: false,
            sessions: BTreeMap::new(),
            next_sid_seq: BTreeMap::new(),
        }
    }

    #[test]
    fn classify_walks_thresholds() {
        assert_eq!(classify(&st(0.0)), CostLevel::Ok);
        assert_eq!(classify(&st(20.0)), CostLevel::Ok); // not strictly greater
        assert_eq!(classify(&st(20.01)), CostLevel::SoftWarn);
        assert_eq!(classify(&st(49.99)), CostLevel::SoftWarn);
        assert_eq!(classify(&st(50.0)), CostLevel::MidWarn);
        assert_eq!(classify(&st(199.99)), CostLevel::MidWarn);
        assert_eq!(classify(&st(200.0)), CostLevel::MidWarn);
        assert_eq!(classify(&st(200.01)), CostLevel::HardKill);
    }
}
