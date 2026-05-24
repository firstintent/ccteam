//! ccteam-hooks: handlers for Claude Code hook events. Each handler
//! takes the parsed stdin payload (`serde_json::Value`) and the resolved
//! `CcteamPaths`, performs its side-effecting append / state mutation,
//! and returns. The `ccteam` binary's `hook` subcommand group reads
//! stdin once and dispatches to these.
//!
//! V0.6.1 F139 — the same dispatch is reused by the ccteam-web daemon's
//! `/internal/hook/:kind[/:action]` HTTP route via [`dispatch`] so a
//! warm daemon can answer hook firings in ~10 ms (curl round-trip)
//! instead of ~200 ms (cold `ccteam` binary spawn). The CLI path stays
//! as the supported fallback when the daemon is down — same library
//! call, just paid the binary start-up tax.
//!
//! Wire-up reference: `docs/interfaces.md` §6.1 (settings.json template)
//! and §6.2 / §6.3 (per-hook responsibilities).

use anyhow::{anyhow, Result};
use ccteam_core::CcteamPaths;
use serde_json::Value;

pub mod chat_progress;
pub mod intercept_ask;
pub mod load_context;
pub mod progress;

pub use chat_progress::handle_chat_progress;
pub use intercept_ask::intercept_ask_decision;
pub use load_context::load_context;
pub use progress::progress_append;

/// V0.6.1 F139 — unified dispatch shared by the CLI (`ccteam internal
/// hook <kind> [<action>]`) and the ccteam-web daemon's
/// `/internal/hook/:kind[/:action]` route.
///
/// `kind` is the subcommand name (`progress-append` / `load-context` /
/// `intercept-ask` / `chat-progress`). `action` is the subcommand's
/// required positional argument when the kind takes one
/// (`progress-append <event>` / `chat-progress <event>`), `None`
/// otherwise.
///
/// `stdin` is the parsed Claude Code hook payload — the same JSON the
/// CLI reads from `std::io::stdin().lock()`.
///
/// Returns:
/// - `Some(Value)` when the hook produces a Claude-Code-consumable JSON
///   decision (today: only `intercept-ask`).
/// - `None` when the hook is fire-and-forget (side-effect only). The
///   HTTP layer responds with `{}` so curl's stdout is empty (Claude
///   Code treats empty / whitespace stdout as "allow with no notes").
///
/// Errors propagate to the caller: the CLI prints them to stderr and
/// exits non-zero; the HTTP layer maps them to 5xx with the message in
/// the response body so the script's fallback path can log + retry
/// through the CLI.
pub fn dispatch(
    paths: &CcteamPaths,
    kind: &str,
    action: Option<&str>,
    stdin: &Value,
) -> Result<Option<Value>> {
    match kind {
        "progress-append" => {
            let event_type = action
                .ok_or_else(|| anyhow!("hook `progress-append` requires an event-type argument"))?;
            progress_append(paths, event_type, stdin)?;
            Ok(None)
        }
        "load-context" => {
            load_context(paths, stdin)?;
            Ok(None)
        }
        "intercept-ask" => Ok(Some(intercept_ask_decision())),
        "chat-progress" => {
            let event =
                action.ok_or_else(|| anyhow!("hook `chat-progress` requires an event argument"))?;
            handle_chat_progress(paths, event, stdin)?;
            Ok(None)
        }
        other => Err(anyhow!("unknown hook kind: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::CcteamPaths;
    use serde_json::json;
    use tempfile::TempDir;

    fn fake_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn dispatch_intercept_ask_returns_decision_json() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let v = dispatch(&paths, "intercept-ask", None, &json!({})).unwrap();
        let body = v.expect("intercept-ask must return a decision JSON value");
        assert_eq!(
            body["hookSpecificOutput"]["permissionDecision"],
            json!("deny"),
        );
    }

    #[test]
    fn dispatch_unknown_kind_errors() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let err = dispatch(&paths, "bogus", None, &json!({})).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown hook kind"),
            "expected unknown-kind error, got: {err}",
        );
    }

    #[test]
    fn dispatch_progress_append_requires_action() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let err = dispatch(&paths, "progress-append", None, &json!({"cwd": "/tmp"})).unwrap_err();
        assert!(
            format!("{err:#}").contains("event-type"),
            "expected missing-action error, got: {err}",
        );
    }

    #[test]
    fn dispatch_chat_progress_requires_action() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let err = dispatch(&paths, "chat-progress", None, &json!({"cwd": "/tmp"})).unwrap_err();
        assert!(
            format!("{err:#}").contains("event"),
            "expected missing-action error, got: {err}",
        );
    }
}
