//! One JSON shape for "where does this vendor account stand", shared by every
//! machine-readable surface.
//!
//! [`Gateway::account_usage_snapshot`](crate::gateway::Gateway::account_usage_snapshot)
//! answers WHAT is known; this module decides how that is spelled on the wire,
//! once, for both readers of it — the MCP `status{detail:"usage"}` body an
//! agent calls and the `GET /api/v1/usage` a script calls. Two spellings of one
//! fact is how a caller learns to trust one surface and distrust the other.
//!
//! Shape (compact by construction — the agent side pays for every byte in its
//! context, so a key is present only when there is something to say):
//!
//! ```json
//! {"observed":"2026-08-31T09:12:03Z","source":"status card","subscription":"max",
//!  "windows":[{"w":"5h","pct":8,"resets":"…"},
//!             {"w":"7d","pct":23,"resets":"…","severity":"warning"},
//!             {"w":"7d","model":"opus","pct":16,"resets":"…"},
//!             {"w":"credits","pct":3}]}
//! ```
//!
//! HONESTY: every window here survived its own expiry check
//! ([`ccteam_harness::usage_catalog::last_known_entry_in`]) — a percentage the
//! vendor's own reset time has passed is dropped upstream, never rendered
//! stale. `observed` is when ccteam heard it, so a reader can judge the rest.

use ccteam_harness::usage_catalog::VendorAccountUsage;
use ccteam_harness::AccountUsage;
use serde_json::{json, Map, Value};

/// Window token for the 5-hour rolling limit.
pub const WINDOW_FIVE_HOUR: &str = "5h";
/// Window token for the 7-day (weekly) limit — shared pool and per-model rows
/// alike; the per-model ones carry a `model` key, which is the difference.
pub const WINDOW_WEEKLY: &str = "7d";
/// Window token for the purchased extra-credit balance (no reset clock).
pub const WINDOW_CREDITS: &str = "credits";

/// One vendor's entry, provenance included.
pub fn vendor_usage_value(entry: &VendorAccountUsage) -> Value {
    let mut body = Map::new();
    body.insert("observed".into(), json!(entry.observed_at));
    if !entry.source.is_empty() {
        body.insert("source".into(), json!(entry.source));
    }
    if let Some(subscription) = entry.usage.subscription.as_deref() {
        body.insert("subscription".into(), json!(subscription));
    }
    body.insert("windows".into(), json!(usage_windows(&entry.usage)));
    Value::Object(body)
}

/// The flat window list for one account usage — the same order every time
/// (shortest clock first, then the shared weekly, then its per-model
/// refinements, then credits) so a diff between two reads is readable.
pub fn usage_windows(usage: &AccountUsage) -> Vec<Value> {
    let mut windows = Vec::new();
    if let Some(pct) = usage.five_hour_pct {
        windows.push(window(
            WINDOW_FIVE_HOUR,
            None,
            Some(pct),
            usage.five_hour_resets_at.as_deref(),
            None,
        ));
    }
    if let Some(pct) = usage.weekly_pct {
        windows.push(window(
            WINDOW_WEEKLY,
            None,
            Some(pct),
            usage.weekly_resets_at.as_deref(),
            usage.weekly_severity.as_deref(),
        ));
    }
    for model in &usage.model_windows {
        windows.push(window(
            WINDOW_WEEKLY,
            Some(model.model.as_str()),
            model.pct,
            model.resets_at.as_deref(),
            None,
        ));
    }
    if let Some(pct) = usage.credits_pct {
        // The credit balance is bought, not clocked: no reset to report.
        windows.push(window(WINDOW_CREDITS, None, Some(pct), None, None));
    }
    windows
}

fn window(
    w: &str,
    model: Option<&str>,
    pct: Option<u8>,
    resets: Option<&str>,
    severity: Option<&str>,
) -> Value {
    let mut row = Map::new();
    row.insert("w".into(), json!(w));
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        row.insert("model".into(), json!(model));
    }
    if let Some(pct) = pct {
        row.insert("pct".into(), json!(pct));
    }
    if let Some(resets) = resets.filter(|r| !r.is_empty()) {
        row.insert("resets".into(), json!(resets));
    }
    if let Some(severity) = severity.filter(|s| !s.is_empty()) {
        row.insert("severity".into(), json!(severity));
    }
    Value::Object(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::ModelWindow;

    fn full() -> VendorAccountUsage {
        VendorAccountUsage {
            observed_at: "2026-08-31T09:12:03Z".into(),
            source: "status card".into(),
            usage: AccountUsage {
                subscription: Some("max".into()),
                five_hour_pct: Some(8),
                five_hour_resets_at: Some("2026-08-31T14:00:00Z".into()),
                weekly_pct: Some(23),
                weekly_resets_at: Some("2026-09-03T00:00:00Z".into()),
                weekly_severity: Some("warning".into()),
                credits_pct: Some(3),
                model_windows: vec![ModelWindow {
                    model: "opus".into(),
                    pct: Some(16),
                    resets_at: Some("2026-09-03T00:00:00Z".into()),
                }],
            },
        }
    }

    /// The published contract: exact key set per row (no extras, no nulls),
    /// the fact that a per-model row is a weekly row PLUS `model`, and the
    /// window ORDER — which is the part a reader diffing two snapshots relies
    /// on. (Object key order is serde's, not a contract; array order is.)
    #[test]
    fn a_full_account_renders_every_window_in_a_fixed_order() {
        assert_eq!(
            vendor_usage_value(&full()),
            json!({
                "observed": "2026-08-31T09:12:03Z",
                "source": "status card",
                "subscription": "max",
                "windows": [
                    {"w": "5h", "pct": 8, "resets": "2026-08-31T14:00:00Z"},
                    {"w": "7d", "pct": 23, "resets": "2026-09-03T00:00:00Z", "severity": "warning"},
                    {"w": "7d", "model": "opus", "pct": 16, "resets": "2026-09-03T00:00:00Z"},
                    {"w": "credits", "pct": 3}
                ]
            })
        );
    }

    /// Absent facts cost zero bytes: no severity, no reset, no subscription,
    /// no empty strings — and a vendor that reported only credits says exactly
    /// that rather than padding the list with nulls.
    #[test]
    fn nothing_known_is_nothing_printed() {
        let entry = VendorAccountUsage {
            observed_at: "2026-08-31T09:12:03Z".into(),
            source: String::new(),
            usage: AccountUsage {
                credits_pct: Some(3),
                ..Default::default()
            },
        };
        assert_eq!(
            vendor_usage_value(&entry),
            json!({
                "observed": "2026-08-31T09:12:03Z",
                "windows": [{"w": "credits", "pct": 3}]
            })
        );
    }

    /// A model bucket the vendor named but gave no number for still says WHEN
    /// it resets; an unnamed one is never emitted as a bare weekly row.
    #[test]
    fn a_model_row_keeps_only_what_the_vendor_said() {
        let usage = AccountUsage {
            model_windows: vec![
                ModelWindow {
                    model: "sonnet".into(),
                    pct: None,
                    resets_at: Some("2026-09-03T00:00:00Z".into()),
                },
                ModelWindow {
                    model: String::new(),
                    pct: Some(5),
                    resets_at: None,
                },
            ],
            ..Default::default()
        };
        let windows = usage_windows(&usage);
        assert_eq!(
            windows[0],
            json!({"w": "7d", "model": "sonnet", "resets": "2026-09-03T00:00:00Z"})
        );
        assert_eq!(windows[1]["model"], serde_json::Value::Null);
    }
}
