//! V0.6.0 F118 — last-N turn recovery for `mode: chat` bots.
//!
//! The Claude Code session-id can be invalidated by:
//!
//! - a `/compact` that the user aborts mid-flight,
//! - a process crash that leaves the transcript half-written,
//! - the user running `/clear` and wanting the bot's memory rebuilt
//!   from the orchestrator-owned mirror,
//! - the user deleting `~/.claude/projects/<encoded>/<sid>.jsonl`.
//!
//! In all cases the ccteam-owned harness mirror
//! `<project>/.ccteam/chat/<bot>/turns.jsonl` (see
//! [`super::turns_mirror`]) has the canonical conversation. This module
//! takes the last `N` turns from that file, formats them as a
//! `<conversation_history>...</conversation_history>` prefix prompt,
//! and returns it for the caller to send into a freshly-spawned tmux
//! `claude` session via `submit_turn(SystemDirective(...))`.
//!
//! ## Why not push the recovery prompt directly?
//!
//! [`crate::execution::claude_tui::ClaudeTuiAdapter`] owns the tmux +
//! send-keys plumbing. This module stays pure (no tmux calls) so unit
//! tests can verify the prompt shape without spawning real processes;
//! the adapter wires recovery in by:
//!
//! 1. Calling [`build_recovery_prompt`] to get the prefix string.
//! 2. `start_thread` → new tmux session.
//! 3. `submit_turn(SystemDirective(prompt))` to seed the bot.
//! 4. Emitting `chat_session_reset_with_recovery` to progress.jsonl
//!    via [`super::turns_mirror`]'s companion event helpers
//!    (`crate::progress::build_chat_session_reset_with_recovery_event`).

use std::path::Path;

use anyhow::Result;

use super::turns_mirror::{last_n_turns, TurnRecord};

/// Outcome of a recovery attempt — used by the adapter's
/// `resume_thread` to log + emit the matching progress event.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryPlan {
    /// Number of turns the recovery prompt embeds. May be 0 when
    /// `turns.jsonl` is missing or empty (the adapter then treats the
    /// new tmux session as a fresh start).
    pub recovered_turns: usize,
    /// The full system-directive prompt to push to the freshly-spawned
    /// session. Empty when `recovered_turns == 0`.
    pub prompt: String,
}

/// Build a recovery prompt from `<project>/.ccteam/chat/<bot>/turns.jsonl`.
/// Returns a [`RecoveryPlan`] the caller can inspect before sending.
///
/// The prompt shape (one block per turn, oldest first) keeps the bot's
/// memory in chronological order:
///
/// ```text
/// I've just recovered from a small session-id reset; here's our
/// conversation history. Continue naturally from the last assistant
/// message.
///
/// <conversation_history>
/// [user] hello there
/// [assistant] hi, what can I help with?
/// [user] tell me about ccteam
/// [assistant] ccteam is ...
/// </conversation_history>
/// ```
pub fn build_recovery_prompt(
    project_dir: &Path,
    bot_role: &str,
    recover_last_n: usize,
) -> Result<RecoveryPlan> {
    let turns = last_n_turns(project_dir, bot_role, recover_last_n)?;
    if turns.is_empty() {
        return Ok(RecoveryPlan {
            recovered_turns: 0,
            prompt: String::new(),
        });
    }
    let prompt = format_recovery_prompt(&turns);
    Ok(RecoveryPlan {
        recovered_turns: turns.len(),
        prompt,
    })
}

/// Render a recovery prompt from a turn slice. Public for unit testing,
/// and so the adapter can short-circuit when it already has the slice
/// in-memory.
pub fn format_recovery_prompt(turns: &[TurnRecord]) -> String {
    let mut s = String::new();
    s.push_str(
        "I've just recovered from a small session reset. Here's our \
         conversation history so far — please continue naturally from \
         the last assistant message, without re-introducing yourself.\n\n",
    );
    s.push_str("<conversation_history>\n");
    for t in turns {
        if !t.user.is_empty() {
            s.push_str("[user] ");
            s.push_str(&one_line(&t.user));
            s.push('\n');
        }
        if !t.assistant.is_empty() {
            s.push_str("[assistant] ");
            s.push_str(&one_line(&t.assistant));
            s.push('\n');
        }
    }
    s.push_str("</conversation_history>\n");
    s
}

/// Collapse newlines + trim per-turn content so the recovery prompt
/// stays one-line-per-turn. Bounds each line to 2 KiB to keep the
/// total prompt under Claude's first-turn 200 KiB safety budget even
/// when `recover_last_n` is generous.
fn one_line(s: &str) -> String {
    let mut compact = s
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ¶ ");
    const MAX: usize = 2048;
    if compact.chars().count() > MAX {
        compact = compact.chars().take(MAX).collect::<String>() + " …(truncated)";
    }
    compact
}

#[cfg(test)]
mod tests {
    use super::super::turns_mirror::{append_turn, TurnRecord};
    use super::*;
    use chrono::Utc;
    use serde_json::Value;
    use tempfile::TempDir;

    fn mk_turn(id: &str, user: &str, assistant: &str) -> TurnRecord {
        TurnRecord {
            turn_id: id.into(),
            ts: Utc::now(),
            vendor: "claude".into(),
            role: "alice".into(),
            user: user.into(),
            assistant: assistant.into(),
            usage: Value::Null,
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn empty_turns_jsonl_returns_zero_turns() {
        let tmp = TempDir::new().unwrap();
        let plan = build_recovery_prompt(tmp.path(), "alice", 20).unwrap();
        assert_eq!(plan.recovered_turns, 0);
        assert!(plan.prompt.is_empty());
    }

    #[test]
    fn build_recovery_prompt_embeds_chronological_history() {
        let tmp = TempDir::new().unwrap();
        for i in 0..3 {
            let t = mk_turn(&format!("t{i}"), &format!("u{i}"), &format!("a{i}"));
            append_turn(tmp.path(), "alice", &t).unwrap();
        }
        let plan = build_recovery_prompt(tmp.path(), "alice", 20).unwrap();
        assert_eq!(plan.recovered_turns, 3);
        assert!(plan.prompt.contains("<conversation_history>"));
        // Ordering: u0 appears before u2.
        let p_u0 = plan.prompt.find("u0").unwrap();
        let p_u2 = plan.prompt.find("u2").unwrap();
        assert!(p_u0 < p_u2);
        assert!(plan.prompt.contains("[user] u1"));
        assert!(plan.prompt.contains("[assistant] a1"));
        assert!(plan.prompt.trim_end().ends_with("</conversation_history>"));
    }

    #[test]
    fn build_recovery_prompt_honours_last_n_bound() {
        let tmp = TempDir::new().unwrap();
        for i in 0..10 {
            let t = mk_turn(&format!("t{i}"), &format!("u{i}"), &format!("a{i}"));
            append_turn(tmp.path(), "alice", &t).unwrap();
        }
        let plan = build_recovery_prompt(tmp.path(), "alice", 3).unwrap();
        assert_eq!(plan.recovered_turns, 3);
        // Should embed u7/u8/u9 (the tail) — not u0.
        assert!(plan.prompt.contains("u7"));
        assert!(plan.prompt.contains("u9"));
        assert!(!plan.prompt.contains("[user] u0"));
    }

    #[test]
    fn one_line_collapses_newlines_and_caps_length() {
        let big = "line1\nline2\nline3".to_string();
        let out = super::one_line(&big);
        assert!(out.contains("line1"));
        assert!(out.contains("¶"));
        assert!(!out.contains('\n'));
    }

    #[test]
    fn format_recovery_prompt_skips_empty_sides() {
        // System-directive-only turn (no user/assistant text) — should
        // not produce empty `[user]` / `[assistant]` lines.
        let turn = TurnRecord {
            turn_id: "x".into(),
            ts: Utc::now(),
            vendor: "claude".into(),
            role: "z".into(),
            user: "".into(),
            assistant: "".into(),
            usage: Value::Null,
            tool_calls: Vec::new(),
        };
        let s = format_recovery_prompt(&[turn]);
        assert!(!s.contains("[user] "));
        assert!(!s.contains("[assistant] "));
        assert!(s.contains("<conversation_history>"));
    }
}
