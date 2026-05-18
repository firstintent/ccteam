//! ccteam-imd — IM-daemon helpers for ccteam V0.6.0+ chat workflows.
//!
//! This crate hosts the platform-specific onboarding flows (Telegram /
//! Slack / Discord token validation, `chat_id` auto-detection) plus
//! credential persistence under `~/.ccteam/im/`. It is intentionally a
//! **separate crate** from `ccteam-core` so the heavyweight HTTP client
//! (reqwest + rustls) doesn't pull into every consumer of core.
//!
//! ## Modules
//!
//! - [`onboarding`] — `telegram_setup`, future `slack_setup`,
//!   `discord_setup`. Each performs a `getMe`-style token verify + a
//!   long-poll for `chat_id` capture, returns typed credentials.
//! - [`credentials`] — `~/.ccteam/im/credentials.json` reader/writer
//!   with 0600 permission enforcement.
//! - [`register`] — `register_bot()` API surface consumed by
//!   `ccteam-creator` Phase 5. Returns a `BotRegistration` handle the
//!   skill embeds in the rendered `workflow.yaml` + the "好了,在 TG 找
//!   `@xxx`" user reply.
//!
//! ## Stability
//!
//! V0.6.0 Wave 2 lands the Telegram path end-to-end. Slack + Discord
//! are stubbed with `unimplemented!()` and a TODO referencing the
//! follow-up wave. The `register_bot()` signature is locked across the
//! whole V0.6 line so ccteam-creator can compile against it before the
//! Slack / Discord branches land.

pub mod credentials;
pub mod onboarding;
pub mod register;

pub use credentials::{
    default_path, load_credentials, save, write_credentials, Credentials, ImPlatform,
    TelegramCreds,
};
#[allow(deprecated)]
pub use credentials::{credentials_path, TelegramCredentials};
pub use onboarding::{telegram_setup, OnboardingError, TelegramSetupResult};
pub use register::{register_bot, AgentVendor, BotRegistration, RegisterError};
