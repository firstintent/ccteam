//! Back-compat re-exports for adapter implementations behind the
//! [`ccteam_harness::HarnessAdapter`] trait.
//!
//! Claude TUI execution now lives in `ccteam-harness`; this module keeps
//! older `ccteam_core::execution::*` call sites compiling.

pub use ccteam_harness::execution::{
    claude_tui, codex_app_server, codex_exec, codex_typed_events, typed_events, ClaudeTuiAdapter,
    CodexAppServerAdapter, CodexExecAdapter,
};
