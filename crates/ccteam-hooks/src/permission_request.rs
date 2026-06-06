//! `ccteam hook permission-request` — `PermissionRequest` hook for HITL
//! (human-in-the-loop) chat sessions (v0.8.7 W2, DB.3).
//!
//! SMOKE-GATE GROUND TRUTH (proven on a real claude binary): under an
//! interactive `claude --agent <role> --name <sid> --permission-mode default`
//! session, the `PermissionRequest` hook fires for a tool call ONLY when the
//! per-tool permission decision is "ask" — i.e. a non-allowlist tool.
//! Allowlist / auto-allowed tools fire NO hook. That is the whole leverage:
//! ccteam does NOT parse the allowlist; it lets Claude decide what needs a
//! prompt, and only those reach this handler.
//!
//! Behavior: parse the `PermissionRequest` payload from stdin (`tool_name`,
//! `tool_input`, `session_id`, `cwd`), forward it to the daemon over the
//! `mcp.sock` (`permission/ask`, mirroring the v0.8.5 D6 `interaction/ask`
//! forwarder), and BLOCK until the IM user clicks approve / deny (the daemon
//! enforces a ~600s TTL → default deny). The handler then prints the exact
//! `PermissionRequest` decision JSON to stdout and exits 0.
//!
//! FAIL SAFE = DENY: any error (no slug, daemon unreachable, malformed
//! response, timeout) returns a `deny` decision so a supervised session never
//! silently runs an un-approved tool.
//!
//! Return contract (Claude Code `PermissionRequest` hook):
//!
//! ```json
//! {
//!   "hookSpecificOutput": {
//!     "hookEventName": "PermissionRequest",
//!     "decision": { "behavior": "allow" | "deny", "message"?: "<reason>" }
//!   }
//! }
//! ```
//!
//! `behavior:"deny"` blocks just this one tool call (it is NOT a session
//! kill — red line). We do not set `interrupt`, so the turn continues and the
//! assistant sees the denial as a tool error.

use ccteam_core::CcteamPaths;
use serde_json::{json, Value};

use crate::intercept_ask::{mcp_socket_roundtrip, non_empty_env};

/// The deny message surfaced to the assistant when a tool is blocked / the
/// approval path is unavailable. Short + bilingual.
const DENY_MESSAGE: &str =
    "用户未批准该工具调用（或审批通道不可用）。Tool call not approved by the user.";

/// Build the `PermissionRequest` hook decision JSON. `allow == true` →
/// `behavior: "allow"`; otherwise `behavior: "deny"` with [`DENY_MESSAGE`].
/// Pure — the CLI / daemon dispatcher prints whatever this returns.
pub fn decision(allow: bool) -> Value {
    let inner = if allow {
        json!({ "behavior": "allow" })
    } else {
        json!({ "behavior": "deny", "message": DENY_MESSAGE })
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": inner,
        }
    })
}

/// HITL decision for one `PermissionRequest` firing. Asks the IM user over
/// the daemon `mcp.sock`; ALLOW only on an explicit approve click. Any
/// failure / timeout / deny click → [`decision(false)`] (fail-safe deny).
///
/// `stdin` is the full Claude Code `PermissionRequest` payload.
pub fn permission_request_decide(paths: &CcteamPaths, stdin: &Value) -> Value {
    match try_permission_request(paths, stdin) {
        Some(true) => decision(true),
        // Explicit deny OR any inability to ask → deny (fail safe).
        Some(false) | None => decision(false),
    }
}

/// Fallible core. `Some(true)` = approved, `Some(false)` = explicitly denied
/// (or daemon reported timeout), `None` = couldn't even ask (no slug, no
/// tool, daemon unreachable, malformed response) → caller denies.
fn try_permission_request(paths: &CcteamPaths, stdin: &Value) -> Option<bool> {
    // Resolve slug/role the same way intercept-ask does: stdin (daemon HTTP
    // path folds X-Ccteam-{Slug,Role} headers into stdin) first, then the
    // ambient `CCTEAM_CHAT_{SLUG,ROLE}` env (cold CLI path inside claude's
    // tmux pane). No slug ⇒ we can't address an IM chat ⇒ fail-safe deny.
    let slug = stdin
        .get("slug")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| non_empty_env("CCTEAM_CHAT_SLUG"))?;
    let role = stdin
        .get("role")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| non_empty_env("CCTEAM_CHAT_ROLE"))
        .unwrap_or_default();

    let tool_name = stdin
        .get("tool_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    // tool_input is free-form; forward it so the daemon can render a short
    // human summary. Absent → empty object (still ask — a tool with no input
    // is still a tool the user should approve).
    let tool_input = stdin
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    // session_id + cwd let the daemon map this firing back to a gateway sid
    // (for richer "session sX (role) wants to run …" addressing). Optional —
    // the daemon falls back to slug/role addressing when absent.
    let session_id = stdin
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let cwd = stdin
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "permission/ask",
        "params": {
            "slug": slug,
            "role": role,
            "tool_name": tool_name,
            "tool_input": tool_input,
            "session_id": session_id,
            "cwd": cwd,
        },
    });

    let socket = ccteam_core::daemon_socket_path(paths);
    let response = mcp_socket_roundtrip(&socket, &request)?;

    // Daemon response shapes:
    //   {"result":{"behavior":"allow"}}  → approved
    //   {"result":{"behavior":"deny"}}   → explicitly denied
    //   {"result":{"timeout":true}}      → no answer in TTL → deny
    //   {"error":{...}}                   → couldn't ask → deny (None)
    let result = response.get("result")?;
    if result
        .get("timeout")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Some(false);
    }
    match result.get("behavior").and_then(|v| v.as_str()) {
        Some("allow") => Some(true),
        Some("deny") => Some(false),
        // Unrecognized result → couldn't get a clean decision → deny.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn decision_allow_shape() {
        let v = decision(true);
        let out = v.get("hookSpecificOutput").expect("hookSpecificOutput");
        assert_eq!(
            out.get("hookEventName").and_then(|s| s.as_str()),
            Some("PermissionRequest")
        );
        assert_eq!(
            out.pointer("/decision/behavior").and_then(|s| s.as_str()),
            Some("allow")
        );
        // allow carries no deny message.
        assert!(out.pointer("/decision/message").is_none());
    }

    #[test]
    fn decision_deny_shape_carries_message() {
        let v = decision(false);
        let out = v.get("hookSpecificOutput").expect("hookSpecificOutput");
        assert_eq!(
            out.pointer("/decision/behavior").and_then(|s| s.as_str()),
            Some("deny")
        );
        assert!(
            out.pointer("/decision/message")
                .and_then(|s| s.as_str())
                .is_some(),
            "deny must carry a message the assistant can read"
        );
        // We must NOT set interrupt — deny blocks one tool, not the turn.
        assert!(out.pointer("/decision/interrupt").is_none());
    }

    /// With no slug ambient (bg / autonomous), the decider must fail-safe to
    /// deny WITHOUT touching a socket. (Env is process-global; only the
    /// no-slug branch is asserted, so the var is not set here.)
    #[test]
    fn no_slug_fails_safe_to_deny() {
        if std::env::var("CCTEAM_CHAT_SLUG").is_ok() {
            return; // a sibling leaked the var — skip rather than flake.
        }
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let stdin = json!({
            "tool_name": "Bash",
            "tool_input": { "command": "rm -rf /tmp/x" }
        });
        let v = permission_request_decide(&paths, &stdin);
        assert_eq!(
            v.pointer("/hookSpecificOutput/decision/behavior"),
            Some(&json!("deny")),
            "no slug ⇒ fail-safe deny"
        );
    }
}
