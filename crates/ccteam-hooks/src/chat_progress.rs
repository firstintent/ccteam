//! `ccteam internal hook chat-progress <event>` — Claude Code hook
//! handler for `mode: chat` bots. Each event arg maps to one ccteam
//! `chat_*` progress.jsonl line (see
//! `ccteam_core::progress::{CHAT_SESSION_STARTED, CHAT_TURN_USER_PROMPT,
//! CHAT_TURN_COMPLETED, CHAT_SESSION_RESET, CHAT_COMPACT_DONE, ...}`).
//!
//! The 7 chat-mode event types tag onto Claude Code's hook events:
//!
//! | hook arg         | Claude hook         | ccteam emit                     |
//! |------------------|---------------------|---------------------------------|
//! | `session-start`  | SessionStart        | `chat_session_started`          |
//! | `user-prompt`    | UserPromptSubmit    | `chat_turn_user_prompt`         |
//! | `stop`           | Stop                | `chat_turn_completed`           |
//! | `subagent-stop`  | SubagentStop        | `chat_subagent_completed`       |
//! | `tool-use`       | PostToolUse         | `chat_tool_use`                 |
//! | `session-end`    | SessionEnd          | `chat_session_reset` (clear)    |
//! | `pre-compact`    | PreCompact          | `chat_pre_compact`              |
//! | `post-compact`   | PostCompact         | `chat_compact_done`             |
//! | other            | (forwarded as-is)   | `chat_<event>`                  |
//!
//! Unknown events arrive as `chat_<event>` so this layer never silently
//! drops a hook firing — the orchestrator can log + decide.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::Path;

use ccteam_core::progress::{
    append_event, build_chat_compact_done_event, build_chat_session_reset_event,
    build_chat_session_started_event, build_chat_turn_completed_event,
    build_chat_turn_user_prompt_event,
};
use ccteam_core::{session_context_from_cwd, CcteamPaths};

/// V0.6.0 F108 entry — dispatch one `chat-progress <event>` invocation.
///
/// `stdin` is the parsed Claude Code hook payload (carries `cwd`,
/// `session_id`, `transcript_path`, plus event-specific fields like
/// `prompt`, `tool_name`, `last_assistant_message`).
pub fn handle_chat_progress(paths: &CcteamPaths, event: &str, stdin: &Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let role = derive_role_from_payload(stdin).unwrap_or_default();

    // Bot's project_dir is the cwd by convention (chat-mode tmux spawns
    // with `-c <project_dir>` so `cwd` lands inside the project).
    let project_dir = cwd.to_string();
    let context = session_context_from_cwd(Path::new(cwd), paths)?;
    let target = paths.progress_jsonl_for_context(&context);

    let mut ev: Value = match event {
        "session-start" => build_chat_session_started_event(&role, &project_dir),
        "user-prompt" => {
            let prompt = stdin.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            let turn_id = stdin
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            build_chat_turn_user_prompt_event(&role, turn_id, prompt)
        }
        "stop" => {
            let turn_id = stdin
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // No structured usage on the Stop hook — the cost pipeline
            // reads it from the transcript / state.json. Emit with a
            // default-shaped usage so consumers can join on schema.
            let usage = ccteam_core::harness::UnifiedTokenUsage::default();
            build_chat_turn_completed_event(&role, turn_id, &usage)
        }
        "subagent-stop" => json!({
            "event": "chat_subagent_completed",
            "role": role,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
        "tool-use" => json!({
            "event": "chat_tool_use",
            "role": role,
            "tool": stdin.get("tool_name").and_then(|v| v.as_str()).unwrap_or(""),
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
        "session-end" => {
            let reason = stdin
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("other");
            // `/clear` / `clear` `/exit` `exit` all map to session reset.
            if reason == "clear" {
                build_chat_session_reset_event(&role)
            } else {
                json!({
                    "event": "chat_session_end",
                    "role": role,
                    "reason": reason,
                    "ts": chrono::Utc::now().to_rfc3339(),
                })
            }
        }
        "pre-compact" => json!({
            "event": "chat_pre_compact",
            "role": role,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
        "post-compact" => build_chat_compact_done_event(&role),
        other => json!({
            "event": format!("chat_{}", other.replace('-', "_")),
            "role": role,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    };

    // Always carry the session_id pluck so consumers can join across
    // hook firings without re-deriving from cwd.
    if let Some(sid) = stdin.get("session_id").and_then(|v| v.as_str()) {
        ev["claude_session_id"] = json!(sid);
    }

    append_event(&target, &ev)?;
    Ok(())
}

/// Try to recover the bot role from the payload. Wave 2 uses the
/// project-dir basename as a fallback because chat-mode tmux session
/// names are `ccteam-chat-<slug>-<role>` and the hook doesn't carry
/// that name natively; the orchestrator-side stamping (the eventual
/// `ccteam_imd` PR) will inject `role` directly into the env we read.
fn derive_role_from_payload(stdin: &Value) -> Option<String> {
    if let Some(r) = stdin.get("role").and_then(|v| v.as_str()) {
        if !r.is_empty() {
            return Some(r.to_string());
        }
    }
    // CCTEAM_CHAT_ROLE env override — chat-mode hook scaffold sets this
    // when ccteam-imd spawns the tmux session.
    if let Ok(r) = std::env::var("CCTEAM_CHAT_ROLE") {
        if !r.is_empty() {
            return Some(r);
        }
    }
    None
}
