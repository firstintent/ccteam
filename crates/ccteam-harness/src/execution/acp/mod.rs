//! Shared ACP (Agent Client Protocol) stdio core for Grok + OpenCode.
//!
//! Transport / translate / protocol types are vendor-neutral. Vendor-specific
//! spawn argv and adapter wiring live in:
//! - [`crate::execution::grok_acp`]
//! - [`crate::execution::opencode_acp`]

pub mod context_probe;
pub mod protocol;
pub mod translate;
pub mod transport;
pub mod turn_runner;

pub use context_probe::{AcpContextProbe, KIMI_STATUS_PROBE};
pub use protocol::{
    acp_model_picker_options, content_text, cost_from_usage_update, is_replay, is_turn_boundary,
    known_efforts, pluck_model_info, pluck_session_id, split_trailing_effort,
    stop_reason_from_prompt_result, usage_from_prompt_result, AcpModelOption, AcpStopReason,
    AvailableCommand, ModelInfo,
};
pub use translate::{
    apply_notification, apply_notification_shared, fail_turn, finalize_from_prompt_result,
    SessionTranslateState, TurnBuffer,
};
pub use transport::{AcpTransport, InboundPolicy, JsonRpcError, Notification};
pub use turn_runner::{route_acp_turn, AcpTurnRoute, AcpTurnRunner, AcpTurnTuning};

use std::path::Path;

use crate::execution::session_status::read_status_file;
use crate::{ThreadHandle, ThreadStatus};

/// Status for a session this adapter no longer holds live (idle-released,
/// capacity-evicted, or a daemon restart away): read the snapshot written at
/// its last turn boundary, keyed off the handle's own `project_dir` + `sid`.
///
/// An ACP session's status lives in process memory, so without this a
/// released session reports nothing at all — the statusline blanks for
/// exactly the long-lived sessions whose context matters most. All-`None`
/// only when no snapshot exists (a session that never completed a turn).
pub fn released_thread_status(h: &ThreadHandle) -> ThreadStatus {
    let project_dir = h.raw_extras.get("project_dir").and_then(|v| v.as_str());
    let sid = h.raw_extras.get("sid").and_then(|v| v.as_str());
    match (project_dir, sid) {
        (Some(dir), Some(sid)) => read_status_file(Path::new(dir), sid).unwrap_or_default(),
        _ => ThreadStatus::default(),
    }
}
