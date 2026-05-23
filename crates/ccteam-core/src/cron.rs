//! V0.6.3 F142 — `trigger: schedule` cron evaluation.
//!
//! Closes the V0.4.6 stub: `Trigger::Schedule` agents declare a
//! standard 5-field cron expression in `workflow.yaml`
//! (`AgentSpec::schedule`); the orchestrator tick evaluates which
//! schedule agents are due and spawns them through the normal path.
//!
//! ## Design (PRD §1)
//!
//! - **Standard 5-field cron only** — minute / hour / day-of-month /
//!   month / day-of-week. [`croner`] itself accepts 5–7 fields; we
//!   pre-validate the field count so workflow authors get a crisp
//!   error instead of an accidentally-parsed 6-field (seconds) spec.
//! - **skip-missed semantics** — if the daemon was down across one or
//!   more scheduled times we do **not** backfill. [`Schedule::is_due`]
//!   only reports "fire now" when the next occurrence after the
//!   persisted `last_fire` is `<= now`; the caller then advances
//!   `last_fire` to `now`, so the *next* due time is recomputed from
//!   the present and the missed slots are silently dropped. No
//!   restart storm.
//! - **`progress.jsonl` is the SoT** — this module is pure data +
//!   time arithmetic. It never writes `progress.jsonl`, never spawns;
//!   the orchestrator owns those side effects.
//! - **No team-name literals** — schedules are keyed by `role`
//!   (user-defined workflow data) only.

use chrono::{DateTime, Utc};
use croner::Cron;
use std::str::FromStr;

/// A parsed, validated 5-field cron schedule.
#[derive(Debug, Clone)]
pub struct Schedule {
    cron: Cron,
}

/// Errors surfaced while parsing an `AgentSpec::schedule` string.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// The expression did not have exactly 5 whitespace-separated
    /// fields. We reject 6/7-field forms explicitly so a workflow
    /// author who meant minute-granularity doesn't silently get a
    /// seconds field bound to the wrong position.
    #[error(
        "cron schedule must have exactly 5 fields \
         (minute hour day-of-month month day-of-week), got {0}"
    )]
    FieldCount(usize),
    /// `croner` rejected the expression (bad range, unknown token,
    /// out-of-bounds value, etc.).
    #[error("invalid cron schedule: {0}")]
    Parse(String),
}

impl Schedule {
    /// Parse a standard 5-field cron expression.
    ///
    /// Rejects anything that is not exactly 5 fields *before* handing
    /// the string to [`croner`] (which would otherwise accept 5–7).
    pub fn parse(expr: &str) -> Result<Self, ScheduleError> {
        let field_count = expr.split_whitespace().count();
        if field_count != 5 {
            return Err(ScheduleError::FieldCount(field_count));
        }
        let cron = Cron::from_str(expr).map_err(|e| ScheduleError::Parse(e.to_string()))?;
        Ok(Self { cron })
    }

    /// The first scheduled occurrence strictly after `after`.
    ///
    /// Returns `None` only when `croner` cannot find any future match
    /// (effectively never for well-formed expressions; surfaced
    /// defensively so callers can skip rather than panic).
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.cron.find_next_occurrence(&after, false).ok()
    }

    /// **skip-missed due check.** Returns `true` when this schedule
    /// should fire at `now`, given the last time it fired
    /// (`last_fire`, `None` = never fired).
    ///
    /// Semantics:
    /// - **never fired** (`None`) → do **not** fire on this tick. The
    ///   orchestrator records `last_fire = now` the first time it sees
    ///   a schedule agent so the next occurrence is computed forward
    ///   from daemon start (no fire-on-boot surprise).
    /// - **fired before** → fire iff `next_after(last_fire) <= now`.
    ///
    /// This is the single skip-missed invariant: because the caller
    /// advances `last_fire` to `now` (not to the missed slot) after a
    /// fire, a daemon that was down for hours fires **once** on
    /// restart-after-due, then resumes the normal cadence.
    pub fn is_due(&self, last_fire: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        match last_fire {
            Some(last) => match self.next_after(last) {
                Some(next) => next <= now,
                None => false,
            },
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn parses_standard_five_field() {
        assert!(Schedule::parse("*/5 * * * *").is_ok());
        assert!(Schedule::parse("0 3 * * *").is_ok());
        assert!(Schedule::parse("30 9 * * 1-5").is_ok());
    }

    #[test]
    fn rejects_six_field_seconds_form() {
        // croner would happily parse this as a 6-field (seconds) spec;
        // we reject it so workflow authors get a crisp error.
        let err = Schedule::parse("0 */5 * * * *").unwrap_err();
        assert_eq!(err, ScheduleError::FieldCount(6));
    }

    #[test]
    fn rejects_too_few_fields() {
        let err = Schedule::parse("*/5 * *").unwrap_err();
        assert_eq!(err, ScheduleError::FieldCount(3));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            Schedule::parse("not a cron at all"),
            Err(ScheduleError::FieldCount(_)) | Err(ScheduleError::Parse(_))
        ));
        assert!(matches!(
            Schedule::parse("99 * * * *"),
            Err(ScheduleError::Parse(_))
        ));
    }

    #[test]
    fn next_after_advances() {
        let sched = Schedule::parse("*/5 * * * *").unwrap();
        // 12:02 → next */5 slot is 12:05.
        let next = sched.next_after(ts("2026-05-22T12:02:00Z")).unwrap();
        assert_eq!(next, ts("2026-05-22T12:05:00Z"));
    }

    #[test]
    fn never_fired_does_not_backfire() {
        // Cold start: last_fire = None must NOT fire on the first
        // tick (the orchestrator seeds last_fire = now instead).
        let sched = Schedule::parse("*/5 * * * *").unwrap();
        assert!(!sched.is_due(None, ts("2026-05-22T12:07:00Z")));
    }

    #[test]
    fn due_when_next_slot_reached() {
        let sched = Schedule::parse("*/5 * * * *").unwrap();
        let last = ts("2026-05-22T12:00:00Z");
        // 12:04 — next slot after 12:00 is 12:05, not reached yet.
        assert!(!sched.is_due(Some(last), ts("2026-05-22T12:04:00Z")));
        // 12:05 — exactly the slot, due.
        assert!(sched.is_due(Some(last), ts("2026-05-22T12:05:00Z")));
        // 12:06 — slot passed, still due (caught up on next tick).
        assert!(sched.is_due(Some(last), ts("2026-05-22T12:06:00Z")));
    }

    #[test]
    fn skip_missed_no_backfill() {
        // Daemon fired at 12:00, then was DOWN until 14:00. A naive
        // backfill would owe 24 firings (12:05 .. 13:55). skip-missed
        // semantics: is_due is true *once* at 14:00, the caller then
        // advances last_fire to 14:00 — so the very next check uses
        // 14:00 as the anchor and the missed 23 slots are dropped.
        let sched = Schedule::parse("*/5 * * * *").unwrap();
        let fired_at = ts("2026-05-22T12:00:00Z");
        let now = ts("2026-05-22T14:00:00Z");
        assert!(sched.is_due(Some(fired_at), now), "due once on restart");

        // Caller advances last_fire := now. Next tick 14:01 must NOT
        // re-fire (next slot after 14:00 is 14:05).
        assert!(!sched.is_due(Some(now), ts("2026-05-22T14:01:00Z")));
        // ...but 14:05 fires normally — cadence resumed, no storm.
        assert!(sched.is_due(Some(now), ts("2026-05-22T14:05:00Z")));
    }

    #[test]
    fn daily_schedule_skip_missed() {
        // `0 3 * * *` — 03:00 daily. Fired yesterday 03:00, daemon
        // restarts today 09:00: due once (today's 03:00 was missed),
        // then advancing last_fire to 09:00 means tomorrow 03:00 is
        // the next fire, NOT a catch-up of today's.
        let sched = Schedule::parse("0 3 * * *").unwrap();
        let yesterday_3am = Utc.with_ymd_and_hms(2026, 5, 21, 3, 0, 0).unwrap();
        let today_9am = Utc.with_ymd_and_hms(2026, 5, 22, 9, 0, 0).unwrap();
        assert!(sched.is_due(Some(yesterday_3am), today_9am));

        // advance last_fire := today 09:00.
        let today_10am = Utc.with_ymd_and_hms(2026, 5, 22, 10, 0, 0).unwrap();
        assert!(!sched.is_due(Some(today_9am), today_10am));
        let tomorrow_3am = Utc.with_ymd_and_hms(2026, 5, 23, 3, 0, 0).unwrap();
        assert!(sched.is_due(Some(today_9am), tomorrow_3am));
    }
}
