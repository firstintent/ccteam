//! Shared MCP protocol core + daemon-side dispatch.
//!
//! Lives in `ccteam-im` (not `ccteam-cli`) so `ccteam-web` can mount
//! `POST /mcp` later without a dependency cycle (`cli → web → im`).
//!
//! Public surface is the minimum cli + web need:
//! - [`handle_request`] / [`tool_definitions`] — transport-agnostic protocol
//! - [`McpDispatch`] — stateful intercepts for the daemon
//! - [`face`] — the per-caller tool face (`ToolFace`)
//! - [`groups`] — `ToolGroup` / `CCTEAM_DISABLE_TOOLS` / `STUB_TOOLS`

/// Daemon-side stateful MCP intercepts.
pub mod dispatch;
/// Per-caller tool face composition (`initialize` / `tools/list`).
pub mod face;
/// Tool-group enum + `CCTEAM_DISABLE_TOOLS` filter.
pub mod groups;
/// Transport-agnostic JSON-RPC protocol core (local tools + schemas).
pub mod protocol;
/// v0.10 T1 — MCP `status` vendor panel + routing-notes transport.
mod vendor_panel;

pub use dispatch::{GatewayEventSink, GatewayHandle, McpCaller, McpDispatch, PendingRegistry};
pub use face::{FaceIdentity, ToolFace};
pub use groups::{
    disabled_groups_from_env, filter_by_disabled, group_for_tool, parse_disable_env, ToolGroup,
    STUB_TOOLS,
};
pub use protocol::{
    chat_tool_definitions, handle_request, instructions_for, is_known_tool, is_session_tool,
    session_tool_definitions, tool_definitions, MCP_PROTOCOL_VERSION, SESSION_TOOL_NAMES,
    STATUS_BEACON_TOOL_NAME, SUPPORTED_PROTOCOL_VERSIONS,
};
