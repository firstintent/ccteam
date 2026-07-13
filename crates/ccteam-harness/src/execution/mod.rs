//! Execution support modules shared by concrete harness adapters.
//!
pub mod acp;
pub mod claude_bg;
pub mod claude_common;
pub mod claude_stream_json;
pub mod claude_tui;
pub mod codex_app_server;
pub mod codex_exec;
pub mod codex_jsonrpc;
pub mod codex_typed_events;
pub mod delegation;
pub mod experience;
pub mod fs_atomic;
pub mod grok_acp;
pub mod mcp_config;
pub mod opencode_acp;
pub mod process_inspect;
pub mod progress_bridge;
pub mod remote_exec;
pub mod session_meta;
pub mod session_recovery;
pub mod transcript_tail;
pub mod turns_mirror;
pub mod typed_events;

pub use claude_bg::ClaudeBgAdapter;
pub use claude_stream_json::ClaudeStreamJsonAdapter;
pub use claude_tui::ClaudeTuiAdapter;
pub use codex_app_server::CodexAppServerAdapter;
pub use codex_exec::CodexExecAdapter;
pub use grok_acp::GrokAcpAdapter;
pub use opencode_acp::OpencodeAcpAdapter;
