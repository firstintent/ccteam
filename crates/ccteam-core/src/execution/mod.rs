//! Back-compat re-exports for adapter implementations behind the
//! [`ccteam_harness::HarnessAdapter`] trait.
//!
//! Claude TUI execution now lives in `ccteam-harness`; this module keeps
//! older `ccteam_core::execution::*` call sites compiling while the
//! app-server adapter moves over.
//! - [`codex_app_server`] — Wave 3 F112 mode-3 codex bot path; talks
//!   to `codex app-server` over UDS via
//!   [`ccteam_harness::execution::codex_jsonrpc`].

pub mod codex_app_server;
// V0.8 rmux Slice 4 — Codex mode-3 typed-event producer (gated by same flag).
// Bypasses EventMerger; writes progress.jsonl rows directly from the
// `app-server` JSON-RPC notification stream. See module docs.
pub mod codex_typed_events;

pub use ccteam_harness::execution::{
    claude_tui, codex_exec, typed_events, ClaudeTuiAdapter, CodexExecAdapter,
};
pub use codex_app_server::CodexAppServerAdapter;
