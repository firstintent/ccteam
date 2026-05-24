//! Credentials reader for `~/.ccteam/im/credentials.json`.
//!
//! File layout (mode 0600 enforced on POSIX):
//!
//! ```json
//! {
//!   "telegram": { "bot_token": "...", "allowed_chat_ids": ["12345"] },
//!   "slack":    { "bot_token": "xoxb-...", "signing_secret": "..." },
//!   "discord":  { "bot_token": "...", "authorized_user_ids": ["..."] }
//! }
//! ```
//!
//! All platform fields are optional — the daemon enables a Channel
//! only when its credentials block is present and complete.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level credentials document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    /// Telegram bot credentials (long-polling getUpdates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TelegramCreds>,
    /// Slack bot credentials (HTTP chat.postMessage + signing-secret
    /// HMAC verify on incoming events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackCreds>,
    /// Discord bot credentials (REST messages API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordCreds>,
}

/// Telegram bot credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelegramCreds {
    /// The token printed by @BotFather.
    pub bot_token: String,
    /// Chat IDs the daemon is allowed to read from. Empty = accept
    /// any chat the bot has been added to (NOT recommended for prod).
    #[serde(default)]
    pub allowed_chat_ids: Vec<String>,
}

/// Slack bot credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlackCreds {
    /// `xoxb-...` bot token.
    pub bot_token: String,
    /// Used to verify the `X-Slack-Signature` header on incoming
    /// webhook events. Required when running the inbound HTTP
    /// receiver; optional when running pure HTTP polling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<String>,
    /// Channels the daemon should poll. Empty = polling disabled.
    #[serde(default)]
    pub poll_channels: Vec<String>,
}

/// Discord bot credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscordCreds {
    /// Bot token (`Authorization: Bot <token>`).
    pub bot_token: String,
    /// Discord user IDs allowed to direct the bot. Empty = anyone in
    /// the bound channel.
    #[serde(default)]
    pub authorized_user_ids: Vec<String>,
}

/// Default credentials file path.
pub fn default_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".ccteam")
        .join("im")
        .join("credentials.json")
}

/// Load credentials from the given path (or [`default_path`] when
/// `None`). Returns an empty [`Credentials`] when the file is missing
/// — the daemon treats "no creds → no channels" as a normal startup
/// state for mock / dev use.
pub fn load(path: Option<&Path>) -> Result<Credentials> {
    let owned;
    let p: &Path = match path {
        Some(p) => p,
        None => {
            owned = default_path();
            &owned
        }
    };
    if !p.exists() {
        return Ok(Credentials::default());
    }
    let body =
        fs::read_to_string(p).with_context(|| format!("read credentials at {}", p.display()))?;
    let creds: Credentials = serde_json::from_str(&body)
        .with_context(|| format!("parse credentials at {}", p.display()))?;
    #[cfg(unix)]
    enforce_0600(p)?;
    Ok(creds)
}

/// Persist credentials to disk with mode 0600 (POSIX only). Used by
/// `ccteam-creator` skill when first storing a paste-time bot token;
/// production use is read-only.
pub fn save(path: &Path, creds: &Credentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(creds).context("serialize credentials")?;
    fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(path)?.permissions();
        perm.set_mode(0o600);
        fs::set_permissions(path, perm)?;
    }
    Ok(())
}

#[cfg(unix)]
fn enforce_0600(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perm = fs::metadata(p)?.permissions();
    let mode = perm.mode() & 0o777;
    if mode & 0o077 != 0 {
        // The file is readable by group/others — refuse to load.
        anyhow::bail!(
            "credentials file {} has mode {:o}; must be 0600 (chmod 600 {})",
            p.display(),
            mode,
            p.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("none.json");
        let creds = load(Some(&path)).unwrap();
        assert!(creds.telegram.is_none());
        assert!(creds.slack.is_none());
        assert!(creds.discord.is_none());
    }

    #[test]
    fn round_trip_telegram_only() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.json");
        let original = Credentials {
            telegram: Some(TelegramCreds {
                bot_token: "TEST:abc".into(),
                allowed_chat_ids: vec!["12345".into()],
            }),
            ..Default::default()
        };
        save(&path, &original).unwrap();
        let back = load(Some(&path)).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    #[cfg(unix)]
    fn rejects_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("loose.json");
        let original = Credentials {
            telegram: Some(TelegramCreds {
                bot_token: "x".into(),
                allowed_chat_ids: vec![],
            }),
            ..Default::default()
        };
        save(&path, &original).unwrap();
        // Loosen to 0644 — load should refuse.
        let mut p = fs::metadata(&path).unwrap().permissions();
        p.set_mode(0o644);
        fs::set_permissions(&path, p).unwrap();
        let err = load(Some(&path)).unwrap_err();
        assert!(err.to_string().contains("must be 0600"));
    }
}
