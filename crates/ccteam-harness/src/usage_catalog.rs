//! Best-effort last-seen ACCOUNT usage captured from vendor sessions.
//!
//! Sibling of [`crate::model_catalog`], and for the same reason: some vendor
//! facts are **account-scoped, not session-scoped**, yet the only way to read
//! them is to ask a live session. Without a home of their own they exist only
//! while some process happens to be resident — so a surface that wants one has
//! to go scavenging through whatever sessions are alive, and silently shows
//! nothing when none are. [`AccountUsage`] (the 5-hour / weekly rate-limit
//! windows) was exactly that: the IM `/status` card lost its `⚡ 用量` row
//! whenever the focused session had been idle-released, which is precisely
//! when a reader is most likely to be checking where the account stands.
//!
//! So the observation gets recorded where it is made and read back from here.
//! The cache is advisory only: no spawn or turn path reads it, and a cache
//! failure never fails a session. On-disk shape mirrors the model catalog —
//! `{vendor: {observed_at, source, usage}}` at `~/.ccteam/account-usage.json`.
//!
//! SCOPE / HONESTY: this is the daemon's own vendor-account state (the account
//! every session of that vendor spends from), not any user's data, and it is
//! keyed by vendor alone — the same host-agnostic scope the live read it
//! replaces already had. A snapshot is never shown past its usefulness:
//! [`last_known_usage_in`] drops each window once the vendor's OWN declared
//! reset time has passed (falling back to the window's natural length when the
//! vendor declared none), so a stale percentage is omitted rather than
//! presented as current.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::adapter::AccountUsage;
use crate::execution::fs_atomic::atomic_write_durable;

/// A 5-hour window is worthless this long after it was observed, when the
/// vendor declared no reset time of its own.
const FIVE_HOUR_NATURAL: Duration = Duration::hours(5);
/// Same, for the weekly window and the (reset-less) extra-credit balance.
const WEEKLY_NATURAL: Duration = Duration::days(7);

/// Last successful account-usage observation for one vendor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VendorAccountUsage {
    /// RFC3339 timestamp recorded by ccteam at capture time.
    pub observed_at: String,
    /// What produced the capture (`status card` / `session release` …).
    pub source: String,
    /// The vendor's answer, verbatim.
    #[serde(default)]
    pub usage: AccountUsage,
}

/// Per-vendor cache, same transparent `{vendor: …}` top-level shape as
/// [`crate::model_catalog::ModelCatalog`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UsageCatalog(pub BTreeMap<String, VendorAccountUsage>);

static USAGE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Cache path under an injected ccteam root.
pub fn usage_catalog_path_in(root: &Path) -> PathBuf {
    root.join("account-usage.json")
}

/// Read an injected cache. Missing, unreadable, or corrupt files deliberately
/// degrade to an empty catalog: this cache must never obstruct a session.
pub fn load_usage_catalog_in(root: &Path) -> UsageCatalog {
    std::fs::read(usage_catalog_path_in(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Atomically replace one vendor's last-seen entry under an injected root.
/// An all-`None` capture is ignored so a vendor that momentarily answers with
/// nothing cannot erase a useful prior observation.
pub fn record_vendor_usage_in(
    root: &Path,
    vendor: &str,
    source: &str,
    usage: &AccountUsage,
) -> Result<()> {
    let vendor = vendor.trim();
    if vendor.is_empty() || usage == &AccountUsage::default() {
        return Ok(());
    }
    let lock = USAGE_WRITE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("account usage write lock poisoned"))?;
    std::fs::create_dir_all(root)
        .with_context(|| format!("create ccteam root {}", root.display()))?;
    let mut catalog = load_usage_catalog_in(root);
    catalog.0.insert(
        vendor.to_string(),
        VendorAccountUsage {
            observed_at: Utc::now().to_rfc3339(),
            source: source.to_string(),
            usage: usage.clone(),
        },
    );
    let bytes = serde_json::to_vec_pretty(&catalog).context("serialize account usage catalog")?;
    atomic_write_durable(&usage_catalog_path_in(root), &bytes)
}

/// Injected-root recorder that never propagates failure: usage persistence is
/// no part of any spawn / turn / release success contract.
pub fn record_vendor_usage_best_effort(root: &Path, vendor: &str, source: &str, u: &AccountUsage) {
    if let Err(error) = record_vendor_usage_in(root, vendor, source, u) {
        tracing::debug!(%vendor, %error, "account usage capture not persisted");
    }
}

/// The vendor's last observed usage, with every window that can no longer be
/// current removed — `None` when nothing usable survives.
///
/// Validity comes from the VENDOR: a window is live until the `resets_at` it
/// declared. Only when it declared none (or an unparseable one) does the
/// window's natural length from `observed_at` bound it instead. So a snapshot
/// is shown while it still describes the account, and disappears on its own
/// once it cannot — no arbitrary staleness cutoff, and no caller-side choice.
pub fn last_known_usage_in(root: &Path, vendor: &str, now: DateTime<Utc>) -> Option<AccountUsage> {
    let entry = load_usage_catalog_in(root).0.remove(vendor.trim())?;
    let observed = parse_rfc3339(&entry.observed_at)?;
    let alive = |declared: &Option<String>, natural: Duration| -> bool {
        match declared.as_deref().and_then(parse_rfc3339) {
            Some(reset) => now < reset,
            None => now < observed + natural,
        }
    };
    let mut usage = entry.usage;
    if !alive(&usage.five_hour_resets_at, FIVE_HOUR_NATURAL) {
        usage.five_hour_pct = None;
        usage.five_hour_resets_at = None;
    }
    if !alive(&usage.weekly_resets_at, WEEKLY_NATURAL) {
        usage.weekly_pct = None;
        usage.weekly_resets_at = None;
        usage.weekly_severity = None;
    }
    // Extra credits are a purchased balance the vendor gives no reset for;
    // bound them by the longest window ccteam knows about.
    if !alive(&None, WEEKLY_NATURAL) {
        usage.credits_pct = None;
    }
    // A surviving subscription tier alone renders no row (it is a tail segment,
    // never a fact on its own), so say so here rather than hand back a usage
    // that formats to nothing.
    let has_window =
        usage.five_hour_pct.is_some() || usage.weekly_pct.is_some() || usage.credits_pct.is_some();
    has_window.then_some(usage)
}

fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> AccountUsage {
        AccountUsage {
            subscription: Some("max".into()),
            five_hour_pct: Some(6),
            five_hour_resets_at: Some("2026-08-31T14:00:00Z".into()),
            weekly_pct: Some(50),
            weekly_resets_at: Some("2026-09-03T00:00:00Z".into()),
            weekly_severity: Some("normal".into()),
            credits_pct: Some(46),
        }
    }

    fn at(raw: &str) -> DateTime<Utc> {
        parse_rfc3339(raw).expect("test timestamp parses")
    }

    #[test]
    fn injected_cache_round_trips_vendors_without_clobbering() {
        let root = tempfile::tempdir().unwrap();
        record_vendor_usage_in(root.path(), "claude", "status card", &usage()).unwrap();
        record_vendor_usage_in(
            root.path(),
            "codex",
            "session release",
            &AccountUsage {
                weekly_pct: Some(12),
                ..Default::default()
            },
        )
        .unwrap();

        let got = load_usage_catalog_in(root.path());
        assert_eq!(got.0["claude"].usage.five_hour_pct, Some(6));
        assert_eq!(got.0["codex"].source, "session release");
        assert!(!got.0["codex"].observed_at.is_empty());
        assert!(!usage_catalog_path_in(root.path())
            .with_file_name("account-usage.json.tmp")
            .exists());
    }

    /// The whole point of the file: a vendor with NO live session still has an
    /// account state to report, and each window survives exactly as long as the
    /// vendor said it would.
    #[test]
    fn last_known_usage_expires_each_window_at_the_vendors_own_reset() {
        let root = tempfile::tempdir().unwrap();
        record_vendor_usage_in(root.path(), "claude", "status card", &usage()).unwrap();

        // Before every declared reset: the full answer, verbatim.
        let fresh = last_known_usage_in(root.path(), "claude", at("2026-08-31T09:00:00Z")).unwrap();
        assert_eq!(fresh, usage());

        // Past the 5-hour reset only: that window is dropped (never shown as
        // current), the weekly and the credits balance stay.
        let later = last_known_usage_in(root.path(), "claude", at("2026-08-31T15:00:00Z")).unwrap();
        assert_eq!(later.five_hour_pct, None);
        assert_eq!(later.five_hour_resets_at, None);
        assert_eq!(later.weekly_pct, Some(50));
        assert_eq!(later.credits_pct, Some(46));
        assert_eq!(later.subscription.as_deref(), Some("max"));

        // Past every window (weekly reset gone, and beyond the credit bound):
        // nothing usable survives, so the row is omitted rather than stale.
        assert_eq!(
            last_known_usage_in(root.path(), "claude", at("2026-09-30T00:00:00Z")),
            None
        );
        // An unknown vendor never borrows another's account.
        assert_eq!(
            last_known_usage_in(root.path(), "codex", at("2026-08-31T09:00:00Z")),
            None
        );
    }

    /// A vendor that declares no reset time still gets a bounded life — the
    /// window's own natural length from when it was observed.
    #[test]
    fn an_undeclared_reset_falls_back_to_the_windows_natural_length() {
        let root = tempfile::tempdir().unwrap();
        let observed = Utc::now();
        record_vendor_usage_in(
            root.path(),
            "codex",
            "status card",
            &AccountUsage {
                five_hour_pct: Some(30),
                weekly_pct: Some(70),
                ..Default::default()
            },
        )
        .unwrap();

        let soon =
            last_known_usage_in(root.path(), "codex", observed + Duration::hours(1)).unwrap();
        assert_eq!(soon.five_hour_pct, Some(30));
        assert_eq!(soon.weekly_pct, Some(70));

        // Past 5 hours the short window is gone; the weekly one lives a week.
        let later =
            last_known_usage_in(root.path(), "codex", observed + Duration::hours(6)).unwrap();
        assert_eq!(later.five_hour_pct, None);
        assert_eq!(later.weekly_pct, Some(70));
        assert_eq!(
            last_known_usage_in(root.path(), "codex", observed + Duration::days(8)),
            None
        );
    }

    #[test]
    fn corrupt_or_missing_cache_is_tolerated() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(load_usage_catalog_in(root.path()), UsageCatalog::default());
        assert_eq!(last_known_usage_in(root.path(), "claude", Utc::now()), None);
        std::fs::write(usage_catalog_path_in(root.path()), b"{not-json").unwrap();
        assert_eq!(load_usage_catalog_in(root.path()), UsageCatalog::default());
        assert_eq!(last_known_usage_in(root.path(), "claude", Utc::now()), None);
    }

    /// A vendor answering with nothing must not erase what it said before —
    /// same rule as an empty model capture.
    #[test]
    fn empty_capture_preserves_last_seen_entry() {
        let root = tempfile::tempdir().unwrap();
        record_vendor_usage_in(root.path(), "claude", "status card", &usage()).unwrap();
        record_vendor_usage_in(
            root.path(),
            "claude",
            "status card",
            &AccountUsage::default(),
        )
        .unwrap();
        assert_eq!(
            load_usage_catalog_in(root.path()).0["claude"]
                .usage
                .five_hour_pct,
            Some(6)
        );
    }
}
