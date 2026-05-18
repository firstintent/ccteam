//! ccteam-hooks: handlers for Claude Code hook events. Each handler
//! takes the parsed stdin payload (`serde_json::Value`) and the resolved
//! `CcteamPaths`, performs its side-effecting append / state mutation,
//! and returns. The `ccteam` binary's `hook` subcommand group reads
//! stdin once and dispatches to these.
//!
//! Wire-up reference: `docs/interfaces.md` §6.1 (settings.json template)
//! and §6.2 / §6.3 (per-hook responsibilities).

pub mod chat_progress;
pub mod intercept_ask;
pub mod load_context;
pub mod progress;

pub use chat_progress::handle_chat_progress;
pub use intercept_ask::intercept_ask_decision;
pub use load_context::load_context;
pub use progress::progress_append;
