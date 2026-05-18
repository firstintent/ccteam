//! V0.6.0 Wave 2 F114 — `register_bot()` API consumed by
//! `ccteam-creator` Phase 5.
//!
//! **Signature aligned with the in-flight imd-teammate branch
//! (`v0-6-0-wave-2-imd`)** so the two waves' commits merge without an
//! API rewrite. The shape is:
//!
//! ```ignore
//! pub fn register_bot(
//!     workflow_slug: &str,
//!     role: &str,
//!     vendor: AgentVendor,
//!     im_platform: &str,    // "telegram" | "slack" | "discord" | "mock"
//!     im_chat_id: &str,     // platform-specific id captured by onboarding
//! ) -> Result<PathBuf>      // registry file path
//! ```
//!
//! The Wave-2 imd branch ships the full registry-on-disk
//! implementation (`~/.ccteam/im/registrations/<slug>__<role>.json`)
//! plus `unregister_bot` + `list_bots`. This stub returns the path it
//! *would* have written but does not persist — sufficient for the
//! ccteam-creator skill to compile + test against the locked
//! signature before the two branches merge.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::credentials::ImPlatform;

/// Vendor of the underlying agent runtime. Mirrors
/// `ccteam-core::harness::AgentVendor` shape; duplicated here so this
/// crate doesn't depend on ccteam-core (which would create a workspace
/// cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentVendor {
    Claude,
    Codex,
}

/// On-disk shape of a single bot registration entry. Persisted as
/// JSON under `~/.ccteam/im/registrations/<slug>__<role>.json` by
/// imd's daemon-side writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRegistration {
    pub workflow_slug: String,
    pub role: String,
    pub vendor: AgentVendor,
    #[serde(default)]
    pub persona_id: Option<String>,
    /// `"telegram"` / `"slack"` / `"discord"` / `"mock"`. Stringly
    /// typed at the registry boundary so adding new platforms doesn't
    /// require a schema bump.
    pub im_platform: String,
    pub im_chat_id: String,
    pub created_at: String,
}

/// Errors returned by [`register_bot`].
#[derive(Debug, Error)]
pub enum RegisterError {
    #[error("invalid im_platform `{0}` — expected telegram / slack / discord / mock")]
    UnknownPlatform(String),
    #[error("registry path resolution failed: {0}")]
    Path(#[source] anyhow::Error),
}

/// Register a bot for `workflow_slug` / `role` against an IM platform.
///
/// **Stub semantics**: returns the path the imd daemon's registry
/// writer *would* persist to (`~/.ccteam/im/registrations/<slug>__<role>.json`)
/// without actually creating the file. The full implementation lives
/// in the imd-teammate branch (`v0-6-0-wave-2-imd`); when those two
/// commits merge, this stub is replaced verbatim. The signature is
/// frozen across the merge so the ccteam-creator skill compiles
/// against either side without modification.
///
/// `im_chat_id` is the platform-specific identifier captured by
/// [`crate::onboarding::telegram_setup`] (`TelegramCredentials::owner_chat_id`).
pub fn register_bot(
    workflow_slug: &str,
    role: &str,
    vendor: AgentVendor,
    im_platform: &str,
    im_chat_id: &str,
) -> Result<PathBuf, RegisterError> {
    // Sanity-check the platform string against the known enum so
    // typos surface here rather than at daemon-watcher time.
    match im_platform {
        "telegram" => Some(ImPlatform::Telegram),
        "slack" => Some(ImPlatform::Slack),
        "discord" => Some(ImPlatform::Discord),
        "mock" => None, // accepted for tests; no ImPlatform mapping
        other => return Err(RegisterError::UnknownPlatform(other.to_string())),
    };
    let _ = (vendor, im_chat_id); // explicit-use to silence unused-arg lints
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    Ok(base
        .join(".ccteam")
        .join("im")
        .join("registrations")
        .join(format!("{workflow_slug}__{role}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_round_trip() {
        let r = BotRegistration {
            workflow_slug: "my-bot".into(),
            role: "tech-helper".into(),
            vendor: AgentVendor::Claude,
            persona_id: Some("tech-helper".into()),
            im_platform: "telegram".into(),
            im_chat_id: "1234567890".into(),
            created_at: "2026-05-19T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: BotRegistration = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn register_bot_returns_path_under_registrations() {
        let p = register_bot(
            "demo",
            "tech-helper",
            AgentVendor::Claude,
            "telegram",
            "987",
        )
        .unwrap();
        let s = p.to_string_lossy();
        assert!(s.ends_with("registrations/demo__tech-helper.json"));
    }

    #[test]
    fn register_bot_rejects_unknown_platform() {
        let err = register_bot("demo", "r", AgentVendor::Claude, "fax", "x").unwrap_err();
        assert!(matches!(err, RegisterError::UnknownPlatform(_)));
    }
}
