//! `ccteam hook parse-phase-end` — Stop hook handler.
//!
//! Three responsibilities, in order:
//!
//! 1. **Fix-loop ralph-loop pattern** (M0.12, tech-design §3.5). If the
//!    project has a `<project>/.ccteam/auto-loop.state.md`, this hook
//!    drives the loop: read the last assistant text, ask
//!    `auto_loop::decide` whether to re-feed (block exit) or allow
//!    exit (success / iteration cap). Re-injection bumps the
//!    iteration counter in the state file; allow-exit deletes the
//!    state file. Iteration cap without success → `escalate` event.
//!
//! 2. **Normal `PHASE_DONE` / `ESCALATE` parsing** (M0.3). Last
//!    non-empty line of the latest assistant message starts with
//!    `PHASE_DONE:` → `phase_done` event; `ESCALATE:` → `escalate`
//!    event.
//!
//! 3. **V0.2 M0.19 self-loop fallback** (PRD §2.2 / alignment-review
//!    §3.1). A phase ended without producing any of the three legal
//!    outputs — no `PHASE_DONE` / `ESCALATE` in the assistant text and
//!    no fresh outbox file under `<project>/.ccteam/outbox/`. The hook
//!    returns exit 2 + stderr so Claude Code re-injects a coercive
//!    "phase 未正常收尾" prompt and the assistant is forced to pick a
//!    legal output. On the second entry (`stop_hook_active: true`)
//!    the hook stops blocking and writes `needs_attention.outbox.json`
//!    so the watchdog (M0.21) can surface it to the user.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};

use ccteam_core::auto_loop::{self, AutoLoopDecision};
use ccteam_core::{
    progress::{append_event, read_all_events},
    slug_from_project_dir, CcteamPaths,
};

use crate::transcript::{last_assistant_message, message_text};

/// Result of running `parse_phase_end`. The CLI dispatcher emits the
/// Claude Code Stop hook decision JSON when this is `Block`, and exits
/// 2 with the carried stderr message when this is `BlockMissingOutput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDecision {
    /// Default: don't block; let claude continue exiting.
    Continue,
    /// Block the Stop and re-feed `reason` as the next user prompt
    /// (ralph-loop / fix-cycle).
    Block { reason: String },
    /// V0.2 M0.19: phase ended with none of the three legal outputs
    /// (PHASE_DONE / ESCALATE / outbox). The dispatcher prints `stderr`
    /// to fd 2 and exits with code 2. Claude Code interprets exit 2 +
    /// stderr as a blocking system message and re-prompts the model
    /// with the stderr text (`hooks.ts:2784-2805`). The next Stop entry
    /// carries `stop_hook_active: true` and we won't recurse.
    BlockMissingOutput { stderr: String },
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
    let auto_loop_path = auto_loop::path_in(cwd_path);
    if let Some(state) = auto_loop::read(&auto_loop_path)? {
        match auto_loop::decide(&state, &last_text) {
            AutoLoopDecision::Reinject {
                prompt,
                next_iteration,
            } => {
                let mut next = state.clone();
                next.front.iteration = next_iteration;
                next.front.updated_at = Utc::now();
                auto_loop::write(&auto_loop_path, &next)?;
                return Ok(ParseDecision::Block { reason: prompt });
            }
            AutoLoopDecision::AllowExit { succeeded } => {
                auto_loop::delete(&auto_loop_path)?;
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
            // M3.6: PHASE_DONE_PENDING is structurally a phase
            // termination with deferred decisions, not an escalation.
            // Emit a dedicated `phase_done_pending` event so the
            // orchestrator's state machine routes it through the
            // open_decisions check (interfaces §4.1.1, dev-plan M3.6).
            if parsed.kind == EscalateKind::PhaseDonePending {
                let open_decisions = extract_outbox_filenames(&parsed.reason);
                let current_phase = current_phase_from_state(cwd_path)
                    .unwrap_or_default();
                json!({
                    "ts": now_rfc3339(),
                    "event": "phase_done_pending",
                    "phase": current_phase,
                    "open_decisions": open_decisions,
                    "reason": parsed.reason,
                })
            } else {
                json!({
                    "ts": now_rfc3339(),
                    "event": "escalate",
                    "kind": parsed.kind.as_str(),
                    "reason": parsed.reason,
                    "target_phase": parsed.target_phase,
                })
            }
        })
    };

    if let Some(event) = event {
        append_event(&progress_path, &event)?;
        return Ok(ParseDecision::Continue);
    }

    // V0.2 M0.19 self-loop fallback. The phase ended with neither a
    // PHASE_DONE/ESCALATE sigil in the assistant text nor an active
    // ralph-loop state file. Check whether the assistant at least wrote
    // a fresh outbox file (the third legal output); if so, this is a
    // legitimate user-decision pause and we let the orchestrator route
    // it. Otherwise the phase has silently halted — fail loud.
    let phase_started_at = phase_started_at(&progress_path)?;
    let outbox_dir = cwd_path.join(".ccteam").join("outbox");
    let fresh_outbox = fresh_outbox_files(&outbox_dir, phase_started_at)?;

    if !fresh_outbox.is_empty() {
        // Decision pause is legitimate; the orchestrator's existing
        // outbox / decisions queue path takes over from here.
        return Ok(ParseDecision::Continue);
    }

    // No legal output produced. Decide between exit-2 (first Stop) and
    // append-needs-attention (second Stop, recursion guard).
    let stop_hook_active = stdin
        .get("stop_hook_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if stop_hook_active {
        // L3 fail-safe per PRD §2.3: do not block again, just record
        // the stall so the watchdog surfaces it. Claude Code carries
        // `stop_hook_active: true` on the second Stop entry so a phase
        // that keeps producing nothing won't loop forever.
        let pane_tail = capture_pane_tail(&slug, 30);
        write_needs_attention_outbox(
            cwd_path,
            &slug,
            &last_text,
            pane_tail.as_deref(),
        )?;
        return Ok(ParseDecision::Continue);
    }

    eprintln!(
        "[ccteam parse-phase-end] phase未产出 PHASE_DONE/ESCALATE/outbox 任一 (head: {:?})",
        last_text.lines().rev().take(3).collect::<Vec<_>>(),
    );
    Ok(ParseDecision::BlockMissingOutput {
        stderr: SELF_LOOP_FALLBACK_MESSAGE.to_string(),
    })
}

/// V0.2 M0.19: stderr text Claude Code injects back into the model
/// when this hook returns exit 2. Kept short so a single re-prompt
/// fits well within Claude Code's blockingError budget.
const SELF_LOOP_FALLBACK_MESSAGE: &str =
    "phase 未正常收尾。请输出 PHASE_DONE: <phase> / ESCALATE: <reason> / 写 .ccteam/outbox/clarify-<ts>.md 三者之一。\
     直接询问用户的纯文本问句不会被人看见,只会触发本次提示重发。";

/// Walk progress.jsonl for the most recent `phase_inject` event and
/// return its `ts` timestamp. The Stop hook treats this as the lower
/// bound for "outbox file written by *this* phase". `None` (no
/// `phase_inject` yet) means the phase started before progress was
/// initialized — treat every outbox file as fresh.
fn phase_started_at(progress_path: &Path) -> Result<Option<DateTime<Utc>>> {
    if !progress_path.exists() {
        return Ok(None);
    }
    let events = read_all_events(progress_path)?;
    for event in events.iter().rev() {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        if kind == "phase_inject" {
            let ts = event.get("ts").and_then(|s| s.as_str()).unwrap_or("");
            return Ok(DateTime::parse_from_rfc3339(ts)
                .map(|d| d.with_timezone(&Utc))
                .ok());
        }
    }
    Ok(None)
}

/// Return the basenames of outbox files created or modified after
/// `since`. `since: None` collects every outbox file. The Stop hook
/// uses this to detect the third legal phase output.
fn fresh_outbox_files(
    outbox_dir: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(outbox_dir) {
        Ok(it) => it,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(out);
        }
        Err(err) => {
            return Err(err.into());
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        if !name.ends_with(".md") {
            continue;
        }
        let recognised = ["clarify-", "escalation-", "reply-"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        if !recognised {
            continue;
        }
        if let Some(min) = since {
            let stale = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| DateTime::<Utc>::from(t) < min)
                .unwrap_or(false);
            if stale {
                continue;
            }
        }
        out.push(name);
    }
    Ok(out)
}

/// Capture the last `n` lines of the project's tmux pane. Returns
/// `None` if tmux isn't installed, the session is missing, or the
/// invocation otherwise fails. The captured text only ever lands in
/// `needs_attention.outbox.json` as user-facing surface info — never
/// parsed by the orchestrator's state machine (see CLAUDE.md
/// "永不解析 tmux 终端输出" red line).
fn capture_pane_tail(slug: &str, lines: usize) -> Option<String> {
    let session = format!("ccteam-{slug}");
    let output = std::process::Command::new("tmux")
        .args([
            "capture-pane",
            "-p",
            "-t",
            &session,
            "-S",
            &format!("-{lines}"),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// V0.2 M0.19 L3 fail-safe: write `<project>/.ccteam/needs_attention.outbox.json`
/// with the most recent assistant message + pane tail. The watchdog
/// (M0.21) reads it and surfaces it to the user. Idempotent — the file
/// is always overwritten with the latest snapshot.
fn write_needs_attention_outbox(
    cwd: &Path,
    slug: &str,
    last_assistant_message: &str,
    pane_tail: Option<&str>,
) -> Result<()> {
    let dir = cwd.join(".ccteam");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("needs_attention.outbox.json");
    let body = json!({
        "schema_version": 1,
        "ts": now_rfc3339(),
        "slug": slug,
        "reason": "stop_hook_active recursion guard tripped — phase produced no PHASE_DONE/ESCALATE/outbox over two consecutive Stop entries",
        "last_assistant_message": last_assistant_message,
        "pane_tail": pane_tail.unwrap_or(""),
    });
    let pretty = serde_json::to_string_pretty(&body)?;
    std::fs::write(&path, pretty)?;
    Ok(())
}

/// Path the L3 fail-safe writes when the Stop hook recurses. Exposed
/// so tests + the watchdog (M0.21) can locate it without re-deriving
/// the layout.
pub fn needs_attention_outbox_path(cwd: &Path) -> PathBuf {
    cwd.join(".ccteam").join("needs_attention.outbox.json")
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

    #[test]
    fn extract_outbox_filenames_picks_up_clarify_and_escalation_filenames() {
        let r = "deferred storage decision — clarify-2026-05-06-001.md, clarify-tech-stack.md";
        let names = extract_outbox_filenames(r);
        assert_eq!(
            names,
            vec![
                "clarify-2026-05-06-001.md".to_string(),
                "clarify-tech-stack.md".to_string(),
            ],
        );
    }

    #[test]
    fn extract_outbox_filenames_handles_brackets_and_parens() {
        let r = "[clarify-A.md, clarify-B.md] (clarify-C.md)";
        let names = extract_outbox_filenames(r);
        assert_eq!(
            names,
            vec![
                "clarify-A.md".to_string(),
                "clarify-B.md".to_string(),
                "clarify-C.md".to_string(),
            ],
        );
    }

    #[test]
    fn extract_outbox_filenames_skips_unrelated_md_paths() {
        // README.md isn't outbox-shaped → skipped.
        let r = "see clarify-1.md and README.md for context";
        let names = extract_outbox_filenames(r);
        assert_eq!(names, vec!["clarify-1.md".to_string()]);
    }

    #[test]
    fn extract_outbox_filenames_dedupes() {
        let r = "clarify-A.md, again clarify-A.md, and clarify-A.md";
        let names = extract_outbox_filenames(r);
        assert_eq!(names, vec!["clarify-A.md".to_string()]);
    }

    #[test]
    fn extract_outbox_filenames_empty_when_no_outbox_tokens() {
        assert!(extract_outbox_filenames("").is_empty());
        assert!(extract_outbox_filenames("just words and stuff").is_empty());
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Read the current phase string from `<cwd>/.ccteam/state.json`.
/// Returns `None` if the file is missing / unparseable — the Stop
/// hook still emits a `phase_done_pending` event with empty `phase`
/// rather than crashing the hook subprocess. The orchestrator's
/// transition logic matches on the latest event's `phase` field, so
/// an empty value just means "the orchestrator can't infer which
/// phase deferred decisions" and the project sits in InFlight until
/// the user runs `ccteam resume`.
fn current_phase_from_state(cwd: &Path) -> Option<String> {
    let path = ccteam_core::CcteamPaths::project_state_in(cwd);
    let bytes = std::fs::read(&path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("current_phase")
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

/// M3.6: scan free-text for outbox-style filenames so the Stop hook
/// can populate `open_decisions` on the `phase_done_pending` event
/// without forcing the phase markdown to use a strict syntax.
///
/// Matches any whitespace-/comma-/bracket-/paren-separated token
/// whose suffix matches one of the recognised outbox filename prefixes
/// followed by `-...md`. interfaces §3.4.1 documents the canonical
/// `reply-<ts>-<seq>.md` form; informal `clarify-<topic>.md` /
/// `escalation-<topic>.md` are accepted for phase markdown that
/// references decisions by topic rather than timestamp.
///
/// Examples that match:
/// - `PHASE_DONE_PENDING — reply-2026-05-06T100000Z-001.md needs storage call`
/// - `... [clarify-A.md, clarify-B.md] ...`
/// - `... (escalation-stack.md) ...`
pub fn extract_outbox_filenames(reason: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_token in reason
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ',' | '[' | ']' | '(' | ')')
        })
    {
        let token = raw_token.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-');
        if token.is_empty() {
            continue;
        }
        if !token.ends_with(".md") {
            continue;
        }
        let recognised = ["reply-", "clarify-", "escalation-"]
            .iter()
            .any(|prefix| token.starts_with(prefix));
        if recognised && !out.iter().any(|x: &String| x == token) {
            out.push(token.to_string());
        }
    }
    out
}
