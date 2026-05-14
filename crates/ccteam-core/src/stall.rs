//! Stall detection (tech-design §6.8). Classifies "no progress event
//! observed in N minutes" into three soft-warn buckets. M0 logs via
//! tracing; M1 hooks telegram per the design's escalate path.

use chrono::{DateTime, Utc};

use crate::state::ProjectState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallLevel {
    Ok,
    /// `≥ 5 min` since the last progress event.
    Warn,
    /// `≥ 15 min`.
    Suspicious,
    /// `≥ 30 min` — escalation territory; M1 telegram pings the user.
    Escalate,
}

impl StallLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            StallLevel::Ok => "ok",
            StallLevel::Warn => "warn",
            StallLevel::Suspicious => "suspicious",
            StallLevel::Escalate => "escalate",
        }
    }
}

/// Default thresholds when a phase template doesn't declare its own
/// `stall_warn_minutes`. Tech-design §6.8 / interfaces.md §5.1.
pub const STALL_WARN_SECONDS: u64 = 5 * 60;
pub const STALL_SUSPICIOUS_SECONDS: u64 = 15 * 60;
pub const STALL_ESCALATE_SECONDS: u64 = 30 * 60;

/// Per-phase stall thresholds. Three buckets — warn / suspicious /
/// escalate — derived from `phase.stall_warn_minutes` so phases that
/// legitimately wait longer (e.g. research's `04-primary` phase that
/// blocks on user-supplied data) don't fire warnings every 5 minutes.
///
/// Bucket multipliers are 1× / 3× / 6× the warn threshold, matching the
/// default 5/15/30 ratio. So `stall_warn_minutes: 60` → 60/180/360 min.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallThresholds {
    pub warn_seconds: u64,
    pub suspicious_seconds: u64,
    pub escalate_seconds: u64,
}

impl Default for StallThresholds {
    fn default() -> Self {
        Self {
            warn_seconds: STALL_WARN_SECONDS,
            suspicious_seconds: STALL_SUSPICIOUS_SECONDS,
            escalate_seconds: STALL_ESCALATE_SECONDS,
        }
    }
}

impl StallThresholds {
    /// Build thresholds from the optional `stall_warn_minutes` field
    /// on a phase template. `None` → defaults; `Some(n)` → (n, 3n, 6n)
    /// minutes.
    pub fn from_phase(stall_warn_minutes: Option<u64>) -> Self {
        match stall_warn_minutes {
            Some(n) => Self {
                warn_seconds: n * 60,
                suspicious_seconds: n * 60 * 3,
                escalate_seconds: n * 60 * 6,
            },
            None => Self::default(),
        }
    }

    pub fn warn_minutes(&self) -> u64 {
        self.warn_seconds / 60
    }
    pub fn suspicious_minutes(&self) -> u64 {
        self.suspicious_seconds / 60
    }
    pub fn escalate_minutes(&self) -> u64 {
        self.escalate_seconds / 60
    }
}

/// Classify silence against phase-specific thresholds (preferred path).
pub fn classify_with_thresholds(silent_seconds: u64, t: &StallThresholds) -> StallLevel {
    if silent_seconds >= t.escalate_seconds {
        StallLevel::Escalate
    } else if silent_seconds >= t.suspicious_seconds {
        StallLevel::Suspicious
    } else if silent_seconds >= t.warn_seconds {
        StallLevel::Warn
    } else {
        StallLevel::Ok
    }
}

/// Classify silence against the default 5/15/30 thresholds. Equivalent
/// to `classify_with_thresholds(s, &StallThresholds::default())`.
/// Existing callers that don't have a phase context fall back here.
pub fn classify(silent_seconds: u64) -> StallLevel {
    classify_with_thresholds(silent_seconds, &StallThresholds::default())
}

/// How many seconds have passed since the project's most recent
/// progress event? Falls back to `now − created_at` when no events
/// have been observed yet, so the warn-clock starts ticking the moment
/// the project is created.
pub fn silent_seconds(state: &ProjectState, now: DateTime<Utc>) -> u64 {
    let baseline = state.last_progress_event_at.unwrap_or(state.created_at);
    now.signed_duration_since(baseline).num_seconds().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_walks_thresholds() {
        assert_eq!(classify(0), StallLevel::Ok);
        assert_eq!(classify(STALL_WARN_SECONDS - 1), StallLevel::Ok);
        assert_eq!(classify(STALL_WARN_SECONDS), StallLevel::Warn);
        assert_eq!(classify(STALL_SUSPICIOUS_SECONDS - 1), StallLevel::Warn);
        assert_eq!(classify(STALL_SUSPICIOUS_SECONDS), StallLevel::Suspicious);
        assert_eq!(classify(STALL_ESCALATE_SECONDS - 1), StallLevel::Suspicious);
        assert_eq!(classify(STALL_ESCALATE_SECONDS), StallLevel::Escalate);
        assert_eq!(classify(60 * 60), StallLevel::Escalate);
    }

    #[test]
    fn from_phase_none_uses_defaults() {
        let t = StallThresholds::from_phase(None);
        assert_eq!(t, StallThresholds::default());
        assert_eq!(t.warn_seconds, STALL_WARN_SECONDS);
        assert_eq!(t.suspicious_seconds, STALL_SUSPICIOUS_SECONDS);
        assert_eq!(t.escalate_seconds, STALL_ESCALATE_SECONDS);
    }

    #[test]
    fn from_phase_some_scales_1_3_6() {
        // Research's 04-primary needs ~60-min warn; expect 60/180/360.
        let t = StallThresholds::from_phase(Some(60));
        assert_eq!(t.warn_seconds, 60 * 60);
        assert_eq!(t.suspicious_seconds, 60 * 60 * 3);
        assert_eq!(t.escalate_seconds, 60 * 60 * 6);
    }

    #[test]
    fn classify_with_phase_thresholds_respects_60min_baseline() {
        // 04-primary: stall_warn_minutes: 60. 5 minutes of silence is
        // perfectly normal; the old hardcoded 5-min threshold would
        // misfire here.
        let t = StallThresholds::from_phase(Some(60));
        assert_eq!(classify_with_thresholds(5 * 60, &t), StallLevel::Ok);
        assert_eq!(classify_with_thresholds(59 * 60, &t), StallLevel::Ok);
        assert_eq!(classify_with_thresholds(60 * 60, &t), StallLevel::Warn);
        assert_eq!(
            classify_with_thresholds(180 * 60, &t),
            StallLevel::Suspicious
        );
        assert_eq!(classify_with_thresholds(360 * 60, &t), StallLevel::Escalate);
    }

    #[test]
    fn classify_with_default_matches_classify() {
        // Backwards compat: classify() must equal classify_with_thresholds()
        // when called with the default thresholds.
        for s in [0u64, 60, 5 * 60, 14 * 60, 15 * 60, 30 * 60, 60 * 60] {
            assert_eq!(
                classify(s),
                classify_with_thresholds(s, &StallThresholds::default())
            );
        }
    }
}
