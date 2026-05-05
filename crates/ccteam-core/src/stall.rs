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

pub const STALL_WARN_SECONDS: u64 = 5 * 60;
pub const STALL_SUSPICIOUS_SECONDS: u64 = 15 * 60;
pub const STALL_ESCALATE_SECONDS: u64 = 30 * 60;

pub fn classify(silent_seconds: u64) -> StallLevel {
    if silent_seconds >= STALL_ESCALATE_SECONDS {
        StallLevel::Escalate
    } else if silent_seconds >= STALL_SUSPICIOUS_SECONDS {
        StallLevel::Suspicious
    } else if silent_seconds >= STALL_WARN_SECONDS {
        StallLevel::Warn
    } else {
        StallLevel::Ok
    }
}

/// How many seconds have passed since the project's most recent
/// progress event? Falls back to `now − created_at` when no events
/// have been observed yet, so the warn-clock starts ticking the moment
/// the project is created.
pub fn silent_seconds(state: &ProjectState, now: DateTime<Utc>) -> u64 {
    let baseline = state.last_progress_event_at.unwrap_or(state.created_at);
    now.signed_duration_since(baseline)
        .num_seconds()
        .max(0) as u64
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
}
