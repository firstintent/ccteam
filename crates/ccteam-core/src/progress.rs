//! progress.jsonl reader / writer + idle detection.
//!
//! progress.jsonl is the orchestrator's only state-truth source
//! (`docs/tech-design.md` §5.5). This module gives both the hook
//! handlers and the orchestrator a single set of primitives so the
//! file format and idle semantics stay in sync.
//!
//! V0.4.0 F60: the phase-prompt builders (`build_phase_prompt`,
//! `build_phase_prompt_for_template`, `build_phase_prompt_with_attachments`,
//! `build_phase_prompt_for_template_with_team`) were deleted along
//! with the rest of the phase machinery. F66 reintroduces an
//! injection-prompt builder against the new `workflow.yaml` schema.
//! Event-log read/write/idle helpers stay — they're channel-layer
//! primitives shared by every consumer.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// Append `event` as one JSONL line. Creates parent dir + file when
/// missing. POSIX `O_APPEND` is atomic for sub-PIPE_BUF writes (4 KiB
/// on Linux), which our compact event lines comfortably fit under, so
/// concurrent hook + orchestrator writers don't interleave.
pub fn append_event(path: &Path, event: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(event)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(())
}

/// Read + parse the last non-empty line of `path`. `Ok(None)` when the
/// file is absent or contains no events yet.
pub fn last_event(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let Some(line) = content.lines().rev().find(|l| !l.trim().is_empty()) else {
        return Ok(None);
    };
    let v: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("parse last line of {}", path.display()))?;
    Ok(Some(v))
}

/// Read + parse all events from `path`. Skips empty lines and lines
/// that fail to deserialize as JSON (defensive: a half-flushed line
/// shouldn't crash the orchestrator's read).
pub fn read_all_events(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => out.push(v),
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Idle detection per tech-design §6.9.
///
/// `Stop` / `Notification:idle_prompt` are the canonical "claude is
/// waiting" signals. Phase-boundary events (`session_start`,
/// `phase_done`, `escalate`, `SessionEnd`) also imply nothing is
/// in-flight. `SubagentStop` fires 2–5 s after `Stop` whenever the
/// finished turn used `Task`; the main loop is already idle by then,
/// so we treat it the same as `Stop` (E2E 2026-05-06: classifying it
/// as busy caused the next phase prompt to be wrapped in `/btw`,
/// which spawns a tool-less side-agent and stalls the project).
/// Anything else (`PreToolUse`, `PostToolUse`, `phase_inject`) means a
/// tool call is mid-flight — caller should use `/btw` to queue without
/// interrupting.
pub fn is_idle(last: Option<&Value>) -> bool {
    let Some(event) = last else {
        return true;
    };
    let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
    matches!(
        kind,
        "Stop"
            | "SubagentStop"
            | "notification"
            | "session_start"
            | "SessionEnd"
            | "phase_done"
            | "escalate"
    )
}

/// V0.2.2 F36: detect whether a sub-agent (`Task` tool) is currently
/// in flight by walking `events` from the tail and counting how many
/// `PreToolUse(tool=Task)` openings have not yet been matched by a
/// `SubagentStop`. Returns `true` when at least one window is open.
///
/// **Why count, not last-event-match**: Claude Code can launch a
/// sub-agent (`Task`), have it spawn an inner Task, and emit two
/// `PreToolUse(Task)` events in a row before the matching pair of
/// `SubagentStop` events arrives. A naive "is the most recent event a
/// `Task` PreToolUse?" check misses the second-from-top case the
/// moment the inner sub-agent emits its own `PreToolUse`.
///
/// **Why scan from the tail**: every `SubagentStop` past the open
/// window already cancelled an earlier `PreToolUse(Task)` we don't
/// care about. We stop counting as soon as `open_windows` returns to
/// zero — older paired sequences can't reach into the current open
/// state.
///
/// Pure deterministic helper; no I/O. Honors the **"`progress.jsonl`
/// is the only state truth"** red line — F36's send-keys guard reads
/// progress events, never tmux pane text.
pub fn subagent_active(events: &[Value]) -> bool {
    let mut closes_pending: u64 = 0;
    for event in events.iter().rev() {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        match kind {
            "SubagentStop" => {
                closes_pending = closes_pending.saturating_add(1);
            }
            "PreToolUse" => {
                let tool = event.get("tool").and_then(|s| s.as_str()).unwrap_or("");
                if tool == "Task" {
                    if closes_pending == 0 {
                        return true;
                    }
                    closes_pending -= 1;
                }
            }
            _ => {}
        }
    }
    false
}

/// `/btw <prompt>` when claude is busy so the message queues without
/// interrupting; bare prompt when idle.
pub fn idle_aware_message(prompt: &str, idle: bool) -> String {
    if idle {
        prompt.to_string()
    } else {
        format!("/btw {prompt}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn idle_when_no_events_yet() {
        assert!(is_idle(None));
    }

    #[test]
    fn idle_after_stop() {
        let e = json!({"event": "Stop", "ts": "..."});
        assert!(is_idle(Some(&e)));
    }

    #[test]
    fn idle_after_notification() {
        let e = json!({"event": "notification"});
        assert!(is_idle(Some(&e)));
    }

    #[test]
    fn busy_during_tool_use() {
        let e = json!({"event": "PreToolUse", "tool": "Edit"});
        assert!(!is_idle(Some(&e)));
        let e = json!({"event": "PostToolUse"});
        assert!(!is_idle(Some(&e)));
        let e = json!({"event": "phase_inject"});
        assert!(!is_idle(Some(&e)));
    }

    #[test]
    fn phase_boundaries_are_idle() {
        for kind in ["session_start", "phase_done", "escalate", "SessionEnd"] {
            let e = json!({"event": kind});
            assert!(is_idle(Some(&e)), "{kind} should be treated as idle");
        }
    }

    #[test]
    fn idle_treats_subagent_stop_as_idle() {
        // E2E 2026-05-06 F1+F2: Claude Code emits SubagentStop 2–5 s
        // after Stop whenever a turn used Task. The main loop is already
        // idle at that point — classifying it as busy caused the next
        // phase inject to be wrapped in `/btw`, which spawns a toolless
        // side-agent that cannot execute the next phase.
        let e = json!({"event": "SubagentStop"});
        assert!(is_idle(Some(&e)));
    }

    #[test]
    fn idle_aware_message_wraps_with_btw_when_busy() {
        assert_eq!(idle_aware_message("hello", true), "hello");
        assert_eq!(idle_aware_message("hello", false), "/btw hello");
    }

    // ---------------- V0.2.2 F36 subagent_active helper ----------------

    fn pretool_task() -> Value {
        json!({"event": "PreToolUse", "tool": "Task"})
    }
    fn pretool_other(tool: &str) -> Value {
        json!({"event": "PreToolUse", "tool": tool})
    }
    fn subagent_stop() -> Value {
        json!({"event": "SubagentStop"})
    }

    #[test]
    fn subagent_active_empty_log_returns_false() {
        assert!(!subagent_active(&[]));
    }

    #[test]
    fn subagent_active_open_window_after_pretool_task() {
        let events = [
            json!({"event": "phase_inject", "phase": "implement"}),
            pretool_task(),
        ];
        assert!(subagent_active(&events));
    }

    #[test]
    fn subagent_active_paired_pretool_task_and_subagent_stop_returns_false() {
        let events = [pretool_task(), subagent_stop()];
        assert!(!subagent_active(&events));
    }

    #[test]
    fn subagent_active_nested_task_calls_open_two_windows() {
        // outer Task launched, inner Task launched, only one SubagentStop
        // arrived so far → still one open window.
        let events = [pretool_task(), pretool_task(), subagent_stop()];
        assert!(subagent_active(&events));
    }

    #[test]
    fn subagent_active_old_subagent_stop_does_not_close_new_pretool_task() {
        // Old paired sequence (closed) followed by a fresh PreToolUse(Task)
        // with no follow-up — the new window must register as active.
        let events = [
            pretool_task(),
            subagent_stop(),
            json!({"event": "PostToolUse", "tool": "Read"}),
            pretool_task(),
        ];
        assert!(subagent_active(&events));
    }

    #[test]
    fn subagent_active_ignores_non_task_pretool() {
        let events = [pretool_other("Read"), pretool_other("Edit")];
        assert!(!subagent_active(&events));
    }

    #[test]
    fn subagent_active_extra_subagent_stops_do_not_underflow() {
        // Defensive: stray SubagentStop events with no matching open
        // window must not panic / wrap around.
        let events = [subagent_stop(), subagent_stop(), pretool_task()];
        assert!(subagent_active(&events));
    }
}
