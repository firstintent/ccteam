//! `ccteam hook intercept-ask` — PreToolUse hook for `AskUserQuestion`.
//!
//! V0.2 M0.19.3 (PRD §2.4 / alignment-review §3.2). The phase
//! self-loop's Stop-hook fallback can't catch `AskUserQuestion`
//! because that tool blocks the assistant turn synchronously — Stop
//! never fires while the model waits for an answer. The PreToolUse
//! hook intercepts the call before the wait and returns a structured
//! `permissionDecision: deny` so Claude Code feeds the deny reason
//! back into the conversation. The assistant is then forced to route
//! through the outbox / clarify protocol instead.
//!
//! Wire-up:
//!
//! ```json
//! "PreToolUse": [{
//!   "matcher": "AskUserQuestion",
//!   "hooks": [{"type": "command", "command": "<ccteam-bin> hook intercept-ask"}]
//! }]
//! ```
//!
//! Output schema (Claude Code `hooks.ts:608-625`):
//!
//! ```json
//! {
//!   "hookSpecificOutput": {
//!     "hookEventName": "PreToolUse",
//!     "permissionDecision": "deny",
//!     "permissionDecisionReason": "<reason>"
//!   }
//! }
//! ```

use serde_json::{json, Value};

/// The deny reason the assistant sees inline. Kept bilingual + short
/// so a single re-prompt fits well within Claude Code's tool-error
/// budget.
const DENY_REASON: &str = "本 phase 应自决,不能用 AskUserQuestion 阻塞等待用户。\
                           询问用户的唯一合法出口是写 .ccteam/outbox/clarify-<ts>.md,\
                           Stop hook 后写 PHASE_DONE_PENDING。";

/// Build the Claude Code PreToolUse hook decision JSON. Pure — the
/// CLI dispatcher prints whatever this returns to stdout.
pub fn intercept_ask_decision() -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": DENY_REASON,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_targets_pre_tool_use_with_deny() {
        let v = intercept_ask_decision();
        let outer = v
            .get("hookSpecificOutput")
            .expect("payload has hookSpecificOutput");
        assert_eq!(
            outer.get("hookEventName").and_then(|s| s.as_str()),
            Some("PreToolUse"),
        );
        assert_eq!(
            outer.get("permissionDecision").and_then(|s| s.as_str()),
            Some("deny"),
        );
        let reason = outer
            .get("permissionDecisionReason")
            .and_then(|s| s.as_str())
            .expect("reason present");
        assert!(
            reason.contains("outbox"),
            "deny reason must steer the assistant to outbox: {reason}",
        );
    }
}
