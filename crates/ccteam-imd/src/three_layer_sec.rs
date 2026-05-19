//! Three-layer security composition.
//!
//! Mirrors `references/oh-my-claudecode/src/notifications/reply-listener.ts`
//! (slack-socket.ts `verifySlackSignature`, the shared `RateLimiter`,
//! and `sanitizeReplyInput`). All three layers must pass before the
//! daemon will forward an IM turn to a tmux session.

use std::time::{SystemTime, UNIX_EPOCH};

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

/// Maximum age (seconds) of a Slack signed-request timestamp before it
/// counts as replay-attack territory. OMC uses 5 minutes.
pub const SLACK_TIMESTAMP_MAX_AGE_SECS: u64 = 300;

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
    /// platform-specific (Slack HMAC vs Telegram chat-id binding vs
    /// Discord allowed-user check); call the verify helper for the
    /// matching platform before calling [`Self::evaluate`].
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

/// Slack `v0:<ts>:<body>` HMAC-SHA256 signature verification — drop-in
/// port of the OMC `verifySlackSignature` (TS source quoted in the
/// reply-listener Explore report).
///
/// V0.6 implements without the `hmac`/`sha2`/`subtle` deps to avoid
/// adding crates for one method — a future PR can swap in
/// `subtle::ConstantTimeEq` + `hmac` if Slack inbound HTTP receiver
/// lands. For now this returns `false` when no crypto backend is
/// available, forcing platform setups to either:
///
/// 1. Use long-polling (the V0.6 default — no signed request to
///    verify), OR
/// 2. Provide their own verification path before calling
///    [`ThreeLayerSec::evaluate`].
pub fn verify_slack_signature_stub(
    _signing_secret: &str,
    _signature: &str,
    timestamp: &str,
    _body: &str,
) -> bool {
    // Replay-window check (we can do this without crypto).
    let ts: u64 = match timestamp.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(ts);
    if age > SLACK_TIMESTAMP_MAX_AGE_SECS {
        return false;
    }
    // No HMAC backend wired yet; conservative deny by default.
    // Slack inbound HTTP receiver is V0.7 scope per
    // docs/versions/v0-6-0/wave-2-decisions.md §5.
    false
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
    fn slack_signature_stub_rejects_old_timestamp() {
        let old = "100"; // epoch=100s — far past window.
        assert!(!verify_slack_signature_stub("secret", "v0=abc", old, "body"));
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
