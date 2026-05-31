//! Execution support modules shared by concrete harness adapters.
//!
//! The concrete Claude/Codex adapters still live in `ccteam-core` for
//! this slice because they call core progress and workflow helpers.
//! Pure transport, transcript, recovery, and registry helpers live here
//! so IM/CLI/hooks can stop reaching through `ccteam_core::execution`.

pub mod claude_bg;
pub mod codex_jsonrpc;
pub mod marker_reporter;
pub mod process_inspect;
pub mod session_recovery;
pub mod transcript_tail;
pub mod turns_mirror;

pub use claude_bg::ClaudeBgAdapter;
