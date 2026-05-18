//! V0.6.0 F107 — adapter implementations behind the new
//! [`crate::harness::HarnessAdapter`] trait.
//!
//! - [`claude_bg`] — V0.5.x `ClaudeCodeAdapter` renamed +
//!   migrated to the 5-method trait shape. Zero behaviour change vs.
//!   V0.5.1.
//! - [`claude_tui`] — Wave 1 STUB; Wave 2 F108 fills in tmux
//!   long-session + send-keys -l direct user-content passthrough +
//!   dual-track transcript polling.
//! - [`codex_exec`] — V0.5.x `CodexAdapter` renamed + migrated.
//!   Wave 3 F112 will fill in `codex exec --json` + thread/start UDS;
//!   Wave 1 retains tmux + capture-pane parity with V0.5.1.

pub mod claude_bg;
pub mod claude_tui;
pub mod codex_exec;
// V0.6.0 F108 / F118 — chat-mode helpers consumed by ClaudeTuiAdapter.
pub mod session_recovery;
pub mod transcript_tail;
pub mod turns_mirror;

pub use claude_bg::ClaudeBgAdapter;
pub use claude_tui::ClaudeTuiAdapter;
pub use codex_exec::CodexExecAdapter;
