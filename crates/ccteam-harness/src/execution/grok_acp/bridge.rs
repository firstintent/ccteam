//! HITL bridge for `session/request_permission` (W5).
//!
//! MVP: transport default-declines inbound requests. This module is a
//! placeholder for the future IM approval resolver wiring.

/// Future: resolve an ACP permission request for `sid`.
#[allow(dead_code)]
pub trait PermissionResolver: Send + Sync {
    // W5 will flesh this out.
}
