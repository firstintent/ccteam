//! Per-sender token-bucket rate limiter.
//!
//! Mirrors `references/oh-my-claudecode/src/notifications/reply-listener.ts`
//! `RateLimiter` class — sliding 60-second window, simple `Vec<Instant>`
//! storage (low cardinality: typically <50 entries per sender).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Default cap. V0.6.1 raised from 10 to 600 (10/sec sustained) — the
/// original OMC value silently dropped messages during normal personal-bot
/// usage (a user firing 11 pings in <60s saw nothing, no reply, no error
/// — see investigation in commit fixing rate-limit message loss). 600/min
/// preserves the layer as an abuse circuit-breaker against a stuck IM
/// loop without throttling real humans.
pub const DEFAULT_MAX_PER_MINUTE: usize = 600;

/// Per-sender sliding-window limiter. `sender_id` identifies the IM
/// user (e.g. Telegram user_id, Slack user id) — not the bot slug.
#[derive(Debug, Default)]
pub struct RateLimiter {
    max_per_window: usize,
    window: Duration,
    buckets: HashMap<String, Vec<Instant>>,
}

impl RateLimiter {
    /// Build a new limiter with `max` events per `window`.
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max_per_window: max,
            window,
            buckets: HashMap::new(),
        }
    }

    /// Convenience: OMC default (10 / 60 s).
    pub fn default_per_minute() -> Self {
        Self::new(DEFAULT_MAX_PER_MINUTE, Duration::from_secs(60))
    }

    /// Check whether `sender` may send another message; records the
    /// attempt as accepted when the answer is `true`. Returns `false`
    /// (without recording) when the cap is hit.
    pub fn check_and_record(&mut self, sender: &str) -> bool {
        let now = Instant::now();
        let bucket = self.buckets.entry(sender.to_string()).or_default();
        bucket.retain(|ts| now.duration_since(*ts) < self.window);
        if bucket.len() >= self.max_per_window {
            return false;
        }
        bucket.push(now);
        true
    }

    /// Read-only: how many recent attempts the sender has in the
    /// current window (after pruning expired entries).
    pub fn recent_count(&mut self, sender: &str) -> usize {
        let now = Instant::now();
        let bucket = self.buckets.entry(sender.to_string()).or_default();
        bucket.retain(|ts| now.duration_since(*ts) < self.window);
        bucket.len()
    }

    /// Reset state for one sender (e.g. admin override).
    pub fn reset(&mut self, sender: &str) {
        self.buckets.remove(sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn allows_up_to_cap() {
        let mut rl = RateLimiter::new(3, Duration::from_secs(60));
        assert!(rl.check_and_record("u1"));
        assert!(rl.check_and_record("u1"));
        assert!(rl.check_and_record("u1"));
        assert!(!rl.check_and_record("u1"));
    }

    #[test]
    fn separate_senders_have_separate_buckets() {
        let mut rl = RateLimiter::new(1, Duration::from_secs(60));
        assert!(rl.check_and_record("alice"));
        assert!(!rl.check_and_record("alice"));
        assert!(rl.check_and_record("bob"));
    }

    #[test]
    fn window_expires_old_entries() {
        let mut rl = RateLimiter::new(2, Duration::from_millis(80));
        assert!(rl.check_and_record("u1"));
        assert!(rl.check_and_record("u1"));
        assert!(!rl.check_and_record("u1"));
        sleep(Duration::from_millis(120));
        // After the window the bucket prunes and we get fresh capacity.
        assert!(rl.check_and_record("u1"));
    }

    #[test]
    fn reset_clears_bucket() {
        let mut rl = RateLimiter::new(1, Duration::from_secs(60));
        assert!(rl.check_and_record("u1"));
        rl.reset("u1");
        assert!(rl.check_and_record("u1"));
    }

    #[test]
    fn recent_count_reports_active_window_size() {
        let mut rl = RateLimiter::new(5, Duration::from_secs(60));
        assert!(rl.check_and_record("u1"));
        assert!(rl.check_and_record("u1"));
        assert_eq!(rl.recent_count("u1"), 2);
    }

    #[test]
    fn default_per_minute_constructor_matches_omc_value() {
        let rl = RateLimiter::default_per_minute();
        assert_eq!(rl.max_per_window, DEFAULT_MAX_PER_MINUTE);
        assert_eq!(rl.window, Duration::from_secs(60));
    }
}
