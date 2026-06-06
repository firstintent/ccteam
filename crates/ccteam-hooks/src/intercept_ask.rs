//! `ccteam hook intercept-ask` — PreToolUse hook for `AskUserQuestion`.
//!
//! Two behaviors, selected at dispatch time by ambient env + daemon
//! reachability (see [`crate::dispatch`]):
//!
//! - **bg / autonomous** ([`intercept_ask_decision`]): the original V0.2
//!   M0.19.3 behavior. The phase self-loop's Stop-hook fallback can't catch
//!   `AskUserQuestion` (it blocks the assistant turn synchronously, so Stop
//!   never fires while the model waits). The PreToolUse hook returns a
//!   structured `permissionDecision: deny` so Claude Code feeds the deny
//!   reason back and the assistant routes through the outbox / clarify
//!   protocol instead of waiting on an offline user.
//!
//! - **chat / IM** ([`intercept_ask_chat`], v0.8.5 D6): in an IM-driven
//!   `mode: chat` session there IS a live user. The hook turns the
//!   `AskUserQuestion` into an IM choice prompt over the daemon `mcp.sock`
//!   (`interaction/ask`), blocks on the answer, and returns
//!   `permissionDecision: allow` with the user's selection pre-filled into
//!   `updatedInput.answers` so the tool resolves with the real answer
//!   instead of blocking the TUI. On timeout / error / no-slug it degrades
//!   to the bg deny.
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
//!     "permissionDecision": "deny" | "allow",
//!     "permissionDecisionReason"?: "<reason>",
//!     "updatedInput"?: { ...echoed tool_input..., "answers": {<q>:<label>} }
//!   }
//! }
//! ```

use ccteam_core::CcteamPaths;
use serde_json::{json, Value};

/// The deny reason the assistant sees inline. Kept bilingual + short
/// so a single re-prompt fits well within Claude Code's tool-error
/// budget.
const DENY_REASON: &str = "本 phase 应自决,不能用 AskUserQuestion 阻塞等待用户。\
                           询问用户的唯一合法出口是写 .ccteam/outbox/clarify-<ts>.md,\
                           Stop hook 后写 PHASE_DONE_PENDING。";

/// Read timeout for the `mcp.sock` round-trip. The DAEMON enforces the real
/// 600s answer TTL; this is a generous client-side guard (slightly above the
/// daemon TTL) so a wedged daemon never hangs the Claude TUI forever.
const SOCKET_READ_TIMEOUT_SECS: u64 = 660;

/// Build the Claude Code PreToolUse hook decision JSON (bg variant). Pure —
/// the CLI dispatcher prints whatever this returns to stdout.
pub fn intercept_ask_decision() -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": DENY_REASON,
        }
    })
}

/// v0.8.5 D6 — chat-variant decision. Parse the `AskUserQuestion`
/// `tool_input` from the hook `stdin` payload, ask the IM user over the
/// daemon `mcp.sock`, and return an `allow` decision with the answer
/// pre-filled. Any failure (no slug, daemon unreachable, timeout, parse
/// error) degrades to [`intercept_ask_decision`] (deny-with-reason).
///
/// `stdin` is the full Claude Code PreToolUse hook payload; the tool input
/// lives at `tool_input`.
pub fn intercept_ask_chat(paths: &CcteamPaths, stdin: &Value) -> Value {
    match try_intercept_ask_chat(paths, stdin) {
        Some(decision) => decision,
        None => intercept_ask_decision(),
    }
}

/// Fallible core of [`intercept_ask_chat`]: `None` ⇒ caller falls back to the
/// bg deny. Split out so each early-return reads as "can't do chat → deny".
fn try_intercept_ask_chat(paths: &CcteamPaths, stdin: &Value) -> Option<Value> {
    // Resolve slug/role from the stdin payload first, then env. The CLI
    // cold-fallback runs inside claude's tmux pane (which has the F175
    // `CCTEAM_CHAT_{SLUG,ROLE}` env); the daemon HTTP fast-path instead folds
    // them into the stdin `slug`/`role` fields from `X-Ccteam-{Slug,Role}`
    // headers (the daemon process doesn't inherit claude's env) — see
    // `ccteam-web` `internal_hook::inject_headers`. Honor both so D6 works on
    // either transport.
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

    let tool_input = stdin.get("tool_input")?;
    // AskUserQuestion tool_input: { questions: [{ question, header?,
    // options: [{label, description?}], multiSelect? }] }. W2 handles the
    // FIRST question (multi-question is a documented follow-up).
    let questions = tool_input.get("questions").and_then(|v| v.as_array())?;
    let first = questions.first()?;
    let question = first.get("question").and_then(|v| v.as_str())?.to_string();
    if question.is_empty() {
        return None;
    }
    let multi = first
        .get("multiSelect")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let options: Vec<String> = first
        .get("options")
        .and_then(|v| v.as_array())?
        .iter()
        .filter_map(|o| {
            // Each option is `{label, description?}`; tolerate a bare string.
            o.get("label")
                .and_then(|v| v.as_str())
                .or_else(|| o.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    if options.is_empty() {
        return None;
    }

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "interaction/ask",
        "params": {
            "slug": slug,
            "role": role,
            "question": question,
            "options": options,
            "multi": multi,
        },
    });

    let socket = ccteam_core::daemon_socket_path(paths);
    let response = mcp_socket_roundtrip(&socket, &request)?;

    // `{"result":{"answers":{<question>:<label>}}}` → allow with the answer
    // echoed into the tool input. Anything else (timeout / error) → deny.
    let answers = response.pointer("/result/answers")?;
    if !answers.is_object() {
        return None;
    }
    let mut updated_input = tool_input.clone();
    if let Some(obj) = updated_input.as_object_mut() {
        obj.insert("answers".to_string(), answers.clone());
    }
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": updated_input,
        }
    }))
}

/// `std::env::var` filtered to a non-empty value.
pub(crate) fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// Send one newline-framed JSON-RPC request to the daemon `mcp.sock` and read
/// one newline-framed JSON-RPC response. Blocking (the hook is a short-lived
/// subprocess). `None` on any IO / parse failure → the caller denies.
///
/// v0.8.7 W2 — `pub(crate)` so the `permission_request` hook can reuse the
/// exact same long-held (≤660s client guard, daemon owns the real TTL)
/// round-trip rather than duplicating the socket plumbing.
#[cfg(unix)]
pub(crate) fn mcp_socket_roundtrip(socket: &std::path::Path, request: &Value) -> Option<Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket).ok()?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(
            SOCKET_READ_TIMEOUT_SECS,
        )))
        .ok()?;
    let mut line = serde_json::to_string(request).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).ok()?;
    if buf.trim().is_empty() {
        return None;
    }
    serde_json::from_str(buf.trim()).ok()
}

#[cfg(not(unix))]
pub(crate) fn mcp_socket_roundtrip(_socket: &std::path::Path, _request: &Value) -> Option<Value> {
    None
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

    /// With no `CCTEAM_CHAT_SLUG` ambient (the bg / autonomous case), the chat
    /// variant must fall straight back to the deny decision — never block on a
    /// socket. (Env is process-global; this test only asserts the no-slug
    /// branch, so it does not set the var.)
    #[test]
    fn chat_variant_without_slug_falls_back_to_deny() {
        // Guard: only meaningful when the var is genuinely absent. If a
        // sibling test leaked it, skip rather than flake.
        if std::env::var("CCTEAM_CHAT_SLUG").is_ok() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let stdin = json!({
            "tool_input": {
                "questions": [{
                    "question": "Pick one",
                    "options": [{"label": "A"}, {"label": "B"}],
                    "multiSelect": false
                }]
            }
        });
        let v = intercept_ask_chat(&paths, &stdin);
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecision"],
            json!("deny"),
            "no slug ⇒ degrade to deny",
        );
    }
}
