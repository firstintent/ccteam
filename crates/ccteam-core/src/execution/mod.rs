//! Back-compat re-exports for adapter implementations behind the
//! [`ccteam_harness::HarnessAdapter`] trait.
//!
//! Claude TUI execution now lives in `ccteam-harness`; this module keeps
//! older `ccteam_core::execution::*` call sites compiling while the rest
//! of the concrete adapters move over.
//! - [`codex_exec`] — V0.5.x `CodexAdapter` renamed + migrated.
//!   Wave 3 F112 fills `submit_turn` / `resume_thread` / event stream
//!   via `codex exec --json` stdin pipe + `codex resume <UUID> --json`.
//! - [`codex_app_server`] — Wave 3 F112 mode-3 codex bot path; talks
//!   to `codex app-server` over UDS via
//!   [`ccteam_harness::execution::codex_jsonrpc`].

pub mod codex_app_server;
pub mod codex_exec;
// V0.8 rmux Slice 4 — Codex mode-3 typed-event producer (gated by same flag).
// Bypasses EventMerger; writes progress.jsonl rows directly from the
// `app-server` JSON-RPC notification stream. See module docs.
pub mod codex_typed_events;

pub use ccteam_harness::execution::{claude_tui, typed_events, ClaudeTuiAdapter};
pub use codex_app_server::CodexAppServerAdapter;
pub use codex_exec::CodexExecAdapter;
