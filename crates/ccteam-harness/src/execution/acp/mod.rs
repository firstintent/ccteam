//! Shared ACP (Agent Client Protocol) stdio core for Grok + OpenCode.
//!
//! Transport / translate / protocol types are vendor-neutral. Vendor-specific
//! spawn argv and adapter wiring live in:
//! - [`crate::execution::grok_acp`]
//! - [`crate::execution::opencode_acp`]

pub mod protocol;
pub mod translate;
pub mod transport;
pub mod turn_runner;

pub use protocol::{
    acp_model_picker_options, content_text, cost_from_usage_update, is_replay, is_turn_boundary,
    known_efforts, pluck_model_info, pluck_session_id, split_trailing_effort,
    usage_from_prompt_result, AcpModelOption, AvailableCommand, ModelInfo,
};
pub use translate::{
    apply_notification, apply_notification_shared, fail_turn, finalize_from_prompt_result,
    SessionTranslateState, TurnBuffer,
};
pub use transport::{AcpTransport, InboundPolicy, JsonRpcError, Notification};
pub use turn_runner::{route_acp_turn, AcpTurnRoute, AcpTurnRunner, AcpTurnTuning};
