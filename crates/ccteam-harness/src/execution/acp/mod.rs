//! Shared ACP (Agent Client Protocol) stdio core for Grok + OpenCode.
//!
//! Transport / translate / protocol types are vendor-neutral. Vendor-specific
//! spawn argv and adapter wiring live in:
//! - [`crate::execution::grok_acp`]
//! - [`crate::execution::opencode_acp`]

pub mod protocol;
pub mod translate;
pub mod transport;

pub use protocol::{
    content_text, cost_from_usage_update, is_replay, is_turn_boundary, pluck_model_info,
    pluck_session_id, usage_from_prompt_result, AvailableCommand, ModelInfo,
};
pub use translate::{
    apply_notification, apply_notification_shared, fail_turn, finalize_from_prompt_result,
    SessionTranslateState, TurnBuffer,
};
pub use transport::{AcpTransport, InboundPolicy, JsonRpcError, Notification};
