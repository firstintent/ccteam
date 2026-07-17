//! HITL bridge for `session/request_permission`.
//!
//! MVP: transport policy decides inbound permission requests (skip →
//! auto-allow, hitl → default-decline). This module is a placeholder for
//! the future IM approval resolver wiring.

/// Future: resolve an ACP permission request for `sid`.
#[allow(dead_code)]
pub trait PermissionResolver: Send + Sync {
    // W5 will flesh this out.
}
