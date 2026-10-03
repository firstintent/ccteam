//! Three-layer security composition.
//!
//! Mirrors `references/oh-my-claudecode/src/notifications/reply-listener.ts`
//! (the shared `RateLimiter` and `sanitizeReplyInput`). All three layers
//! must pass before the daemon will forward an IM turn to a session. Slack
//! needs no request-signature layer: its inbound is a Socket Mode connection
//! the daemon opens itself, never a signed webhook.

use crate::acl::AclPolicy;
use crate::rate_limit::RateLimiter;
use crate::sanitize::sanitize_reply_input;

/// Outcome of a single `evaluate` call. `Accept(payload)` carries the
/// sanitized form; the other variants name which layer rejected.
#[derive(Debug, Clone, PartialEq)]
pub enum SecOutcome {
    /// Accepted. `payload` is the post-sanitize content.
    Accept {
        /// Sanitized payload ready to forward.
        payload: String,
    },
    /// ACL denied (sender not on the bot's allowlist).
    AclDenied,
    /// Rate limit exceeded for this sender.
    RateLimited,
    /// Signature / replay verification failed.
    BadSignature(String),
    /// After sanitization the payload was empty (all-stripped).
    EmptyAfterSanitize,
}

/// Stateless evaluator. The daemon holds one [`ThreeLayerSec`] per
/// bot — the [`RateLimiter`] inside lives across IM events but is
/// owned by the caller (so tests can inject a deterministic clock by
/// constructing the limiter directly).
pub struct ThreeLayerSec {
    /// Bot ACL (workflow.yaml `chat_acl`).
    pub acl: AclPolicy,
    /// Per-sender token bucket (default OMC parity = 10 / 60 s).
    pub rate: RateLimiter,
}

impl ThreeLayerSec {
    /// Build with explicit ACL and OMC-default rate limit.
    pub fn new(acl: AclPolicy) -> Self {
        Self {
            acl,
            rate: RateLimiter::default_per_minute(),
        }
    }

    /// Layer 1 + 2 + 3 in order: ACL → rate limit → sanitize.
    /// Signature verification is **not** included here because it's
    /// platform-specific (Telegram chat-id binding vs Discord allowed-user
    /// check vs the providers' own allowlists); call the verify helper for
    /// the matching platform before calling [`Self::evaluate`].
    pub fn evaluate(&mut self, platform: &str, sender_id: &str, raw_text: &str) -> SecOutcome {
        if !self.acl.allow(platform, sender_id) {
            return SecOutcome::AclDenied;
        }
        if !self.rate.check_and_record(sender_id) {
            return SecOutcome::RateLimited;
        }
        let cleaned = sanitize_reply_input(raw_text);
        if cleaned.is_empty() {
            return SecOutcome::EmptyAfterSanitize;
        }
        SecOutcome::Accept { payload: cleaned }
    }
}

/// Telegram chat-id binding check — the bot only accepts updates from
/// `allowed_chat_ids` configured in `credentials.json`.
pub fn verify_telegram_chat_binding(allowed: &[String], inbound_chat_id: &str) -> bool {
    if allowed.is_empty() {
        // Open mode for dev; production sets explicit chat IDs.
        return true;
    }
    allowed.iter().any(|id| id == inbound_chat_id)
}

/// Discord authorised-user check.
pub fn verify_discord_user(authorized: &[String], inbound_user_id: &str) -> bool {
    if authorized.is_empty() {
        return true;
    }
    authorized.iter().any(|id| id == inbound_user_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_sec() -> ThreeLayerSec {
        ThreeLayerSec::new(AclPolicy::default())
    }

    #[test]
    fn accept_passes_through_sanitized() {
        let mut sec = open_sec();
        let out = sec.evaluate("telegram", "u1", "hello `pwd` $(rm) world");
        match out {
            SecOutcome::Accept { payload } => {
                assert!(payload.contains("\\`pwd\\`"));
                assert!(payload.contains("\\$("));
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn denies_when_acl_blocks() {
        let mut sec = ThreeLayerSec::new(AclPolicy {
            telegram_user_ids: vec!["alice".into()],
            ..Default::default()
        });
        assert_eq!(
            sec.evaluate("telegram", "mallory", "hi"),
            SecOutcome::AclDenied
        );
    }

    #[test]
    fn rate_limit_eventually_triggers() {
        use crate::rate_limit::DEFAULT_MAX_PER_MINUTE;
        let mut sec = open_sec();
        // Burst (DEFAULT_MAX_PER_MINUTE + 1) — the last one trips.
        for _ in 0..DEFAULT_MAX_PER_MINUTE {
            assert!(matches!(
                sec.evaluate("telegram", "u1", "msg"),
                SecOutcome::Accept { .. }
            ));
        }
        assert_eq!(
            sec.evaluate("telegram", "u1", "msg"),
            SecOutcome::RateLimited
        );
    }

    #[test]
    fn empty_after_sanitize_rejected() {
        let mut sec = open_sec();
        // Only control chars — sanitize returns "".
        assert_eq!(
            sec.evaluate("telegram", "u1", "\x00\x01\x07"),
            SecOutcome::EmptyAfterSanitize
        );
    }

    #[test]
    fn telegram_chat_binding_open_when_empty() {
        assert!(verify_telegram_chat_binding(&[], "12345"));
    }

    #[test]
    fn telegram_chat_binding_rejects_unknown() {
        assert!(!verify_telegram_chat_binding(
            &["12345".to_string()],
            "99999"
        ));
    }

    #[test]
    fn discord_user_open_when_empty() {
        assert!(verify_discord_user(&[], "any"));
    }
}
