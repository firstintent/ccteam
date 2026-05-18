//! `~/.ccteam/im/credentials.json` reader / writer with 0600
//! permission enforcement.
//!
//! Layout (V0.6.0 Wave 2):
//!
//! ```jsonc
//! {
//!   "telegram": {
//!     "bot_token": "1234:abcd",
//!     "bot_username": "@helpful_assistant",
//!     "owner_chat_id": 987654321
//!   }
//! }
//! ```
//!
//! Per-platform sub-objects are added as new IM transports land
//! (`slack`, `discord`, ...). Reading the file with a missing platform
//! key returns `None` rather than an error — partial state during
//! onboarding is the common case.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// IM platform discriminator. Re-exported to ccteam-creator + skill
/// surfaces so call sites can pass a typed value instead of stringly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImPlatform {
    Telegram,
    Slack,
    Discord,
}

impl ImPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            ImPlatform::Telegram => "telegram",
            ImPlatform::Slack => "slack",
            ImPlatform::Discord => "discord",
        }
    }
}

/// Telegram-specific credential record. Stored under the `telegram`
/// key of [`Credentials`].
///
/// Schema is aligned with the in-flight imd-teammate branch
/// (`v0-6-0-wave-2-imd`) so merging the two branches is a no-op
/// rather than a struct-rename diff. Notably `allowed_chat_ids` is a
/// `Vec<String>` (not a single `i64`) so the daemon-side router can
/// extend the ACL without a schema migration when the user adds
/// secondary recipients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramCredentials {
    /// Bot token from @BotFather (`<botid>:<hash>`).
    pub bot_token: String,
    /// Bot handle returned by `getMe`, including leading `@`. Kept as
    /// a convenience for the skill UX ("在 TG 找 @xxx") even though
    /// imd's daemon path only reads `allowed_chat_ids` directly.
    #[serde(default)]
    pub bot_username: String,
    /// chat_ids the bot is permitted to talk to. Index 0 is the first
    /// captured chat_id from onboarding (the "owner" chat).
    /// Stringly-typed because Slack / Discord chat ids are not always
    /// numeric.
    #[serde(default)]
    pub allowed_chat_ids: Vec<String>,
}

impl TelegramCredentials {
    /// Convenience accessor — the first chat_id captured during
    /// onboarding, used as the "owner" everywhere unsolicited DMs are
    /// sent. Returns `None` only on pathologically empty records.
    pub fn owner_chat_id(&self) -> Option<&str> {
        self.allowed_chat_ids.first().map(|s| s.as_str())
    }
}

/// Top-level credentials document persisted to
/// `~/.ccteam/im/credentials.json`. Each platform key is `Option` so
/// partial onboarding state round-trips cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub telegram: Option<TelegramCredentials>,
    // V0.7: pub slack: Option<SlackCredentials>,
    // V0.7: pub discord: Option<DiscordCredentials>,
}

/// Resolve the absolute path to the credentials file
/// (`~/.ccteam/im/credentials.json`). Honors `HOME` / Windows
/// equivalents via the `dirs` crate. Falls back to `/tmp` on systems
/// without a home directory (tests rely on env-var overrides
/// upstream, so this is best-effort only).
pub fn credentials_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(".ccteam").join("im").join("credentials.json")
}

/// Load the credentials document from disk. Returns
/// `Ok(Credentials::default())` when the file does not exist (first
/// run); returns an error only on read / parse failure.
pub fn load_credentials() -> Result<Credentials> {
    load_credentials_from(&credentials_path())
}

/// Like [`load_credentials`] but with an explicit path — used by
/// integration tests that override `HOME`.
pub fn load_credentials_from(path: &std::path::Path) -> Result<Credentials> {
    if !path.exists() {
        return Ok(Credentials::default());
    }
    let body = fs::read_to_string(path)
        .with_context(|| format!("read credentials file {path:?}"))?;
    let creds = serde_json::from_str(&body)
        .with_context(|| format!("parse credentials JSON at {path:?}"))?;
    Ok(creds)
}

/// Persist `creds` to `~/.ccteam/im/credentials.json` with 0600
/// permissions. Creates the parent dir if missing. Atomic-ish:
/// writes to `<path>.tmp` then renames.
pub fn write_credentials(creds: &Credentials) -> Result<()> {
    write_credentials_to(&credentials_path(), creds)
}

/// Like [`write_credentials`] but with an explicit path.
pub fn write_credentials_to(path: &std::path::Path, creds: &Credentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let body = serde_json::to_string_pretty(creds).context("serialize credentials")?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body).with_context(|| format!("write {tmp:?}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&tmp, perms)
            .with_context(|| format!("chmod 0600 {tmp:?}"))?;
    }

    fs::rename(&tmp, path)
        .with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let c = Credentials::default();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("creds.json");
        write_credentials_to(&p, &c).unwrap();
        let back = load_credentials_from(&p).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn round_trip_telegram() {
        let c = Credentials {
            telegram: Some(TelegramCredentials {
                bot_token: "123:abc".into(),
                bot_username: "@helpful_assistant".into(),
                allowed_chat_ids: vec!["987".into()],
            }),
        };
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("creds.json");
        write_credentials_to(&p, &c).unwrap();
        let back = load_credentials_from(&p).unwrap();
        assert_eq!(c, back);
        assert_eq!(c.telegram.as_ref().unwrap().owner_chat_id(), Some("987"));
    }

    #[cfg(unix)]
    #[test]
    fn write_is_chmod_0600() {
        use std::os::unix::fs::PermissionsExt;
        let c = Credentials::default();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("creds.json");
        write_credentials_to(&p, &c).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.json");
        let creds = load_credentials_from(&p).unwrap();
        assert_eq!(creds, Credentials::default());
    }

    #[test]
    fn platform_as_str_stable() {
        assert_eq!(ImPlatform::Telegram.as_str(), "telegram");
        assert_eq!(ImPlatform::Slack.as_str(), "slack");
        assert_eq!(ImPlatform::Discord.as_str(), "discord");
    }
}
