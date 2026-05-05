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
        last_line.strip_prefix("ESCALATE:").map(|reason| {
            json!({
                "ts": now_rfc3339(),
                "event": "escalate",
                "reason": reason.trim(),
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

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
