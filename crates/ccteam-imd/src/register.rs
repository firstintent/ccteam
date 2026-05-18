//! V0.6.0 Wave 2 F114 — `register_bot()` API consumed by
//! `ccteam-creator` Phase 5.
//!
//! Returns a [`BotRegistration`] handle that captures the
//! `(workflow_slug, role, vendor, im_platform, bot_handle)` quintuple
//! the daemon needs to wire the IM bridge to the spawned session. The
//! current Wave 2 implementation is a thin facade over
//! [`crate::credentials`] — the actual bridge wire-up (long-poll loop,
//! webhook receiver, etc.) is being landed by the `imd` teammate in
//! parallel under the same crate.
//!
//! ## Signature stability
//!
//! Locked across V0.6.x. Future variants (Slack, Discord, multi-bot
//! per workflow) extend [`BotRegistration`] rather than changing the
//! call shape so `ccteam-creator` does not need to track this crate's
//! point releases.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::credentials::{load_credentials, ImPlatform};

/// Vendor of the underlying agent runtime — same shape as the
/// `ccteam-core::harness::AgentVendor` enum but kept independent here
/// so this crate doesn't pull in ccteam-core as a dep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BotVendor {
    Claude,
    Codex,
}

/// Result of [`register_bot`]. Embedded in the rendered
/// `workflow.yaml` (`chat.bot_name`) and surfaced to the user in the
/// "好了,在 TG 找 @xxx 私聊试试" reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRegistration {
    pub workflow_slug: String,
    pub role: String,
    pub vendor: BotVendor,
    pub im_platform: ImPlatform,
    /// `@`-prefixed handle the user can address in their IM client.
    pub bot_handle: String,
}

/// Errors returned by [`register_bot`].
#[derive(Debug, Error)]
pub enum RegisterError {
    #[error("no credentials found for platform {0:?} — run `/ccteam-im-setup` first")]
    MissingCredentials(ImPlatform),
    #[error("credentials store unreadable: {0}")]
    CredentialsRead(#[source] anyhow::Error),
}

/// Register a bot for `workflow_slug` / `role` against the IM
/// platform whose credentials are already in
/// `~/.ccteam/im/credentials.json`. Pure synchronous lookup — does
/// **not** spin up the daemon-side bridge; the daemon picks the
/// registration up from the rendered `workflow.yaml` at next reload.
///
/// `vendor` discriminates Claude vs Codex so future pricing /
/// capability checks (Codex has no chat tools today) can be enforced
/// here. Currently both vendors are accepted unconditionally.
pub fn register_bot(
    workflow_slug: &str,
    role: &str,
    vendor: BotVendor,
    im_platform: ImPlatform,
) -> Result<BotRegistration, RegisterError> {
    let creds = load_credentials().map_err(RegisterError::CredentialsRead)?;
    let bot_handle = match im_platform {
        ImPlatform::Telegram => creds
            .telegram
            .as_ref()
            .map(|c| c.bot_username.clone())
            .ok_or(RegisterError::MissingCredentials(ImPlatform::Telegram))?,
        ImPlatform::Slack | ImPlatform::Discord => {
            // V0.7: implement Slack / Discord credential lookup. For
            // now treat as missing so ccteam-creator's user-facing
            // error message is consistent.
            return Err(RegisterError::MissingCredentials(im_platform));
        }
    };
    Ok(BotRegistration {
        workflow_slug: workflow_slug.into(),
        role: role.into(),
        vendor,
        im_platform,
        bot_handle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_serialises_round_trip() {
        let r = BotRegistration {
            workflow_slug: "my-bot".into(),
            role: "helper".into(),
            vendor: BotVendor::Claude,
            im_platform: ImPlatform::Telegram,
            bot_handle: "@helpful_assistant".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: BotRegistration = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}
