//! V0.6.8 F196 — process-global registry mapping a `(slug, role)` pair
//! to the [`MarkerReporter`](ccteam_harness::MarkerReporter) the chat-
//! mode adapter's transcript tail loop should poke on every observed
//! tick.
//!
//! Why a registry instead of plumbing the reporter through
//! [`ccteam_harness::HarnessAdapter::events`]: the adapter trait is
//! shared with Codex (UDS app-server) and the bg adapters. Adding a
//! parameter would ripple to every impl + every test fixture (and to
//! the public stream signature consumers spell out). The registry
//! sidesteps that — supervisors register themselves under their bot
//! identity before the adapter's `events()` spawn fires, the tail loop
//! looks the reporter up by `(slug, role)`, and adapters that don't
//! care never see the trait.
//!
//! Cycle safety: the registry holds [`std::sync::Weak`] references so
//! a supervisor that's been dropped (bot shutdown, daemon restart)
//! cleanly disappears from observation without leaking the Arc. A
//! tail loop that outlives its supervisor sees `upgrade() = None` and
//! the report is silently swallowed — correct behaviour: the dead
//! supervisor's state machine no longer matters.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use ccteam_harness::MarkerReporter;

/// `(slug, role)` lookup key. Owned so the map can outlive any one
/// supervisor's borrowed strings.
type Key = (String, String);

/// Internal storage — `Weak<dyn MarkerReporter>` so the registry never
/// keeps a supervisor alive past its rightful drop.
type Registry = Mutex<HashMap<Key, Weak<dyn MarkerReporter>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register `reporter` under `(slug, role)`. Overwrites any previous
/// registration for the same key — restart / reset paths re-register
/// against the fresh supervisor Arc, and the old Weak (if any) is
/// implicitly dropped.
///
/// Called by [`crate::execution::ClaudeTuiAdapter`]'s supervisor wiring
/// in the imd crate, immediately after `ensure_started` succeeds and
/// before the events consumer task begins draining the stream.
pub fn register(slug: &str, role: &str, reporter: Weak<dyn MarkerReporter>) {
    let key: Key = (slug.to_string(), role.to_string());
    if let Ok(mut g) = registry().lock() {
        g.insert(key, reporter);
    }
}

/// Drop the entry for `(slug, role)`. Idempotent — missing key is a
/// no-op. Called from the supervisor's shutdown path so a long-lived
/// daemon doesn't accumulate dead Weaks (though Weak entries are
/// cheap, explicit cleanup keeps the map's iteration shape bounded).
pub fn unregister(slug: &str, role: &str) {
    let key: Key = (slug.to_string(), role.to_string());
    if let Ok(mut g) = registry().lock() {
        g.remove(&key);
    }
}

/// Look up the live reporter for `(slug, role)`. Returns `None` when
/// no supervisor ever registered for the key OR when the registered
/// supervisor has since been dropped (Weak fails to upgrade).
pub fn lookup(slug: &str, role: &str) -> Option<Arc<dyn MarkerReporter>> {
    let key: Key = (slug.to_string(), role.to_string());
    let weak = {
        let g = registry().lock().ok()?;
        g.get(&key).cloned()?
    };
    weak.upgrade()
}

/// Test-only — drop every entry. Keeps the per-test state machine
/// clean when several supervisor instances are exercised in the same
/// process. Public to dependents' integration tests.
#[doc(hidden)]
pub fn _clear_for_tests() {
    if let Ok(mut g) = registry().lock() {
        g.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingReporter {
        missing: AtomicUsize,
        found: AtomicUsize,
    }

    #[async_trait]
    impl MarkerReporter for CountingReporter {
        async fn report_marker_missing(&self) {
            self.missing.fetch_add(1, Ordering::SeqCst);
        }
        async fn report_marker_found(&self) {
            self.found.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn register_then_lookup_returns_live_reporter() {
        _clear_for_tests();
        let r = Arc::new(CountingReporter {
            missing: AtomicUsize::new(0),
            found: AtomicUsize::new(0),
        });
        let weak: Weak<dyn MarkerReporter> = Arc::downgrade(&r) as Weak<dyn MarkerReporter>;
        register("dev-foo", "alice", weak);
        let looked = lookup("dev-foo", "alice").expect("registered key resolves");
        looked.report_marker_missing().await;
        assert_eq!(r.missing.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn lookup_unknown_key_returns_none() {
        _clear_for_tests();
        assert!(lookup("dev-foo", "bob").is_none());
    }

    #[tokio::test]
    async fn dropped_supervisor_becomes_invisible() {
        _clear_for_tests();
        let r = Arc::new(CountingReporter {
            missing: AtomicUsize::new(0),
            found: AtomicUsize::new(0),
        });
        let weak: Weak<dyn MarkerReporter> = Arc::downgrade(&r) as Weak<dyn MarkerReporter>;
        register("dev-foo", "alice", weak);
        drop(r);
        // Weak upgrade fails after the Arc is dropped — lookup must
        // return None, NOT a stale clone that panics on call.
        assert!(lookup("dev-foo", "alice").is_none());
    }

    #[tokio::test]
    async fn unregister_drops_entry() {
        _clear_for_tests();
        let r = Arc::new(CountingReporter {
            missing: AtomicUsize::new(0),
            found: AtomicUsize::new(0),
        });
        let weak: Weak<dyn MarkerReporter> = Arc::downgrade(&r) as Weak<dyn MarkerReporter>;
        register("dev-foo", "alice", weak);
        unregister("dev-foo", "alice");
        assert!(lookup("dev-foo", "alice").is_none());
    }
}
