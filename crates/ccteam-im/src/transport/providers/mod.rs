//! Per-platform [`crate::transport::Channel`] implementations.
//!
//! `mock` is always built (used by tests). The three real providers
//! are cargo-feature gated so a slim build can be produced for, e.g.,
//! a Slack-only deployment.

pub mod mock;

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "slack")]
pub mod slack;

#[cfg(feature = "discord")]
pub mod discord;
