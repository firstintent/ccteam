//! MCP tool group enum + `CCTEAM_DISABLE_TOOLS` env filter.
//!
//! Canonical implementation lives in [`ccteam_im::mcp::groups`]. This module
//! re-exports the pieces doctor still imports via `crate::mcp_tool_groups`.

pub use ccteam_im::mcp::{group_for_tool, STUB_TOOLS};
