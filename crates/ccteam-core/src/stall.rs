//! Stall detection (tech-design §6.8). Classifies "no progress event
//! observed in N minutes" into three soft-warn buckets. M0 logs via
//! tracing; M1 hooks telegram per the design's escalate path.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::progress;
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

/// Read-side status for the core chat loop, sourced from the last
/// `progress.jsonl` event plus its age. Unlike the legacy phase-level
/// [`classify`] helper, this never promotes pure age to STUCK: a stuck verdict
/// requires a file-backed event written by the live watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressStallStatus {
    pub level: &'static str,
    pub verdict: &'static str,
    pub activity: &'static str,
}

/// Read-side activity for one chat session after selecting its relevant
/// progress event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressActivityStatus {
    pub status: ProgressStallStatus,
    /// Raw age of the selected event, when the event carries a timestamp.
    pub event_age_seconds: Option<u64>,
    /// Age suitable for user-facing session status surfaces. Active working
    /// turns hide the ever-growing prompt age; stale/stuck/idle still show it.
    pub last_activity_seconds: Option<u64>,
}

/// Classify chat-loop activity from the file-backed progress truth.
///
/// - `chat_turn_timeout` / `stuck:true` is the only STUCK source.
/// - long silence without that event is only `warn`, because the live daemon
///   might be down and CLI/web cannot see the watchdog's process memory.
/// - non-idle recent events render as `working`; non-idle events older than
///   the warn threshold render as `stale`; idle boundaries render `idle`.
pub fn classify_progress_stall(
    last_event: Option<&Value>,
    silent_seconds: u64,
) -> ProgressStallStatus {
    let stuck = last_event.is_some_and(|event| {
        event.get("event").and_then(Value::as_str) == Some(progress::CHAT_TURN_TIMEOUT)
            || event.get("stuck").and_then(Value::as_bool) == Some(true)
    });
    if stuck {
        return ProgressStallStatus {
            level: "stuck",
            verdict: "STUCK",
            activity: "stuck",
        };
    }
    let level = if silent_seconds >= STALL_WARN_SECONDS {
        "warn"
    } else {
        "ok"
    };
    let idle = progress::is_idle(last_event);
    let activity = if idle {
        "idle"
    } else if level == "warn" {
        "stale"
    } else {
        "working"
    };
    ProgressStallStatus {
        level,
        verdict: if level == "warn" { "warn" } else { "OK" },
        activity,
    }
}

/// A turn the DAEMON knows is in flight right now for one session — its own
/// pending state, cheap to read (no I/O) and available only to a caller that
/// holds the live session map.
///
/// This is not a second source of truth: `progress.jsonl` stays the state SoT,
/// and this only says "the writer has an open turn it has not closed yet".
/// It matters because the file can go quiet on a reader for reasons that have
/// nothing to do with the session — a torn line, a rotated stream, an
/// unreadable path — and every one of those reads as `idle`, which is a lie
/// about a session that is mid-turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveTurn {
    /// Seconds since the in-flight turn's most recent event (clamped to the
    /// turn's own start, so a freshly submitted turn is never born silent).
    pub silent_seconds: u64,
    /// Seconds since the turn was submitted — display only (`working 3m`).
    pub elapsed_seconds: u64,
    /// The watchdog's idle window: silence at or past it is the same STUCK the
    /// turn-timeout watchdog would flag.
    pub stuck_after_seconds: u64,
}

impl LiveTurn {
    /// Silent for a full watchdog window ⇒ the watchdog's own STUCK verdict.
    pub fn is_stuck(&self) -> bool {
        self.silent_seconds >= self.stuck_after_seconds
    }
}

/// **The one session-activity resolver.** Every live surface — IM `/status`,
/// MCP `session_list` / `session_collect`, the web session list feeding the
/// SPA rail — answers "what is this session doing" through here, so the two
/// user-facing ends cannot disagree by construction. Surfaces with no live
/// view (`ccteam status` and other daemonless readers) pass `live: None` and
/// get the pure file verdict.
///
/// The fold is MONOTONE — an in-flight turn can only report MORE work than the
/// file did, never silence work the file reported:
/// - file says STUCK (the watchdog wrote it) ⇒ stays STUCK.
/// - a turn is in flight ⇒ `working`, or `stuck` once it has been silent for a
///   full watchdog window (the same definition the watchdog applies).
/// - no turn in flight ⇒ the file verdict stands; the daemon having no open
///   turn is itself evidence the session is not mid-turn.
pub fn classify_session_activity(
    events: &[Value],
    sid: &str,
    fallback_silent_seconds: u64,
    live: Option<LiveTurn>,
    now: DateTime<Utc>,
) -> ProgressActivityStatus {
    let base = classify_progress_activity_for_sid(events, sid, fallback_silent_seconds, now);
    let Some(live) = live else {
        return base;
    };
    if base.status.activity == "stuck" {
        return base;
    }
    if live.is_stuck() {
        return ProgressActivityStatus {
            status: ProgressStallStatus {
                level: "stuck",
                verdict: "STUCK",
                activity: "stuck",
            },
            event_age_seconds: base.event_age_seconds,
            last_activity_seconds: Some(live.silent_seconds),
        };
    }
    ProgressActivityStatus {
        status: ProgressStallStatus {
            level: "ok",
            verdict: "OK",
            activity: "working",
        },
        event_age_seconds: base.event_age_seconds,
        // An active turn hides the ever-growing age, same as the file path.
        last_activity_seconds: None,
    }
}

/// Select and classify the read-side activity for one gateway session id.
///
/// The project-level fallback is only the file's global tail event when that
/// tail has no `sid`/`session_id`, matching the old `last_event` fallback
/// without letting one session's tail event bleed into its siblings.
///
/// File-only: a caller that can see the daemon's in-flight turns wants
/// [`classify_session_activity`] instead.
pub fn classify_progress_activity_for_sid(
    events: &[Value],
    sid: &str,
    fallback_silent_seconds: u64,
    now: DateTime<Utc>,
) -> ProgressActivityStatus {
    let project_fallback = events
        .last()
        .filter(|event| progress::event_sid(event).is_none());
    let sid_last = if sid.is_empty() {
        None
    } else {
        events
            .iter()
            .rev()
            .find(|event| progress::event_sid(event) == Some(sid))
    };
    classify_progress_activity(sid_last.or(project_fallback), fallback_silent_seconds, now)
}

/// Classify an already-selected progress event and compute age visibility.
pub fn classify_progress_activity(
    selected: Option<&Value>,
    fallback_silent_seconds: u64,
    now: DateTime<Utc>,
) -> ProgressActivityStatus {
    let event_age_seconds = selected.and_then(|event| progress_event_age_seconds(event, now));
    let age_for_classification = event_age_seconds.unwrap_or(fallback_silent_seconds);
    let status = classify_progress_stall(selected, age_for_classification);
    let last_activity_seconds = if status.activity == "working" {
        None
    } else {
        event_age_seconds
    };
    ProgressActivityStatus {
        status,
        event_age_seconds,
        last_activity_seconds,
    }
}

pub fn progress_event_age_seconds(event: &Value, now: DateTime<Utc>) -> Option<u64> {
    let ts = event.get("ts").and_then(Value::as_str)?;
    let ts = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    Some(now.signed_duration_since(ts).num_seconds().max(0) as u64)
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
    fn progress_stall_requires_file_backed_timeout_for_stuck() {
        let idle = serde_json::json!({"event": progress::CHAT_TURN_COMPLETED});
        let status = classify_progress_stall(Some(&idle), STALL_SUSPICIOUS_SECONDS);
        assert_eq!(status.level, "warn");
        assert_eq!(status.verdict, "warn");
        assert_eq!(status.activity, "idle");
    }

    #[test]
    fn progress_stall_marks_timeout_event_stuck_even_when_recent() {
        let timeout = serde_json::json!({
            "event": progress::CHAT_TURN_TIMEOUT,
            "stuck": true,
        });
        let status = classify_progress_stall(Some(&timeout), 1);
        assert_eq!(status.level, "stuck");
        assert_eq!(status.verdict, "STUCK");
        assert_eq!(status.activity, "stuck");
    }

    #[test]
    fn progress_stall_marks_non_idle_event_working() {
        let prompt = serde_json::json!({"event": progress::CHAT_TURN_USER_PROMPT});
        let status = classify_progress_stall(Some(&prompt), 10);
        assert_eq!(status.level, "ok");
        assert_eq!(status.activity, "working");
    }

    #[test]
    fn progress_stall_marks_old_non_idle_event_stale() {
        let prompt = serde_json::json!({"event": progress::CHAT_TURN_USER_PROMPT});
        let status = classify_progress_stall(Some(&prompt), STALL_WARN_SECONDS);
        assert_eq!(status.level, "warn");
        assert_eq!(status.verdict, "warn");
        assert_eq!(status.activity, "stale");
    }

    fn live(silent_seconds: u64) -> LiveTurn {
        LiveTurn {
            silent_seconds,
            elapsed_seconds: silent_seconds + 1,
            stuck_after_seconds: 300,
        }
    }

    /// The bug this resolver exists for: an in-flight turn must outrank a file
    /// that says nothing about the session (unreadable stream, rotated log, no
    /// event for this sid yet). Without the fold, "no evidence" minted a
    /// confident `idle` — a session hard at work reported green on every
    /// surface.
    #[test]
    fn live_turn_outranks_an_uninformative_stream() {
        let now = Utc::now();
        assert_eq!(
            classify_session_activity(&[], "s1", 0, None, now)
                .status
                .activity,
            "idle",
            "no live view (daemonless reader) keeps the file verdict"
        );
        let working = classify_session_activity(&[], "s1", 0, Some(live(3)), now);
        assert_eq!(working.status.activity, "working");
        // An active turn hides the ever-growing age, same as the file path.
        assert_eq!(working.last_activity_seconds, None);
    }

    /// The fold is monotone — it only ever reports MORE work than the file did.
    #[test]
    fn live_turn_never_silences_the_file_verdict() {
        let now = Utc::now();
        // A completed turn 20 min ago + an in-flight turn = a NEW turn started
        // after that boundary; the daemon's open turn wins.
        let idle_tail = vec![serde_json::json!({
            "event": progress::CHAT_TURN_COMPLETED,
            "sid": "s1",
            "ts": (now - chrono::Duration::minutes(20)).to_rfc3339(),
        })];
        assert_eq!(
            classify_session_activity(&idle_tail, "s1", 0, Some(live(5)), now)
                .status
                .activity,
            "working"
        );
        // The watchdog's own STUCK verdict is file-backed and never downgraded.
        let stuck_tail = vec![serde_json::json!({
            "event": progress::CHAT_TURN_TIMEOUT,
            "sid": "s1",
            "stuck": true,
            "ts": now.to_rfc3339(),
        })];
        assert_eq!(
            classify_session_activity(&stuck_tail, "s1", 0, Some(live(1)), now)
                .status
                .activity,
            "stuck"
        );
    }

    /// Silence past the watchdog window is the watchdog's definition of stuck,
    /// so the shared resolver reaches the same verdict `/status` shows.
    #[test]
    fn live_turn_silent_a_full_window_reads_stuck() {
        let now = Utc::now();
        let status = classify_session_activity(&[], "s1", 0, Some(live(300)), now);
        assert_eq!(status.status.activity, "stuck");
        assert_eq!(status.status.verdict, "STUCK");
        assert_eq!(status.last_activity_seconds, Some(300));
        assert!(!live(299).is_stuck());
        assert!(live(300).is_stuck());
    }

    #[test]
    fn progress_activity_selects_sid_and_hides_only_working_age() {
        let now = Utc::now();
        let events = vec![
            serde_json::json!({
                "event": progress::CHAT_TURN_COMPLETED,
                "sid": "s1",
                "ts": (now - chrono::Duration::minutes(10)).to_rfc3339(),
            }),
            serde_json::json!({
                "event": progress::CHAT_TURN_USER_PROMPT,
                "sid": "s2",
                "ts": (now - chrono::Duration::seconds(30)).to_rfc3339(),
            }),
        ];

        let idle = classify_progress_activity_for_sid(&events, "s1", 0, now);
        assert_eq!(idle.status.activity, "idle");
        assert!(idle.last_activity_seconds.is_some());

        let working = classify_progress_activity_for_sid(&events, "s2", 0, now);
        assert_eq!(working.status.activity, "working");
        assert!(working.event_age_seconds.is_some());
        assert_eq!(working.last_activity_seconds, None);
    }

    /// The heartbeat's whole purpose: a turn that has legitimately run past the
    /// warn window still reads `working`, because the pump keeps refreshing the
    /// session's newest row. Without one, the same turn's opening row ages out
    /// and a busy child is reported to its parent as `stale`.
    #[test]
    fn heartbeat_keeps_a_long_turn_working_past_the_warn_window() {
        let now = Utc::now();
        let prompt = serde_json::json!({
            "event": progress::CHAT_TURN_USER_PROMPT,
            "sid": "s3",
            "ts": (now - chrono::Duration::minutes(20)).to_rfc3339(),
        });

        let without_heartbeat =
            classify_progress_activity_for_sid(std::slice::from_ref(&prompt), "s3", 0, now);
        assert_eq!(without_heartbeat.status.activity, "stale");

        let events = vec![
            prompt,
            serde_json::json!({
                "event": progress::CHAT_TURN_RUNNING_LONG,
                "sid": "s3",
                "turn_id": "t1",
                "elapsed_sec": 1200,
                "ts": (now - chrono::Duration::seconds(30)).to_rfc3339(),
            }),
        ];
        let with_heartbeat = classify_progress_activity_for_sid(&events, "s3", 0, now);
        assert_eq!(with_heartbeat.status.activity, "working");
        assert_eq!(with_heartbeat.status.verdict, "OK");
        // A working turn hides its ever-growing age from status surfaces.
        assert_eq!(with_heartbeat.last_activity_seconds, None);
    }

    /// A stuck flag is a verdict about ONE moment of silence, not a permanent
    /// label: the next heartbeat becomes the newest row and the session reads
    /// `working` again. (The classifier only ever looks at the latest sid row,
    /// so this holds by construction — pin it so it stays that way.)
    #[test]
    fn heartbeat_after_a_timeout_flag_returns_to_working() {
        let now = Utc::now();
        let events = vec![
            serde_json::json!({
                "event": progress::CHAT_TURN_TIMEOUT,
                "sid": "s3",
                "stuck": true,
                "ts": (now - chrono::Duration::minutes(6)).to_rfc3339(),
            }),
            serde_json::json!({
                "event": progress::CHAT_TURN_RUNNING_LONG,
                "sid": "s3",
                "ts": (now - chrono::Duration::seconds(10)).to_rfc3339(),
            }),
        ];
        assert_eq!(
            classify_progress_activity_for_sid(&events, "s3", 0, now)
                .status
                .activity,
            "working"
        );
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
