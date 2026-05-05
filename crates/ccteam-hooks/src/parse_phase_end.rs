//! `ccteam hook parse-phase-end` — Stop hook handler.
//!
//! Two responsibilities, in order:
//!
//! 1. **Fix-loop ralph-loop pattern** (M0.12, tech-design §3.5). If the
//!    project has a `<project>/.ccteam/fix-loop.state.md`, this hook
//!    drives the loop: read the last assistant text, ask
//!    `fix_loop::decide` whether to re-feed (block exit) or allow
//!    exit (success / iteration cap). Re-injection bumps the
//!    iteration counter in the state file; allow-exit deletes the
//!    state file. Iteration cap without success → `escalate` event.
//!
//! 2. **Normal `PHASE_DONE` / `ESCALATE` parsing** (M0.3). Last
//!    non-empty line of the latest assistant message starts with
//!    `PHASE_DONE:` → `phase_done` event; `ESCALATE:` → `escalate`
//!    event.

use std::path::Path;

use anyhow::{anyhow, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use ccteam_core::fix_loop::{self, FixLoopDecision};
use ccteam_core::{progress::append_event, slug_from_project_dir, CcteamPaths};

use crate::transcript::{last_assistant_message, message_text};

/// Result of running `parse_phase_end`. The CLI dispatcher emits the
/// Claude Code Stop hook decision JSON when this is `Block`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDecision {
    /// Default: don't block; let claude continue exiting.
    Continue,
    /// Block the Stop and re-feed `reason` as the next user prompt
    /// (ralph-loop / fix-cycle).
    Block { reason: String },
}

pub fn parse_phase_end(paths: &CcteamPaths, stdin: &Value) -> Result<ParseDecision> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let transcript_path = stdin
        .get("transcript_path")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `transcript_path`"))?;

    let cwd_path = Path::new(cwd);
    let slug = slug_from_project_dir(cwd_path)?;
    let progress_path = paths.progress_jsonl(&slug);

    // Prefer Claude Code's stdin field — at Stop time, the transcript
    // file may not yet contain the latest assistant turn (the JSONL
    // flush races with the hook fire), so reading the file alone misses
    // PHASE_DONE in real sessions. Fall back to the transcript only when
    // stdin doesn't carry the message (older Claude Code or other hook
    // events).
    let stdin_last = stdin
        .get("last_assistant_message")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    let last_text = match stdin_last {
        Some(s) if !s.trim().is_empty() => s,
        _ => last_assistant_message(Path::new(transcript_path))?
            .as_ref()
            .and_then(message_text)
            .unwrap_or_default(),
    };

    // Step 1: fix-loop drives if the state file is present.
    let fix_loop_path = fix_loop::path_in(cwd_path);
    if let Some(state) = fix_loop::read(&fix_loop_path)? {
        match fix_loop::decide(&state, &last_text) {
            FixLoopDecision::Reinject {
                prompt,
                next_iteration,
            } => {
                let mut next = state.clone();
                next.front.iteration = next_iteration;
                next.front.updated_at = Utc::now();
                fix_loop::write(&fix_loop_path, &next)?;
                return Ok(ParseDecision::Block { reason: prompt });
            }
            FixLoopDecision::AllowExit { succeeded } => {
                fix_loop::delete(&fix_loop_path)?;
                if !succeeded {
                    let event = json!({
                        "ts": now_rfc3339(),
                        "event": "escalate",
                        "reason": format!(
                            "fix-loop hit {} iterations without {}",
                            state.front.max_iterations, state.front.completion_signal,
                        ),
                        "cycle": state.front.iteration,
                    });
                    append_event(&progress_path, &event)?;
                    return Ok(ParseDecision::Continue);
                }
                // Success: fall through to PHASE_DONE parsing.
            }
        }
    }

    // Step 2: normal PHASE_DONE / ESCALATE sigil parsing.
    let last_line = last_text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|s| s.trim())
        .unwrap_or("");

    let event = if let Some(phase) = last_line.strip_prefix("PHASE_DONE:") {
        Some(json!({
            "ts": now_rfc3339(),
            "event": "phase_done",
            "phase": phase.trim(),
        }))
    } else {
        last_line.strip_prefix("ESCALATE:").map(|raw_reason| {
            let parsed = ParsedEscalate::from_reason(raw_reason);
            json!({
                "ts": now_rfc3339(),
                "event": "escalate",
                "kind": parsed.kind.as_str(),
                "reason": parsed.reason,
                "target_phase": parsed.target_phase,
            })
        })
    };

    if let Some(event) = event {
        append_event(&progress_path, &event)?;
    } else {
        // Hook fired but found no sigil. Log to stderr at debug level so
        // the operator can confirm the hook was reached even when no
        // event was emitted — silent zero-exit is otherwise hard to
        // diagnose in production runs.
        eprintln!(
            "[ccteam parse-phase-end] no PHASE_DONE/ESCALATE in last_text (head: {:?})",
            last_text.lines().rev().take(3).collect::<Vec<_>>(),
        );
    }
    Ok(ParseDecision::Continue)
}

/// Structured ESCALATE grammar (interfaces §4.1.1). Pure string-prefix
/// matching, no LLM — orchestrator stays a dumb Rust program. Bare text
/// degrades to `NEED_USER_INPUT` so old phase markdown keeps working.
///
/// Grammar (everything after `ESCALATE:` whitespace-trimmed):
///
/// - `REVERT_TO_PHASE <name> — <reason>` → `kind="revert"`,
///   `target_phase="<name>"`. Em dash (`—`) preferred but plain `-` /
///   `--` accepted.
/// - `NEED_USER_INPUT — <questions>` → `kind="need_user_input"`.
/// - `ABORT — <reason>` → `kind="abort"`. Project terminally failed.
/// - **M2.3** `INSUFFICIENT_CLARIFICATION — <last_question>` →
///   `kind="insufficient_clarification"`. Phase already produced a
///   best-effort artifact; user picks continue / accept / abort
///   (interfaces §5.6.2).
/// - **M3.6** `PHASE_DONE_PENDING — <reason>` →
///   `kind="phase_done_pending"`. Phase done modulo deferred decisions
///   (interfaces §4.1.1). M2.3 only parses the prefix; orchestrator
///   routing lands in M3.6.
/// - Anything else → `kind="need_user_input"`, `reason=<the whole tail>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalateKind {
    Revert,
    NeedUserInput,
    Abort,
    InsufficientClarification,
    PhaseDonePending,
}

impl EscalateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EscalateKind::Revert => "revert",
            EscalateKind::NeedUserInput => "need_user_input",
            EscalateKind::Abort => "abort",
            EscalateKind::InsufficientClarification => "insufficient_clarification",
            EscalateKind::PhaseDonePending => "phase_done_pending",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEscalate {
    pub kind: EscalateKind,
    /// For REVERT_TO_PHASE: the target phase name. None otherwise.
    pub target_phase: Option<String>,
    /// Free-text reason for the user. For REVERT_TO_PHASE this is the
    /// text after the separator; for NEED_USER_INPUT / ABORT this is
    /// the text after the prefix's separator; for bare text it's the
    /// whole tail.
    pub reason: String,
}

impl ParsedEscalate {
    /// Parse the substring *after* `ESCALATE:` (caller has already
    /// stripped the prefix). Whitespace is trimmed off the input.
    pub fn from_reason(raw: &str) -> Self {
        let trimmed = raw.trim();

        if let Some(rest) = strip_grammar_prefix(trimmed, "REVERT_TO_PHASE") {
            // Expect: `<phase> — <reason>` or `<phase> - <reason>`.
            let (phase, reason) = split_on_dash(rest);
            return Self {
                kind: EscalateKind::Revert,
                target_phase: Some(phase.to_string()),
                reason: reason.to_string(),
            };
        }
        if let Some(rest) = strip_grammar_prefix(trimmed, "NEED_USER_INPUT") {
            // The dash separator is optional — `NEED_USER_INPUT — what?`
            // and `NEED_USER_INPUT what?` should both work.
            let reason = trim_leading_dash(rest);
            return Self {
                kind: EscalateKind::NeedUserInput,
                target_phase: None,
                reason: reason.to_string(),
            };
        }
        if let Some(rest) = strip_grammar_prefix(trimmed, "ABORT") {
            let reason = trim_leading_dash(rest);
            return Self {
                kind: EscalateKind::Abort,
                target_phase: None,
                reason: reason.to_string(),
            };
        }
        // M2.3: phase exhausted max_clarify_rounds with a best-effort
        // artifact already on disk — user picks continue / accept / abort.
        // Match before NEED_USER_INPUT but the keywords don't share a
        // prefix, so order is for readability.
        if let Some(rest) = strip_grammar_prefix(trimmed, "INSUFFICIENT_CLARIFICATION") {
            let reason = trim_leading_dash(rest);
            return Self {
                kind: EscalateKind::InsufficientClarification,
                target_phase: None,
                reason: reason.to_string(),
            };
        }
        // M3.6: phase produced its required outputs but some sub-tasks
        // are deferred (decisions queue). Parsing landed in M2.3 so
        // phase markdown can use the prefix today; orchestrator routing
        // for `phase_done_pending` ships in M3.6.
        if let Some(rest) = strip_grammar_prefix(trimmed, "PHASE_DONE_PENDING") {
            let reason = trim_leading_dash(rest);
            return Self {
                kind: EscalateKind::PhaseDonePending,
                target_phase: None,
                reason: reason.to_string(),
            };
        }

        // No grammar prefix → treat as legacy free-text NEED_USER_INPUT.
        Self {
            kind: EscalateKind::NeedUserInput,
            target_phase: None,
            reason: trimmed.to_string(),
        }
    }
}

/// Strip `<keyword>` from the front of `s` only when it's followed by
/// a word boundary (whitespace, em dash, or `-`). Prevents e.g.
/// `ABORTED` matching `ABORT`. Documented separators in
/// `interfaces.md §4.1.1`: em dash, `--`, and ` - ` (whitespace
/// bounded). Colon is *not* a separator — `ABORT:foo` is bare text.
fn strip_grammar_prefix<'a>(s: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(keyword)?;
    let next = rest.chars().next();
    match next {
        None => Some(rest),
        Some(c) if c.is_whitespace() || c == '-' || c == '\u{2014}' => Some(rest),
        _ => None, // word continues — not a real prefix match
    }
}

/// Split `<phase> <separator> <reason>`. Separators tried in priority
/// order: em dash, `--`, then ` - ` (single dash *with* whitespace on
/// both sides) — the whitespace guard keeps phase names like `plan-eng`
/// from splitting in the middle. When no separator, the trimmed input
/// is treated as the phase name and the reason is empty.
fn split_on_dash(s: &str) -> (&str, &str) {
    let s = s.trim();
    for sep in ["\u{2014}", "--"] {
        if let Some((head, tail)) = s.split_once(sep) {
            return (head.trim(), tail.trim());
        }
    }
    if let Some((head, tail)) = s.split_once(" - ") {
        return (head.trim(), tail.trim());
    }
    (s, "")
}

/// Drop a leading em dash / `--` / `-` (with surrounding whitespace)
/// from `s`, if present. Used after the grammar prefix to strip the
/// optional separator before the human-readable reason.
fn trim_leading_dash(s: &str) -> &str {
    let s = s.trim_start();
    for sep in ["\u{2014}", "--", "-"] {
        if let Some(rest) = s.strip_prefix(sep) {
            return rest.trim_start();
        }
    }
    s
}

#[cfg(test)]
mod parse_escalate_tests {
    use super::*;

    #[test]
    fn revert_to_phase_with_em_dash() {
        let p = ParsedEscalate::from_reason(
            " REVERT_TO_PHASE plan-eng \u{2014} fix-loop 撞顶,根因在选型",
        );
        assert_eq!(p.kind, EscalateKind::Revert);
        assert_eq!(p.target_phase.as_deref(), Some("plan-eng"));
        assert_eq!(p.reason, "fix-loop 撞顶,根因在选型");
    }

    #[test]
    fn revert_to_phase_with_double_dash() {
        let p = ParsedEscalate::from_reason("REVERT_TO_PHASE plan-eng -- need redesign");
        assert_eq!(p.kind, EscalateKind::Revert);
        assert_eq!(p.target_phase.as_deref(), Some("plan-eng"));
        assert_eq!(p.reason, "need redesign");
    }

    #[test]
    fn revert_to_phase_with_single_dash() {
        let p = ParsedEscalate::from_reason("REVERT_TO_PHASE implement - context drift");
        assert_eq!(p.kind, EscalateKind::Revert);
        assert_eq!(p.target_phase.as_deref(), Some("implement"));
        assert_eq!(p.reason, "context drift");
    }

    #[test]
    fn need_user_input_with_dash_separator() {
        let p = ParsedEscalate::from_reason(
            " NEED_USER_INPUT \u{2014} (1) target platform? (2) target user?",
        );
        assert_eq!(p.kind, EscalateKind::NeedUserInput);
        assert_eq!(p.target_phase, None);
        assert_eq!(p.reason, "(1) target platform? (2) target user?");
    }

    #[test]
    fn need_user_input_without_separator() {
        let p = ParsedEscalate::from_reason("NEED_USER_INPUT spec.md only has 'mdeditor'");
        assert_eq!(p.kind, EscalateKind::NeedUserInput);
        assert_eq!(p.reason, "spec.md only has 'mdeditor'");
    }

    #[test]
    fn abort_with_reason() {
        let p = ParsedEscalate::from_reason("ABORT \u{2014} ccteam capability exceeded");
        assert_eq!(p.kind, EscalateKind::Abort);
        assert_eq!(p.target_phase, None);
        assert_eq!(p.reason, "ccteam capability exceeded");
    }

    #[test]
    fn bare_text_degrades_to_need_user_input() {
        let p = ParsedEscalate::from_reason(" fix-cycle 已 3 轮未通过 ");
        assert_eq!(p.kind, EscalateKind::NeedUserInput);
        assert_eq!(p.target_phase, None);
        assert_eq!(p.reason, "fix-cycle 已 3 轮未通过");
    }

    #[test]
    fn keyword_must_have_word_boundary() {
        // ABORTED is not a real ABORT prefix.
        let p = ParsedEscalate::from_reason("ABORTED a misnomer");
        assert_eq!(p.kind, EscalateKind::NeedUserInput);
        assert_eq!(p.reason, "ABORTED a misnomer");
    }

    #[test]
    fn colon_is_not_a_grammar_separator() {
        // `ABORT:foo` is bare text — the only documented separators
        // are em-dash, --, and ` - `. A colon is not a word boundary
        // for the prefix match, so this should fall through to the
        // bare-text NEED_USER_INPUT branch.
        let p = ParsedEscalate::from_reason("ABORT:foo");
        assert_eq!(p.kind, EscalateKind::NeedUserInput);
        assert_eq!(p.reason, "ABORT:foo");
    }

    #[test]
    fn revert_without_separator_keeps_phase_only() {
        // No dash → entire tail is the phase name, reason empty.
        let p = ParsedEscalate::from_reason("REVERT_TO_PHASE plan-eng");
        assert_eq!(p.kind, EscalateKind::Revert);
        assert_eq!(p.target_phase.as_deref(), Some("plan-eng"));
        assert_eq!(p.reason, "");
    }

    #[test]
    fn empty_reason_after_prefix_is_handled() {
        let p = ParsedEscalate::from_reason("ABORT");
        assert_eq!(p.kind, EscalateKind::Abort);
        assert_eq!(p.reason, "");
    }

    #[test]
    fn insufficient_clarification_with_em_dash() {
        let p = ParsedEscalate::from_reason(
            "INSUFFICIENT_CLARIFICATION \u{2014} 已 3 轮 CLARIFY 未收到目标平台答复",
        );
        assert_eq!(p.kind, EscalateKind::InsufficientClarification);
        assert_eq!(p.target_phase, None);
        assert_eq!(p.reason, "已 3 轮 CLARIFY 未收到目标平台答复");
    }

    #[test]
    fn insufficient_clarification_keyword_serializes_to_snake_case() {
        // Stop hook writes `kind` into progress.jsonl; the snake-case
        // form is the only valid wire shape (interfaces §4.1.1).
        let p = ParsedEscalate::from_reason("INSUFFICIENT_CLARIFICATION");
        assert_eq!(p.kind.as_str(), "insufficient_clarification");
    }

    #[test]
    fn phase_done_pending_parses() {
        let p = ParsedEscalate::from_reason("PHASE_DONE_PENDING -- waiting on storage decision");
        assert_eq!(p.kind, EscalateKind::PhaseDonePending);
        assert_eq!(p.reason, "waiting on storage decision");
        assert_eq!(p.kind.as_str(), "phase_done_pending");
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
