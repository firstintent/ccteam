//! Account usage from the rate-limit state claude already receives (#244).
//!
//! Every API response claude gets carries the account's unified rate-limit
//! headers, and claude reports their changes on its stream-json output as
//! `rate_limit_event` lines. Their `rate_limit_info` (claude 2.1.325):
//!
//! ```text
//! {status, rateLimitType, utilization?, resetsAt?,
//!  unifiedWindows?: {five_hour: {utilization, resetsAt, status}, seven_day: {…}, …}}
//! ```
//!
//! `utilization` is a 0–1 fraction and `resetsAt` epoch seconds. A line
//! without `unifiedWindows` speaks for the one window named by
//! `rateLimitType`.
//!
//! Reading usage from these costs nothing. The other way to read it, the
//! `get_usage` control request, makes claude read `/api/oauth/usage` whenever
//! its 60 s snapshot is stale. That endpoint allows each token about 30 reads
//! an hour, shared with every claude process on the account and with cswap,
//! and ccteam's reads helped run it out into 429s that froze cswap's view of
//! the default login. So ccteam does not ask: a live session reports the
//! windows its own turns last saw, and one that has not run a turn yet
//! reports none (the gateway's recorded observation answers instead).
//!
//! What the headers do not carry is not reported: the subscription tier, the
//! extra-credit balance, and a per-model window claude names only from its
//! own configuration (`seven_day_overage_included`).

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::{AccountUsage, ModelWindow};

#[derive(Debug, Clone, Default, PartialEq)]
struct Window {
    pct: Option<u8>,
    /// Epoch seconds.
    resets_at: Option<i64>,
    status: Option<String>,
}

/// Per-model weekly windows the headers name, and the model each one meters
/// (the same names claude's own usage answer gives them).
const MODEL_WINDOWS: [(&str, &str); 2] =
    [("seven_day_opus", "Opus"), ("seven_day_sonnet", "Sonnet")];

/// The windows one process's `rate_limit_event`s have reported, latest wins
/// per window.
#[derive(Debug, Clone, Default)]
pub struct RateLimitWindows {
    windows: BTreeMap<String, Window>,
}

impl RateLimitWindows {
    /// Fold one `rate_limit_info` object in.
    pub fn observe(&mut self, info: &Value) {
        match info.get("unifiedWindows").and_then(Value::as_object) {
            Some(windows) => {
                for (name, window) in windows {
                    self.update(name, window);
                }
            }
            None => {
                if let Some(name) = info.get("rateLimitType").and_then(Value::as_str) {
                    self.update(name, info);
                }
            }
        }
    }

    fn update(&mut self, name: &str, reported: &Value) {
        let window = self.windows.entry(name.to_string()).or_default();
        let resets_at = reported
            .get("resetsAt")
            .and_then(Value::as_f64)
            .map(|r| r as i64);
        let pct = reported
            .get("utilization")
            .and_then(Value::as_f64)
            .map(|fraction| (fraction * 100.0).round().clamp(0.0, 100.0) as u8);
        // A new reset time without a figure is a new window: the old figure
        // belonged to the one that ended.
        if resets_at.is_some() && resets_at != window.resets_at && pct.is_none() {
            window.pct = None;
        }
        if resets_at.is_some() {
            window.resets_at = resets_at;
        }
        if pct.is_some() {
            window.pct = pct;
        }
        if let Some(status) = reported.get("status").and_then(Value::as_str) {
            window.status = Some(status.to_string());
        }
    }

    /// The account usage these windows describe at `now`. A window whose
    /// reset has passed is left out: its figure belongs to the window that
    /// ended. `None` when nothing is left to report.
    pub fn usage(&self, now: DateTime<Utc>) -> Option<AccountUsage> {
        let current = |name: &str| {
            self.windows
                .get(name)
                .filter(|w| w.pct.is_some())
                .filter(|w| w.resets_at.is_none_or(|at| at > now.timestamp()))
        };
        let iso = |at: Option<i64>| {
            at.and_then(|at| Utc.timestamp_opt(at, 0).single())
                .map(|at| at.to_rfc3339())
        };
        let five = current("five_hour");
        let week = current("seven_day");
        let usage = AccountUsage {
            five_hour_pct: five.and_then(|w| w.pct),
            five_hour_resets_at: five.and_then(|w| iso(w.resets_at)),
            weekly_pct: week.and_then(|w| w.pct),
            weekly_resets_at: week.and_then(|w| iso(w.resets_at)),
            weekly_severity: week.and_then(|w| severity(w.status.as_deref())),
            model_windows: MODEL_WINDOWS
                .iter()
                .filter_map(|(name, model)| {
                    current(name).map(|w| ModelWindow {
                        model: model.to_string(),
                        pct: w.pct,
                        resets_at: iso(w.resets_at),
                    })
                })
                .collect(),
            ..Default::default()
        };
        usage.has_window().then_some(usage)
    }
}

/// A window's header status in the words a usage answer uses for severity.
fn severity(status: Option<&str>) -> Option<String> {
    Some(
        match status? {
            "allowed" => "normal",
            "allowed_warning" => "warning",
            other => other,
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(epoch: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(epoch, 0).single().unwrap()
    }

    /// The live shape (claude 2.1.325, one turn on a Team account).
    fn live_event() -> Value {
        json!({
            "status": "allowed",
            "resetsAt": 1794696600,
            "rateLimitType": "five_hour",
            "overageStatus": "rejected",
            "overageDisabledReason": "org_level_disabled",
            "isUsingOverage": false,
            "unifiedWindows": {
                "five_hour": {"utilization": 0.33, "resetsAt": 1794696600, "status": "allowed"},
                "seven_day": {"utilization": 0.62, "resetsAt": 1794920400, "status": "allowed"}
            }
        })
    }

    #[test]
    fn unified_windows_become_the_five_hour_and_weekly_figures() {
        let mut windows = RateLimitWindows::default();
        windows.observe(&live_event());
        let usage = windows.usage(at(1794680000)).expect("an observation");
        assert_eq!(usage.five_hour_pct, Some(33));
        assert_eq!(
            usage.five_hour_resets_at.as_deref(),
            Some("2026-11-14T22:50:00+00:00")
        );
        assert_eq!(usage.weekly_pct, Some(62));
        assert_eq!(
            usage.weekly_resets_at.as_deref(),
            Some("2026-11-17T13:00:00+00:00")
        );
        assert_eq!(usage.weekly_severity.as_deref(), Some("normal"));
        assert_eq!(usage.subscription, None, "the headers carry no tier");
        assert!(usage.model_windows.is_empty());
    }

    #[test]
    fn a_later_event_updates_only_the_windows_it_reports() {
        let mut windows = RateLimitWindows::default();
        windows.observe(&live_event());
        // No `unifiedWindows`: the line speaks for `rateLimitType` alone.
        windows.observe(&json!({
            "status": "allowed_warning",
            "rateLimitType": "seven_day",
            "utilization": 0.81,
            "resetsAt": 1794920400
        }));
        let usage = windows.usage(at(1794680000)).unwrap();
        assert_eq!(usage.five_hour_pct, Some(33), "untouched");
        assert_eq!(usage.weekly_pct, Some(81));
        assert_eq!(usage.weekly_severity.as_deref(), Some("warning"));

        // A status-only line keeps the figure.
        windows.observe(&json!({"status": "allowed", "rateLimitType": "five_hour"}));
        assert_eq!(
            windows.usage(at(1794680000)).unwrap().five_hour_pct,
            Some(33)
        );
    }

    #[test]
    fn a_window_past_its_reset_is_not_reported() {
        let mut windows = RateLimitWindows::default();
        windows.observe(&live_event());
        // After the 5-hour reset only the weekly figure still holds.
        let usage = windows.usage(at(1794696601)).unwrap();
        assert_eq!(usage.five_hour_pct, None);
        assert_eq!(usage.weekly_pct, Some(62));
        // After both, nothing does.
        assert!(windows.usage(at(1794920401)).is_none());
    }

    #[test]
    fn a_new_reset_time_without_a_figure_drops_the_old_figure() {
        let mut windows = RateLimitWindows::default();
        windows.observe(&live_event());
        windows.observe(&json!({
            "status": "allowed",
            "rateLimitType": "five_hour",
            "resetsAt": 1794714600
        }));
        assert_eq!(windows.usage(at(1794680000)).unwrap().five_hour_pct, None);
    }

    #[test]
    fn named_model_windows_are_reported_and_unnamed_ones_are_not() {
        let mut windows = RateLimitWindows::default();
        windows.observe(&json!({
            "status": "allowed",
            "rateLimitType": "seven_day",
            "unifiedWindows": {
                "seven_day": {"utilization": 0.4, "resetsAt": 1794920400, "status": "allowed"},
                "seven_day_opus": {"utilization": 0.9, "resetsAt": 1794920400, "status": "allowed_warning"},
                "seven_day_overage_included": {"utilization": 0.91, "resetsAt": 1794920400}
            }
        }));
        let usage = windows.usage(at(1794680000)).unwrap();
        assert_eq!(usage.model_windows.len(), 1);
        assert_eq!(usage.model_windows[0].model, "Opus");
        assert_eq!(usage.model_windows[0].pct, Some(90));
    }

    #[test]
    fn nothing_observed_is_no_usage() {
        assert!(RateLimitWindows::default().usage(Utc::now()).is_none());
        let mut windows = RateLimitWindows::default();
        windows.observe(&json!({"status": "allowed"}));
        assert!(windows.usage(Utc::now()).is_none());
    }
}
