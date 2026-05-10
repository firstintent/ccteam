//! V0.3 M5.1 — read-only status badge.
//!
//! Reuses the V0.2.2 F35 `silence_classifier::classify` (a pure
//! deterministic match — no I/O, no LLM, no side effects) to map a
//! project's progress.jsonl tail + silent-second budget to a stable
//! presentation label. The web layer NEVER triggers the classifier's
//! associated `LimboAction` — only the orchestrator does that. Even
//! when the classification surfaces `PostStopLimbo` /
//! `SubagentRunaway`, the dashboard just colors the badge; it does
//! not send keys, escalate, or mutate state. (CLAUDE.md §三 read-only
//! red line.)

use ccteam_core::silence_classifier::SilenceClass;
use ccteam_core::stall::StallThresholds;
use ccteam_core::{classify_silence, ProjectState};
use serde_json::Value;

/// Presentation-only badge for the dashboard. Maps 1:1 onto the F35
/// `SilenceClass` taxonomy plus an `Unknown` bucket for the
/// (vanishingly rare) future where classifier returns an enum variant
/// the dashboard hasn't been re-built against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBadge {
    Healthy,
    Terminal,
    SubagentBusy,
    SubagentRunaway,
    MidToolHung,
    PostStopLimbo,
    InjectLimbo,
}

impl StatusBadge {
    /// User-facing label rendered inside the badge pill.
    pub fn label(self) -> &'static str {
        match self {
            StatusBadge::Healthy => "healthy",
            StatusBadge::Terminal => "terminal",
            StatusBadge::SubagentBusy => "subagent",
            StatusBadge::SubagentRunaway => "runaway",
            StatusBadge::MidToolHung => "tool-hung",
            StatusBadge::PostStopLimbo => "limbo",
            StatusBadge::InjectLimbo => "limbo",
        }
    }

    /// CSS class hooked by `style.css` (.badge.healthy / .terminal /
    /// .busy / .hung / .limbo / .runaway).
    pub fn css_class(self) -> &'static str {
        match self {
            StatusBadge::Healthy => "healthy",
            StatusBadge::Terminal => "terminal",
            StatusBadge::SubagentBusy => "busy",
            StatusBadge::SubagentRunaway => "runaway",
            StatusBadge::MidToolHung => "hung",
            StatusBadge::PostStopLimbo | StatusBadge::InjectLimbo => "limbo",
        }
    }
}

impl From<SilenceClass> for StatusBadge {
    fn from(c: SilenceClass) -> Self {
        match c {
            SilenceClass::Healthy => StatusBadge::Healthy,
            SilenceClass::Terminal => StatusBadge::Terminal,
            SilenceClass::SubagentBusy => StatusBadge::SubagentBusy,
            SilenceClass::SubagentRunaway => StatusBadge::SubagentRunaway,
            SilenceClass::MidToolHung(_) => StatusBadge::MidToolHung,
            SilenceClass::PostStopLimbo => StatusBadge::PostStopLimbo,
            SilenceClass::InjectLimbo => StatusBadge::InjectLimbo,
        }
    }
}

/// Compute a badge from the per-project read-side inputs the dashboard
/// already collects (state.json + tail of progress.jsonl + the
/// silent-seconds value derived in `ProjectSummary`). **Read-only** —
/// returns a `StatusBadge`, does not invoke `LimboAction::from` or
/// re-inject anything.
pub fn status_badge(_state: &ProjectState, recent: &[Value], silent_seconds: u64) -> StatusBadge {
    // M5.1 uses the global default thresholds. M5.4 may want a
    // per-phase override (the orchestrator does pass
    // `StallThresholds::from_phase(...)` based on phase yaml), but the
    // dashboard's "is this stuck?" signal is fine at the global default
    // — over-counting limbo on long-running phases is a presentation
    // imperfection, not a correctness bug, and labelling never
    // triggers a side effect.
    let class = classify_silence(recent, silent_seconds, &StallThresholds::default());
    StatusBadge::from(class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::ProjectState;
    use serde_json::json;

    fn fake_state() -> ProjectState {
        ProjectState::initial("dev-foo".to_string())
    }

    #[test]
    fn empty_log_is_healthy() {
        let badge = status_badge(&fake_state(), &[], 0);
        assert_eq!(badge, StatusBadge::Healthy);
        assert_eq!(badge.css_class(), "healthy");
        assert_eq!(badge.label(), "healthy");
    }

    #[test]
    fn phase_done_is_terminal() {
        let events = vec![json!({"event": "phase_done", "phase": "implement"})];
        let badge = status_badge(&fake_state(), &events, 999);
        assert_eq!(badge, StatusBadge::Terminal);
        assert_eq!(badge.css_class(), "terminal");
    }

    #[test]
    fn pretool_task_within_threshold_is_busy() {
        let events = vec![json!({"event": "PreToolUse", "tool": "Task"})];
        let badge = status_badge(&fake_state(), &events, 10 * 60);
        assert_eq!(badge, StatusBadge::SubagentBusy);
        assert_eq!(badge.css_class(), "busy");
    }

    #[test]
    fn stop_after_warn_is_limbo() {
        let events = vec![json!({"event": "Stop"})];
        let badge = status_badge(&fake_state(), &events, 10 * 60);
        assert_eq!(badge, StatusBadge::PostStopLimbo);
        assert_eq!(badge.css_class(), "limbo");
    }

    #[test]
    fn mid_tool_hung_maps_to_hung() {
        let events = vec![json!({"event": "PreToolUse", "tool": "Read"})];
        let badge = status_badge(&fake_state(), &events, 10 * 60);
        assert_eq!(badge, StatusBadge::MidToolHung);
        assert_eq!(badge.css_class(), "hung");
        assert_eq!(badge.label(), "tool-hung");
    }

    #[test]
    fn from_silence_class_covers_all_variants() {
        // Tripwire: if F35 grows a new variant, this match must be
        // updated. `match` on `SilenceClass` in `From` already
        // enforces exhaustiveness at compile time, but the asserts
        // also lock the css_class mapping for each.
        assert_eq!(
            StatusBadge::from(SilenceClass::Healthy).css_class(),
            "healthy"
        );
        assert_eq!(
            StatusBadge::from(SilenceClass::Terminal).css_class(),
            "terminal"
        );
        assert_eq!(
            StatusBadge::from(SilenceClass::SubagentBusy).css_class(),
            "busy"
        );
        assert_eq!(
            StatusBadge::from(SilenceClass::SubagentRunaway).css_class(),
            "runaway"
        );
        assert_eq!(
            StatusBadge::from(SilenceClass::MidToolHung("R".into())).css_class(),
            "hung"
        );
        assert_eq!(
            StatusBadge::from(SilenceClass::PostStopLimbo).css_class(),
            "limbo"
        );
        assert_eq!(
            StatusBadge::from(SilenceClass::InjectLimbo).css_class(),
            "limbo"
        );
    }
}
