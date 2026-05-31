//! V0.6.0 F107 — adapter implementations behind the new
//! [`ccteam_harness::HarnessAdapter`] trait.
//!
//! - [`claude_bg`] — V0.5.x `ClaudeCodeAdapter` renamed +
//!   migrated to the 5-method trait shape. Zero behaviour change vs.
//!   V0.5.1.
//! - [`claude_tui`] — Wave 2 F108 fills in tmux long-session +
//!   send-keys -l direct user-content passthrough + dual-track
//!   transcript polling.
//! - [`codex_exec`] — V0.5.x `CodexAdapter` renamed + migrated.
//!   Wave 3 F112 fills `submit_turn` / `resume_thread` / event stream
//!   via `codex exec --json` stdin pipe + `codex resume <UUID> --json`.
//! - [`codex_app_server`] — Wave 3 F112 mode-3 codex bot path; talks
//!   to `codex app-server` over UDS via
//!   [`ccteam_harness::execution::codex_jsonrpc`].

pub mod claude_bg;
pub mod claude_tui;
pub mod codex_app_server;
pub mod codex_exec;
// V0.8 rmux — Slice 1 typed-event consumer (gated by CCTEAM_TYPED_EVENTS).
pub mod typed_events;
// V0.8 rmux Slice 4 — Codex mode-3 typed-event producer (gated by same flag).
// Bypasses EventMerger; writes progress.jsonl rows directly from the
// `app-server` JSON-RPC notification stream. See module docs.
pub mod codex_typed_events;

pub use claude_bg::ClaudeBgAdapter;
pub use claude_tui::ClaudeTuiAdapter;
pub use codex_app_server::CodexAppServerAdapter;
pub use codex_exec::CodexExecAdapter;
