//! Re-export shared ACP transport (Kimi uses the opencode-style inbound
//! policy: skip → auto-allow permission requests, hitl → default-decline).
pub use crate::execution::acp::transport::*;
