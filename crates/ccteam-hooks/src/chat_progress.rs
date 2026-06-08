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
//! | `pre-tool-use`   | PreToolUse          | `chat_tool_call_started`        |
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
    build_chat_session_started_event, build_chat_tool_call_started_event,
    build_chat_turn_completed_event, build_chat_turn_user_prompt_event,
};
use ccteam_core::{session_context_from_cwd, CcteamPaths};
use ccteam_harness::execution::transcript_tail::active_session_id_path;

/// Refresh the F176 `active-session-id` marker from a hook payload's
/// `session_id`. The chat-mode tail loop reads this marker to pick the
/// correct `<sid>.jsonl`. Originally only `SessionStart` wrote it, so a
/// missed SessionStart (or a mid-session transcript rotation, e.g.
/// `/compact` / auto-compact) left the marker stale and the bot went
/// silent despite a healthy pane (the reply landed in a jsonl the tail
/// wasn't following). Refreshing it on every turn start (`user-prompt`,
/// which carries the same `session_id`) keeps it current and self-heals
/// a missed/stale marker.
///
/// v0.8.8 F1 — marker 路径键由 role 改为 ccteam 会话 **sid**(`ccteam_sid`,
/// 来自 `CCTEAM_CHAT_SID` env / stdin),与 tail_loop 的读侧严格同键 ——
/// 这是 rendezvous 的两半之一,任一滞后 → marker 永不会合 → bot 静默。
/// **注意**:marker 的【内容】仍是 Anthropic 原生 session UUID
/// (`payload.session_id`),它与路径键 `ccteam_sid` 是两个 ID 层,别混淆。
/// `ccteam_sid` 或 `session_id` 为空时整个 no-op,绝不用空数据覆盖好 marker。
fn refresh_active_session_marker(cwd: &str, ccteam_sid: &str, stdin: &Value) {
    if ccteam_sid.is_empty() {
        return;
    }
    let Some(sid) = stdin.get("session_id").and_then(|v| v.as_str()) else {
        return;
    };
    if sid.is_empty() {
        return;
    }
    let marker = active_session_id_path(Path::new(cwd), ccteam_sid);
    if let Err(err) = write_marker_atomic(&marker, sid) {
        tracing::warn!(
            ccteam_sid = %ccteam_sid,
            session_id = %sid,
            path = %marker.display(),
            error = %err,
            "chat-progress: failed to refresh active-session-id marker"
        );
    }
}

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
    // v0.8.8 F1 — marker 路径键(会话 sid),与 tail_loop 读侧同键。
    // progress.jsonl 事件本身仍带 role 维度(state-SoT,不动)。
    let ccteam_sid = derive_sid_from_payload(stdin).unwrap_or_default();

    // Bot's project_dir is the cwd by convention (chat-mode tmux spawns
    // with `-c <project_dir>` so `cwd` lands inside the project).
    let project_dir = cwd.to_string();
    let context = session_context_from_cwd(Path::new(cwd), paths)?;
    let target = paths.progress_jsonl_for_context(&context);

    let mut ev: Value = match event {
        "session-start" => {
            // F176 — persist the bot's currently-active Anthropic
            // session_id so the chat-mode tail loop can target the
            // correct jsonl deterministically. Without this marker
            // three bots in one project dir all read whichever jsonl
            // was most recently modified (the F177 fan-out bug).
            // v0.8.8 F1 — marker 按会话 sid 寻址(键由 role 改为 sid)。
            refresh_active_session_marker(cwd, &ccteam_sid, stdin);
            build_chat_session_started_event(&role, &project_dir)
        }
        "user-prompt" => {
            // Refresh the marker on every turn start too — SessionStart can
            // be missed (env propagation, resume) or the transcript can
            // rotate mid-session (/compact, auto-compact), which otherwise
            // strands the tail on a stale jsonl and the reply is never read.
            // `user-prompt` carries the same `session_id`, so this self-heals.
            // v0.8.8 F1 — marker 按会话 sid 寻址。
            refresh_active_session_marker(cwd, &ccteam_sid, stdin);
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
            let usage = ccteam_harness::UnifiedTokenUsage::default();
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
        "pre-tool-use" => {
            // V0.8 Slice 4 — Claude Code `PreToolUse` hook for mode-3
            // chat. Mirrors `tool-use` (PostToolUse): reads the same
            // `tool_name` + `tool_input` fields. Writes
            // `chat_tool_call_started` (NOT `PreToolUse` — that string
            // is owned by the mode-2 silence classifier;
            // CHAT_TOOL_CALL_STARTED docstring in progress.rs has the
            // full rationale).
            let tool = stdin
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            build_chat_tool_call_started_event(&role, tool)
        }
        "session-end" => {
            let reason = stdin
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("other");
            // `/clear` / `clear` `/exit` `exit` all map to session reset.
            if reason == "clear" {
                // F176 — clear the active-session-id marker on the
                // rotation event. Other `session-end` reasons (process
                // exit, daemon kill, network drop) do NOT rotate the
                // sid — leave the marker alone so the next SessionStart
                // hook overwrites with the new sid atomically.
                // v0.8.8 F1 — marker 按会话 sid 寻址(键由 role 改为 sid)。
                if !ccteam_sid.is_empty() {
                    let marker = active_session_id_path(Path::new(cwd), &ccteam_sid);
                    if marker.exists() {
                        if let Err(err) = std::fs::remove_file(&marker) {
                            tracing::warn!(
                                ccteam_sid = %ccteam_sid,
                                path = %marker.display(),
                                error = %err,
                                "chat-progress: failed to clear active-session-id marker"
                            );
                        }
                    }
                }
                build_chat_session_reset_event(&role, &ccteam_sid)
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

/// Atomic write of `body` to `path` via `<path>.tmp` + rename. Same
/// pattern as `TranscriptCursor::save`. Creates parent dirs as needed
/// because the hook subprocess may race ahead of the orchestrator's
/// `ensure_dir` call on a brand-new spawn.
fn write_marker_atomic(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| anyhow!("create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).map_err(|e| anyhow!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| anyhow!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

/// Try to recover the bot role from the payload. Wave 2 uses the
/// project-dir basename as a fallback because chat-mode tmux session
/// names are `ccteam-chat-<slug>-<role>` and the hook doesn't carry
/// that name natively; the orchestrator-side stamping (the eventual
/// `ccteam_im` PR) will inject `role` directly into the env we read.
fn derive_role_from_payload(stdin: &Value) -> Option<String> {
    if let Some(r) = stdin.get("role").and_then(|v| v.as_str()) {
        if !r.is_empty() {
            return Some(r.to_string());
        }
    }
    // CCTEAM_CHAT_ROLE env override — chat-mode hook scaffold sets this
    // when ccteam-im spawns the tmux session.
    if let Ok(r) = std::env::var("CCTEAM_CHAT_ROLE") {
        if !r.is_empty() {
            return Some(r);
        }
    }
    None
}

/// v0.8.8 F1 — recover the ccteam session **sid**(`s<N>`)from the payload.
/// 优先读 stdin 显式 `ccteam_sid`(测试 / 未来注入),再回退到
/// `CCTEAM_CHAT_SID` env —— 后者由 spawn 时 `chat_spawn_env_owned` 注入。
/// sid 是 marker / turns / cursor 的存储键,与 tail_loop 读侧同源。
/// **红线**:这是 ccteam 的 `s<N>`,绝非 Anthropic 原生 session UUID
/// (`payload.session_id`)—— 后者只进 marker 内容,不当 ccteam 身份键。
fn derive_sid_from_payload(stdin: &Value) -> Option<String> {
    if let Some(s) = stdin.get("ccteam_sid").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Ok(s) = std::env::var("CCTEAM_CHAT_SID") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}
