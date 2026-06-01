//! `chat_acl` allowlist enforcement.
//!
//! workflow.yaml (Wave 2 creator teammate adds this) can carry a
//! `chat_acl` block per chat bot:
//!
//! ```yaml
//! chat_acl:
//!   telegram_user_ids: ["12345", "67890"]
//!   slack_user_ids: ["U0123ABC"]
//!   discord_user_ids: ["..."]
//!   ws_user_ids: ["alice"]
//! ```
//!
//! [`AclPolicy::allow`] returns `true` when (a) the platform block is
//! empty (open) or (b) the sender id is present in the list.
//! Decisions are platform-scoped so a TG user-id can't accidentally
//! authorise a Slack request through global allowlists.

use serde::{Deserialize, Serialize};

/// Per-bot allowlist policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AclPolicy {
    /// Telegram user IDs (stringified). Empty = open.
    #[serde(default)]
    pub telegram_user_ids: Vec<String>,
    /// Slack user IDs. Empty = open.
    #[serde(default)]
    pub slack_user_ids: Vec<String>,
    /// Discord user IDs. Empty = open.
    #[serde(default)]
    pub discord_user_ids: Vec<String>,
    /// Local WebSocket / browser web-chat user IDs. Empty = open.
    #[serde(default)]
    pub ws_user_ids: Vec<String>,
}

impl AclPolicy {
    /// True iff the sender is allowed to drive the bot.
    pub fn allow(&self, platform: &str, sender_id: &str) -> bool {
        let list = match platform {
            "telegram" => &self.telegram_user_ids,
            "slack" => &self.slack_user_ids,
            "discord" => &self.discord_user_ids,
            "ws" | "web" => &self.ws_user_ids,
            // Unknown platform: deny by default (fail-closed).
            _ => return false,
        };
        if list.is_empty() {
            // Open: every sender allowed (mock / dev scope).
            return true;
        }
        list.iter().any(|id| id == sender_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_open() {
        let p = AclPolicy::default();
        assert!(p.allow("telegram", "any"));
        assert!(p.allow("slack", "any"));
        assert!(p.allow("ws", "any"));
        assert!(p.allow("web", "any"));
    }

    #[test]
    fn allowed_user_passes() {
        let p = AclPolicy {
            telegram_user_ids: vec!["12345".into()],
            ..Default::default()
        };
        assert!(p.allow("telegram", "12345"));
        assert!(!p.allow("telegram", "99999"));
    }

    #[test]
    fn unknown_platform_denies() {
        let p = AclPolicy::default();
        assert!(!p.allow("matrix", "any"));
    }

    #[test]
    fn cross_platform_isolation() {
        let p = AclPolicy {
            telegram_user_ids: vec!["alice-tg".into()],
            slack_user_ids: vec!["alice-slack".into()],
            ..Default::default()
        };
        // alice's TG id must not unlock the Slack channel.
        assert!(!p.allow("slack", "alice-tg"));
        assert!(p.allow("slack", "alice-slack"));
    }
}
