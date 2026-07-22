//! v0.9.7 — lazy latest-version check (PRD F3.4).
//!
//! `state/version.json` caches the latest release seen from GitHub. The
//! refresh is **lazy and injected**: it only fires on the
//! status / doctor / update command paths (never in a daemon loop — the
//! "daemon doesn't tick" red line), is gated to at most once every
//! [`REFRESH_INTERVAL_HOURS`], and the actual network fetch is a closure
//! the caller passes in — so this module stays network-agnostic (the real
//! GitHub fetcher lives in the CLI) and every gate is unit-testable with a
//! fake. A fetch failure degrades silently to the old cache.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::paths::CcteamPaths;

/// Cache file name under `<root>/state/`.
pub const VERSION_CACHE_NAME: &str = "version.json";

/// Minimum hours between network refreshes (lazy check; never a loop).
pub const REFRESH_INTERVAL_HOURS: i64 = 20;

/// Persisted latest-version cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionCache {
    /// Latest release tag last seen (e.g. `v0.9.8` or `0.9.8`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// RFC3339 time of the last successful check (drives the 20h gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    /// A version the user dismissed — [`update_available`] stays quiet for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_version: Option<String>,
}

/// Resolve `<root>/state/version.json`.
pub fn version_cache_path(paths: &CcteamPaths) -> PathBuf {
    paths.state_dir().join(VERSION_CACHE_NAME)
}

/// Read the cache. `None` for missing / unreadable / corrupt (all three
/// simply mean "no cached knowledge").
pub fn cached_latest(paths: &CcteamPaths) -> Option<VersionCache> {
    let body = std::fs::read_to_string(version_cache_path(paths)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Atomically publish the cache (tmp + rename in the same dir).
fn write_cache(paths: &CcteamPaths, cache: &VersionCache) -> Result<()> {
    let path = version_cache_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(cache).context("serialize version cache")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    if let Err(err) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("publish version cache {}", path.display()));
    }
    Ok(())
}

/// Refresh the cache iff enabled AND due (no cache yet, or the last check
/// is ≥ [`REFRESH_INTERVAL_HOURS`] old). `fetch` is the injected network
/// call (the real GitHub-latest fetcher in production, a fake in tests);
/// `None` from it — or a disabled/not-due gate — returns the existing
/// cache unchanged (silent degrade). A successful fetch updates
/// `latest_version` + `last_checked_at`, preserving any `dismissed_version`.
pub fn maybe_refresh_latest(
    paths: &CcteamPaths,
    prefs_enabled: bool,
    now: DateTime<Utc>,
    fetch: impl FnOnce() -> Option<String>,
) -> VersionCache {
    let existing = cached_latest(paths).unwrap_or_default();
    if !prefs_enabled {
        return existing;
    }
    if !refresh_due(&existing, now) {
        return existing;
    }
    match fetch() {
        Some(latest) => {
            let updated = VersionCache {
                latest_version: Some(latest),
                last_checked_at: Some(now.to_rfc3339()),
                dismissed_version: existing.dismissed_version.clone(),
            };
            // A persistence failure must not break the caller; the value is
            // still returned for this invocation.
            let _ = write_cache(paths, &updated);
            updated
        }
        None => existing,
    }
}

/// True when a refresh is warranted: no prior check, an unparseable
/// timestamp, or ≥ [`REFRESH_INTERVAL_HOURS`] since the last one.
fn refresh_due(cache: &VersionCache, now: DateTime<Utc>) -> bool {
    match &cache.last_checked_at {
        None => true,
        Some(ts) => match DateTime::parse_from_rfc3339(ts) {
            Ok(t) => {
                now.signed_duration_since(t.with_timezone(&Utc)).num_hours()
                    >= REFRESH_INTERVAL_HOURS
            }
            Err(_) => true,
        },
    }
}

/// `Some(latest)` when the cached latest is strictly newer than `current`
/// AND is not the dismissed version; `None` otherwise. Both operands are
/// normalized (leading `v` stripped, dotted numeric compare) so `v0.9.8`
/// vs `0.9.7` resolves correctly.
pub fn update_available(cache: &VersionCache, current: &str) -> Option<String> {
    let latest = cache.latest_version.as_deref()?;
    if version_gt(latest, current) && cache.dismissed_version.as_deref() != Some(latest) {
        Some(latest.to_string())
    } else {
        None
    }
}

/// Parse a version into dotted numeric components, tolerating a leading
/// `v` and trailing non-numeric suffixes on each component (`0.9.8-rc1`
/// → `[0, 9, 8]`).
fn normalize(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

/// Strict "a is a newer version than b" over normalized components.
fn version_gt(a: &str, b: &str) -> bool {
    let (na, nb) = (normalize(a), normalize(b));
    let len = na.len().max(nb.len());
    for i in 0..len {
        let x = na.get(i).copied().unwrap_or(0);
        let y = nb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn paths(tmp: &tempfile::TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn corrupt_or_missing_cache_reads_as_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        assert_eq!(cached_latest(&p), None);
        std::fs::create_dir_all(p.state_dir()).unwrap();
        std::fs::write(version_cache_path(&p), b"{ not json").unwrap();
        assert_eq!(cached_latest(&p), None);
    }

    #[test]
    fn refresh_gate_fetches_when_no_cache_and_persists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let calls = AtomicUsize::new(0);
        let now = Utc::now();
        let cache = maybe_refresh_latest(&p, true, now, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some("v0.9.8".to_string())
        });
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "fetch must run on cold cache"
        );
        assert_eq!(cache.latest_version.as_deref(), Some("v0.9.8"));
        // Persisted and readable back.
        assert_eq!(
            cached_latest(&p).unwrap().latest_version.as_deref(),
            Some("v0.9.8")
        );
    }

    #[test]
    fn refresh_gate_skips_within_20h_window() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let now = Utc::now();
        // Seed a fresh (19h-old) check.
        let seeded = VersionCache {
            latest_version: Some("v0.9.7".to_string()),
            last_checked_at: Some((now - chrono::Duration::hours(19)).to_rfc3339()),
            dismissed_version: None,
        };
        write_cache(&p, &seeded).unwrap();

        let calls = AtomicUsize::new(0);
        let cache = maybe_refresh_latest(&p, true, now, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some("v9.9.9".to_string())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0, "must NOT fetch within 20h");
        assert_eq!(cache.latest_version.as_deref(), Some("v0.9.7"));
    }

    #[test]
    fn refresh_gate_fetches_after_20h() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let now = Utc::now();
        let seeded = VersionCache {
            latest_version: Some("v0.9.7".to_string()),
            last_checked_at: Some((now - chrono::Duration::hours(21)).to_rfc3339()),
            dismissed_version: None,
        };
        write_cache(&p, &seeded).unwrap();

        let calls = AtomicUsize::new(0);
        let cache = maybe_refresh_latest(&p, true, now, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some("v0.9.8".to_string())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must fetch after 20h");
        assert_eq!(cache.latest_version.as_deref(), Some("v0.9.8"));
    }

    #[test]
    fn disabled_prefs_never_fetch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let calls = AtomicUsize::new(0);
        let cache = maybe_refresh_latest(&p, false, Utc::now(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some("v0.9.8".to_string())
        });
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "disabled prefs must not fetch"
        );
        assert_eq!(cache, VersionCache::default());
    }

    #[test]
    fn failed_fetch_keeps_old_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = paths(&tmp);
        let now = Utc::now();
        let seeded = VersionCache {
            latest_version: Some("v0.9.7".to_string()),
            last_checked_at: Some((now - chrono::Duration::hours(48)).to_rfc3339()),
            dismissed_version: None,
        };
        write_cache(&p, &seeded).unwrap();
        let cache = maybe_refresh_latest(&p, true, now, || None);
        // Old value preserved; no last_checked_at churn on failure.
        assert_eq!(cache.latest_version.as_deref(), Some("v0.9.7"));
        assert_eq!(cache.last_checked_at, seeded.last_checked_at);
    }

    #[test]
    fn update_available_compares_and_respects_dismissed() {
        let newer = VersionCache {
            latest_version: Some("v0.9.8".to_string()),
            last_checked_at: None,
            dismissed_version: None,
        };
        assert_eq!(update_available(&newer, "0.9.7").as_deref(), Some("v0.9.8"));
        // Equal → no update.
        assert_eq!(update_available(&newer, "0.9.8"), None);
        // Current newer than cached → no update.
        assert_eq!(update_available(&newer, "0.10.0"), None);
        // Dismissed → quiet even though it's newer.
        let dismissed = VersionCache {
            dismissed_version: Some("v0.9.8".to_string()),
            ..newer.clone()
        };
        assert_eq!(update_available(&dismissed, "0.9.7"), None);
        // Empty cache → None.
        assert_eq!(update_available(&VersionCache::default(), "0.9.7"), None);
    }

    #[test]
    fn version_gt_normalizes_v_prefix_and_suffixes() {
        assert!(version_gt("v0.9.8", "0.9.7"));
        assert!(version_gt("0.10.0", "0.9.9"));
        assert!(!version_gt("0.9.7", "0.9.7"));
        assert!(!version_gt("0.9.7", "v0.9.8"));
        assert!(version_gt("1.0.0", "0.9.99"));
        // Trailing suffix tolerated.
        assert!(version_gt("0.9.8-rc1", "0.9.7"));
    }
}
