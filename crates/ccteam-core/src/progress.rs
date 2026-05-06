//! progress.jsonl reader / writer + idle detection.
//!
//! progress.jsonl is the orchestrator's only state-truth source
//! (`docs/tech-design.md` §5.5). This module gives both the hook
//! handlers and the orchestrator a single set of primitives so the
//! file format and idle semantics stay in sync.

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
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
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
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
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
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
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

/// Walk `events` in reverse and return the most recent `phase_done` /
/// `escalate` event whose `phase` (for phase_done) matches `current` —
/// stopping if we hit a `phase_inject` for `current` first (anything
/// older than that injection belongs to a previous round of the same
/// phase, e.g. after a context reset).
///
/// This is needed because Claude Code emits `Stop` *and* `SubagentStop`
/// after a turn finishes. If `parse-phase-end` writes `phase_done` and
/// then a downstream hook appends `SubagentStop`, the *last* event
/// is no longer `phase_done` — but the phase did finish. Reading just
/// the last line under-reports completions.
pub fn latest_terminal_event_for_phase<'a>(
    events: &'a [Value],
    current: &str,
) -> Option<&'a Value> {
    for event in events.iter().rev() {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        match kind {
            "phase_done" | "phase_done_pending" => {
                let phase = event.get("phase").and_then(|s| s.as_str()).unwrap_or("");
                if phase == current {
                    return Some(event);
                }
            }
            "escalate" => return Some(event),
            "phase_inject" => {
                let phase = event.get("phase").and_then(|s| s.as_str()).unwrap_or("");
                if phase == current {
                    // Anything before this inject is a stale terminal
                    // from a prior round of the same phase.
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Idle detection per tech-design §6.9.
///
/// `Stop` / `Notification:idle_prompt` are the canonical "claude is
/// waiting" signals. Phase-boundary events (`session_start`,
/// `phase_done`, `escalate`, `SessionEnd`) also imply nothing is
/// in-flight. Anything else (`PreToolUse`, `PostToolUse`, `phase_inject`,
/// `SubagentStop`) means a tool call is mid-flight — caller should use
/// `/btw` to queue without interrupting.
pub fn is_idle(last: Option<&Value>) -> bool {
    let Some(event) = last else {
        return true;
    };
    let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
    matches!(
        kind,
        "Stop" | "notification" | "session_start" | "SessionEnd" | "phase_done" | "escalate"
    )
}

/// Build the canonical phase-injection prompt (tech-design §4.4 / §6.9).
/// Short enough to fit comfortably in one tmux send-keys; relies on the
/// project's `.ccteam/phases/<phase>.md` for the full body.
pub fn build_phase_prompt(phase: &str) -> String {
    build_phase_prompt_with_attachments(phase, &[])
}

/// M2.1: same as `build_phase_prompt` but appends `@<rel>` references
/// for each `attachment`. Used by the orchestrator to surface prior
/// phase's `sub_skills` outputs (e.g. `.ccteam/code-review.md`) so the
/// next phase reads them automatically without having to know which
/// sub-skills produced them.
///
/// `attachments` are project-relative paths (e.g. `.ccteam/code-review.md`).
/// Empty list yields the same string as `build_phase_prompt`.
pub fn build_phase_prompt_with_attachments(phase: &str, attachments: &[&str]) -> String {
    let base = format!(
        "请按 @.ccteam/phases/{phase}.md 完成本阶段。完成后写 .ccteam/{phase}-report.md，并在最后单独输出一行：PHASE_DONE: {phase}（或 ESCALATE: <一句话原因>）。"
    );
    if attachments.is_empty() {
        return base;
    }
    let refs: Vec<String> = attachments
        .iter()
        .map(|p| format!("@{p}"))
        .collect();
    format!("{base}\n\n上轮 sub-skill 产出可参考: {}", refs.join(" "))
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
    fn idle_aware_message_wraps_with_btw_when_busy() {
        let p = build_phase_prompt("implement");
        assert_eq!(idle_aware_message(&p, true), p);
        let busy = idle_aware_message(&p, false);
        assert!(busy.starts_with("/btw "));
        assert!(busy.contains("@.ccteam/phases/implement.md"));
    }

    #[test]
    fn build_phase_prompt_mentions_phase_done_sigil() {
        let p = build_phase_prompt("implement");
        assert!(p.contains("PHASE_DONE: implement"));
        assert!(p.contains("ESCALATE"));
    }

    #[test]
    fn latest_terminal_event_finds_phase_done_when_subagent_stop_is_last() {
        // Real Claude Code 2.x event order: parse-phase-end appends
        // phase_done, then SubagentStop fires later. Naive last-event
        // lookup returns SubagentStop and the orchestrator never
        // advances. This helper must walk back past SubagentStop.
        let events = vec![
            json!({"event": "phase_inject", "phase": "plan-eng"}),
            json!({"event": "PostToolUse", "tool": "Write"}),
            json!({"event": "Stop"}),
            json!({"event": "phase_done", "phase": "plan-eng"}),
            json!({"event": "SubagentStop"}),
        ];
        let found = latest_terminal_event_for_phase(&events, "plan-eng").unwrap();
        assert_eq!(found["event"], "phase_done");
        assert_eq!(found["phase"], "plan-eng");
    }

    #[test]
    fn latest_terminal_event_stops_at_phase_inject_for_same_phase() {
        // After a context reset for plan-eng, an old phase_done from
        // the prior round should not be picked up — re-injection draws
        // a new boundary.
        let events = vec![
            json!({"event": "phase_done", "phase": "plan-eng"}),
            json!({"event": "phase_inject", "phase": "plan-eng"}),
            json!({"event": "PreToolUse"}),
        ];
        assert!(latest_terminal_event_for_phase(&events, "plan-eng").is_none());
    }

    #[test]
    fn latest_terminal_event_picks_escalate_over_irrelevant_events() {
        let events = vec![
            json!({"event": "phase_inject", "phase": "fix"}),
            json!({"event": "escalate", "reason": "fix-loop cap"}),
            json!({"event": "Stop"}),
        ];
        let e = latest_terminal_event_for_phase(&events, "fix").unwrap();
        assert_eq!(e["event"], "escalate");
    }

    #[test]
    fn latest_terminal_event_skips_phase_done_for_other_phase() {
        let events = vec![
            json!({"event": "phase_inject", "phase": "implement"}),
            json!({"event": "phase_done", "phase": "plan-eng"}), // stale
        ];
        assert!(latest_terminal_event_for_phase(&events, "implement").is_none());
    }
}
